//! The interactive rebase editor (owned by G9 after G0b; 08 §6, 20 §5.6) over `RepoUi::rebase_plan`.
//!
//! The tab creates the view while a plan is being edited (the frame hides the side panel then). The plan lives in
//! `RepoUi`; every edit goes through the pure functions of [`plan`] and notifies the UI entity. Rows are rendered
//! lazily by a variable-height `list` (commit messages wrap), no row owns an entity: one shared text area edits the
//! message of the Reword row the user clicked. Drag-sorting reorders live (`move_step`) while the pointer moves.

use std::collections::HashMap;

use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::{
    App, AppContext as _, Context, DragMoveEvent, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyBinding, ListAlignment, ListState, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, list, px,
};

use crate::backend::{self, ObjectId, RebasePlan};
use crate::metrics::dp;
use crate::metrics::rebase as m;
use crate::tab::ops::OpKind;
use crate::tab::ui::{RepoUiEvent, Selection};
use crate::tab::{RepoTabEvent, TabCtx};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::scroll::list_scrollbar;
use crate::widgets::{PrimaryButton, observed};

pub mod plan;
mod row;

#[cfg(test)]
mod tests;

/// Key context of the editor's step list (`alt-up` / `alt-down` move the selected step).
pub const REBASE_LIST_CONTEXT: &str = "RebaseList";

gpui_kit::actions!(
    gitcthulhu_rebase,
    [
        /// Moves the selected step one line up (towards the oldest commit).
        MoveStepUp,
        /// Moves the selected step one line down.
        MoveStepDown,
    ]
);

/// Element ids of the rebase editor.
pub mod ids {
    /// The rebase editor.
    pub const REBASE: &str = "rebase-interactive";
    /// The title text.
    pub const TITLE: &str = "rebase-title";
    /// The note about flattened merge commits.
    pub const MERGES_NOTE: &str = "rebase-merges-note";
    /// The step list; rows are `(ROW, index)`.
    pub const LIST: &str = "rebase-list";
    /// A step row, `(ROW, index)`.
    pub const ROW: &str = "rebase-row";
    /// The action dropdown of a row, `(ACTION, index)`.
    pub const ACTION: &str = "rebase-action";
    /// The message field of a row, `(MESSAGE, index)`.
    pub const MESSAGE: &str = "rebase-message";
    /// The list's scrollbar.
    pub const SCROLLBAR: &str = "rebase-scrollbar";
    /// The Cancel button.
    pub const CANCEL: &str = "rebase-cancel";
    /// The "Complete rebase" button.
    pub const COMPLETE: &str = "rebase-complete";
    /// The text shown when there is nothing to rebase.
    pub const EMPTY: &str = "rebase-empty";
}

/// The payload of a step drag: the commit being moved (stable while the rows shift).
#[derive(Clone)]
pub(crate) struct RebaseDrag {
    pub(crate) commit: ObjectId,
}

/// Identity of the plan the list state was built for: HEAD, base and the number of steps.
type PlanKey = (ObjectId, Option<ObjectId>, usize);

/// The editor of the plan in `RepoUi::rebase_plan`.
pub struct RebaseInteractiveView {
    ctx: TabCtx,
    /// Focus target of the key context; deliberately not an ancestor of the text area (a root `track_focus` would
    /// steal the focus from an input on every click).
    focus: FocusHandle,
    list: ListState,
    plan_key: Option<PlanKey>,
    /// The one text area that edits the message of the Reword row `editing`.
    editor: Entity<TextareaState>,
    editing: Option<ObjectId>,
    /// Edited messages of steps that are not Reword at the moment (restored when Reword is chosen again).
    drafts: HashMap<ObjectId, String>,
    /// The commit being dragged (dims its row); cleared when the drag ends.
    dragging: Option<ObjectId>,
    _subscriptions: Vec<Subscription>,
}

