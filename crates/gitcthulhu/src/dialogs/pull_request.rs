//! The Create pull request dialog (D0.1; owned by X4; spec 21 §5.5).
//!
//! Title (with a "Generate" link when an AI provider is ready), description (prefilled from the repository's pull
//! request template, else from the commit messages of the branch), base branch (filterable, default: the remote's
//! default branch), the head (`owner:branch`, read only), "Draft", and "Push the branch first" (shown and checked
//! when the branch has no upstream or is ahead). Create pushes first (a tab operation, so hooks, credentials and
//! errors behave as everywhere), then creates the review in the background; success shows a toast with
//! "Open in browser". Everything that waits runs off the UI thread.

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div,
};

use crate::backend::forge::{CreatedReview, Failure, NewReview, PrDraft, RepoHandle};
use crate::backend::{ByteSlice as _, FullName};
use crate::dialogs::register::common::{self, FocusOnce};
use crate::forge::flows::describe_failure;
use crate::metrics::dialogs::CLONE_W;
use crate::metrics::dp;
use crate::notifications::{ToastAction, ToastKind};
use crate::tab::{RepoTabEvent, TabCtx};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::dropdown::{FilterDropdown, FilterDropdownEvent};
use crate::widgets::field::{FieldEvent, FilledTextArea, OutlinedField};
use crate::widgets::{CheckboxText, PrimaryButton, TextLink, observed};

/// Height of the description area (21 §5.5).
pub const DESCRIPTION_H: f32 = 200.;
/// Width of the base branch dropdown.
pub const BASE_W: f32 = 240.;

/// Element ids of the dialog (tests, QA).
pub mod ids {
    /// The dialog content.
    pub const DIALOG: &str = "pr-dialog";
    /// The title field.
    pub const TITLE: &str = "pr-title";
    /// The "Generate" link.
    pub const GENERATE: &str = "pr-generate";
    /// The description area.
    pub const DESCRIPTION: &str = "pr-description";
    /// The base branch dropdown (`<id>-button`, `<id>-popup`, ...).
    pub const BASE: &str = "pr-base";
    /// The head line.
    pub const HEAD: &str = "pr-head";
    /// The "Draft" checkbox.
    pub const DRAFT: &str = "pr-draft";
    /// The "Push the branch first" checkbox.
    pub const PUSH: &str = "pr-push";
    /// The error banner.
    pub const ERROR: &str = "pr-error";
    /// The progress line while pushing or creating.
    pub const PROGRESS: &str = "pr-progress";
    /// The Create button.
    pub const CREATE: &str = "pr-create";
    /// The Cancel button.
    pub const CANCEL: &str = "dialog-cancel";
}

/// "feature/add-login_page" as a title: the last path part with spaces, first letter capital.
pub fn title_from_branch(branch: &str) -> String {
    let name = branch.rsplit('/').next().unwrap_or(branch);
    let words = name.replace(['-', '_'], " ");
    let words = words.trim();
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn subject(message: &str) -> &str {
    message.lines().next().unwrap_or_default().trim()
}

/// The title and description a new pull request starts with: with one commit its subject (and body); with several
/// the branch name and a list of the subjects; a template replaces the description.
pub fn default_text(
    branch: &str,
    template: Option<&str>,
    commit_messages: &[String],
) -> (String, String) {
    let title = match commit_messages {
        [only] => subject(only).to_owned(),
        _ => title_from_branch(branch),
    };
    let body = match (template, commit_messages) {
        (Some(template), _) => template.to_owned(),
        (None, [only]) => only
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned(),
        (None, many) => many
            .iter()
            .map(|message| format!("- {}", subject(message)))
            .filter(|line| line.len() > 2)
            .collect::<Vec<_>>()
            .join("\n"),
    };
    (title, body)
}

/// Where the dialog is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The form.
    Form,
    /// The branch is being pushed.
    Pushing,
    /// The forge is creating the review.
    Creating,
}

