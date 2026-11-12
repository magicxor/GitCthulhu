//! The commit box (owned by G5; 08 §4.6, spec 21 §5.3): the message field, the Amend checkbox, the Commit button
//! and the state buttons of merge, rebase, cherry-pick and revert. The public message API is used by the AI
//! controller (X3); `ai::commit_box_slot` is called at the right end of the checkbox row.
//!
//! Which controls show and when they are enabled is the pure function [`layout`] of [`BoxInputs`], tested apart
//! from rendering. The message draft is saved 1 s after the last keystroke (05 §2.16) and loaded again by the
//! tab's REPO_STATE load; the box adopts it when the repository state changes or while the field is blank.

use std::time::Duration;

use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div,
};

use crate::app::AppState;
use crate::backend::{CommitOptions, InProgress, ObjectId, RepoState, bg, status};
use crate::dialogs::author::{IdentityChoice, open_commit_identity};
use crate::metrics::{self, dp, status as m};
use crate::settings::Signoff;
use crate::tab::model::RepoModelEvent;
use crate::tab::ops::OpKind;
use crate::tab::ui::RepoUiEvent;
use crate::tab::{RepoTabEvent, TabCtx};
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::widgets::{CheckboxText, observed};

gpui_kit::actions!(gitcthulhu_status, [SubmitCommit]);

/// Element id of the commit box.
pub const COMMIT_BOX: &str = "commit-box";
/// Key context of the commit box; Ctrl+Enter is bound in `"CommitBox > Input"` (the kit binds the same chord
/// inside every input, and the deeper context wins).
pub const COMMIT_BOX_CONTEXT: &str = "CommitBox";

/// Element ids inside the commit box.
pub mod ids {
    /// The message field (frame around the text area).
    pub const MESSAGE: &str = "commit-message";
    /// The row that holds the Amend checkbox and the AI slot.
    pub const CHECKBOX_ROW: &str = "commit-checkbox-row";
    /// The Amend checkbox (its clickable row).
    pub const AMEND: &str = "commit-amend";
    /// The main button: Commit, Amend, Merge, Continue or Skip.
    pub const PRIMARY: &str = "commit-primary";
    /// The Abort button.
    pub const ABORT: &str = "commit-abort";
    /// The row of the main button (and of Abort).
    pub const BUTTON_ROW: &str = "commit-button-row";
}

/// Registers the box's key bindings (G5).
pub fn register(cx: &mut App) {
    cx.bind_keys([gpui_kit::KeyBinding::new(
        "secondary-enter",
        SubmitCommit,
        Some("CommitBox > Input"),
    )]);
}

// ── what the box shows ──────────────────────────────────────────────────────────────────────────

/// The repository state as far as the box cares (08 §4.6 table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxMode {
    /// SAFE: no operation in progress.
    Safe,
    /// MERGING.
    Merging,
    /// REBASING; `amendable` at an `edit` stop.
    Rebasing { amendable: bool },
    /// CHERRY_PICKING; `sequence` = more than one commit.
    CherryPicking { sequence: bool },
    /// REVERTING; `sequence` = more than one commit.
    Reverting { sequence: bool },
}

impl BoxMode {
    /// The mode of a repository state (`None` before it is loaded: SAFE).
    pub fn of(state: Option<&RepoState>) -> Self {
        let Some(state) = state else {
            return BoxMode::Safe;
        };
        match state.in_progress {
            Some(InProgress::Merge) => BoxMode::Merging,
            Some(InProgress::CherryPick) => BoxMode::CherryPicking { sequence: false },
            Some(InProgress::CherryPickSequence) => BoxMode::CherryPicking { sequence: true },
            Some(InProgress::Revert) => BoxMode::Reverting { sequence: false },
            Some(InProgress::RevertSequence) => BoxMode::Reverting { sequence: true },
            Some(
                InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase,
            ) => BoxMode::Rebasing {
                amendable: state.rebase.as_ref().is_some_and(|r| r.amend.is_some()),
            },
            Some(InProgress::ApplyMailbox | InProgress::Bisect) | None => BoxMode::Safe,
        }
    }
}

