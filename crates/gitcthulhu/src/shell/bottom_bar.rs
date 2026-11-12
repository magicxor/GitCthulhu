//! The bottom info bar (owned by G1 after G0b; 07 §7). The placeholder has the final 32 dp height and already calls
//! `update::bottom_bar_slot` (D0.5).

use gpui_kit::{Context, IntoElement, ParentElement as _, Render, Styled as _, Window};

use crate::ids;
use crate::metrics::{self, dp};
use crate::tab::TabCtx;
use crate::theme::ActiveTokens as _;
use crate::widgets::observed;

/// The bottom bar of a repository tab (`ctx = Some`) or of the welcome page.
pub struct BottomBarView {
    ctx: Option<TabCtx>,
}

impl BottomBarView {
    /// A bottom bar for the tab of `ctx` (or the welcome page with `None`).
    pub fn new(ctx: Option<TabCtx>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this bar, if any.
    pub fn ctx(&self) -> Option<&TabCtx> {
        self.ctx.as_ref()
    }
}

impl Render for BottomBarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let slot = crate::update::bottom_bar_slot(window, cx);
        observed(ids::BOTTOM_BAR)
            .flex_none()
            .w_full()
            .h(dp(metrics::BOTTOM_BAR_H))
            .px(dp(metrics::BOTTOM_BAR_PAD_H))
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .bg(tokens.surface)
            .children(slot)
    }
}
