//! Buttons (Gitnuro 07 §5.1.2, §5.1.3, §10.7-§10.9, §4.3; 08 §0.5; 09 §0.3).
//!
//! All buttons are flat: the background is the surrounding surface and only the hover and press layers show the
//! bounds. Disabled buttons keep the hand cursor but lose click handling.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    transparent_black,
};
use std::cell::Cell;
use std::rc::Rc;

use super::tooltip::TooltipExt as _;
use super::{BoundsCell, Handler, track_bounds, with_layer};
use crate::icons::{AppIcon, icon};
use crate::menus::{self, MenuItem};
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};

fn handler(f: impl Fn(&mut Window, &mut App) + 'static) -> Handler {
    Rc::new(f)
}

/// Gitnuro `PrimaryButton`: filled with `primary`, 14 sp label, height about 29 dp. The *cancel* variant used by
/// dialogs is the same button with a transparent background and `on_background` text.
#[derive(IntoElement)]
pub struct PrimaryButton {
    id: ElementId,
    label: SharedString,
    enabled: bool,
    cancel: bool,
    on_click: Option<Handler>,
}

impl PrimaryButton {
    /// An enabled primary button. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            enabled: true,
            cancel: false,
            on_click: None,
        }
    }

    /// The transparent variant used for "Cancel".
    pub fn cancel(mut self) -> Self {
        self.cancel = true;
        self
    }

    /// Enables or disables the button (disabled: 50 % alpha, no click, no hover).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the click handler.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }
}

impl RenderOnce for PrimaryButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let (bg, fg) = if self.cancel {
            (transparent_black(), tokens.on_background)
        } else {
            (tokens.primary, tokens.on_primary)
        };
        let (bg, fg) = if self.enabled {
            (bg, fg)
        } else {
            (bg.opacity(0.5), fg.opacity(0.5))
        };
        let on_click = self.on_click;
        div()
            .id(self.id)
            .flex_none()
            .px(dp(metrics::PRIMARY_BUTTON_PAD_H))
            .py(dp(metrics::PRIMARY_BUTTON_PAD_V))
            .rounded(dp(metrics::RADIUS))
            .bg(bg)
            .text_color(fg)
            .text_body1()
            .cursor_pointer()
            .when(self.enabled, |b| {
                b.hover(move |s| s.bg(with_layer(bg, tokens.hover_layer())))
                    .active(move |s| s.bg(with_layer(bg, tokens.press_layer())))
                    .when_some(on_click, |b, f| {
                        b.on_click(move |_, window, cx| f(window, cx))
                    })
            })
            .child(self.label)
            .test_support()
    }
}

/// Gitnuro `SecondaryButton`: `primary` (or a given) background, 13 sp label, height about 24 dp. No disabled state.
#[derive(IntoElement)]
pub struct SecondaryButton {
    id: ElementId,
    label: SharedString,
    background: Option<Hsla>,
    on_click: Option<Handler>,
}

impl SecondaryButton {
    /// A secondary button. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            background: None,
            on_click: None,
        }
    }

    /// Overrides the background (for example a status colour).
    pub fn background(mut self, color: Hsla) -> Self {
        self.background = Some(color);
        self
    }

    /// Sets the click handler.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }
}

impl RenderOnce for SecondaryButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let bg = self.background.unwrap_or(tokens.primary);
        let on_click = self.on_click;
        div()
            .id(self.id)
            .flex_none()
            .px(dp(metrics::SECONDARY_BUTTON_PAD_H))
            .py(dp(metrics::SECONDARY_BUTTON_PAD_V))
            .rounded(dp(metrics::RADIUS))
            .bg(bg)
            .text_color(tokens.on_primary)
            .text_body2()
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |s| s.bg(with_layer(bg, tokens.hover_layer())))
            .active(move |s| s.bg(with_layer(bg, tokens.press_layer())))
            .when_some(on_click, |b, f| {
                b.on_click(move |_, window, cx| f(window, cx))
            })
            .child(self.label)
            .test_support()
    }
}

/// `SecondaryButton` with an 18 dp icon whose label disappears when `compact` (used by the log header).
#[derive(IntoElement)]
pub struct SecondaryButtonCompactable {
    id: ElementId,
    role: AppIcon,
    label: SharedString,
    compact: bool,
    on_click: Option<Handler>,
}

impl SecondaryButtonCompactable {
    /// A button with `role` and `label`; `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, role: AppIcon, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            role,
            label: label.into(),
            compact: false,
            on_click: None,
        }
    }

    /// Hides the label.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Sets the click handler.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }
}

