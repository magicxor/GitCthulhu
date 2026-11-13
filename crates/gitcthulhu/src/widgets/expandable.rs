//! `VerticalExpandable` (Gitnuro 07 §10.10): a clickable header above content that is shown only when expanded.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use crate::icons::{AppIcon, icon};
use crate::metrics;
use crate::theme::ActiveTokens as _;

/// A header with an expand/collapse chevron and optional content. The owner keeps the `expanded` state.
#[derive(IntoElement)]
pub struct VerticalExpandable {
    id: ElementId,
    header: AnyElement,
    content: Option<AnyElement>,
    expanded: bool,
    on_toggle: Option<Rc<dyn Fn(bool, &mut Window, &mut App)>>,
}

impl VerticalExpandable {
    /// An expandable with the given `header`. `id` names the header row and must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, header: impl IntoElement, expanded: bool) -> Self {
        Self {
            id: id.into(),
            header: header.into_any_element(),
            content: None,
            expanded,
            on_toggle: None,
        }
    }

    /// The content shown while expanded.
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    /// Sets the handler that receives the new expanded value when the header is clicked.
    pub fn on_toggle(mut self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for VerticalExpandable {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let expanded = self.expanded;
        let on_toggle = self.on_toggle;
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(
                div()
                    .id(self.id)
                    .flex()
                    .flex_row()
                    .items_center()
                    .cursor_pointer()
                    .when_some(on_toggle, |h, f| {
                        h.on_click(move |_, window, cx| f(!expanded, window, cx))
                    })
                    .child(icon(
                        if expanded {
                            AppIcon::ExpandMore
                        } else {
                            AppIcon::ChevronRight
                        },
                        metrics::ICON_SM,
                        tokens.on_background,
                    ))
                    .child(self.header)
                    .test_support(),
            )
            .when(expanded, |col| col.children(self.content))
    }
}