/// Opens the Create pull request dialog for the local branch `branch`, or for a remote-tracking branch of the push
/// remote (then nothing is pushed).
pub fn open(window: &mut Window, cx: &mut App, tab: TabCtx, branch: FullName) {
    let Some(controller) = crate::forge::controller_of(tab.id, cx) else {
        crate::app_view::show_toast(
            cx,
            Some(tab.id),
            ToastKind::Warning,
            "Pull requests are not available for this repository",
            None,
        );
        return;
    };
    let (handle, usable) = {
        let controller = controller.read(cx);
        (controller.handle().cloned(), controller.can_use_reviews())
    };
    let Some(handle) = handle else {
        crate::app_view::show_toast(
            cx,
            Some(tab.id),
            ToastKind::Warning,
            "The forge of this repository is not detected yet",
            None,
        );
        return;
    };
    if !usable {
        crate::forge::accounts::explain_missing_account(window, cx, &tab);
        return;
    }
    let Some(plan) = Plan::from_model(&tab, &handle, &branch, cx) else {
        crate::app_view::show_toast(
            cx,
            Some(tab.id),
            ToastKind::Warning,
            "The branch does not exist any more",
            None,
        );
        return;
    };
    // The Generate link shows when AI is ready (the hook it calls asks the same hub).
    #[cfg(feature = "services")]
    let ai = {
        crate::ai::hub::ensure(cx);
        crate::app::services(cx).filter(|_| crate::ai::is_ready(cx))
    };
    #[cfg(not(feature = "services"))]
    let ai: Option<crate::backend::services::ServicesHandle> = None;
    let title = cx.new(|cx| OutlinedField::new(ids::TITLE, window, cx).hint("Title", window, cx));
    let description = cx.new(|cx| FilledTextArea::new(ids::DESCRIPTION, "Description", window, cx));
    let base_options: Vec<SharedString> =
        plan.base_names.iter().map(|n| n.clone().into()).collect();
    let base = cx.new(|cx| FilterDropdown::new(ids::BASE, base_options, None, window, cx));
    let view = cx.new(|cx| {
        let subscriptions = vec![
            cx.subscribe_in(
                &title,
                window,
                |this: &mut PullRequestDialog, _, event: &FieldEvent, window, cx| match event {
                    FieldEvent::Changed => {
                        this.error = None;
                        cx.notify();
                    }
                    FieldEvent::Enter => this.create(window, cx),
                },
            ),
            cx.subscribe(
                &description,
                |this: &mut PullRequestDialog, _, event: &FieldEvent, _| {
                    if *event == FieldEvent::Changed {
                        this.description_edited = true;
                    }
                },
            ),
            cx.subscribe(
                &base,
                |this: &mut PullRequestDialog, _, event: &FilterDropdownEvent, cx| {
                    let FilterDropdownEvent::Selected(index) = event;
                    if let Some(name) = this.plan.base_names.get(*index) {
                        this.base_choice = name.clone();
                    }
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &tab.events,
                window,
                |this: &mut PullRequestDialog, _, event: &RepoTabEvent, window, cx| {
                    if let RepoTabEvent::OpFinished {
                        kind: crate::tab::OpKind::Push,
                        ok,
                    } = event
                        && this.phase == Phase::Pushing
                    {
                        this.pushed(*ok, window, cx);
                    }
                },
            ),
        ];
        let mut dialog = PullRequestDialog {
            tab,
            handle,
            controller,
            base_choice: plan.default_base.clone(),
            push: plan.needs_push,
            plan,
            title,
            description,
            base,
            draft: false,
            phase: Phase::Form,
            error: None,
            description_edited: false,
            commit_messages: Vec::new(),
            ai,
            generating: None,
            focus: FocusOnce::default(),
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        dialog.load_draft(window, cx);
        dialog
    });
    #[cfg(all(test, feature = "services"))]
    cx.set_global(OpenDialog(view.downgrade()));
    common::open_view(window, cx, CLONE_W, 640., view);
}

/// The dialog opened last (tests read its state through it).
#[cfg(all(test, feature = "services"))]
pub(crate) struct OpenDialog(pub gpui_kit::WeakEntity<PullRequestDialog>);

#[cfg(all(test, feature = "services"))]
impl gpui_kit::Global for OpenDialog {}

/// What a test sees of the dialog.
#[cfg(all(test, feature = "services"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub title: String,
    pub body: String,
    pub base: String,
    pub base_options: Vec<String>,
    pub head: String,
    pub draft: bool,
    pub push: bool,
    pub shows_push: bool,
    pub phase: Phase,
    pub error: Option<String>,
}

/// A running Generate request.
#[cfg(feature = "services")]
type Generating = crate::ai::AiHandle;
/// Without the integrations there is no AI (the link is never shown).
#[cfg(not(feature = "services"))]
type Generating = crate::backend::CancelToken;

/// What the dialog needs of the branch and the repository, read from the model on the UI thread.
#[derive(Clone, Debug)]
struct Plan {
    /// The local branch (or the remote-tracking branch the dialog was opened for).
    local: FullName,
    /// Its short name.
    name: String,
    /// The branch on the remote: the upstream's, else the local name.
    source: String,
    /// The remote-tracking ref the push updates.
    remote_ref: Option<FullName>,
    /// The branch tip.
    tip: crate::backend::ObjectId,
    /// No upstream, or commits the upstream does not have.
    needs_push: bool,
    /// The branch has an upstream (a push for it keeps it; without one the push sets it).
    has_upstream: bool,
    /// The remote branches of the base remote.
    base_names: Vec<String>,
    /// The default base branch.
    default_base: String,
    /// `owner:branch`.
    head_label: String,
}

impl Plan {
    fn from_model(tab: &TabCtx, handle: &RepoHandle, local: &FullName, cx: &App) -> Option<Plan> {
        let model = tab.model.read(cx);
        let refs = model.refs.value.as_ref()?;
        let facts = handle.facts();
        let base_names = || {
            let mut names: Vec<String> = refs
                .remotes
                .iter()
                .filter(|remote| Some(&remote.name) == facts.base_remote.as_ref())
                .flat_map(|remote| remote.branches.iter().map(|b| b.name.clone()))
                .filter(|name| name != "HEAD")
                .collect();
            names.sort();
            names
        };
        let head_label = |source: &str| match &facts.head_owner {
            Some(owner) => format!("{owner}:{source}"),
            None => source.to_owned(),
        };
        // A remote-tracking branch (the remote branch menu) is already on the forge: nothing to push.
        if let Some(remote) = refs
            .remotes
            .iter()
            .flat_map(|remote| &remote.branches)
            .find(|b| &b.full == local)
        {
            return Some(Plan {
                local: local.clone(),
                name: remote.name.clone(),
                source: remote.name.clone(),
                remote_ref: Some(local.clone()),
                tip: remote.target,
                needs_push: false,
                has_upstream: true,
                base_names: base_names(),
                default_base: String::new(),
                head_label: head_label(&remote.name),
            });
        }
        let branch = refs.local.iter().find(|b| &b.full == local)?;
        let push_remote = branch
            .upstream
            .as_ref()
            .map(|upstream| upstream.remote.clone())
            .or_else(|| facts.push_remote.clone())
            .or_else(|| facts.base_remote.clone());
        let source = branch
            .upstream
            .as_ref()
            .map(|upstream| upstream.merge.shorten().to_str_lossy().into_owned())
            .unwrap_or_else(|| branch.name.clone());
        let remote_ref = push_remote
            .as_ref()
            .and_then(|remote| FullName::try_from(format!("refs/remotes/{remote}/{source}")).ok());
        let tracked_tip = branch
            .upstream
            .as_ref()
            .and_then(|upstream| upstream.tracking.as_ref())
            .and_then(|tracking| {
                refs.remotes
                    .iter()
                    .flat_map(|remote| &remote.branches)
                    .find(|b| &b.full == tracking)
                    .map(|b| b.target)
            });
        let needs_push = match tracked_tip {
            None => true,
            Some(tip) => model
                .ahead_behind
                .get(&(branch.target, tip))
                .map_or(tip != branch.target, |ab| ab.ahead > 0),
        };
        Some(Plan {
            local: local.clone(),
            name: branch.name.clone(),
            head_label: head_label(&source),
            source,
            remote_ref,
            tip: branch.target,
            needs_push,
            has_upstream: branch.upstream.is_some(),
            base_names: base_names(),
            default_base: String::new(),
        })
    }
}

/// The content of an open dialog.
pub struct PullRequestDialog {
    tab: TabCtx,
    handle: RepoHandle,
    controller: Entity<crate::forge::ForgeController>,
    plan: Plan,
    title: Entity<OutlinedField>,
    description: Entity<FilledTextArea>,
    base: Entity<FilterDropdown>,
    base_choice: String,
    draft: bool,
    push: bool,
    phase: Phase,
    error: Option<SharedString>,
    /// The user typed in the description: never replace it.
    description_edited: bool,
    commit_messages: Vec<String>,
    ai: Option<crate::backend::services::ServicesHandle>,
    /// The running Generate request (through the AI hooks: privacy confirmation, QA refusal, cancellation).
    generating: Option<Generating>,
    focus: FocusOnce,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl PullRequestDialog {
    /// Reads the default base branch, the template and the commit messages (background) and fills the form.
    fn load_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.tab.backend.repo().cloned() else {
            // The inert test backend has no repository: the form starts empty.
            self.apply_draft(PrDraft::default(), window, cx);
            return;
        };
        let handle = self.handle.clone();
        let head = self.plan.tip;
        let work = crate::backend::bg(cx, move || handle.draft(&repo, head, None));
        let task = cx.spawn_in(window, async move |this, cx| {
            let draft = work.await;
            this.update_in(cx, |dialog, window, cx| {
                dialog.apply_draft(draft, window, cx)
            })
            .ok();
        });
        self.tasks.push(task);
    }

    fn apply_draft(&mut self, draft: PrDraft, window: &mut Window, cx: &mut Context<Self>) {
        let base = if draft.base.is_empty() {
            "main".to_owned()
        } else {
            draft.base.clone()
        };
        self.plan.default_base = base.clone();
        self.base_choice = base.clone();
        if !self.plan.base_names.contains(&base) {
            self.plan.base_names.insert(0, base.clone());
        }
        let options: Vec<SharedString> = self
            .plan
            .base_names
            .iter()
            .map(|n| n.clone().into())
            .collect();
        let selected = self.plan.base_names.iter().position(|n| *n == base);
        self.base.update(cx, |dropdown, cx| {
            dropdown.set_options(options, selected, cx)
        });
        self.commit_messages = draft.commit_messages.clone();
        let (title, body) = default_text(
            &self.plan.name,
            draft.template_text.as_deref(),
            &draft.commit_messages,
        );
        if common::text_of(&self.title, cx).trim().is_empty() {
            self.title
                .update(cx, |field, cx| field.set_text(&title, window, cx));
        }
        if !self.description_edited {
            self.description
                .update(cx, |area, cx| area.set_text(&body, window, cx));
        }
        cx.notify();
    }

    /// Starts the push (when asked for) and then the creation.
    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.phase != Phase::Form {
            return;
        }
        let title = common::text_of(&self.title, cx).trim().to_owned();
        if title.is_empty() {
            self.error = Some("Enter a title".into());
            cx.notify();
            return;
        }
        if self.base_choice.trim().is_empty() {
            self.error = Some("Choose the branch to merge into".into());
            cx.notify();
            return;
        }
        self.error = None;
        if self.push && self.plan.needs_push {
            let (Some(remote_ref), local) = (self.plan.remote_ref.clone(), self.plan.local.clone())
            else {
                self.error = Some("There is no remote to push the branch to".into());
                cx.notify();
                return;
            };
            let started = std::rc::Rc::new(std::cell::Cell::new(false));
            let flag = started.clone();
            let has_upstream = self.plan.has_upstream;
            self.phase = Phase::Pushing;
            self.tab.with_tab(window, cx, move |tab, window, cx| {
                flag.set(if has_upstream {
                    tab.push_branch_to(local, remote_ref, false, window, cx)
                } else {
                    // A first push sets the upstream, so the toolbar's Push works for the branch afterwards.
                    tab.publish_branch_to(local, remote_ref, window, cx)
                });
            });
            if !started.get() {
                // Another operation is running; it told the user.
                self.phase = Phase::Form;
            }
            cx.notify();
            return;
        }
        self.submit(window, cx);
    }

    /// The push operation finished.
    fn pushed(&mut self, ok: bool, window: &mut Window, cx: &mut Context<Self>) {
        if ok {
            self.submit(window, cx);
        } else {
            // The operation runner showed why.
            self.phase = Phase::Form;
            self.error =
                Some("The branch could not be pushed, so no pull request was created.".into());
            cx.notify();
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.phase = Phase::Creating;
        let review = NewReview {
            title: common::text_of(&self.title, cx).trim().to_owned(),
            body: self.description.read(cx).text(cx).to_string(),
            source_branch: self.plan.source.clone(),
            target_branch: self.base_choice.clone(),
            draft: self.draft,
        };
        let handle = self.handle.clone();
        let work = crate::backend::bg(cx, move || handle.create_review(review));
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |dialog, window, cx| dialog.created(result, window, cx))
                .ok();
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn created(
        &mut self,
        result: Result<CreatedReview, Failure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(created) => {
                let action = created.url.clone().map(|url| ToastAction {
                    label: "Open in browser".into(),
                    run: std::rc::Rc::new(move |_, cx| common::open_url(cx, &url)),
                });
                crate::app_view::show_toast(
                    cx,
                    Some(self.tab.id),
                    ToastKind::Positive,
                    format!("Pull request {}{} created", created.symbol, created.number),
                    action,
                );
                self.controller
                    .update(cx, |controller, cx| controller.refresh_now(cx));
                window.close_dialog(cx);
            }
            Err(failure) => {
                self.phase = Phase::Form;
                self.error = Some(describe_failure(&failure).into());
                cx.notify();
            }
        }
    }

    /// Asks the AI provider for a title and a description (the link is shown only when one is ready), through the
    /// AI hook (spec 21 §5.3): the one-time privacy confirmation for a provider off this machine comes first, and
    /// the text streams into the fields.
    #[cfg(feature = "services")]
    fn generate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.is_none() || self.generating.is_some() {
            return;
        }
        let request = crate::backend::ai::PrRequest {
            title: common::text_of(&self.title, cx),
            body: self.description.read(cx).text(cx).to_string(),
            commit_messages: self.commit_messages.clone(),
        };
        let streamed = cx.weak_entity();
        let finished = cx.weak_entity();
        let handle = crate::ai::generate_pr_text(
            window,
            cx,
            request,
            move |text, window, cx| {
                streamed
                    .update(cx, |dialog, cx| {
                        dialog.show_text(&text.title, &text.body, window, cx)
                    })
                    .ok();
            },
            move |result, window, cx| {
                finished
                    .update(cx, |dialog, cx| {
                        dialog.generating = None;
                        match result {
                            Ok(text) => dialog.show_text(&text.title, &text.body, window, cx),
                            Err(crate::backend::ai::AiFailure::Cancelled) => {}
                            Err(failure) => dialog.error = Some(failure.message().into()),
                        }
                        cx.notify();
                    })
                    .ok();
            },
        );
        self.generating = Some(handle);
        cx.notify();
    }

    #[cfg(not(feature = "services"))]
    fn generate(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// Puts generated text into the fields (an empty part leaves its field alone while the text streams).
    #[cfg(feature = "services")]
    fn show_text(&mut self, title: &str, body: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !title.is_empty() {
            self.title
                .update(cx, |field, cx| field.set_text(title, window, cx));
        }
        if !body.is_empty() {
            self.description
                .update(cx, |area, cx| area.set_text(body, window, cx));
        }
        cx.notify();
    }
}

