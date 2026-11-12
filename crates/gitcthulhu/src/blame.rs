//! The blame view and its minimized bar (owned by G8 after G0b; 09 §2.1). The tab creates it while
//! `RepoUi::blame` is set; it renders the full view or, when minimized, the bar under the log/diff column. The
//! placeholder fills its slot.

use gpui_kit::{App, Context, IntoElement, Render, Styled as _, Window};

use crate::tab::TabCtx;
use crate::widgets::observed;

/// Element ids of the blame view.
pub mod ids {
    /// The blame view (full or minimized).
    pub const BLAME: &str = "blame-view";
}

/// The blame of `RepoUi::blame`.
pub struct BlameView {
    ctx: TabCtx,
}

impl BlameView {
    /// The blame view of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

impl Render for BlameView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::BLAME).size_full()
    }
}

/// Registers the blame view's key bindings (G8).
pub fn register(_cx: &mut App) {}
