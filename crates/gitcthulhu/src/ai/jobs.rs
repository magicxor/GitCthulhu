//! Running one AI request off the UI thread and delivering its streamed text and result back on it, and the
//! hooks other packages use (pull-request text, branch-name suggestion; spec 21 §5.3, §5.5).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::future::{Either, select};
use gpui_kit::{App, Window};

use super::hub::{self, Readiness};
use super::metrics::STREAM_APPLY_MS;
use crate::app;
use crate::backend::ai::{self as backend_ai, AiFailure, BranchNameContext, PrRequest, PrText};
use crate::backend::services::ServicesHandle;
use crate::backend::{self, CancelToken, Repo};
use crate::notifications::ToastKind;
use crate::settings;
use crate::tab::TabId;

/// A running request. Dropping it cancels the request (the HTTP connection closes at once).
pub struct AiJob {
    cancel: CancelToken,
}

impl AiJob {
    /// Cancels the request; its completion callback then receives [`AiFailure::Cancelled`].
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Whether [`cancel`](Self::cancel) was called.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The request's cancel token (tests watch it).
    #[cfg(test)]
    pub(crate) fn token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl Drop for AiJob {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Runs `run` on a background thread. Items it emits are delivered to `on_item` on the UI thread, only the newest
/// one at a time and at most every [`STREAM_APPLY_MS`]; the result goes to `on_done` as soon as the job ended.
/// No item is delivered after `on_done` (items still queued then are superseded by the result), so a caller never
/// sees a stale partial text replace the final one.
pub(crate) fn drive<T, R>(
    window: &mut Window,
    cx: &mut App,
    run: impl FnOnce(&CancelToken, &mut (dyn FnMut(T) + Send)) -> Result<R, AiFailure> + Send + 'static,
    on_item: impl Fn(T, &mut Window, &mut App) + 'static,
    on_done: impl FnOnce(Result<R, AiFailure>, &mut Window, &mut App) + 'static,
) -> AiJob
where
    T: Send + 'static,
    R: Send + 'static,
{
    let cancel = CancelToken::new();
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<T>();
    let job_cancel = cancel.clone();
    let job = backend::bg(cx, move || {
        let mut emit = |item: T| {
            // The receiver is gone when the window closed: nothing to deliver.
            let _ = tx.unbounded_send(item);
        };
        run(&job_cancel, &mut emit)
    });
    enum Step<T, R> {
        Item(T),
        Closed,
        Done(R),
    }
    let mut job = Box::pin(job);
    window
        .spawn(cx, async move |cx| {
            let result = loop {
                // An item, or the end of the job (whichever comes first; queued items lose to the result).
                let step = match select(rx.next(), &mut job).await {
                    Either::Left((Some(item), _)) => Step::Item(item),
                    Either::Left((None, _)) => Step::Closed,
                    Either::Right((result, _)) => Step::Done(result),
                };
                let mut item = match step {
                    Step::Item(item) => item,
                    // The sender lives in the job's closure: closed means the job has returned.
                    Step::Closed => break (&mut job).await,
                    Step::Done(result) => break result,
                };
                while let Ok(newer) = rx.try_recv() {
                    item = newer;
                }
                if cx.update(|window, cx| on_item(item, window, cx)).is_err() {
                    return;
                }
                let pause = cx
                    .background_executor()
                    .timer(Duration::from_millis(STREAM_APPLY_MS));
                if let Either::Right((result, _)) = select(pause, &mut job).await {
                    break result;
                }
            };
            cx.update(|window, cx| on_done(result, window, cx)).ok();
        })
        .detach();
    AiJob { cancel }
}

/// Handle of a request started through the hooks of this module; cancelling it also withdraws a request that still
/// waits for the privacy confirmation.
#[derive(Clone, Default)]
pub struct AiHandle(Rc<RefCell<HandleState>>);

#[derive(Default)]
struct HandleState {
    job: Option<AiJob>,
    cancelled: bool,
}

impl AiHandle {
    /// Cancels the request (or withdraws it before it started).
    pub fn cancel(&self) {
        let mut state = self.0.borrow_mut();
        state.cancelled = true;
        if let Some(job) = &state.job {
            job.cancel();
        }
    }
}

/// Whether AI generation can run now (the service is configured and ready).
pub fn is_ready(cx: &App) -> bool {
    app::services(cx).is_some() && hub::readiness(cx).is_ready()
}

/// Why this process must not send a request to the selected provider, or `None`: a QA run never contacts a real AI
/// service, whatever a scenario clicks (the guard sits on every path that sends: generation, hooks, connection test).
pub(crate) fn offline_refusal(cx: &App) -> Option<String> {
    #[cfg(feature = "qa")]
    {
        super::qa::refuse_hosted(cx)
    }
    #[cfg(not(feature = "qa"))]
    {
        let _ = cx;
        None
    }
}

/// The `{provider}@{host}` the selected provider would receive code at, when that is not this machine and the user
/// has not confirmed exactly that yet; `None` when no confirmation is due.
fn consent_due(current: &settings::Settings) -> Option<(String, String)> {
    let settings = backend_ai::parse_settings(&current.ai);
    let target = backend_ai::hosted_target(&settings)?;
    (backend_ai::consent_key(&settings).as_deref() != Some(current.ai_hosted_consent.as_str()))
        .then_some(target)
}

/// Whether the privacy confirmation is due: the selected provider is not on this machine and the user has not
/// confirmed this provider at this host yet (changing either asks again).
pub(crate) fn needs_consent(cx: &App) -> bool {
    consent_due(&settings::get_or_default(cx)).is_some()
}

/// Opens the privacy confirmation if the selected provider is not on this machine and the user has not confirmed
/// this provider at this host yet, then runs `then` (at once when no confirmation is due, else on the turn after the
/// answer).
/// Declining, or closing the confirmation without an answer, runs `declined` instead.
pub(crate) fn with_consent(
    window: &mut Window,
    cx: &mut App,
    then: impl FnOnce(&mut Window, &mut App) + 'static,
    declined: impl FnOnce(&mut Window, &mut App) + 'static,
) {
    let current = settings::get_or_default(cx);
    let Some((provider, host)) = consent_due(&current) else {
        then(window, cx);
        return;
    };
    let confirmed = backend_ai::consent_key(&backend_ai::parse_settings(&current.ai));
    let mut request = backend::prompt::ConfirmRequest::new(
        format!("Send your changes to {provider}?"),
        format!(
            "AI features send your changes (file names and changed lines of the staged changes, within a size \
             budget), commit messages and the text you already wrote to {provider}. That text leaves this computer \
             for {host}. Continue only if you may share this code with that service. You are asked again if you change \
             the provider or its host."
        ),
    );
    request.confirm_label = "Send".into();
    // The dialog drops its answer callback when it is closed without an answer: the dropped sender then reads as
    // "declined", so a caller waiting for the outcome always gets one.
    let (answer, answered) = futures::channel::oneshot::channel::<bool>();
    crate::dialogs::confirm::open_with(
        window,
        cx,
        request,
        Box::new(move |yes, _, cx| {
            if yes {
                settings::update(cx, |s| {
                    s.ai_hosted_consent = confirmed.clone().unwrap_or_default();
                });
            }
            let _ = answer.send(yes);
        }),
    );
    window
        .spawn(cx, async move |cx| {
            let yes = answered.await.unwrap_or(false);
            cx.update(|window, cx| {
                if yes {
                    then(window, cx);
                } else {
                    declined(window, cx);
                }
            })
            .ok();
        })
        .detach();
}

/// The completion callback of a hook request.
type Done<R> = Box<dyn FnOnce(Result<R, AiFailure>, &mut Window, &mut App)>;

/// Starts a hook request after the privacy confirmation. Every path ends in exactly one call of `on_done`:
/// declining or cancelling before the start answers [`AiFailure::Cancelled`].
fn start_hook<R: 'static>(
    window: &mut Window,
    cx: &mut App,
    handle: &AiHandle,
    on_done: impl FnOnce(Result<R, AiFailure>, &mut Window, &mut App) + 'static,
    start: impl FnOnce(ServicesHandle, Done<R>, &mut Window, &mut App) -> AiJob + 'static,
) {
    let done: Rc<RefCell<Option<Done<R>>>> = Rc::new(RefCell::new(Some(Box::new(on_done))));
    let declined_done = done.clone();
    let started = handle.clone();
    with_consent(
        window,
        cx,
        move |window, cx| {
            let Some(on_done) = done.borrow_mut().take() else {
                return;
            };
            let cancelled = started.0.borrow().cancelled;
            if let Some(refusal) = offline_refusal(cx) {
                // May run inside the caller's call (no confirmation due): answer on the next turn.
                answer_later(window, cx, AiFailure::Failed(refusal), on_done);
                return;
            }
            match app::services(cx) {
                Some(services) if !cancelled => {
                    let job = start(services, on_done, window, cx);
                    started.0.borrow_mut().job = Some(job);
                }
                // Only reachable after a confirmation (the services were checked just before otherwise), so this
                // runs on a later turn than the caller's call.
                _ => on_done(Err(AiFailure::Cancelled), window, cx),
            }
        },
        move |window, cx| {
            if let Some(on_done) = declined_done.borrow_mut().take() {
                on_done(Err(AiFailure::Cancelled), window, cx);
            }
        },
    );
}

/// Answers `on_done` with `failure` on the next turn of the window, never inside the caller's call: the caller is
/// typically inside an update of its own entity, which `on_done` updates again ("already being updated").
fn answer_later<R: 'static>(
    window: &mut Window,
    cx: &mut App,
    failure: AiFailure,
    on_done: impl FnOnce(Result<R, AiFailure>, &mut Window, &mut App) + 'static,
) {
    window.defer(cx, move |window, cx| on_done(Err(failure), window, cx));
}