/// Everything [`layout`] decides on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxInputs {
    /// The repository state.
    pub mode: BoxMode,
    /// Staged files.
    pub staged: usize,
    /// Unstaged files (conflicts included).
    pub unstaged: usize,
    /// Conflicted files in the unstaged list.
    pub conflicted: usize,
    /// HEAD points to a commit (Amend needs one).
    pub has_head: bool,
    /// The Amend checkbox is ticked.
    pub amend: bool,
    /// The message is empty or whitespace.
    pub message_blank: bool,
}

/// What the main button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primary {
    /// `git commit`.
    Commit,
    /// `git commit --amend`.
    Amend,
    /// Concludes a merge.
    Merge,
    /// Concludes the cherry-pick (one commit) or continues the sequence.
    PickCommit,
    /// Continues the revert.
    RevertContinue,
    /// Continues the rebase (amending first at an `edit` stop when the box says so).
    RebaseContinue,
    /// Skips the rebase step.
    Skip,
}

/// The controls of the box for one state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxLayout {
    /// The 28 dp row for the Amend checkbox and the AI slot exists.
    pub checkbox_row: bool,
    /// The Amend checkbox shows in it.
    pub amend_checkbox: bool,
    /// The Abort button shows next to the main button.
    pub abort: bool,
    /// What the main button does.
    pub primary: Primary,
    /// Its label.
    pub label: &'static str,
    /// Whether it can be clicked.
    pub enabled: bool,
    /// The message field is read-only (a rebase step that cannot be amended).
    pub read_only: bool,
}

/// 08 §4.6: the controls and their enabled state per repository state.
pub fn layout(i: &BoxInputs) -> BoxLayout {
    let has_message = !i.message_blank;
    match i.mode {
        BoxMode::Safe => BoxLayout {
            checkbox_row: true,
            amend_checkbox: i.has_head,
            abort: false,
            primary: if i.amend {
                Primary::Amend
            } else {
                Primary::Commit
            },
            label: if i.amend { "Amend" } else { "Commit" },
            enabled: has_message && if i.amend { i.has_head } else { i.staged > 0 },
            read_only: false,
        },
        BoxMode::Merging => BoxLayout {
            checkbox_row: false,
            amend_checkbox: false,
            abort: true,
            primary: Primary::Merge,
            label: "Merge",
            enabled: i.unstaged == 0 && has_message,
            read_only: false,
        },
        BoxMode::Rebasing { amendable } => {
            let amending = amendable && i.amend && has_message;
            let show_continue = i.staged > 0 || i.unstaged > 0 || amending;
            BoxLayout {
                checkbox_row: amendable,
                amend_checkbox: amendable && i.has_head,
                abort: true,
                primary: if show_continue {
                    Primary::RebaseContinue
                } else {
                    Primary::Skip
                },
                label: if show_continue { "Continue" } else { "Skip" },
                enabled: !show_continue || i.unstaged == 0,
                read_only: !amendable,
            }
        }
        BoxMode::CherryPicking { .. } => BoxLayout {
            checkbox_row: false,
            amend_checkbox: false,
            abort: true,
            primary: Primary::PickCommit,
            label: "Commit",
            enabled: i.unstaged == 0 && has_message,
            read_only: false,
        },
        BoxMode::Reverting { .. } => BoxLayout {
            checkbox_row: false,
            amend_checkbox: false,
            abort: true,
            primary: Primary::RevertContinue,
            label: "Continue",
            enabled: has_message && i.conflicted == 0,
            read_only: false,
        },
    }
}

/// What the box last adopted a draft for: a new key means the repository entered or left an operation (or the
/// rebase moved to another `edit` stop).
#[derive(Clone, Debug, PartialEq, Eq)]
struct AdoptKey {
    in_progress: Option<InProgress>,
    amend: Option<ObjectId>,
}

/// Where a fetched HEAD message goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// Only into a blank field (Amend prefill).
    IfBlank,
    /// Always (the read-only message of a rebase step).
    Replace,
}

#[cfg(test)]
type SlotFn =
    fn(&TabCtx, gpui_kit::WeakEntity<CommitBox>, &mut Window, &mut App) -> Option<AnyElement>;

