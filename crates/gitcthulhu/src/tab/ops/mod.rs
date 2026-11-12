//! The operation runner (spec 21 §2.5): `OpSpec`, `RepoTab::run_op`, `run_restore`, `cancel_op`, the processing
//! overlay timer, the progress pump and the presentation of results.
//!
//! One operation at a time per tab. The whole backend sequence of an operation runs in ONE background closure:
//! `prepare` (no guard; may ask through the `Asker`) → for `Guard::Exclusive`: `try_lock(2 s)`, watcher suppression,
//! `oplog_prepare`, the backend call, `oplog_finish`, `after` → guard dropped; for `Guard::None` (fetch, push family):
//! the backend call, then the guard only around `oplog_marker`. No dialog is ever shown while the guard is held. Completion (refresh, toasts, `then` / `on_error`) runs on the UI thread inside the tab's update.
//!
//! The catalogue methods live in the submodules (one file per area, each owned by its region package); the ★
//! methods of spec 21 §2.11 are the reference patterns.

#[cfg(test)]
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{App, AppContext as _, Context, SharedString, Window};

use super::{RepoTab, RepoTabEvent};
use crate::backend::handle::{RepoHandle, TabBackend};
use crate::backend::oplog::{OplogErrorInfo, PrepareResult};
use crate::backend::present::{self, ErrorDialogData, ErrorPresentation, OpFailure, RecoveryFlow};
use crate::backend::prompt::{Asker, ConfirmRequest, ProgressPump, op_context, progress_text};
use crate::backend::{
    CancelToken, GitError, GitErrorKind, NetworkMarker, OpContext, OpOutcome, OperationDescriptor,
    OperationResult, OplogRecovery, RefreshSet, Repo, RepoExclusive, RestoreOptions, UndoOutcome,
    UntrackedCapture,
};
use crate::notifications::{ToastAction, ToastKind};

pub mod branch;
pub mod commit;
pub mod config;
pub mod files;
pub mod history;
pub mod kind;
pub mod network;
pub mod remote;
pub mod stash;
pub mod submodule;
pub mod tag;
pub mod undo;
pub mod worktree;

pub use crate::backend::oplog::RestoreKind;
pub use kind::OpKind;

/// How long an operation waits for the repository guard before it fails with `RepoBusy`.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
/// The processing overlay appears after this delay (07 §3.2).
pub const OVERLAY_DELAY: Duration = Duration::from_millis(crate::metrics::PROCESSING_DELAY_MS);
/// The progress line is updated at most this often.
pub const PROGRESS_THROTTLE: Duration = Duration::from_millis(50);

/// Whether an operation holds the repository's exclusive guard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Guard {
    Exclusive,
    /// Only `network::fetch` and the push family (20 §1.5); the guard is taken only around `record_marker`.
    None,
}

/// What the oplog records for an operation.
pub enum Record {
    None,
    /// A snapshot pair; `desc.untracked` is replaced by `Prepared::Continue { untracked: Some(..) }`.
    Snapshot(OperationDescriptor),
    /// A network marker built by `marker` from the push result.
    Marker,
}

/// Whether the processing overlay offers Cancel (20 §1.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cancellable {
    No,
    Yes,
    /// Asks "The operation may stop half-way. Cancel it anyway?" first.
    WithWarning,
}

/// What a backend call of an operation gets.
pub struct OpEnv<'a> {
    pub repo: &'a Repo,
    perm: Option<&'a mut RepoExclusive>,
    pub ctx: &'a OpContext,
}

impl OpEnv<'_> {
    /// The write permission.
    ///
    /// # Panics
    /// With "operation declared Guard::None but needs the guard" when the operation was declared unguarded (a
    /// catalogue bug).
    pub fn perm(&mut self) -> &mut RepoExclusive {
        self.perm
            .as_deref_mut()
            .expect("operation declared Guard::None but needs the guard")
    }
}

/// The result of `prepare`.
pub enum Prepared {
    /// Run the operation; `Some` replaces the snapshot's `UntrackedCapture`.
    Continue { untracked: Option<UntrackedCapture> },
    /// The user declined: nothing runs.
    Cancelled,
}

