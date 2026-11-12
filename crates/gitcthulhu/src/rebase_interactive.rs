//! The interactive rebase editor (owned by G9 after G0b; 08 §6, 20 §5.6) over `RepoUi::rebase_plan`. The tab
//! creates it while a plan is being edited (the side panel is hidden then). The placeholder fills the centre pane.

use gpui_kit::{App, Context, IntoElement, Render, Styled as _, Window};

use crate::tab::TabCtx;
use crate::widgets::observed;

/// Element ids of the rebase editor.
pub mod ids {
    /// The rebase editor.
    pub const REBASE: &str = "rebase-interactive";
}

/// The editor of the plan in `RepoUi::rebase_plan`.
pub struct RebaseInteractiveView {
    ctx: TabCtx,
}

impl RebaseInteractiveView {
    /// The editor of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

impl Render for RebaseInteractiveView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::REBASE).size_full()
    }
}

/// Registers the editor's key bindings (`alt-up` / `alt-down` in `RebaseList`, G9).
pub fn register(_cx: &mut App) {}
