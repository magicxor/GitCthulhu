//! The repository toolbar (owned by G2 after G0b; 07 §5.1). The placeholder has the final 64 dp height and already
//! renders the D0.3 undo slot at the start of the centred group.

use gpui_kit::{App, Context, IntoElement, ParentElement as _, Render, Styled as _, Window, div};

use crate::ids;
use crate::metrics::{self, dp};
use crate::tab::TabCtx;
use crate::widgets::observed;

pub mod open_popup;

/// The toolbar of a repository tab.
pub struct ToolbarView {
    ctx: TabCtx,
}

impl ToolbarView {
    /// The toolbar of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this toolbar.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

impl Render for ToolbarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let undo = crate::undo::toolbar_slot(&self.ctx, window, cx);
        observed(ids::TOOLBAR)
            .flex_none()
            .w_full()
            .h(dp(metrics::TOOLBAR_H))
            .py(dp(metrics::TOOLBAR_PAD_V))
            .flex()
            .flex_row()
            .justify_center()
            .child(div().flex().flex_row().child(undo))
    }
}

/// Registers the toolbar's key bindings and actions (G2).
pub fn register(_cx: &mut App) {}