impl RebaseInteractiveView {
    /// The editor of the tab of `ctx`.
    pub fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, m::MESSAGE_MAX_LINES));
        let subscriptions = vec![
            cx.subscribe(&editor, |this, editor, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = editor.read(cx).value();
                    this.message_edited(&text, cx);
                }
            }),
            cx.subscribe(&ctx.events, |this, _, event: &RepoTabEvent, cx| {
                // The rebase started (or stopped at a conflict or an edit step): the plan is spent. A failed
                // start keeps the plan for another try.
                if matches!(
                    event,
                    RepoTabEvent::OpFinished {
                        kind: OpKind::RebaseInteractive,
                        ok: true
                    }
                ) {
                    this.discard_plan(cx);
                }
            }),
            cx.observe(&ctx.ui, |_, _, cx| cx.notify()),
        ];
        Self {
            focus: cx.focus_handle(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            plan_key: None,
            editor,
            editing: None,
            drafts: HashMap::new(),
            dragging: None,
            ctx,
            _subscriptions: subscriptions,
        }
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// Runs `f` on the plan being edited and notifies the UI entity (the tab re-renders). `None` when no plan is
    /// open.
    fn edit_plan<R>(&self, cx: &mut App, f: impl FnOnce(&mut RebasePlan) -> R) -> Option<R> {
        self.ctx.ui.update(cx, |ui, cx| {
            let result = ui.rebase_plan.as_mut().map(f);
            if result.is_some() {
                cx.notify();
            }
            result
        })
    }

    /// Rebuilds the list state and drops stale edit state when the plan is another one than last frame.
    fn sync_plan(&mut self, cx: &App) {
        let key = self
            .ctx
            .ui
            .read(cx)
            .rebase_plan
            .as_ref()
            .map(|p| (p.head, p.base, p.steps.len()));
        if key != self.plan_key {
            self.list.reset(key.map_or(0, |k| k.2));
            self.editing = None;
            self.drafts.clear();
            self.dragging = None;
            self.plan_key = key;
        }
    }

    /// Selects the commit of a step (the right pane shows its files).
    fn select(&self, commit: ObjectId, cx: &mut App) {
        self.ctx.ui.update(cx, |ui, cx| {
            ui.select(
                Selection::Commit {
                    id: commit,
                    is_stash: false,
                    scroll_to: false,
                },
                cx,
            )
        });
    }

    /// A click on a row: select the line's commit and take the keyboard focus for `alt-up` / `alt-down`.
    pub(crate) fn row_clicked(
        &mut self,
        commit: ObjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(commit, cx);
        window.focus(&self.focus, cx);
    }

    /// Applies the action chosen in a row's dropdown.
    pub(crate) fn choose_action(
        &mut self,
        commit: ObjectId,
        action: backend::RebaseAction,
        cx: &mut Context<Self>,
    ) {
        let mut drafts = std::mem::take(&mut self.drafts);
        self.edit_plan(cx, |plan| {
            if let Some(ix) = plan::position(plan, commit) {
                plan::set_action(plan, ix, action, &mut drafts);
            }
        });
        self.drafts = drafts;
        if action != backend::RebaseAction::Reword && self.editing == Some(commit) {
            self.editing = None;
        }
        self.list.remeasure();
        cx.notify();
    }

    /// Starts editing the message of a Reword row: the shared text area moves into it and takes the focus.
    pub(crate) fn begin_edit(
        &mut self,
        commit: ObjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(commit, cx);
        let text = self
            .ctx
            .ui
            .read(cx)
            .rebase_plan
            .as_ref()
            .and_then(|p| p.steps.iter().find(|s| s.commit == commit))
            .map(|s| plan::display_message(s).to_owned());
        let Some(text) = text else { return };
        if self.editing != Some(commit) {
            self.editing = Some(commit);
            self.editor.update(cx, |editor, cx| {
                editor.set_value(SharedString::from(text), window, cx)
            });
            self.list.remeasure();
        }
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
        cx.notify();
    }

    /// The text area changed: store it as the edited message of the row being edited.
    fn message_edited(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(commit) = self.editing else { return };
        let row = self.edit_plan(cx, |plan| {
            let ix = plan::position(plan, commit)?;
            plan::set_message(plan, ix, text);
            Some(ix)
        });
        if let Some(Some(ix)) = row {
            self.list.remeasure_items(ix..ix + 1);
        }
        cx.notify();
    }

    /// Moves the selected step by `delta` lines (`alt-up` / `alt-down`).
    pub(crate) fn move_selected(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some((selected, _)) = self.ctx.ui.read(cx).selection.commit() else {
            return;
        };
        let moved = self.edit_plan(cx, |plan| {
            let from = plan::position(plan, selected)?;
            let to = from.checked_add_signed(delta)?;
            plan::move_step(plan, from, to).then_some((from, to))
        });
        if let Some(Some((from, to))) = moved {
            self.moved(from, to);
            self.list.scroll_to_reveal_item(to);
            cx.notify();
        }
    }

    /// The rows between `from` and `to` changed places: their cached heights belong to other steps now.
    fn moved(&self, from: usize, to: usize) {
        self.list.remeasure_items(from.min(to)..from.max(to) + 1);
    }

    /// Live drag-sorting (08 §6): the dragged step swaps into the row under the pointer once the pointer passes
    /// that row's midpoint; dragging beyond the list's edge scrolls by the overshoot.
    fn drag_moved(&mut self, event: &DragMoveEvent<RebaseDrag>, cx: &mut Context<Self>) {
        let commit = event.drag(cx).commit;
        if self.dragging != Some(commit) {
            self.dragging = Some(commit);
            cx.notify();
        }
        let y = event.event.position.y;
        let viewport = self.list.viewport_bounds();
        if y < viewport.top() {
            self.list
                .scroll_by(-(viewport.top() - y) * m::AUTOSCROLL_GAIN);
        } else if y > viewport.bottom() {
            self.list
                .scroll_by((y - viewport.bottom()) * m::AUTOSCROLL_GAIN);
        }
        let Some(from) = self
            .ctx
            .ui
            .read(cx)
            .rebase_plan
            .as_ref()
            .and_then(|p| plan::position(p, commit))
        else {
            return;
        };
        let Some((target, bounds)) = self.row_under(y, cx) else {
            return;
        };
        let past_midpoint = if target > from {
            y >= bounds.center().y
        } else {
            y <= bounds.center().y
        };
        if target != from
            && past_midpoint
            && self.edit_plan(cx, |plan| plan::move_step(plan, from, target)) == Some(true)
        {
            self.moved(from, target);
            cx.notify();
        }
    }

    /// The rendered row at height `y` (the nearest rendered row when `y` is outside them) and its bounds.
    fn row_under(
        &self,
        y: gpui_kit::Pixels,
        cx: &App,
    ) -> Option<(usize, gpui_kit::Bounds<gpui_kit::Pixels>)> {
        let count = self.ctx.ui.read(cx).rebase_plan.as_ref()?.steps.len();
        let mut found = None;
        for ix in self.list.logical_scroll_top().item_ix..count {
            let Some(bounds) = self.list.bounds_for_item(ix) else {
                break;
            };
            found = Some((ix, bounds));
            if y < bounds.bottom() {
                break;
            }
        }
        found
    }

    /// Cancel: nothing was started, the plan is simply dropped.
    pub(crate) fn discard_plan(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.drafts.clear();
        self.ctx.ui.update(cx, |ui, cx| {
            if ui.rebase_plan.take().is_some() {
                cx.emit(RepoUiEvent::CenterChanged);
                cx.notify();
            }
        });
    }

    /// "Complete rebase": hands the edited plan to the interactive rebase operation. The plan stays until the
    /// operation reports success (a failed start can be retried).
    pub(crate) fn complete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self
            .ctx
            .ui
            .read(cx)
            .rebase_plan
            .as_ref()
            .filter(|p| plan::blocker(p).is_none())
            .map(plan::final_plan)
        else {
            return;
        };
        #[cfg(test)]
        tests::record_start(&plan);
        self.ctx.with_tab(window, cx, move |tab, window, cx| {
            tab.start_interactive_rebase(plan, window, cx);
        });
    }
}