/// Runs on the background thread BEFORE the guard is taken; may block on the `Asker` (dialogs allowed here only).
pub type OpPrepare = Box<dyn FnOnce(&RepoHandle, &Asker) -> Result<Prepared, OpFailure> + Send>;
/// The backend call.
pub type OpRun<T> = Box<dyn FnOnce(&mut OpEnv<'_>) -> Result<OpOutcome<T>, GitError> + Send>;
/// Push family: builds the network marker; also called on `Err` (rejections carry the full report).
pub type OpMarker<T> = Box<dyn FnOnce(Result<&T, &GitError>) -> Option<NetworkMarker> + Send>;
/// Success only, still under the guard (e.g. `clear_linked_worktrees` after a worktree removal).
pub type OpAfter<T> = Box<dyn FnOnce(&RepoHandle, &mut RepoExclusive, &T) + Send>;
/// Runs on the UI thread after success, inside the tab's update.
pub type OpThen<T> = Box<dyn FnOnce(&mut RepoTab, &T, &mut Window, &mut Context<RepoTab>)>;
/// Runs on the UI thread after a failure; `Handled::Yes` suppresses the standard presentation.
pub type OpOnError =
    Box<dyn FnOnce(&mut RepoTab, &OpFailure, &mut Window, &mut Context<RepoTab>) -> Handled>;
/// A retry offered by the error dialog ("Retry without hooks").
pub type OpRetry = Box<dyn FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>)>;

/// Whether an `on_error` handler took care of a failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handled {
    Yes,
    No,
}

/// Everything about one operation. Build with [`OpSpec::new`] and the builder methods.
pub struct OpSpec<T: Send + 'static> {
    pub kind: OpKind,
    pub guard: Guard,
    pub prepare: Option<OpPrepare>,
    pub record: Record,
    pub cancellable: Cancellable,
    pub run: OpRun<T>,
    pub marker: Option<OpMarker<T>>,
    pub after: Option<OpAfter<T>>,
    /// The Refresh column of 20 §2.6 for this call; refreshed on failure too.
    pub base_refresh: RefreshSet,
    pub extra_refresh: RefreshSet,
    /// `OpContext::label`, shown in credential dialogs ("Push to origin").
    pub label: String,
    /// Overrides the `OpKind` text; `Some("")` = no toast.
    pub success_toast: Option<SharedString>,
    pub then: Option<OpThen<T>>,
    pub on_error: Option<OpOnError>,
    /// Offered by the HookFailed error dialog (commit, merge, push family, rebase only).
    pub retry_without_hooks: Option<OpRetry>,
}

impl<T: Send + 'static> OpSpec<T> {
    /// `Guard::Exclusive`, `Record::None`, `Cancellable::No`, empty base refresh, label = processing title.
    pub fn new(
        kind: OpKind,
        run: impl FnOnce(&mut OpEnv<'_>) -> Result<OpOutcome<T>, GitError> + Send + 'static,
    ) -> Self {
        Self {
            kind,
            guard: Guard::Exclusive,
            prepare: None,
            record: Record::None,
            cancellable: Cancellable::No,
            run: Box::new(run),
            marker: None,
            after: None,
            base_refresh: RefreshSet::empty(),
            extra_refresh: RefreshSet::empty(),
            label: kind.processing_title().to_owned(),
            success_toast: None,
            then: None,
            on_error: None,
            retry_without_hooks: None,
        }
    }

    /// `Guard::None` (fetch and the push family only).
    pub fn unguarded(mut self) -> Self {
        self.guard = Guard::None;
        self
    }

    /// Runs `f` before the guard is taken.
    pub fn prepare(
        mut self,
        f: impl FnOnce(&RepoHandle, &Asker) -> Result<Prepared, OpFailure> + Send + 'static,
    ) -> Self {
        self.prepare = Some(Box::new(f));
        self
    }

    /// Records a snapshot pair described by `desc`.
    pub fn record(mut self, desc: OperationDescriptor) -> Self {
        self.record = Record::Snapshot(desc);
        self
    }

    /// Records a network marker built by `f` (sets `Record::Marker`).
    pub fn marker(
        mut self,
        f: impl FnOnce(Result<&T, &GitError>) -> Option<NetworkMarker> + Send + 'static,
    ) -> Self {
        self.record = Record::Marker;
        self.marker = Some(Box::new(f));
        self
    }

    /// Runs `f` after success, still under the guard.
    pub fn after(
        mut self,
        f: impl FnOnce(&RepoHandle, &mut RepoExclusive, &T) + Send + 'static,
    ) -> Self {
        self.after = Some(Box::new(f));
        self
    }

    /// Sets whether the overlay offers Cancel.
    pub fn cancellable(mut self, c: Cancellable) -> Self {
        self.cancellable = c;
        self
    }

    /// Sets the base refresh set (refreshed on failure too).
    pub fn base_refresh(mut self, r: RefreshSet) -> Self {
        self.base_refresh = r;
        self
    }

    /// Adds categories refreshed after success and failure.
    pub fn refresh(mut self, extra: RefreshSet) -> Self {
        self.extra_refresh |= extra;
        self
    }

    /// Sets the label shown in credential dialogs.
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.label = l.into();
        self
    }

    /// Overrides the success toast; `""` = none.
    pub fn toast(mut self, text: impl Into<SharedString>) -> Self {
        self.success_toast = Some(text.into());
        self
    }

    /// Runs `f` on the UI thread after success.
    pub fn then(
        mut self,
        f: impl FnOnce(&mut RepoTab, &T, &mut Window, &mut Context<RepoTab>) + 'static,
    ) -> Self {
        self.then = Some(Box::new(f));
        self
    }

    /// Runs `f` on the UI thread after a failure.
    pub fn on_error(
        mut self,
        f: impl FnOnce(&mut RepoTab, &OpFailure, &mut Window, &mut Context<RepoTab>) -> Handled
        + 'static,
    ) -> Self {
        self.on_error = Some(Box::new(f));
        self
    }

    /// Offers "Retry without hooks" in the HookFailed error dialog.
    pub fn retry_without_hooks(
        mut self,
        f: impl FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>) + 'static,
    ) -> Self {
        self.retry_without_hooks = Some(Box::new(f));
        self
    }

    /// The oplog title of the operation, if it records a snapshot.
    pub fn title(&self) -> Option<String> {
        match &self.record {
            Record::Snapshot(desc) => Some(desc.title.clone()),
            _ => None,
        }
    }
}