impl RenderOnce for SecondaryButtonCompactable {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let on_click = self.on_click;
        div()
            .id(self.id)
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(dp(if self.compact { 0. } else { 8. }))
            .px(dp(if self.compact {
                8.
            } else {
                metrics::SECONDARY_BUTTON_PAD_H
            }))
            .py(dp(metrics::SECONDARY_BUTTON_PAD_V))
            .rounded(dp(metrics::RADIUS))
            .bg(tokens.primary)
            .text_color(tokens.on_primary)
            .text_body2()
            .cursor_pointer()
            .hover(move |s| s.bg(with_layer(tokens.primary, tokens.hover_layer())))
            .when_some(on_click, |b, f| {
                b.on_click(move |_, window, cx| f(window, cx))
            })
            .child(icon(self.role, metrics::ICON_MD, tokens.on_primary))
            .when(!self.compact, |b| b.child(self.label))
            .test_support()
    }
}

/// Gitnuro toolbar `MenuButton`: 56 x 56, 24 dp icon above an 11 sp caption, instant tooltip with keybinding hint.
#[derive(IntoElement)]
pub struct MenuButton {
    id: ElementId,
    role: AppIcon,
    title: SharedString,
    enabled: bool,
    tooltip: Option<SharedString>,
    hint: Option<SharedString>,
    margin_start: f32,
    margin_end: f32,
    on_click: Option<Handler>,
}

impl MenuButton {
    /// A toolbar button. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, role: AppIcon, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            role,
            title: title.into(),
            enabled: true,
            tooltip: None,
            hint: None,
            margin_start: 0.,
            margin_end: 0.,
            on_click: None,
        }
    }

    /// Enables or disables the button (disabled: 50 % alpha, no click, hand cursor stays).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Tooltip text (the caller passes the enabled or disabled wording).
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Keybinding hint shown in the tooltip while the button is enabled.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Outer margins in dp.
    pub fn margins(mut self, start: f32, end: f32) -> Self {
        self.margin_start = start;
        self.margin_end = end;
        self
    }

    /// Sets the click handler.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }
}

fn toolbar_content(role: AppIcon, title: SharedString, color: Hsla) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .child(icon(role, metrics::TOOLBAR_ICON, color))
        .child(
            div()
                .text_caption()
                .text_color(color)
                .whitespace_nowrap()
                .child(title),
        )
}

impl RenderOnce for MenuButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let color = if self.enabled {
            tokens.on_background
        } else {
            tokens.disabled_content()
        };
        let on_click = self.on_click;
        let hint = if self.enabled { self.hint } else { None };
        div()
            .ml(dp(self.margin_start))
            .mr(dp(self.margin_end))
            .flex_none()
            .child(
                div()
                    .id(self.id)
                    .size(dp(metrics::MENU_BUTTON))
                    .rounded(dp(metrics::RADIUS))
                    .bg(tokens.surface)
                    .cursor_pointer()
                    .when(self.enabled, |b| {
                        b.hover(move |s| s.bg(with_layer(tokens.surface, tokens.hover_layer())))
                            .active(move |s| s.bg(with_layer(tokens.surface, tokens.press_layer())))
                            .when_some(on_click, |b, f| {
                                b.on_click(move |_, window, cx| f(window, cx))
                            })
                    })
                    .child(toolbar_content(self.role, self.title, color))
                    .when_some(self.tooltip, |b, text| b.instant_tooltip(text, hint))
                    .test_support(),
            )
    }
}

/// Gitnuro `ExtendedMenuButton` (split button): 64 x 56, a 48 dp main part and a 16 dp chevron strip that opens a
/// dropdown anchored at the strip's bottom-left corner.
#[derive(IntoElement)]
pub struct ExtendedMenuButton {
    id: ElementId,
    role: AppIcon,
    title: SharedString,
    enabled: bool,
    tooltip: Option<SharedString>,
    hint: Option<SharedString>,
    margin_start: f32,
    margin_end: f32,
    on_click: Option<Handler>,
    items: Vec<MenuItem>,
    show_icons: bool,
}

