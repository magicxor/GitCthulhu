//! The status pane (owned by G5 after G0b; 08 §4): staged and unstaged changes lists and the commit box. The
//! placeholder fills the right pane and owns the commit box entity whose public API X3 uses.

use gpui_kit::{App, AppContext as _, Context, Entity, IntoElement, Render, Styled as _, Window};

use crate::tab::TabCtx;
use crate::widgets::observed;

pub mod changes_list;
pub mod commit_box;

use commit_box::CommitBox;

/// Element ids of the status pane.
pub mod ids {
    /// The status pane.
    pub const STATUS_PANE: &str = "status-pane";
}

/// The status pane of a repository tab.
pub struct StatusPaneView {
    ctx: TabCtx,
    commit_box: Entity<CommitBox>,
}

impl StatusPaneView {
    /// The status pane of the tab of `ctx`.
    pub fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let box_ctx = ctx.clone();
        let commit_box = cx.new(|cx| CommitBox::new(box_ctx, window, cx));
        Self { ctx, commit_box }
    }

    /// The tab of this pane.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The commit box (its message API is used by the AI controller, X3).
    pub fn commit_box(&self) -> &Entity<CommitBox> {
        &self.commit_box
    }
}

impl Render for StatusPaneView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::STATUS_PANE).size_full()
    }
}

/// Registers the status pane's key bindings, actions and menu extenders (G5).
pub fn register(_cx: &mut App) {}