#[cfg(test)]
thread_local! {
    /// Tests put a test element into the AI slot.
    pub(crate) static SLOT_OVERRIDE: std::cell::Cell<Option<SlotFn>> = const { std::cell::Cell::new(None) };
}

/// The commit box of a status pane.
pub struct CommitBox {
    ctx: TabCtx,
    input: Entity<TextareaState>,
    /// The AI controller streams into the field.
    api_read_only: bool,
    seen: Option<AdoptKey>,
    save_task: Option<Task<()>>,
    fill_generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl CommitBox {
    /// The commit box of the tab of `ctx`.
    pub fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.schedule_save(cx);
                    }
                },
            ),
            cx.subscribe_in(
                &ctx.model,
                window,
                |this, _, event: &RepoModelEvent, window, cx| {
                    if event.0.intersects(crate::backend::RefreshSet::REPO_STATE) {
                        this.adopt(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &ctx.events,
                window,
                |this, _, event: &RepoTabEvent, window, cx| match event {
                    RepoTabEvent::OpFinished { kind, ok } => {
                        this.on_op_finished(*kind, *ok, window, cx)
                    }
                    RepoTabEvent::FocusCommitMessage => this.focus(window, cx),
                    _ => {}
                },
            ),
            // "Show status to amend this commit" (log menu) turns Amend on from outside the box: prefill like the
            // checkbox does (08 §2.13).
            cx.subscribe_in(
                &ctx.ui,
                window,
                |this, ui, event: &RepoUiEvent, window, cx| {
                    if matches!(event, RepoUiEvent::TogglesChanged)
                        && ui.read(cx).amend
                        && this.message(cx).trim().is_empty()
                    {
                        this.fetch_head_message(Fill::IfBlank, window, cx);
                    }
                },
            ),
            cx.observe(&ctx.model, |_, _, cx| cx.notify()),
            cx.observe(&ctx.ui, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            ctx,
            input,
            api_read_only: false,
            seen: None,
            save_task: None,
            fill_generation: 0,
            _subscriptions: subscriptions,
        };
        this.adopt(window, cx);
        this
    }

    /// The tab of this box.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The current message text.
    pub fn message(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// Replaces the message text (streamed AI text, Amend prefill) and schedules the draft save.
    pub fn set_message(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.message(cx) != text {
            self.apply_text(text, window, cx);
            self.schedule_save(cx);
        }
    }

    /// Makes the message field read-only (while AI text streams in).
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if self.api_read_only != read_only {
            self.api_read_only = read_only;
            cx.notify();
        }
    }

    /// Whether the message field is read-only: while the AI controller streams, or at a rebase step that cannot
    /// be amended.
    pub fn is_read_only(&self) -> bool {
        self.api_read_only || self.state_read_only_cached()
    }

    fn state_read_only_cached(&self) -> bool {
        self.seen.as_ref().is_some_and(|key| {
            key.in_progress.is_some_and(|p| {
                matches!(
                    p,
                    InProgress::Rebase
                        | InProgress::RebaseInteractive
                        | InProgress::ApplyMailboxRebase
                )
            }) && key.amend.is_none()
        })
    }

    /// Moves keyboard focus into the message field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn apply_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    // ── inputs of the layout ─────────────────────────────────────────────────────────────────

    fn inputs(&self, cx: &App) -> BoxInputs {
        let model = self.ctx.model.read(cx);
        let (staged, unstaged, conflicted) = model.status.value.as_ref().map_or((0, 0, 0), |s| {
            (
                s.staged.len(),
                s.unstaged.len(),
                s.counts.unstaged.conflicted as usize,
            )
        });
        let state = model.state.value.as_deref();
        BoxInputs {
            mode: BoxMode::of(state),
            staged,
            unstaged,
            conflicted,
            has_head: state
                .is_none_or(|s| !matches!(s.head, crate::backend::HeadState::Unborn { .. })),
            amend: self.ctx.ui.read(cx).amend,
            message_blank: self.input.read(cx).value().trim().is_empty(),
        }
    }

    // ── draft ────────────────────────────────────────────────────────────────────────────────

    /// Takes over the persisted draft after a REPO_STATE load: always when the repository entered or left an
    /// operation (the message of a merge, the message of an `edit` stop), otherwise only into a blank field, so
    /// a late load never overwrites what the user types (05 §2.16).
    fn adopt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (key, draft) = {
            let model = self.ctx.model.read(cx);
            let Some(state) = model.state.value.as_ref() else {
                return;
            };
            (
                AdoptKey {
                    in_progress: state.in_progress,
                    amend: state.rebase.as_ref().and_then(|r| r.amend),
                },
                model.draft_message.clone(),
            )
        };
        let transition = self.seen.as_ref() != Some(&key);
        if transition {
            // A save scheduled before the repository entered or left an operation would write the half-typed text
            // into the new state's message (MERGE_MSG): the new state's draft wins.
            self.save_task = None;
        }
        self.seen = Some(key.clone());
        let read_only_step = self.state_read_only_cached();
        if transition {
            let amendable = key.amend.is_some();
            self.ctx.ui.update(cx, |ui, cx| {
                if ui.amend != amendable {
                    ui.amend = amendable;
                    cx.notify();
                }
            });
        }
        if read_only_step {
            // Nothing is typed into a read-only step, and HEAD moves from one stopped step to the next without the
            // key changing (two conflict stops in a row; the box was cleared after Continue): re-read every time.
            self.fetch_head_message(Fill::Replace, window, cx);
            return;
        }
        if transition || self.message(cx).trim().is_empty() {
            let text = draft.unwrap_or_default();
            if self.message(cx) != text {
                self.apply_text(text, window, cx);
            }
        }
    }

    fn saving_blocked(&self) -> bool {
        self.seen.as_ref().is_some_and(|key| {
            matches!(
                key.in_progress,
                Some(
                    InProgress::Rebase
                        | InProgress::RebaseInteractive
                        | InProgress::ApplyMailboxRebase
                )
            )
        })
    }

    /// Saves the draft 1 s after the last change.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.ctx.backend.repo().is_none() || self.saving_blocked() {
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(m::DRAFT_SAVE_MS))
                .await;
            this.update(cx, |this, cx| this.flush_draft(cx)).ok();
        }));
    }

    fn flush_draft(&mut self, cx: &mut Context<Self>) {
        self.save_task = None;
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            return;
        };
        let text = self.message(cx);
        let task = bg(cx, move || {
            let message = (!text.trim().is_empty()).then_some(text);
            status::save_draft(&repo, message.as_deref())
        });
        cx.spawn(async move |_, _| {
            if let Err(err) = task.await {
                tracing::warn!("cannot save the commit message draft: {err}");
            }
        })
        .detach();
    }

    /// Loads the HEAD commit's message into the field (Amend prefill, read-only rebase step).
    fn fetch_head_message(&mut self, fill: Fill, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            return;
        };
        self.fill_generation += 1;
        let generation = self.fill_generation;
        let task = bg(cx, move || status::head_message(&repo));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                if this.fill_generation != generation {
                    return;
                }
                match result {
                    Ok(Some(message))
                        if fill == Fill::Replace || this.message(cx).trim().is_empty() =>
                    {
                        this.apply_text(message, window, cx)
                    }
                    Ok(_) => {}
                    Err(err) => tracing::warn!("cannot read the HEAD commit message: {err}"),
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_op_finished(
        &mut self,
        kind: OpKind,
        ok: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let concluding = matches!(
            kind,
            OpKind::Commit
                | OpKind::Amend
                | OpKind::ContinueOperation
                | OpKind::SkipRebase
                | OpKind::ResetRepositoryState
        );
        if !concluding {
            return;
        }
        if ok {
            // The operation consumed the message; the next REPO_STATE load brings the draft of what follows.
            self.save_task = None;
            self.fill_generation += 1;
            if !self.message(cx).is_empty() {
                self.apply_text(String::new(), window, cx);
            }
            self.ctx.ui.update(cx, |ui, cx| {
                if ui.amend {
                    ui.amend = false;
                    cx.notify();
                }
            });
        } else if matches!(kind, OpKind::Commit | OpKind::Amend) {
            self.schedule_save(cx);
        }
    }

    // ── actions ──────────────────────────────────────────────────────────────────────────────

    fn set_amend(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.ctx.ui.update(cx, |ui, cx| {
            ui.amend = on;
            cx.notify();
        });
        if on && self.message(cx).trim().is_empty() {
            self.fetch_head_message(Fill::IfBlank, window, cx);
        }
    }

    fn signoff_pref(&self, cx: &App) -> Signoff {
        let workdir = self.ctx.backend.handle().and_then(|h| h.workdir());
        match (cx.try_global::<AppState>(), workdir) {
            (Some(state), Some(dir)) => state.repo_prefs.get(&dir).signoff,
            _ => Signoff::default(),
        }
    }

    /// Commits `opts`: asks for an identity first when Git has none (09 §3.11), applies sign-off (after the
    /// identity is known, so a custom text gets the chosen name), then runs the catalogue's `commit`
    /// (`amend_then_continue` at an amendable rebase stop).
    fn commit(
        &mut self,
        opts: CommitOptions,
        continue_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_task = None;
        let signoff = self.signoff_pref(cx);
        let identity_missing = self
            .ctx
            .model
            .read(cx)
            .identity
            .value
            .as_ref()
            .is_some_and(|i| i.effective.is_none());
        let ctx = self.ctx.clone();
        let run = move |mut opts: CommitOptions, window: &mut Window, cx: &mut App| {
            apply_signoff(&signoff, &mut opts, &ctx, cx);
            ctx.with_tab(window, cx, move |tab, window, cx| {
                if continue_after {
                    tab.amend_then_continue(opts, window, cx);
                } else {
                    tab.commit(opts, window, cx);
                }
            });
        };
        if identity_missing && opts.identity_override.is_none() {
            open_commit_identity(
                window,
                cx,
                self.ctx.clone(),
                std::rc::Rc::new(move |choice, window, cx| {
                    let next = match choice {
                        IdentityChoice::Override(identity) => CommitOptions {
                            identity_override: Some(identity),
                            ..opts.clone()
                        },
                        IdentityChoice::Saved => opts.clone(),
                    };
                    run(next, window, cx);
                }),
            );
        } else {
            run(opts, window, cx);
        }
    }

    /// The main button (also Ctrl+Enter).
    pub fn run_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = self.inputs(cx);
        let layout = layout(&inputs);
        if !layout.enabled {
            return;
        }
        let message = self.message(cx);
        let options = |amend: bool, allow_empty: bool| CommitOptions {
            message: message.clone(),
            amend,
            allow_empty,
            ..CommitOptions::default()
        };
        match layout.primary {
            Primary::Commit => self.commit(options(false, false), false, window, cx),
            Primary::Amend => self.commit(options(true, false), false, window, cx),
            Primary::Merge => self.commit(options(false, true), false, window, cx),
            Primary::PickCommit | Primary::RevertContinue => {
                let sequence = matches!(
                    inputs.mode,
                    BoxMode::CherryPicking { sequence: true }
                        | BoxMode::Reverting { sequence: true }
                );
                if sequence {
                    self.save_task = None;
                    self.ctx.with_tab(window, cx, move |tab, window, cx| {
                        tab.continue_operation(Some(message), window, cx);
                    });
                } else {
                    self.commit(options(false, false), false, window, cx);
                }
            }
            Primary::RebaseContinue => {
                let amending = matches!(inputs.mode, BoxMode::Rebasing { amendable: true })
                    && inputs.amend
                    && !inputs.message_blank;
                if amending {
                    self.commit(options(true, false), true, window, cx);
                } else {
                    self.ctx.with_tab(window, cx, |tab, window, cx| {
                        tab.continue_operation(None, window, cx);
                    });
                }
            }
            Primary::Skip => self.ctx.with_tab(window, cx, |tab, window, cx| {
                tab.skip_operation(window, cx);
            }),
        }
    }

    /// Ctrl+Enter in the message field (08 §4.6: "commit if Commit/Amend would be enabled"): the main button, but
    /// never from a read-only field (a stopped rebase step, or AI text still streaming in) and never Skip, which
    /// drops a commit and is not what a "submit the message" chord should do.
    fn submit_from_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_read_only() || layout(&self.inputs(cx)).primary == Primary::Skip {
            return;
        }
        self.run_primary(window, cx);
    }

    fn abort(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ctx.with_tab(window, cx, |tab, window, cx| {
            tab.abort_operation(window, cx);
        });
    }

    // ── rendering ────────────────────────────────────────────────────────────────────────────

    fn ai_slot(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let weak = cx.entity().downgrade();
        #[cfg(test)]
        if let Some(slot) = SLOT_OVERRIDE.with(|s| s.get()) {
            return slot(&self.ctx, weak, window, cx);
        }
        crate::ai::commit_box_slot(&self.ctx, weak, window, cx)
    }

    fn message_area(
        &self,
        window: &Window,
        read_only: bool,
        label: &'static str,
        blank: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *cx.tokens();
        let focus = self.input.read(cx).focus_handle(cx);
        let focused = focus.contains_focused(window, cx);
        let floating = focused || !blank;
        let fill = tokens.background.blend(tokens.on_background.alpha(0.04));
        let indicator = if focused {
            tokens.primary_variant
        } else {
            tokens.on_background.alpha(0.42)
        };
        let input = self.input.clone();
        let focus_input = self.input.clone();
        div()
            .id(ids::MESSAGE)
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(fill)
            .rounded_t(dp(metrics::RADIUS))
            .border_b(dp(if focused { 2. } else { 1. }))
            .border_color(indicator)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                focus_input.update(cx, |input, cx| input.focus(window, cx));
            })
            .child(
                div()
                    .absolute()
                    .left(dp(m::MESSAGE_PAD_H))
                    .top(dp(if floating {
                        m::MESSAGE_LABEL_TOP_FLOATING
                    } else {
                        m::MESSAGE_LABEL_TOP_RESTING
                    }))
                    .text_body2()
                    .when(floating, |l| {
                        l.text_size(dp(m::MESSAGE_LABEL_FLOATING_SIZE))
                    })
                    .text_color(tokens.primary_variant)
                    .child(label),
            )
            .child(
                div()
                    .absolute()
                    .left(dp(m::MESSAGE_PAD_H))
                    .right(dp(m::MESSAGE_PAD_H))
                    .top(dp(m::MESSAGE_TEXT_TOP))
                    .bottom(dp(8.))
                    .text_body1()
                    .text_color(tokens.on_background)
                    .child(
                        Textarea::new(&input)
                            .appearance(false)
                            .bordered(false)
                            .readonly(read_only)
                            .h_full(),
                    ),
            )
            .test_support()
            .into_any_element()
    }
}