impl ExtendedMenuButton {
    /// A split button. `id` must be unique in the frame; the chevron strip gets the id `(id, "chevron")`.
    pub fn new(id: impl Into<ElementId>, role: AppIcon, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            role,
            title: title.into(),
            enabled: true,
            tooltip: None,
            hint: None,
            margin_start: 0.,
            margin_end: 0.,
            on_click: None,
            items: Vec::new(),
            show_icons: true,
        }
    }

    /// Enables or disables both parts.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Tooltip text of the main part.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Keybinding hint shown in the tooltip while the button is enabled.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Outer margins in dp.
    pub fn margins(mut self, start: f32, end: f32) -> Self {
        self.margin_start = start;
        self.margin_end = end;
        self
    }

    /// Sets the main action.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }

    /// The dropdown items.
    pub fn dropdown(mut self, items: Vec<MenuItem>, show_icons: bool) -> Self {
        self.items = items;
        self.show_icons = show_icons;
        self
    }
}

impl RenderOnce for ExtendedMenuButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let color = if self.enabled {
            tokens.on_background
        } else {
            tokens.disabled_content()
        };
        let enabled = self.enabled;
        let on_click = self.on_click;
        let hint = if enabled { self.hint } else { None };
        let chevron_bounds: BoundsCell = Rc::new(Cell::new(Default::default()));
        let items = self.items;
        let show_icons = self.show_icons;
        let chevron_id = ElementId::from((self.id.clone(), "chevron"));
        let main = div()
            .id((self.id.clone(), "main"))
            .flex_1()
            .h_full()
            .cursor_pointer()
            .when(enabled, |m| {
                m.hover(move |s| s.bg(tokens.hover_layer()))
                    .active(move |s| s.bg(tokens.press_layer()))
                    .when_some(on_click, |m, f| {
                        m.on_click(move |_, window, cx| f(window, cx))
                    })
            })
            .child(toolbar_content(self.role, self.title, color))
            .when_some(self.tooltip, |m, text| m.instant_tooltip(text, hint))
            .test_support();
        let bounds_for_click = chevron_bounds.clone();
        let chevron = div()
            .id(chevron_id)
            .relative()
            .flex_none()
            .w(dp(metrics::EXT_CHEVRON_W))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(track_bounds(&chevron_bounds))
            .child(icon(AppIcon::ExpandMore, metrics::ICON_SM, color))
            .when(enabled && !items.is_empty(), |c| {
                c.hover(move |s| s.bg(tokens.hover_layer()))
                    .active(move |s| s.bg(tokens.press_layer()))
                    .on_click(move |_, window, cx| {
                        let anchor = bounds_for_click.get().bottom_left();
                        menus::open_dropdown(window, cx, anchor, items.clone(), show_icons);
                    })
            })
            .test_support();
        div()
            .ml(dp(self.margin_start))
            .mr(dp(self.margin_end))
            .flex_none()
            .child(
                div()
                    .id(self.id)
                    .w(dp(metrics::EXT_MENU_BUTTON_W))
                    .h(dp(metrics::MENU_BUTTON))
                    .flex()
                    .flex_row()
                    .rounded(dp(metrics::RADIUS))
                    .overflow_hidden()
                    .bg(tokens.surface)
                    .child(main)
                    .child(chevron)
                    .test_support(),
            )
    }
}

/// Gitnuro `IconButton`: a 48 dp hit box (clamped by its row) with a circular hover layer and an 18 dp icon.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    role: AppIcon,
    color: Option<Hsla>,
    size: f32,
    tooltip: Option<SharedString>,
    on_click: Option<Handler>,
}

impl IconButton {
    /// An icon button. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, role: AppIcon) -> Self {
        Self {
            id: id.into(),
            role,
            color: None,
            size: metrics::ICON_MD,
            tooltip: None,
            on_click: None,
        }
    }

    /// Overrides the icon tint (default `on_background`).
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Overrides the icon size in dp.
    pub fn icon_size(mut self, size_dp: f32) -> Self {
        self.size = size_dp;
        self
    }

    /// Instant tooltip text.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Sets the click handler.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let on_click = self.on_click;
        div()
            .id(self.id)
            .flex_none()
            .min_w(dp(48.))
            .h(dp(48.))
            .max_h_full()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .hover(move |s| s.bg(tokens.hover_layer()))
            .active(move |s| s.bg(tokens.press_layer()))
            .when_some(on_click, |b, f| {
                b.on_click(move |_, window, cx| f(window, cx))
            })
            .child(icon(
                self.role,
                self.size,
                self.color.unwrap_or(tokens.on_background),
            ))
            .when_some(self.tooltip, |b, text| b.instant_tooltip(text, None))
            .test_support()
    }
}

