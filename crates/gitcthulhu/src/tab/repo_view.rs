//! The repository frame ("RepoView", spec 21 §1.1, §2.5, 07 §5): toolbar, three-pane split (or file history),
//! terminal slot, divider, bottom bar and the processing overlay, plus the repository-level action handlers. It is
//! the `Render` of `RepoTab`, so its handlers reach the tab directly; region views only render inside the slots.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Styled as _, TestSupportExt as _, Window, div,
};

use super::RepoTab;
use super::ids;
use super::ops::Cancellable;
use super::ui::{CenterContent, RightContent};
use crate::actions::{
    CancelOperation, CloseLastView, CreateBranch, PopStash, Pull, Push, REPO_CONTEXT, Redo,
    Refresh, Stash, ToggleTerminal, Undo,
};
use crate::backend::{RefreshSet, StartPoint};
use crate::metrics::{self, dp};
use crate::settings::state;
use crate::theme::ActiveTokens as _;
use crate::widgets::observed;
use crate::widgets::split::{SplitWidths, ThreePaneSplit};

impl RepoTab {
    /// The pane widths in effect: the live drag, else the persisted `UiState` widths.
    pub fn split_widths(&self, cx: &gpui_kit::App) -> SplitWidths {
        self.drag_widths.unwrap_or_else(|| {
            cx.try_global::<state::UiStateGlobal>()
                .map(|s| SplitWidths {
                    first: s.0.first_pane_width,
                    third: s.0.third_pane_width,
                })
                .unwrap_or_default()
        })
    }

    /// Creates the views that exist only while shown (blame, file history, interactive rebase) and drops the ones
    /// no longer shown.
    fn ensure_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (blame, history, rebase) = {
            let ui = self.ctx.ui.read(cx);
            (
                ui.blame.is_some(),
                ui.history.clone(),
                ui.rebase_plan.is_some(),
            )
        };
        let mut dropped = false;
        if blame && self.views.blame.is_none() {
            let ctx = self.ctx.clone();
            self.views.blame = Some(cx.new(|cx| crate::blame::BlameView::new(ctx, window, cx)));
        } else if !blame {
            dropped |= self.views.blame.take().is_some();
        }
        match history {
            Some(path) if self.views.history.as_ref().map(|(p, _)| p) != Some(&path) => {
                let ctx = self.ctx.clone();
                let view_path = path.clone();
                let view = cx.new(|cx| {
                    crate::file_history::FileHistoryView::new(ctx, view_path, window, cx)
                });
                self.views.history = Some((path, view));
            }
            Some(_) => {}
            None => dropped |= self.views.history.take().is_some(),
        }
        if rebase && self.views.rebase.is_none() {
            let ctx = self.ctx.clone();
            self.views.rebase =
                Some(cx.new(|cx| {
                    crate::rebase_interactive::RebaseInteractiveView::new(ctx, window, cx)
                }));
        } else if !rebase {
            dropped |= self.views.rebase.take().is_some();
        }
        // A view that was dropped while it held the keyboard focus (closed by something other than its own close
        // button, for example a new selection) must not leave the focus nowhere: the frame takes it back.
        // The dropped entity releases its focus handle only after this render, hence the deferral.
        if dropped {
            cx.defer_in(window, |this, window, cx| {
                if window.focused(cx).is_none() {
                    window.focus(&this.focus, cx);
                }
            });
        }
    }

    fn center(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = crate::settings::get_or_default(cx);
        let center = {
            let model = self.ctx.model.read(cx);
            self.ctx.ui.read(cx).center(&settings, model)
        };
        match center {
            CenterContent::RebaseInteractive => match &self.views.rebase {
                Some(view) => view.clone().into_any_element(),
                None => div().into_any_element(),
            },
            CenterContent::Blame => match &self.views.blame {
                Some(view) => view.clone().into_any_element(),
                None => div().into_any_element(),
            },
            CenterContent::LogAndDiff {
                log,
                diff,
                minimized_blame,
            } => div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .when(log, |col| {
                            col.child(
                                observed(ids::CENTER_LOG)
                                    .flex_1()
                                    .min_h_0()
                                    .w_full()
                                    .child(self.views.log.clone()),
                            )
                        })
                        .when(diff.is_some(), |col| {
                            col.child(
                                observed(ids::CENTER_DIFF)
                                    .flex_1()
                                    .min_h_0()
                                    .w_full()
                                    .child(self.views.diff.clone()),
                            )
                        }),
                )
                .when(minimized_blame, |col| {
                    col.children(self.views.blame.clone().map(|b| div().flex_none().child(b)))
                })
                .into_any_element(),
        }
    }

    fn right(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.ctx.ui.read(cx).right() {
            RightContent::CommitChanges { .. } => {
                self.views.commit_changes.clone().into_any_element()
            }
            RightContent::Status => self.views.status.clone().into_any_element(),
            RightContent::Empty => div().into_any_element(),
        }
    }

    fn split(&self, cx: &mut Context<Self>) -> AnyElement {
        let editing_rebase = self.ctx.ui.read(cx).rebase_plan.is_some();
        let first = (!editing_rebase).then(|| self.views.side_panel.clone().into_any_element());
        let tab = cx.weak_entity();
        let tab_commit = tab.clone();
        ThreePaneSplit::new(
            first,
            self.center(cx),
            self.right(cx),
            self.split_widths(cx),
        )
        .on_change(move |widths, _, cx| {
            tab.update(cx, |t, cx| {
                t.drag_widths = Some(widths);
                cx.notify();
            })
            .ok();
        })
        .on_commit(move |_, _, cx| {
            tab_commit
                .update(cx, |t, cx| {
                    t.drag_widths = None;
                    cx.notify();
                })
                .ok();
        })
        .into_any_element()
    }

    fn overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let busy = self.busy.as_ref().filter(|b| b.overlay_visible)?;
        let on_cancel: Option<Rc<dyn Fn(&mut Window, &mut gpui_kit::App)>> =
            (busy.cancellable != Cancellable::No).then(|| {
                let tab = cx.weak_entity();
                Rc::new(move |window: &mut Window, cx: &mut gpui_kit::App| {
                    tab.update(cx, |t, cx| t.cancel_op(window, cx)).ok();
                }) as Rc<dyn Fn(&mut Window, &mut gpui_kit::App)>
            });
        let content = crate::shell::loading::processing_overlay(busy, on_cancel, cx);
        Some(
            div()
                .id(ids::PROCESSING_OVERLAY)
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .key_context(crate::actions::PROCESSING_CONTEXT)
                .track_focus(&self.overlay_focus)
                .on_action(
                    cx.listener(|tab, _: &CancelOperation, window, cx| tab.cancel_op(window, cx)),
                )
                .child(content)
                .test_support()
                .into_any_element(),
        )
    }

    fn on_pop_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let newest = self
            .ctx
            .model
            .read(cx)
            .refs
            .value
            .as_ref()
            .and_then(|r| r.stashes.first().map(|s| s.id));
        if let Some(id) = newest {
            self.pop_stash(id, window, cx);
        }
    }

    fn on_close_last_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let closed = self.ctx.ui.update(cx, |ui, cx| ui.close_last_view(cx));
        if closed {
            window.focus(&self.focus, cx);
        }
    }

    /// Calls a D0 slot after the tab's update (the slot may update the tab itself).
    fn defer_slot(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        slot: fn(&super::TabCtx, &mut Window, &mut gpui_kit::App),
    ) {
        let ctx = self.ctx.clone();
        window.defer(cx, move |window, cx| slot(&ctx, window, cx));
    }
}