/// Applies the repository's sign-off setting `pref`: Git's own `--signoff` for the default text, otherwise the
/// configured text (with `%user` and `%email` replaced by the override or the configured identity) as a trailer.
fn apply_signoff(pref: &Signoff, opts: &mut CommitOptions, ctx: &TabCtx, cx: &App) {
    if !pref.enabled {
        return;
    }
    let text = pref.text.trim();
    if text.is_empty() {
        opts.signoff = true;
        return;
    }
    let identity = opts.identity_override.clone().or_else(|| {
        ctx.model
            .read(cx)
            .identity
            .value
            .as_ref()
            .and_then(|i| i.effective.clone())
    });
    let (name, email) = identity.map(|i| (i.name, i.email)).unwrap_or_default();
    let text = text.replace("%user", &name).replace("%email", &email);
    match text.split_once(':') {
        Some((key, value)) => opts
            .trailers
            .push((key.trim().to_owned(), value.trim().to_owned())),
        None => {
            opts.message.push_str("\n\n");
            opts.message.push_str(&text);
        }
    }
}

/// A filled 4 dp button that fills its parent (Gitnuro `ConfirmationButton` and `AbortButton`, 08 §4.6).
fn button(
    id: &'static str,
    label: &'static str,
    background: Hsla,
    foreground: Hsla,
    enabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(dp(metrics::RADIUS))
        .bg(background)
        .text_color(foreground)
        .text_body1()
        .cursor_pointer()
        .when(enabled, |b| {
            b.on_click(move |_, window, cx| on_click(window, cx))
        })
        .child(label)
        .test_support()
        .into_any_element()
}

