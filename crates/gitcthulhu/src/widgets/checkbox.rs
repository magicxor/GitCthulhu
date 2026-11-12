//! `CheckboxText` (Gitnuro 07 §10.5, 09 §0.3): a Material checkbox with a caption.
//!
//! The Material checkbox is 12 dp with 8 dp padding (a 28 dp slot) but draws a 20 dp box that overflows the slot.
//! The whole row is the click target, and the row is exactly as wide as its content (it never stretches to the
//! parent), so a click beside the caption does not toggle it.

use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};
use std::rc::Rc;

use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};

/// Checkbox box size drawn by Material (the slot is 28 dp including padding).
const BOX: f32 = 20.;

/// A checkbox with a caption. `on_toggle` receives the new value.
#[derive(IntoElement)]
pub struct CheckboxText {
    id: ElementId,
    checked: bool,
    label: SharedString,
    enabled: bool,
    on_toggle: Option<Rc<dyn Fn(bool, &mut Window, &mut App)>>,
}

impl CheckboxText {
    /// A checkbox row. `id` names the clickable row and must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, checked: bool, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            checked,
            label: label.into(),
            enabled: true,
            on_toggle: None,
        }
    }

    /// Enables or disables the checkbox.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the handler that receives the toggled value.
    pub fn on_toggle(mut self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for CheckboxText {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let checked = self.checked;
        let enabled = self.enabled;
        let on_toggle = self.on_toggle;
        let alpha = if enabled { 1.0 } else { 0.38 };
        let accent = tokens.secondary.opacity(alpha);
        // Outer row takes the parent's width; the clickable row inside is content-sized.
        div().flex().flex_row().child(
            div()
                .id(self.id)
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .cursor_pointer()
                .when(enabled, |row| {
                    row.when_some(on_toggle, |row, f| {
                        row.on_click(move |_, window, cx| f(!checked, window, cx))
                    })
                })
                .child(
                    div()
                        .flex_none()
                        .size(dp(metrics::CHECKBOX_SLOT))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .flex_none()
                                .size(dp(BOX))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(dp(2.))
                                .border(dp(2.))
                                .border_color(if checked {
                                    accent
                                } else {
                                    tokens.on_background.opacity(0.6 * alpha)
                                })
                                .when(checked, |b| b.bg(accent))
                                .when(checked, |b| {
                                    b.child(
                                        Icon::new(IconName::Check)
                                            .size(dp(14.))
                                            .text_color(tokens.surface),
                                    )
                                }),
                        ),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(tokens.on_background.opacity(alpha))
                        .whitespace_nowrap()
                        .child(self.label),
                )
                .test_support(),
        )
    }
}
