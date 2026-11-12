//! The commit changes view (owned by G6 after G0b; 08 §5): message, changed files and author footer of the
//! selected commit. The placeholder fills the right pane.

use gpui_kit::{App, Context, IntoElement, Render, Styled as _, Window};

use crate::tab::TabCtx;
use crate::widgets::observed;

/// Element ids of the commit changes view.
pub mod ids {
    /// The commit changes view.
    pub const COMMIT_CHANGES: &str = "commit-changes";
}

/// The commit changes view of a repository tab.
pub struct CommitChangesView {
    ctx: TabCtx,
}

impl CommitChangesView {
    /// The commit changes view of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

impl Render for CommitChangesView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::COMMIT_CHANGES).size_full()
    }
}

/// Registers the view's key bindings, actions and menu extenders (G6).
pub fn register(_cx: &mut App) {}