/// Background and text colour of the main button (disabled: `on_background` at 12 % over the surface, text at 38 %).
fn confirm_colors(tokens: &Tokens, enabled: bool) -> (Hsla, Hsla) {
    if enabled {
        (tokens.primary, tokens.on_primary)
    } else {
        (
            tokens.surface.blend(tokens.on_background.alpha(0.12)),
            tokens.on_background.alpha(0.38),
        )
    }
}

impl Render for CommitBox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let inputs = self.inputs(cx);
        let layout = layout(&inputs);
        let read_only = layout.read_only || self.api_read_only;
        let label = if layout.read_only {
            "Commit message (read-only)"
        } else {
            "Write your commit message here"
        };
        let message = self.message_area(window, read_only, label, inputs.message_blank, cx);
        let this = cx.entity().downgrade();

        let checkbox_row = layout.checkbox_row.then(|| {
            let toggle_this = this.clone();
            let slot = self.ai_slot(window, cx);
            div()
                .id(ids::CHECKBOX_ROW)
                .w_full()
                .h(dp(m::CHECKBOX_ROW_H))
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .when(layout.amend_checkbox, |row| {
                    row.child(
                        CheckboxText::new(ids::AMEND, inputs.amend, "Amend previous commit")
                            .on_toggle(move |on, window, cx| {
                                toggle_this
                                    .update(cx, |b, cx| b.set_amend(on, window, cx))
                                    .ok();
                            }),
                    )
                })
                .child(div().flex_1())
                .when_some(slot, |row, slot| row.child(div().flex_none().child(slot)))
                .test_support()
        });

        let primary_this = this.clone();
        let (bg, fg) = confirm_colors(&tokens, layout.enabled);
        let primary = button(
            ids::PRIMARY,
            layout.label,
            bg,
            fg,
            layout.enabled,
            move |window, cx| {
                primary_this
                    .update(cx, |b, cx| b.run_primary(window, cx))
                    .ok();
            },
        );
        let buttons = if layout.abort {
            let abort_this = this.clone();
            let abort = button(
                ids::ABORT,
                "Abort",
                tokens.error,
                tokens.on_error,
                true,
                move |window, cx| {
                    abort_this.update(cx, |b, cx| b.abort(window, cx)).ok();
                },
            );
            div()
                .id(ids::BUTTON_ROW)
                .w_full()
                .flex_none()
                .pt(dp(m::STATE_ROW_PAD_TOP))
                .child(
                    div()
                        .w_full()
                        .h(dp(m::BUTTON_H))
                        .flex()
                        .flex_row()
                        .child(
                            div()
                                .flex_1()
                                .h_full()
                                .pr(dp(m::STATE_BUTTON_PAD))
                                .child(abort),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h_full()
                                .pl(dp(m::STATE_BUTTON_PAD))
                                .child(primary),
                        ),
                )
                .test_support()
                .into_any_element()
        } else {
            div()
                .id(ids::BUTTON_ROW)
                .w_full()
                .flex_none()
                .pt(dp(m::COMMIT_BUTTON_GAP_TOP))
                .child(div().w_full().h(dp(m::BUTTON_H)).child(primary))
                .test_support()
                .into_any_element()
        };

        let submit_this = this;
        div()
            .key_context(COMMIT_BOX_CONTEXT)
            .on_action(move |_: &SubmitCommit, window, cx| {
                submit_this
                    .update(cx, |b, cx| b.submit_from_keyboard(window, cx))
                    .ok();
            })
            .w_full()
            .h(dp(m::COMMIT_BOX_H))
            .flex_none()
            .child(
                observed(COMMIT_BOX)
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(message)
                    .children(checkbox_row)
                    .child(buttons),
            )
    }
}
