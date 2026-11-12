//! The diff view (owned by G7a after G0b; 09 §1, spec 21 §4.1): one entity per tab that loads the selected file's
//! diff and renders it with `list(ListState)`. The interface to G7b (selection, highlighting) is fixed by the stubs
//! in `selection.rs` and `highlight.rs`. The placeholder fills its centre slot.

use gpui_kit::{App, Context, IntoElement, Render, Styled as _, Window};

use crate::tab::TabCtx;
use crate::widgets::observed;

pub mod header;
pub mod highlight;
pub mod model;
pub mod other;
pub mod rows;
pub mod selection;

/// Element ids of the diff view.
pub mod ids {
    /// The diff view.
    pub const DIFF_VIEW: &str = "diff-view";
}

/// The diff view of a repository tab.
pub struct DiffView {
    ctx: TabCtx,
}

impl DiffView {
    /// The diff view of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::DIFF_VIEW).size_full()
    }
}

/// Registers the diff view's key bindings (navigation keys in the `Diff` context) and menu extenders (G7a).
pub fn register(_cx: &mut App) {}
