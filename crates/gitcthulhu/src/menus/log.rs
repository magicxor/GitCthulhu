//! The commit-row menu of the log (owned by G4 after G0b; 08 §2.13); calls `menus::extend` at `End`.
//!
//! Gitnuro conditions: "Show status to amend this commit" (+ separator) only on HEAD's row with a clean worktree;
//! "Rebase interactive" on every row except HEAD's. Gitnuro menus have no disabled items: conditions only add or omit
//! items.

use gpui_kit::App;

use super::{MenuItem, MenuSlot, MenuTarget, extend};
use crate::backend::{HeadState, ObjectId, StartPoint};
use crate::icons::AppIcon;
use crate::tab::model::RepoModel;
use crate::tab::ui::{RepoUiEvent, Selection};

/// HEAD's commit, if HEAD is born.
fn head_commit(model: &RepoModel) -> Option<ObjectId> {
    let head = model
        .state
        .value
        .as_ref()
        .map(|s| &s.head)
        .or_else(|| model.refs.value.as_ref().map(|r| &r.head))?;
    match head {
        HeadState::Branch { target, .. } | HeadState::Detached { target } => Some(*target),
        HeadState::Unborn { .. } => None,
    }
}

/// The commit-row menu (slot `End`).
pub fn commit(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if let Some(id) = target.commit {
        let model = target.tab.model.read(cx);
        let is_last = head_commit(model) == Some(id);
        let dirty = model
            .status
            .value
            .as_ref()
            .is_some_and(|s| !s.counts.is_clean());
        if is_last && !dirty {
            let ctx = target.tab.clone();
            items.push(
                MenuItem::action(
                    "log-amend",
                    "Show status to amend this commit",
                    move |_, cx| {
                        ctx.ui.update(cx, |ui, cx| {
                            ui.select(Selection::UncommittedChanges, cx);
                            if !ui.amend {
                                ui.amend = true;
                                cx.emit(RepoUiEvent::TogglesChanged);
                            }
                            cx.notify();
                        });
                    },
                )
                .with_icon(AppIcon::Edit),
            );
            items.push(MenuItem::Separator);
        }
        let ctx = target.tab.clone();
        items.push(
            MenuItem::action("log-checkout", "Checkout commit", move |window, cx| {
                ctx.with_tab(window, cx, |tab, window, cx| {
                    tab.checkout_commit(id, window, cx);
                });
            })
            .with_icon(AppIcon::Start),
        );
        let ctx = target.tab.clone();
        items.push(
            MenuItem::action("log-create-branch", "Create branch", move |window, cx| {
                crate::dialogs::branch::open_new_branch(
                    window,
                    cx,
                    ctx.clone(),
                    StartPoint::Commit(id),
                );
            })
            .with_icon(AppIcon::Branch),
        );
        let ctx = target.tab.clone();
        items.push(
            MenuItem::action("log-create-tag", "Create tag", move |window, cx| {
                crate::dialogs::tag::open_new_tag(window, cx, ctx.clone(), id);
            })
            .with_icon(AppIcon::Tag),
        );
        items.push(MenuItem::Separator);
        if !is_last {
            let ctx = target.tab.clone();
            items.push(MenuItem::action(
                "log-rebase-interactive",
                "Rebase interactive",
                move |_, cx| crate::log_view::start_rebase_interactive(&ctx, id, cx),
            ));
        }
        let ctx = target.tab.clone();
        items.push(
            MenuItem::action("log-revert", "Revert commit", move |window, cx| {
                ctx.with_tab(window, cx, |tab, window, cx| {
                    tab.revert(id, None, window, cx);
                });
            })
            .with_icon(AppIcon::Revert),
        );
        let ctx = target.tab.clone();
        items.push(MenuItem::action(
            "log-cherry-pick",
            "Cherry-pick commit",
            move |window, cx| {
                ctx.with_tab(window, cx, |tab, window, cx| {
                    tab.cherry_pick(id, None, window, cx);
                });
            },
        ));
        let ctx = target.tab.clone();
        items.push(
            MenuItem::action(
                "log-reset",
                "Reset current branch to this commit",
                move |window, cx| {
                    crate::dialogs::reset::open(window, cx, ctx.clone(), id);
                },
            )
            .with_icon(AppIcon::Undo),
        );
    }
    extend(cx, MenuSlot::End, target, &mut items);
    items
}
