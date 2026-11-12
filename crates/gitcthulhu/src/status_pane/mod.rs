//! The status pane (owned by G5; 08 §4): the staged and unstaged changes lists and the commit box. It fills the
//! right pane while the uncommitted changes are selected and owns the commit box entity whose public API X3 uses.
//!
//! Layout (08 §4.1): a column with 8 dp padding at the end and the bottom, the two lists (equal heights, 4 dp gap
//! below each, staged on top unless "swap staged and unstaged" is set) and the 192 dp commit box.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div,
};

use crate::metrics::{self, dp, status as m};
use crate::settings::SettingsGlobal;
use crate::tab::TabCtx;
use crate::tab::ui::FileList;
use crate::theme::ActiveTokens as _;
use crate::widgets::{LinearProgress, observed};

pub mod actions;
pub mod changes_list;
pub mod commit_box;
pub mod data;
pub mod platform;

use changes_list::ChangesList;
use commit_box::CommitBox;

/// Element ids of one changes list.
#[derive(Clone, Copy, Debug)]
pub struct ListIds {
    /// The whole list: header, optional filter and rows.
    pub list: &'static str,
    /// The 34 dp header.
    pub header: &'static str,
    /// The list/tree toggle in the header.
    pub tree_toggle: &'static str,
    /// The search toggle in the header.
    pub search_toggle: &'static str,
    /// The Stage all / Unstage all button.
    pub action: &'static str,
    /// The filter strip (its field is `<search>-field`, its close button `<search>-close`).
    pub search: &'static str,
    /// The area under the header that holds the rows.
    pub list_area: &'static str,
    /// The virtualised row list.
    pub rows: &'static str,
    /// Prefix of a file row id: `(row, index)`.
    pub row: &'static str,
    /// Prefix of a directory row wrapper id: `(dir, index)`.
    pub dir: &'static str,
    /// Prefix of a directory row id: `(dir_row, index)`.
    pub dir_row: &'static str,
    /// Prefix of the hover button id of a file row: `(row_action, index)`.
    pub row_action: &'static str,
    /// The list's scrollbar.
    pub scrollbar: &'static str,
}

/// Element ids of the status pane.
pub mod ids {
    use super::ListIds;
    use crate::tab::ui::FileList;

    /// The status pane.
    pub const STATUS_PANE: &str = "status-pane";
    /// The ids of the staged list.
    pub const STAGED: ListIds = ListIds {
        list: "staged-list",
        header: "staged-header",
        tree_toggle: "staged-tree-toggle",
        search_toggle: "staged-search-toggle",
        action: "staged-action",
        search: "staged-search",
        list_area: "staged-list-area",
        rows: "staged-rows",
        row: "staged-row",
        dir: "staged-dir",
        dir_row: "staged-dir-row",
        row_action: "staged-row-action",
        scrollbar: "staged-scrollbar",
    };
    /// The ids of the unstaged list.
    pub const UNSTAGED: ListIds = ListIds {
        list: "unstaged-list",
        header: "unstaged-header",
        tree_toggle: "unstaged-tree-toggle",
        search_toggle: "unstaged-search-toggle",
        action: "unstaged-action",
        search: "unstaged-search",
        list_area: "unstaged-list-area",
        rows: "unstaged-rows",
        row: "unstaged-row",
        dir: "unstaged-dir",
        dir_row: "unstaged-dir-row",
        row_action: "unstaged-row-action",
        scrollbar: "unstaged-scrollbar",
    };
    /// The progress bar at the top of the pane.
    pub const PROGRESS: &str = "status-progress";

    /// The ids of `list` (the staged ids for anything but `Unstaged`).
    pub fn of(list: FileList) -> ListIds {
        match list {
            FileList::Unstaged => UNSTAGED,
            _ => STAGED,
        }
    }
}

/// The status pane of a repository tab.
pub struct StatusPaneView {
    ctx: TabCtx,
    staged: Entity<ChangesList>,
    unstaged: Entity<ChangesList>,
    commit_box: Entity<CommitBox>,
    _subscriptions: Vec<Subscription>,
}

impl StatusPaneView {
    /// The status pane of the tab of `ctx`.
    pub fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (staged_ctx, unstaged_ctx, box_ctx) = (ctx.clone(), ctx.clone(), ctx.clone());
        let staged = cx.new(|cx| ChangesList::new(staged_ctx, FileList::Staged, window, cx));
        let unstaged = cx.new(|cx| ChangesList::new(unstaged_ctx, FileList::Unstaged, window, cx));
        let commit_box = cx.new(|cx| CommitBox::new(box_ctx, window, cx));
        let subscriptions = vec![
            cx.observe(&ctx.model, |_, _, cx| cx.notify()),
            cx.observe_global::<SettingsGlobal>(|_, cx| cx.notify()),
        ];
        Self {
            ctx,
            staged,
            unstaged,
            commit_box,
            _subscriptions: subscriptions,
        }
    }

    /// The tab of this pane.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The commit box (its message API is used by the AI controller, X3).
    pub fn commit_box(&self) -> &Entity<CommitBox> {
        &self.commit_box
    }

    /// The staged list (tests).
    pub fn staged(&self) -> &Entity<ChangesList> {
        &self.staged
    }

    /// The unstaged list (tests).
    pub fn unstaged(&self) -> &Entity<ChangesList> {
        &self.unstaged
    }
}

impl Render for StatusPaneView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let swap = view_settings(cx).1;
        let loading = self.ctx.model.read(cx).status.loading;
        let (first, second) = if swap {
            (self.unstaged.clone(), self.staged.clone())
        } else {
            (self.staged.clone(), self.unstaged.clone())
        };
        observed(ids::STATUS_PANE)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .pr(dp(metrics::PANE_EDGE_PAD))
            .pb(dp(metrics::PANE_EDGE_PAD))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .pb(dp(m::LIST_GAP))
                            .child(first),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .pb(dp(m::LIST_GAP))
                            .child(second),
                    ),
            )
            .child(self.commit_box.clone())
            .when(loading, |pane| {
                // 08 §4.1: an indeterminate bar while the status loads. Drawn over the top edge so a reload does
                // not move the lists.
                pane.child(
                    observed(ids::PROGRESS)
                        .absolute()
                        .top_0()
                        .left_0()
                        .right(dp(metrics::PANE_EDGE_PAD))
                        .h(dp(m::PROGRESS_H))
                        .child(LinearProgress::new().color(tokens.primary_variant)),
                )
            })
    }
}

/// `(changes_as_tree, swap_status_panes)` of the current settings, without cloning the settings (the lists read it
/// every frame).
pub(crate) fn view_settings(cx: &App) -> (bool, bool) {
    cx.try_global::<SettingsGlobal>()
        .map_or((false, false), |g| {
            (g.0.changes_as_tree, g.0.swap_status_panes)
        })
}

/// Registers the status pane's key bindings, actions and menu extenders (G5).
pub fn register(cx: &mut App) {
    commit_box::register(cx);
}

#[cfg(test)]
mod tests;