/// Why a hook cannot start a request now (the caller tells the user).
fn not_ready_reason(cx: &App) -> Option<String> {
    if app::services(cx).is_none() {
        return Some("AI is unavailable: the integrations did not start".to_owned());
    }
    match hub::readiness(cx) {
        Readiness::Ready { .. } => None,
        _ => Some("AI isn't set up. Choose a provider in Settings > AI.".to_owned()),
    }
}

/// Generates a pull-request title and description for the pull-request dialog (spec 21 §5.5). `on_text` receives
/// the newest text while it streams in, `on_done` the final text or the failure (also `Cancelled`), both on the UI
/// thread, never before this function returned and no `on_text` after `on_done`. Asks for the privacy confirmation
/// first when needed. When AI is not usable `on_done` runs on the next turn with the reason.
pub fn generate_pr_text(
    window: &mut Window,
    cx: &mut App,
    request: PrRequest,
    on_text: impl Fn(PrText, &mut Window, &mut App) + 'static,
    on_done: impl FnOnce(Result<PrText, AiFailure>, &mut Window, &mut App) + 'static,
) -> AiHandle {
    let handle = AiHandle::default();
    if let Some(reason) = not_ready_reason(cx) {
        answer_later(window, cx, AiFailure::Failed(reason), on_done);
        return handle;
    }
    start_hook(
        window,
        cx,
        &handle,
        on_done,
        move |services, on_done, window, cx| {
            drive(
                window,
                cx,
                move |cancel, emit| {
                    let mut forward = |text: &PrText| emit(text.clone());
                    backend_ai::generate_pr_text(&services, &request, cancel, &mut forward)
                },
                on_text,
                on_done,
            )
        },
    );
    handle
}