/// A tinted icon of a given size (status icons, state letters).
#[derive(IntoElement)]
pub struct StateIcon {
    role: AppIcon,
    color: Hsla,
    size: f32,
}

impl StateIcon {
    /// An icon of `size_dp` tinted `color`.
    pub fn new(role: AppIcon, color: Hsla, size_dp: f32) -> Self {
        Self {
            role,
            color,
            size: size_dp,
        }
    }
}

impl RenderOnce for StateIcon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        icon(self.role, self.size, self.color)
    }
}

/// Welcome-page `ButtonTile`: 280 x 56, `primary` background, 1 dp border, 24 dp icon and 14 sp label.
#[derive(IntoElement)]
pub struct ButtonTile {
    id: ElementId,
    role: AppIcon,
    label: SharedString,
    on_click: Option<Handler>,
}

impl ButtonTile {
    /// A tile. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, role: AppIcon, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            role,
            label: label.into(),
            on_click: None,
        }
    }

    /// Sets the click handler.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }
}

impl RenderOnce for ButtonTile {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let on_click = self.on_click;
        div()
            .id(self.id)
            .flex_none()
            .w(dp(280.))
            .h(dp(56.))
            .px(dp(16.))
            .py(dp(8.))
            .flex()
            .flex_row()
            .items_center()
            .rounded(dp(metrics::RADIUS))
            .border(dp(1.))
            .border_color(tokens.on_background.alpha(0.12))
            .bg(tokens.primary)
            .text_body1()
            .text_color(tokens.on_primary)
            .cursor_pointer()
            .hover(move |s| s.bg(with_layer(tokens.primary, tokens.hover_layer())))
            .active(move |s| s.bg(with_layer(tokens.primary, tokens.press_layer())))
            .when_some(on_click, |b, f| {
                b.on_click(move |_, window, cx| f(window, cx))
            })
            .child(
                div()
                    .mr(dp(8.))
                    .child(icon(self.role, 24., tokens.on_primary)),
            )
            .child(div().whitespace_nowrap().child(self.label))
            .test_support()
    }
}

/// Gitnuro `IconTextButton` (a text button with a 24 dp icon): 280 x 40, `primary_variant` content, 14 sp medium.
#[derive(IntoElement)]
pub struct IconTextButton {
    id: ElementId,
    role: AppIcon,
    label: SharedString,
    on_click: Option<Handler>,
}

impl IconTextButton {
    /// A text button. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, role: AppIcon, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            role,
            label: label.into(),
            on_click: None,
        }
    }

    /// Sets the click handler.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }
}

impl RenderOnce for IconTextButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let on_click = self.on_click;
        div()
            .id(self.id)
            .flex_none()
            .w(dp(280.))
            .h(dp(40.))
            .px(dp(8.))
            .py(dp(8.))
            .flex()
            .flex_row()
            .items_center()
            .rounded(dp(metrics::RADIUS))
            .text_button()
            .text_color(tokens.primary_variant)
            .cursor_pointer()
            .hover(move |s| s.bg(tokens.hover_layer()))
            .active(move |s| s.bg(tokens.press_layer()))
            .when_some(on_click, |b, f| {
                b.on_click(move |_, window, cx| f(window, cx))
            })
            .child(
                div()
                    .mr(dp(8.))
                    .child(icon(self.role, 24., tokens.primary_variant)),
            )
            .child(div().whitespace_nowrap().child(self.label))
            .test_support()
    }
}

/// Gitnuro `TextLink`: `primary_variant` text that turns `on_background` on hover, optional URL tooltip.
#[derive(IntoElement)]
pub struct TextLink {
    id: ElementId,
    text: SharedString,
    tooltip: Option<SharedString>,
    on_click: Option<Handler>,
}

impl TextLink {
    /// A link. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            tooltip: None,
            on_click: None,
        }
    }

    /// Delayed tooltip, typically the URL.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// Sets the click handler.
    pub fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(handler(f));
        self
    }
}

impl RenderOnce for TextLink {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let on_click = self.on_click;
        div()
            .id(self.id)
            .flex_none()
            .whitespace_nowrap()
            .text_color(tokens.primary_variant)
            .cursor_pointer()
            .hover(move |s| s.text_color(tokens.on_background))
            .when_some(on_click, |b, f| {
                b.on_click(move |_, window, cx| f(window, cx))
            })
            .child(self.text)
            .when_some(self.tooltip, |b, text| b.delayed_tooltip(text))
            .test_support()
    }
}