impl Render for RepoTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_views(window, cx);
        let tokens = *cx.tokens();
        let history = self.views.history.as_ref().map(|(_, v)| v.clone());
        let middle = match history {
            Some(view) => view.into_any_element(),
            None => self.split(cx),
        };
        let terminal = crate::terminal::panel_slot(&self.ctx, window, cx);
        let overlay = self.overlay(cx);
        div()
            .id(crate::ids::REPO_FRAME)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.surface)
            .key_context(REPO_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|tab, _: &Pull, window, cx| {
                tab.pull(None, window, cx);
            }))
            .on_action(cx.listener(|tab, _: &Push, window, cx| {
                tab.push_current(false, false, window, cx);
            }))
            .on_action(cx.listener(|tab, _: &CreateBranch, window, cx| {
                let ctx = tab.ctx.clone();
                window.defer(cx, move |window, cx| {
                    crate::dialogs::branch::open_new_branch(window, cx, ctx, StartPoint::Head)
                });
            }))
            .on_action(cx.listener(|tab, _: &Stash, window, cx| {
                tab.stash(None, true, window, cx);
            }))
            .on_action(cx.listener(|tab, _: &PopStash, window, cx| tab.on_pop_stash(window, cx)))
            .on_action(cx.listener(|tab, _: &Refresh, _, cx| {
                if tab.busy.is_none() {
                    tab.reload(RefreshSet::ALL, cx);
                }
            }))
            .on_action(
                cx.listener(|tab, _: &CloseLastView, window, cx| {
                    tab.on_close_last_view(window, cx)
                }),
            )
            .on_action(
                cx.listener(|tab, _: &gpui_kit::base::input::Escape, window, cx| {
                    tab.on_close_last_view(window, cx)
                }),
            )
            .on_action(cx.listener(|tab, _: &Undo, window, cx| {
                tab.defer_slot(window, cx, crate::undo::on_undo)
            }))
            .on_action(cx.listener(|tab, _: &Redo, window, cx| {
                tab.defer_slot(window, cx, crate::undo::on_redo)
            }))
            .on_action(cx.listener(|tab, _: &ToggleTerminal, window, cx| {
                tab.defer_slot(window, cx, crate::terminal::toggle)
            }))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|tab, _, window, cx| {
                    if !tab.focus.contains_focused(window, cx) {
                        window.focus(&tab.focus, cx);
                    }
                }),
            )
            .child(div().flex_none().w_full().child(self.views.toolbar.clone()))
            .child(
                div()
                    .flex_1()
                    .min_h(dp(metrics::TERMINAL_MIN_SPLIT_H))
                    .w_full()
                    .child(middle),
            )
            .child(terminal)
            .child(
                observed(crate::ids::FRAME_DIVIDER)
                    .flex_none()
                    .w_full()
                    .h(dp(metrics::FRAME_DIVIDER_H))
                    .bg(tokens.primary_variant.opacity(0.2)),
            )
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .child(self.views.bottom_bar.clone()),
            )
            .children(overlay)
            .test_support()
    }
}