impl Render for RebaseInteractiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        self.sync_plan(cx);
        if self.dragging.is_some() && !cx.has_active_drag() {
            self.dragging = None;
        }
        let (count, merges_dropped, blocker) = self
            .ctx
            .ui
            .read(cx)
            .rebase_plan
            .as_ref()
            .map_or((0, 0, None), |p| {
                (p.steps.len(), p.merges_dropped, plan::blocker(p))
            });
        let view = cx.entity();
        let rows_view = view.clone();
        let steps = list(self.list.clone(), move |ix, window, cx| {
            row::render(&rows_view, ix, window, cx)
        })
        .size_full();
        let body = if count == 0 {
            observed(ids::EMPTY)
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_body1()
                .text_color(tokens.on_background_secondary)
                .child("There are no commits to rebase")
                .into_any_element()
        } else {
            observed(ids::LIST)
                .relative()
                .flex_1()
                .min_h_0()
                .child(steps)
                .child(list_scrollbar(ids::SCROLLBAR, &self.list))
                .into_any_element()
        };
        let merges_note = (merges_dropped > 0).then(|| {
            let text = if merges_dropped == 1 {
                "1 merge commit in this range will be flattened".to_owned()
            } else {
                format!("{merges_dropped} merge commits in this range will be flattened")
            };
            observed(ids::MERGES_NOTE)
                .flex_none()
                .px(dp(m::TITLE_PAD_START))
                .pb(dp(m::TITLE_PAD_BOTTOM))
                .text_body2()
                .text_color(tokens.on_background_secondary)
                .child(text)
        });
        let cancel = {
            let view = view.downgrade();
            PrimaryButton::new(ids::CANCEL, "Cancel")
                .cancel()
                .on_click(move |_, cx| {
                    view.update(cx, |this, cx| this.discard_plan(cx)).ok();
                })
        };
        let complete = {
            let view = view.downgrade();
            PrimaryButton::new(ids::COMPLETE, "Complete rebase")
                .enabled(blocker.is_none())
                .on_click(move |window, cx| {
                    view.update(cx, |this, cx| this.complete(window, cx)).ok();
                })
        };
        observed(ids::REBASE).size_full().bg(tokens.surface).child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(dp(m::CLIP_RADIUS))
                .bg(tokens.background)
                .key_context(REBASE_LIST_CONTEXT)
                .on_action(cx.listener(|this, _: &MoveStepUp, _, cx| this.move_selected(-1, cx)))
                .on_action(cx.listener(|this, _: &MoveStepDown, _, cx| this.move_selected(1, cx)))
                .on_drag_move(
                    cx.listener(|this, event: &DragMoveEvent<RebaseDrag>, _, cx| {
                        this.drag_moved(event, cx)
                    }),
                )
                .child(div().track_focus(&self.focus).size_0())
                .child(
                    observed(ids::TITLE)
                        .flex_none()
                        .pl(dp(m::TITLE_PAD_START))
                        .pt(dp(m::TITLE_PAD_TOP))
                        .pb(dp(m::TITLE_PAD_BOTTOM))
                        .text_h3()
                        .text_color(tokens.on_background)
                        .child("Rebase interactive"),
                )
                .children(merges_note)
                .child(body)
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .pb(dp(m::FOOTER_PAD_BOTTOM))
                        .child(div().flex_1())
                        .child(div().mr(dp(m::CANCEL_PAD_END)).child(cancel))
                        .child(div().mr(dp(m::COMPLETE_PAD_END)).child(complete)),
                ),
        )
    }
}

