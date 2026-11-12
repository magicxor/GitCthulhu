//! The side panel (owned by G3 after G0b; 07 §6, spec 21 §4.3): filter field and one flattened `uniform_list` of
//! section headers and ref rows. The placeholder fills the first pane.

use gpui_kit::{App, Context, IntoElement, Render, Styled as _, Window};

use crate::tab::TabCtx;
use crate::widgets::observed;

pub mod model;
pub mod rows;

/// Element ids of the side panel.
pub mod ids {
    /// The side panel.
    pub const SIDE_PANEL: &str = "side-panel";
}

/// The side panel of a repository tab.
pub struct SidePanelView {
    ctx: TabCtx,
}

impl SidePanelView {
    /// The side panel of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this panel.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

impl Render for SidePanelView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::SIDE_PANEL).size_full()
    }
}

/// Registers the side panel's key bindings, actions and menu extenders (G3).
pub fn register(_cx: &mut App) {}