/// The running operation of a tab.
pub struct BusyState {
    pub kind: OpKind,
    pub cancel: CancelToken,
    pub cancellable: Cancellable,
    pub started: Instant,
    /// The 300 ms delay has passed: the processing overlay is shown.
    pub overlay_visible: bool,
    /// The newest progress line ("Receiving objects 42%").
    pub phase: Option<SharedString>,
}

/// The background half of an operation.
struct BgParts<T: Send + 'static> {
    guard: Guard,
    prepare: Option<OpPrepare>,
    record: Record,
    run: OpRun<T>,
    marker: Option<OpMarker<T>>,
    after: Option<OpAfter<T>>,
    base_refresh: RefreshSet,
    extra_refresh: RefreshSet,
}

/// The UI half of an operation.
struct Completion<T: Send + 'static> {
    kind: OpKind,
    success_toast: Option<SharedString>,
    then: Option<OpThen<T>>,
    on_error: Option<OpOnError>,
    retry_without_hooks: Option<OpRetry>,
}

/// What the background half produced.
pub struct BgResult<T> {
    pub result: Result<OpOutcome<T>, OpFailure>,
    pub refresh: RefreshSet,
    /// "{title} can't be undone: {error}" and similar oplog warnings.
    pub warning: Option<String>,
    /// The oplog recorded an entry or marker.
    pub recorded: bool,
}

impl<T> BgResult<T> {
    fn failed(failure: OpFailure, refresh: RefreshSet) -> Self {
        Self {
            result: Err(failure),
            refresh,
            warning: None,
            recorded: false,
        }
    }
}

/// The refresh set after an operation (spec 21 §2.5 step 3): success = the outcome's set ∪ extra; failure = base ∪
/// the error's `refresh_on_failure()` ∪ extra; ∪ UNDO when the oplog recorded something.
pub fn refresh_after<T>(
    result: &Result<OpOutcome<T>, GitError>,
    base: RefreshSet,
    extra: RefreshSet,
    recorded: bool,
) -> RefreshSet {
    let set = match result {
        Ok(outcome) => outcome.refresh | extra,
        Err(err) => base | err.refresh_on_failure() | extra,
    };
    if recorded {
        set | RefreshSet::UNDO
    } else {
        set
    }
}