/// Opens the editor on a fresh plan of the commits above `base` (`None` = from the root): reads the plan off the UI
/// thread, then sets `RepoUi::rebase_plan` (the tab shows the editor and hides the side panel). Failures and
/// empty ranges are toasts. The log's "Rebase interactive" menu item calls this.
pub fn open(ctx: &TabCtx, base: Option<ObjectId>, cx: &mut App) {
    let Some(repo) = ctx.backend.repo().cloned() else {
        return;
    };
    let (ui, tab) = (ctx.ui.clone(), ctx.id);
    // Only the newest request of a tab applies: an older planning that finishes later is dropped.
    let generation = {
        let pending = cx.default_global::<PendingPlans>();
        let generation = pending.0.entry(tab).or_default();
        *generation += 1;
        *generation
    };
    let task = backend::bg(cx, move || backend::rebase::plan_interactive(&repo, base));
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            let pending = cx.default_global::<PendingPlans>();
            if pending.0.get(&tab) != Some(&generation) {
                return;
            }
            pending.0.remove(&tab);
            apply_opened(&ui, tab, result, cx);
        });
    })
    .detach();
}

/// The newest `open` request of each tab.
#[derive(Default)]
struct PendingPlans(HashMap<crate::tab::TabId, u64>);

impl gpui_kit::Global for PendingPlans {}

/// Shows the plan `open` read, or says why there is none.
fn apply_opened(
    ui: &Entity<crate::tab::ui::RepoUi>,
    tab: crate::tab::TabId,
    result: Result<RebasePlan, backend::GitError>,
    cx: &mut App,
) {
    match result {
        Ok(plan) if plan.steps.is_empty() => crate::app_view::show_toast(
            cx,
            Some(tab),
            crate::notifications::ToastKind::Warning,
            "There are no commits to rebase",
            None,
        ),
        Ok(plan) => {
            ui.update(cx, |ui, cx| {
                ui.rebase_plan = Some(plan);
                cx.emit(RepoUiEvent::CenterChanged);
                cx.notify();
            });
        }
        Err(error) => crate::app_view::show_toast(
            cx,
            Some(tab),
            crate::notifications::ToastKind::Error,
            format!("Planning the interactive rebase failed: {error}"),
            None,
        ),
    }
}

/// Registers the editor's key bindings (`alt-up` / `alt-down` in `RebaseList`, also while its text area has the
/// focus).
pub fn register(cx: &mut App) {
    let bindings = [REBASE_LIST_CONTEXT, "RebaseList > Input"]
        .into_iter()
        .flat_map(|context| {
            [
                KeyBinding::new("alt-up", MoveStepUp, Some(context)),
                KeyBinding::new("alt-down", MoveStepDown, Some(context)),
            ]
        });
    cx.bind_keys(bindings);
}
