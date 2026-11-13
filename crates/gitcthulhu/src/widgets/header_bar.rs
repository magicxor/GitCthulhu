//! `HeaderBar34` (Gitnuro 08 §0.5): the 34 dp `tertiary_surface` header row of lists and panes.

use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    Styled as _, TestSupportExt as _, Window, div,
};
use smallvec::SmallVec;

use crate::metrics::{self, dp};
use crate::theme::ActiveTokens as _;

/// A 34 dp header row; children are laid out left to right, vertically centred.
#[derive(IntoElement)]
pub struct HeaderBar34 {
    id: Option<ElementId>,
    children: SmallVec<[AnyElement; 4]>,
}

impl HeaderBar34 {
    /// An empty header.
    pub fn new() -> Self {
        Self {
            id: None,
            children: SmallVec::new(),
        }
    }

    /// Registers the bar under `id` for tests; ids must be unique in the frame.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }
}

impl Default for HeaderBar34 {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for HeaderBar34 {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for HeaderBar34 {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let bar = div()
            .w_full()
            .h(dp(metrics::HEADER_BAR_H))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .bg(tokens.tertiary_surface)
            .children(self.children);
        match self.id {
            Some(id) => bar.id(id).test_support().into_any_element(),
            None => bar.into_any_element(),
        }
    }
}
