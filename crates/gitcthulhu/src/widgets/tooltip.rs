//! Tooltips (Gitnuro 07 §9.1, §9.2, §10.9): `InstantTooltip` shows as soon as the pointer is over the anchor,
//! `DelayedTooltip` after 500 ms. Both follow GPUI's tooltip placement (next to the pointer).

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyView, App, AppContext as _, BoxShadow, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, RenderOnce, SharedString, StatefulInteractiveElement, Styled as _,
    Window, div, point, px,
};

use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};

/// The view shown by [`InstantTooltip`]: 12 sp text, optional keybinding hint, shadow, dark-theme border.
pub struct InstantTooltip {
    text: SharedString,
    hint: Option<SharedString>,
}

impl InstantTooltip {
    /// A tooltip view with an optional trailing keybinding hint such as `Ctrl+O`.
    pub fn new(text: impl Into<SharedString>, hint: Option<SharedString>) -> Self {
        Self {
            text: text.into(),
            hint,
        }
    }
}

impl Render for InstantTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        div().pt(dp(metrics::TOOLTIP_GAP)).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .p(dp(metrics::TOOLTIP_PAD))
                .rounded(dp(metrics::RADIUS))
                .bg(tokens.background)
                .shadow(vec![BoxShadow {
                    color: tokens.menu_shadow(),
                    offset: point(px(0.), px(2.)),
                    blur_radius: px(8.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .when(!tokens.is_light, |t| {
                    t.border(dp(2.)).border_color(tokens.popup_border())
                })
                .text_tooltip()
                .text_color(tokens.on_background)
                .whitespace_nowrap()
                .child(self.text.clone())
                .when_some(self.hint.clone(), |t, hint| {
                    t.child(div().w(dp(8.))).child(KeybindingHint::new(hint))
                }),
        )
    }
}

/// The view shown by [`TooltipExt::delayed_tooltip`]: a card with 14 sp text.
pub struct DelayedTooltip {
    text: SharedString,
}

impl DelayedTooltip {
    /// A tooltip view with plain text.
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl Render for DelayedTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        div()
            .p(dp(metrics::TOOLTIP_PAD))
            .rounded(dp(metrics::RADIUS))
            .bg(tokens.background)
            .shadow(vec![BoxShadow {
                color: tokens.menu_shadow(),
                offset: point(px(0.), px(3.)),
                blur_radius: px(10.),
                spread_radius: px(0.),
                inset: false,
            }])
            .when(!tokens.is_light, |t| {
                t.border(dp(2.))
                    .border_color(tokens.on_background_secondary.alpha(0.2))
            })
            .text_body1()
            .text_color(tokens.on_background)
            .child(self.text.clone())
    }
}

/// Tooltip helpers for elements with an id.
pub trait TooltipExt: StatefulInteractiveElement + Sized {
    /// Shows an [`InstantTooltip`] without delay.
    fn instant_tooltip(self, text: impl Into<SharedString>, hint: Option<SharedString>) -> Self {
        let text = text.into();
        self.tooltip(move |_, cx: &mut App| {
            let view = cx.new(|_| InstantTooltip::new(text.clone(), hint.clone()));
            AnyView::from(view)
        })
        .tooltip_show_delay(Duration::ZERO)
    }

    /// Shows a [`DelayedTooltip`] after [`metrics::TOOLTIP_DELAY_MS`].
    fn delayed_tooltip(self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        self.tooltip(move |_, cx: &mut App| {
            let view = cx.new(|_| DelayedTooltip::new(text.clone()));
            AnyView::from(view)
        })
        .tooltip_show_delay(Duration::from_millis(metrics::TOOLTIP_DELAY_MS))
    }
}

impl<T: StatefulInteractiveElement> TooltipExt for T {}

/// Text with a delayed tooltip (Gitnuro `TooltipText`).
#[derive(gpui_kit::IntoElement)]
pub struct TooltipText {
    id: SharedString,
    text: SharedString,
    tooltip: SharedString,
}

impl TooltipText {
    /// `text` that shows `tooltip` after 500 ms of hovering. `id` must be unique in the frame.
    pub fn new(
        id: impl Into<SharedString>,
        text: impl Into<SharedString>,
        tooltip: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            tooltip: tooltip.into(),
        }
    }
}

impl RenderOnce for TooltipText {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .id(self.id)
            .child(self.text)
            .delayed_tooltip(self.tooltip)
    }
}

/// A keybinding hint such as `Ctrl+Shift+S` (07 §5.1.4): mono 11 sp medium, `on_background_secondary`.
#[derive(gpui_kit::IntoElement)]
pub struct KeybindingHint {
    text: SharedString,
}

impl KeybindingHint {
    /// A hint with ready-made text (`Ctrl+O`).
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }

    /// Builds the hint text from its parts in Gitnuro's order: `Ctrl`, `Meta`, `Alt`, `Shift`, then the key.
    pub fn text_of(ctrl: bool, meta: bool, alt: bool, shift: bool, key: &str) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if ctrl {
            parts.push("Ctrl");
        }
        if meta {
            parts.push("⌘");
        }
        if alt {
            parts.push("Alt");
        }
        if shift {
            parts.push("Shift");
        }
        parts.push(key);
        parts.join("+")
    }
}

impl RenderOnce for KeybindingHint {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .text_mono_caption()
            .text_color(cx.tokens().on_background_secondary)
            .child(self.text)
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn keybinding_text_follows_gitnuro_order() {
        assert_eq!(
            KeybindingHint::text_of(true, false, false, false, "O"),
            "Ctrl+O"
        );
        assert_eq!(
            KeybindingHint::text_of(true, false, false, true, "S"),
            "Ctrl+Shift+S"
        );
        assert_eq!(
            KeybindingHint::text_of(true, false, true, false, "S"),
            "Ctrl+Alt+S"
        );
    }
}