/// The background sequence of `run_op` (spec 21 §2.5).
fn execute<T: Send>(h: &RepoHandle, p: BgParts<T>, ctx: OpContext, asker: Asker) -> BgResult<T> {
    let mut record = p.record;
    if let Some(prepare) = p.prepare {
        match prepare(h, &asker) {
            Ok(Prepared::Continue {
                untracked: Some(untracked),
            }) => {
                if let Record::Snapshot(desc) = &mut record {
                    desc.untracked = untracked;
                }
            }
            Ok(Prepared::Continue { untracked: None }) => {}
            Ok(Prepared::Cancelled) => {
                return BgResult::failed(OpFailure::Cancelled, RefreshSet::empty());
            }
            Err(failure) => return BgResult::failed(failure, RefreshSet::empty()),
        }
    }
    let (mut warning, mut recorded) = (None, false);
    let result =
        match p.guard {
            Guard::Exclusive => {
                let Some(mut guard) = h.try_lock(LOCK_TIMEOUT) else {
                    return BgResult::failed(OpFailure::RepoBusy, RefreshSet::empty());
                };
                let _suppress = h.suppress_watcher();
                let pending = match record {
                    Record::Snapshot(desc) => {
                        let title = desc.title.clone();
                        match h.oplog_prepare(desc, guard.write_permission()) {
                            PrepareResult::Recording(pending) => Some((pending, title)),
                            PrepareResult::Disabled => None,
                            PrepareResult::Interrupted => {
                                return BgResult::failed(
                                    OpFailure::InterruptedRestore,
                                    RefreshSet::UNDO,
                                );
                            }
                            PrepareResult::Failed(err) => {
                                warning = Some(format!("{title} can't be undone: {err}"));
                                None
                            }
                        }
                    }
                    Record::None | Record::Marker => None,
                };
                let r = (p.run)(&mut OpEnv {
                    repo: &h.repo,
                    perm: Some(guard.write_permission()),
                    ctx: &ctx,
                });
                if let Some((pending, title)) = pending {
                    let outcome = match &r {
                        Ok(_) => OperationResult::Succeeded,
                        Err(e) if e.kind() == GitErrorKind::Cancelled => OperationResult::Cancelled,
                        Err(e) => OperationResult::Failed {
                            message: e.to_string(),
                        },
                    };
                    match h.oplog_finish(pending, outcome, guard.write_permission()) {
                        Ok(()) => recorded = true,
                        Err(err) => warning = Some(format!("{title} can't be undone: {err}")),
                    }
                }
                if let (Ok(outcome), Some(after)) = (&r, p.after) {
                    after(h, guard.write_permission(), &outcome.value);
                }
                r
            }
            Guard::None => {
                let r = (p.run)(&mut OpEnv {
                    repo: &h.repo,
                    perm: None,
                    ctx: &ctx,
                });
                if let Some(marker) = p.marker.and_then(|f| f(r.as_ref().map(|o| &o.value))) {
                    match h.try_lock(LOCK_TIMEOUT) {
                        Some(mut guard) => match h.oplog_marker(marker, guard.write_permission()) {
                            Ok(()) => recorded = true,
                            Err(err) => {
                                warning = Some(format!(
                                    "The push was not recorded in the undo history: {err}"
                                ))
                            }
                        },
                        None => warning = Some(
                            "The push was not recorded in the undo history: the repository is busy"
                                .into(),
                        ),
                    }
                }
                r
            }
        };
    let refresh = refresh_after(&result, p.base_refresh, p.extra_refresh, recorded);
    BgResult {
        result: result.map_err(|e| OpFailure::Git(Arc::new(e))),
        refresh,
        warning,
        recorded,
    }
}

/// What a restore produced (UI side).
pub enum RestoreDone {
    /// The oplog's answer (never `NeedsConfirmation`: the loop asks).
    Outcome(UndoOutcome),
    /// The user declined a confirmation.
    Declined,
}

/// The background loop of `run_restore`: the guard is dropped before every confirmation (spec 21 §2.5).
fn execute_restore(
    h: &RepoHandle,
    kind: RestoreKind,
    asker: &Asker,
) -> Result<RestoreDone, OpFailure> {
    let mut opts = RestoreOptions::default();
    loop {
        let outcome = {
            let Some(mut guard) = h.try_lock(LOCK_TIMEOUT) else {
                break Err(OpFailure::RepoBusy);
            };
            let _suppress = h.suppress_watcher();
            h.oplog_restore(&kind, opts.clone(), guard.write_permission())
        };
        match outcome {
            Ok(UndoOutcome::NeedsConfirmation(confirmation)) => {
                if asker.confirm(present::confirm_restore(&kind, &confirmation.reasons)) {
                    opts.confirmation = Some(confirmation.token);
                } else {
                    break Ok(RestoreDone::Declined);
                }
            }
            Ok(outcome) => break Ok(RestoreDone::Outcome(outcome)),
            Err(OplogErrorInfo::Interrupted) => break Err(OpFailure::InterruptedRestore),
            Err(OplogErrorInfo::Other(text)) => break Err(OpFailure::Other(text)),
        }
    }
}

/// Toast-action hook of the undo UI (X2): the action of a success toast, if any.
fn undo_toast_action(tab: &RepoTab, what: crate::undo::ToastContext) -> Option<ToastAction> {
    crate::undo::toast_action(&tab.ctx, what)
}

impl RepoTab {
    /// The running operation, if any.
    pub fn busy(&self) -> Option<&BusyState> {
        self.busy.as_ref()
    }

