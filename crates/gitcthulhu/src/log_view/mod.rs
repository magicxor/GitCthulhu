//! The log view (owned by G4 after G0b; 08 §2, spec 21 §4.2): header, `uniform_list` of commit rows with the graph
//! decoration, graph divider, scroll pill, search bar. The placeholder fills its centre slot.

use gpui_kit::{App, Context, IntoElement, Render, Styled as _, Window};

use crate::tab::TabCtx;
use crate::widgets::observed;

pub mod chips;
pub mod graph;
pub mod paging;
pub mod rows;
pub mod search;

/// Element ids of the log view.
pub mod ids {
    /// The log view.
    pub const LOG_VIEW: &str = "log-view";
    /// A log row: `(LOG_ROW, layout row index)`.
    pub const LOG_ROW: &str = "log-row";
}

/// The log of a repository tab.
pub struct LogView {
    ctx: TabCtx,
}

impl LogView {
    /// The log of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this log.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

impl Render for LogView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::LOG_VIEW).size_full()
    }
}

/// Registers the log's key bindings, actions and menu extenders (G4).
pub fn register(_cx: &mut App) {}