#[cfg(all(test, feature = "services"))]
impl PullRequestDialog {
    pub(crate) fn snapshot(&self, cx: &App) -> Snapshot {
        Snapshot {
            title: common::text_of(&self.title, cx),
            body: self.description.read(cx).text(cx).to_string(),
            base: self.base_choice.clone(),
            base_options: self.plan.base_names.clone(),
            head: self.plan.head_label.clone(),
            draft: self.draft,
            push: self.push,
            shows_push: self.plan.needs_push,
            phase: self.phase,
            error: self.error.as_ref().map(ToString::to_string),
        }
    }
}

impl Drop for PullRequestDialog {
    fn drop(&mut self) {
        if let Some(cancel) = &self.generating {
            cancel.cancel();
        }
    }
}

impl Render for PullRequestDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        self.focus.run(&self.title, false, window, cx);
        let this = cx.weak_entity();
        let busy = self.phase != Phase::Form;
        let can_create = !busy && !common::text_of(&self.title, cx).trim().is_empty();
        let unit = self.handle.facts().unit_name.clone();
        let heading = format!("Create {unit}");
        let generate = self.ai.as_ref().map(|_| {
            let this = this.clone();
            let label = if self.generating.is_some() {
                "Generating…"
            } else {
                "Generate"
            };
            TextLink::new(ids::GENERATE, label).on_click(move |window, cx| {
                this.update(cx, |dialog, cx| dialog.generate(window, cx))
                    .ok();
            })
        });
        let draft_toggle = {
            let this = this.clone();
            CheckboxText::new(ids::DRAFT, self.draft, "Draft")
                .enabled(!busy)
                .on_toggle(move |checked, _, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.draft = checked;
                        cx.notify();
                    })
                    .ok();
                })
        };
        let push_toggle = self.plan.needs_push.then(|| {
            let this = this.clone();
            CheckboxText::new(ids::PUSH, self.push, "Push the branch first")
                .enabled(!busy)
                .on_toggle(move |checked, _, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.push = checked;
                        cx.notify();
                    })
                    .ok();
                })
        });
        let progress = match self.phase {
            Phase::Form => None,
            Phase::Pushing => Some("Pushing the branch…"),
            Phase::Creating => Some("Creating the pull request…"),
        };
        let create_this = this;
        observed(ids::DIALOG)
            .w(dp(CLONE_W))
            .flex()
            .flex_col()
            .child(
                div()
                    .pb(dp(16.))
                    .text_h4()
                    .semibold()
                    .text_color(tokens.on_background)
                    .child(heading.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(common::form_label("Title", &tokens))
                    .children(generate),
            )
            .child(div().py(dp(8.)).child(self.title.clone()))
            .child(common::form_label("Description", &tokens))
            .child(
                div()
                    .py(dp(8.))
                    .h(dp(DESCRIPTION_H + 16.))
                    .child(self.description.clone()),
            )
            .child(common::form_label("Base branch", &tokens))
            .child(
                div()
                    .py(dp(8.))
                    .flex()
                    .flex_row()
                    .child(div().w(dp(BASE_W)).child(self.base.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(common::form_label("Head", &tokens))
                    .child(
                        observed(ids::HEAD)
                            .pl(dp(16.))
                            .text_body2()
                            .text_color(tokens.on_background_secondary)
                            .child(SharedString::from(self.plan.head_label.clone())),
                    ),
            )
            .child(
                div()
                    .pt(dp(8.))
                    .flex()
                    .flex_col()
                    .child(draft_toggle)
                    .children(push_toggle),
            )
            .when_some(self.error.clone(), |col, message| {
                col.child(observed(ids::ERROR).child(common::error_banner(message, &tokens)))
            })
            .when_some(progress, |col, text| {
                col.child(
                    observed(ids::PROGRESS)
                        .pt(dp(8.))
                        .text_body2()
                        .text_color(tokens.on_background_secondary)
                        .child(text),
                )
            })
            .child(
                div()
                    .pt(dp(16.))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(
                        div().pr(dp(8.)).child(
                            PrimaryButton::new(ids::CANCEL, "Cancel")
                                .cancel()
                                .on_click(|window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(
                        PrimaryButton::new(ids::CREATE, heading)
                            .enabled(can_create)
                            .on_click(move |window, cx| {
                                create_this
                                    .update(cx, |dialog, cx| dialog.create(window, cx))
                                    .ok();
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn titles_come_from_the_branch_name() {
        assert_eq!(
            title_from_branch("feature/add-login_page"),
            "Add login page"
        );
        assert_eq!(title_from_branch("main"), "Main");
        assert_eq!(title_from_branch(""), "");
    }

    #[test]
    fn default_text_follows_the_commits_and_the_template() {
        let one = ["Fix the crash\n\nIt happened on empty input.".to_owned()];
        assert_eq!(
            default_text("fix-crash", None, &one),
            (
                "Fix the crash".to_owned(),
                "It happened on empty input.".to_owned()
            ),
            "one commit: its subject and body"
        );
        let two = ["Add a".to_owned(), "Add b\n\nbody".to_owned()];
        assert_eq!(
            default_text("feature/add-things", None, &two),
            ("Add things".to_owned(), "- Add a\n- Add b".to_owned()),
            "several commits: the branch name and the subjects"
        );
        assert_eq!(
            default_text("x", Some("## Summary\n"), &one).1,
            "## Summary\n",
            "a template replaces the description"
        );
        assert_eq!(
            default_text("x", None, &[]),
            ("X".to_owned(), String::new())
        );
    }
}