    /// Starts an operation. `false` (with a Warning toast) when another one runs. Never blocks the UI thread.
    pub fn run_op<T: Send + 'static>(
        &mut self,
        spec: OpSpec<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.busy.is_some() {
            crate::app_view::show_toast(
                cx,
                Some(self.ctx.id),
                ToastKind::Warning,
                "Another operation is running",
                None,
            );
            return false;
        }
        #[cfg(test)]
        let title = spec.title();
        let OpSpec {
            kind,
            guard,
            prepare,
            record,
            cancellable,
            run,
            marker,
            after,
            base_refresh,
            extra_refresh,
            label,
            success_toast,
            then,
            on_error,
            retry_without_hooks,
        } = spec;
        let completion = Completion {
            kind,
            success_toast,
            then,
            on_error,
            retry_without_hooks,
        };
        let cancel = CancelToken::new();
        self.begin_busy(kind, cancel.clone(), cancellable, window, cx);
        match self.ctx.backend.clone() {
            TabBackend::Real(handle) => {
                let (pump, progress) = ProgressPump::channel();
                self.start_progress_pump(progress, cx);
                let ctx = op_context(cancel, pump, self.prompter.clone(), label);
                let asker = self.asker.clone();
                let parts = BgParts {
                    guard,
                    prepare,
                    record,
                    run,
                    marker,
                    after,
                    base_refresh,
                    extra_refresh,
                };
                let bg = cx.background_spawn(async move { execute(&handle, parts, ctx, asker) });
                self.op_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let result = bg.await;
                    this.update_in(cx, |tab, window, cx| {
                        tab.finish_op(result, completion, window, cx)
                    })
                    .ok();
                }));
            }
            #[cfg(test)]
            TabBackend::Inert(inert) => {
                let _ = (guard, prepare, record, run, marker, after, label);
                inert.record(crate::test_support::TestCall::Op { kind, title });
                let scripted = inert.next_scripted();
                self.op_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let Some(scripted) = scripted else {
                        this.update_in(cx, |tab, _, cx| tab.finish_unscripted(kind, cx))
                            .ok();
                        return;
                    };
                    if !scripted.delay.is_zero() {
                        cx.background_executor().timer(scripted.delay).await;
                    }
                    let result = scripted.into_result::<OpOutcome<T>>();
                    let refresh = match &result {
                        Ok(outcome) => outcome.refresh | extra_refresh,
                        Err(OpFailure::Git(err)) => {
                            base_refresh | err.refresh_on_failure() | extra_refresh
                        }
                        Err(_) => RefreshSet::empty(),
                    };
                    let result = BgResult {
                        result,
                        refresh,
                        warning: None,
                        recorded: false,
                    };
                    this.update_in(cx, |tab, window, cx| {
                        tab.finish_op(result, completion, window, cx)
                    })
                    .ok();
                }));
            }
        }
        true
    }

    fn begin_busy(
        &mut self,
        kind: OpKind,
        cancel: CancelToken,
        cancellable: Cancellable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.op_seq += 1;
        let seq = self.op_seq;
        self.busy = Some(BusyState {
            kind,
            cancel,
            cancellable,
            started: Instant::now(),
            overlay_visible: false,
            phase: None,
        });
        self.emit(RepoTabEvent::OpStarted { kind }, cx);
        self.overlay_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(OVERLAY_DELAY).await;
            this.update_in(cx, |tab, window, cx| {
                if tab.op_seq == seq
                    && let Some(busy) = &mut tab.busy
                {
                    busy.overlay_visible = true;
                    window.focus(&tab.overlay_focus, cx);
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn end_busy(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<BusyState> {
        let busy = self.busy.take();
        self.overlay_task = None;
        self.progress_task = None;
        if busy.as_ref().is_some_and(|b| b.overlay_visible) && self.overlay_focus.is_focused(window)
        {
            window.focus(&self.focus, cx);
        }
        cx.notify();
        busy
    }

    /// Drains the progress pump: keeps the newest line and updates `BusyState::phase` at most every 50 ms.
    fn start_progress_pump(
        &mut self,
        mut progress: futures::channel::mpsc::UnboundedReceiver<crate::backend::Progress>,
        cx: &mut Context<Self>,
    ) {
        let seq = self.op_seq;
        self.progress_task = Some(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(first) = progress.next().await {
                let mut latest = progress_text(&first);
                while let Ok(event) = progress.try_recv() {
                    if let Some(text) = progress_text(&event) {
                        latest = Some(text);
                    }
                }
                if let Some(text) = latest {
                    let alive = this
                        .update(cx, |tab, cx| {
                            if tab.op_seq == seq
                                && let Some(busy) = &mut tab.busy
                            {
                                busy.phase = Some(text);
                                cx.notify();
                            }
                        })
                        .is_ok();
                    if !alive {
                        break;
                    }
                }
                cx.background_executor().timer(PROGRESS_THROTTLE).await;
            }
        }));
    }

    #[cfg(test)]
    fn finish_unscripted(&mut self, kind: OpKind, cx: &mut Context<Self>) {
        self.busy = None;
        self.overlay_task = None;
        self.emit(RepoTabEvent::OpFinished { kind, ok: true }, cx);
        self.after_op(cx);
        cx.notify();
    }

    fn finish_op<T: Send + 'static>(
        &mut self,
        bg: BgResult<T>,
        completion: Completion<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = completion.kind;
        self.end_busy(window, cx);
        let BgResult {
            result,
            mut refresh,
            warning,
            recorded,
        } = bg;
        let stale_diff = matches!(&result, Err(f) if f.git_kind() == Some(GitErrorKind::StaleDiff));
        if stale_diff {
            refresh |= RefreshSet::STATUS;
        }
        self.reload(refresh, cx);
        self.emit(
            RepoTabEvent::OpFinished {
                kind,
                ok: result.is_ok(),
            },
            cx,
        );
        if stale_diff {
            self.emit(RepoTabEvent::ReloadDiff, cx);
        }
        let tab_id = Some(self.ctx.id);
        if let Some(warning) = warning {
            crate::app_view::show_toast(cx, tab_id, ToastKind::Warning, warning, None);
        }
        match result {
            Ok(outcome) => {
                if let Some((text, data)) = present::post_hook_warning(kind, &outcome.warnings) {
                    crate::app_view::show_toast(
                        cx,
                        tab_id,
                        ToastKind::Warning,
                        text,
                        Some(show_output_action(data)),
                    );
                } else {
                    let text = completion
                        .success_toast
                        .clone()
                        .or_else(|| kind.success_toast().map(SharedString::from));
                    if let Some(text) = text.filter(|t| !t.is_empty()) {
                        let action = if recorded {
                            undo_toast_action(self, crate::undo::ToastContext::Recorded { kind })
                        } else {
                            None
                        };
                        crate::app_view::show_toast(cx, tab_id, ToastKind::Positive, text, action);
                    }
                }
                if let Some(then) = completion.then {
                    then(self, &outcome.value, window, cx);
                }
            }
            Err(failure) => {
                let handled = match completion.on_error {
                    Some(on_error) => on_error(self, &failure, window, cx),
                    None => Handled::No,
                };
                if handled == Handled::No {
                    let presentation = present::present(
                        kind,
                        &failure,
                        completion.retry_without_hooks,
                        cx.weak_entity(),
                        cx,
                    );
                    self.apply_presentation(presentation, window, cx);
                }
            }
        }
        self.after_op(cx);
    }

    /// Work queued while an operation ran (undo settings).
    fn after_op(&mut self, cx: &mut Context<Self>) {
        self.after_op_undo_settings(cx);
    }

    /// Shows a presentation of `present` (toast, error dialog, recovery flow).
    pub fn apply_presentation(
        &mut self,
        presentation: ErrorPresentation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match presentation {
            ErrorPresentation::None => {}
            ErrorPresentation::Toast { kind, text } => {
                crate::app_view::show_toast(cx, Some(self.ctx.id), kind, text, None)
            }
            ErrorPresentation::Dialog(data) => crate::app_view::show_error(cx, data),
            ErrorPresentation::Recovery(flow) => self.run_recovery(flow, window, cx),
        }
    }

    /// The generic recovery flows (spec 21 §2.9).
    pub fn run_recovery(
        &mut self,
        flow: RecoveryFlow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = cx.weak_entity();
        match flow {
            RecoveryFlow::UnsafeRepository { path } => {
                let mut request = ConfirmRequest::new(
                    "Trust this repository?",
                    format!(
                        "Git refuses to work in '{}' because it is owned by someone else. Add it to Git's safe.directory list?",
                        path.display()
                    ),
                );
                request.confirm_label = "Trust".into();
                crate::dialogs::confirm::open_with(
                    window,
                    cx,
                    request,
                    Box::new(move |yes, _window, cx| {
                        if !yes {
                            return;
                        }
                        let Some(rt) = crate::app::git_runtime(cx) else {
                            return;
                        };
                        let task = crate::backend::bg(cx, move || {
                            crate::backend::handle::add_safe_directory(&rt, &path)
                        });
                        cx.spawn(async move |cx| {
                            let result = task.await;
                            cx.update(|cx| match result {
                                Ok(()) => {
                                    tab.update(cx, |t, cx| t.reload(RefreshSet::ALL, cx)).ok();
                                }
                                Err(err) => crate::app_view::show_error(
                                    cx,
                                    ErrorDialogData::new(
                                        "Could not trust the repository",
                                        err.to_string(),
                                    ),
                                ),
                            })
                        })
                        .detach();
                    }),
                );
            }
            RecoveryFlow::OplogInterrupted => {
                let spec = crate::dialogs::choice::ChoiceSpec {
                    title: "An interrupted undo must be finished first".into(),
                    message: "An earlier undo or redo was interrupted. Finish it, or roll the repository back to the state before it.".into(),
                    options: vec!["Finish".into(), "Roll back".into()],
                    checkbox: None,
                };
                crate::dialogs::choice::open(
                    window,
                    cx,
                    spec,
                    Box::new(move |choice, window, cx| {
                        let how = match choice.option {
                            Some(0) => OplogRecovery::Complete,
                            Some(1) => OplogRecovery::RollBack,
                            _ => return,
                        };
                        tab.update(cx, |t, cx| t.recover_interrupted(how, window, cx))
                            .ok();
                    }),
                );
            }
        }
    }

    /// Finishes or rolls back an interrupted restore in the background (`try_lock(2 s)` + `oplog_recover`), then
    /// reloads everything.
    pub fn recover_interrupted(
        &mut self,
        how: OplogRecovery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            crate::app_view::show_toast(
                cx,
                Some(self.ctx.id),
                ToastKind::Warning,
                "Another operation is running",
                None,
            );
            return;
        }
        let Some(handle) = self.ctx.backend.handle().cloned() else {
            #[cfg(test)]
            if let TabBackend::Inert(inert) = &self.ctx.backend {
                inert.record(crate::test_support::TestCall::Op {
                    kind: OpKind::RecoverUndo,
                    title: None,
                });
            }
            return;
        };
        self.begin_busy(
            OpKind::RecoverUndo,
            CancelToken::new(),
            Cancellable::No,
            window,
            cx,
        );
        let bg = cx.background_spawn(async move {
            let Some(mut guard) = handle.try_lock(LOCK_TIMEOUT) else {
                return Err(SharedString::from(present::REPO_BUSY));
            };
            let _suppress = handle.suppress_watcher();
            handle.oplog_recover(how, guard.write_permission())
        });
        self.op_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = bg.await;
            this.update_in(cx, |tab, window, cx| {
                tab.end_busy(window, cx);
                tab.reload(RefreshSet::ALL, cx);
                let ok = result.is_ok();
                match result {
                    Ok(()) => crate::app_view::show_toast(
                        cx,
                        Some(tab.ctx.id),
                        ToastKind::Positive,
                        OpKind::RecoverUndo.success_toast().unwrap_or_default(),
                        None,
                    ),
                    Err(text) => crate::app_view::show_error(
                        cx,
                        ErrorDialogData::new(OpKind::RecoverUndo.error_title(), text),
                    ),
                }
                tab.emit(
                    RepoTabEvent::OpFinished {
                        kind: OpKind::RecoverUndo,
                        ok,
                    },
                    cx,
                );
                tab.after_op(cx);
            })
            .ok();
        }));
    }

    /// Undo / Redo / Restore to (17 §4.4): busy handling like `run_op`, not cancellable, `reload(ALL)` afterwards.
    /// Confirmations are asked with the guard released (one background loop).
    pub fn run_restore(
        &mut self,
        kind: RestoreKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.busy.is_some() {
            crate::app_view::show_toast(
                cx,
                Some(self.ctx.id),
                ToastKind::Warning,
                "Another operation is running",
                None,
            );
            return false;
        }
        let op_kind = match kind {
            RestoreKind::Undo => OpKind::Undo,
            RestoreKind::Redo => OpKind::Redo,
            RestoreKind::RestoreTo(_) => OpKind::RestoreTo,
        };
        self.begin_busy(op_kind, CancelToken::new(), Cancellable::No, window, cx);
        match self.ctx.backend.clone() {
            TabBackend::Real(handle) => {
                let asker = self.asker.clone();
                let bg = cx.background_spawn(async move { execute_restore(&handle, kind, &asker) });
                self.op_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let result = bg.await;
                    this.update_in(cx, |tab, window, cx| {
                        tab.finish_restore(kind, result, window, cx)
                    })
                    .ok();
                }));
            }
            #[cfg(test)]
            TabBackend::Inert(inert) => {
                inert.record(crate::test_support::TestCall::Restore(kind));
                let scripted = inert.next_scripted();
                self.op_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let Some(scripted) = scripted else {
                        this.update_in(cx, |tab, _, cx| tab.finish_unscripted(op_kind, cx))
                            .ok();
                        return;
                    };
                    if !scripted.delay.is_zero() {
                        cx.background_executor().timer(scripted.delay).await;
                    }
                    let result = scripted.into_result::<RestoreDone>();
                    this.update_in(cx, |tab, window, cx| {
                        tab.finish_restore(kind, result, window, cx)
                    })
                    .ok();
                }));
            }
        }
        true
    }

    fn finish_restore(
        &mut self,
        kind: RestoreKind,
        result: Result<RestoreDone, OpFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let op_kind = match kind {
            RestoreKind::Undo => OpKind::Undo,
            RestoreKind::Redo => OpKind::Redo,
            RestoreKind::RestoreTo(_) => OpKind::RestoreTo,
        };
        self.end_busy(window, cx);
        let tab_id = Some(self.ctx.id);
        let ok = matches!(result, Ok(RestoreDone::Outcome(UndoOutcome::Done(_))));
        match result {
            Ok(RestoreDone::Outcome(UndoOutcome::Done(report))) => {
                let text = match kind {
                    RestoreKind::Undo => format!("Undone: {}", report.title),
                    RestoreKind::Redo => format!("Redone: {}", report.title),
                    RestoreKind::RestoreTo(_) => "Repository restored".to_owned(),
                };
                let action = undo_toast_action(self, crate::undo::ToastContext::Restored { kind });
                crate::app_view::show_toast(cx, tab_id, ToastKind::Positive, text, action);
                for warning in &report.warnings {
                    crate::app_view::show_toast(
                        cx,
                        tab_id,
                        ToastKind::Warning,
                        present::restore_warning_text(warning),
                        None,
                    );
                }
            }
            Ok(RestoreDone::Outcome(UndoOutcome::Nothing)) => {
                let text = match kind {
                    RestoreKind::Redo => "Nothing to redo",
                    _ => "Nothing to undo",
                };
                crate::app_view::show_toast(cx, tab_id, ToastKind::Positive, text, None);
            }
            Ok(RestoreDone::Outcome(UndoOutcome::Refused(why))) => {
                let title = match kind {
                    RestoreKind::Undo => "Can't undo",
                    RestoreKind::Redo => "Can't redo",
                    RestoreKind::RestoreTo(_) => "Can't restore",
                };
                crate::app_view::show_error(
                    cx,
                    ErrorDialogData::new(title, present::refusal_text(&why)),
                );
            }
            Ok(RestoreDone::Outcome(UndoOutcome::NeedsConfirmation(_)))
            | Ok(RestoreDone::Declined) => {}
            Err(failure) => {
                let presentation = present::present(op_kind, &failure, None, cx.weak_entity(), cx);
                self.apply_presentation(presentation, window, cx);
            }
        }
        self.reload(RefreshSet::ALL, cx);
        self.emit(RepoTabEvent::OpFinished { kind: op_kind, ok }, cx);
        self.after_op(cx);
    }

    /// Overlay Cancel / Escape on the overlay. `WithWarning` first asks "The operation may stop half-way. Cancel it
    /// anyway?".
    pub fn cancel_op(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(busy) = &self.busy else {
            return;
        };
        match busy.cancellable {
            Cancellable::No => {}
            Cancellable::Yes => busy.cancel.cancel(),
            Cancellable::WithWarning => {
                let seq = self.op_seq;
                let tab = cx.weak_entity();
                let mut request = ConfirmRequest::new(
                    "Cancel the operation?",
                    "The operation may stop half-way. Cancel it anyway?",
                );
                request.confirm_label = "Cancel operation".into();
                request.cancel_label = "Keep running".into();
                request.destructive = true;
                crate::dialogs::confirm::open_with(
                    window,
                    cx,
                    request,
                    Box::new(move |yes, _, cx| {
                        if !yes {
                            return;
                        }
                        tab.update(cx, |tab, _| {
                            if tab.op_seq == seq
                                && let Some(busy) = &tab.busy
                            {
                                busy.cancel.cancel();
                            }
                        })
                        .ok();
                    }),
                );
            }
        }
    }

    /// Shows the Info toast of a catalogue method that its owner has not implemented yet.
    pub(crate) fn not_implemented(&self, op: &str, cx: &mut App) {
        crate::app_view::show_toast(
            cx,
            Some(self.ctx.id),
            ToastKind::Positive,
            format!("Not implemented yet: {op}"),
            None,
        );
    }
}

/// The "Show output" toast action: opens the warning-mode error dialog once.
fn show_output_action(data: ErrorDialogData) -> ToastAction {
    let data = Rc::new(RefCell::new(Some(data)));
    ToastAction {
        label: "Show output".into(),
        run: Rc::new(move |_, cx| {
            if let Some(data) = data.borrow_mut().take() {
                crate::app_view::show_error(cx, data);
            }
        }),
    }
}

/// A scripted outcome of the inert test backend (`test_support::script_op`).
#[cfg(test)]
pub struct Scripted {
    pub delay: Duration,
    pub result: Box<dyn Any + Send>,
}

#[cfg(test)]
impl Scripted {
    /// The scripted `Result<V, OpFailure>`.
    ///
    /// # Panics
    /// When the test scripted a different value type than the operation produces (a test bug).
    pub fn into_result<V: 'static>(self) -> Result<V, OpFailure> {
        match self.result.downcast::<Result<V, OpFailure>>() {
            Ok(result) => *result,
            Err(_) => panic!(
                "the scripted outcome has the wrong type, expected Result<{}, OpFailure>",
                std::any::type_name::<V>()
            ),
        }
    }
}