/// What a branch name is suggested from.
pub enum BranchNameSource {
    /// The staged changes of this repository.
    Staged(Repo),
    /// A commit message (the commit being branched from).
    CommitMessage(String),
}

/// Suggests a branch name for the new-branch dialog; the caller prefills the name field and never creates the
/// branch itself. `on_done` receives the name or the failure on the UI thread, never before this function
/// returned; when AI is not usable it runs on the next turn with the reason.
pub fn suggest_branch_name(
    window: &mut Window,
    cx: &mut App,
    source: BranchNameSource,
    on_done: impl FnOnce(Result<String, AiFailure>, &mut Window, &mut App) + 'static,
) -> AiHandle {
    let handle = AiHandle::default();
    if let Some(reason) = not_ready_reason(cx) {
        answer_later(window, cx, AiFailure::Failed(reason), on_done);
        return handle;
    }
    start_hook(
        window,
        cx,
        &handle,
        on_done,
        move |services, on_done, window, cx| {
            drive(
                window,
                cx,
                move |cancel, _emit: &mut (dyn FnMut(()) + Send)| match &source {
                    BranchNameSource::Staged(repo) => {
                        backend_ai::suggest_branch_name_for_staged(&services, repo, cancel)
                    }
                    BranchNameSource::CommitMessage(message) => backend_ai::suggest_branch_name(
                        &services,
                        &BranchNameContext::CommitMessage(message.clone()),
                        cancel,
                    ),
                },
                |_, _, _| {},
                on_done,
            )
        },
    );
    handle
}

/// Shows a generation failure as an Error toast of `tab`, with an "Open AI settings" action when the settings are
/// the problem.
pub(crate) fn show_failure(cx: &mut App, tab: Option<TabId>, what: &str, failure: &AiFailure) {
    let action =
        matches!(failure, AiFailure::NotConfigured(_)).then(|| crate::notifications::ToastAction {
            label: "Open AI settings".into(),
            run: Rc::new(|window, cx| {
                crate::dialogs::settings::open(
                    window,
                    cx,
                    Some(crate::dialogs::settings::SettingsPage::Ai),
                );
            }),
        });
    let text = match failure {
        AiFailure::NotConfigured(_) | AiFailure::NothingStaged => failure.message(),
        other => format!("{what}: {}", other.message()),
    };
    crate::app_view::show_toast(cx, tab, ToastKind::Error, text, action);
}
