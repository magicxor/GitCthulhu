//! Projection of [`Tokens`] onto the gpui-kit `Theme` (spec 21 §3.3).
//!
//! Called from `theme::apply` after `Theme::change`, inside a second `Theme::update` (an update in the same closure
//! as a mode change is overwritten).

use gpui_kit::component::Theme;
use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::{Hsla, Rgba, px, transparent_black};

use super::Tokens;
use crate::fonts::{FONT_MONO, FONT_UI};
use crate::metrics::{BASE_REM, RADIUS, RADIUS_LG};

/// Mono font size in dp at scale 1.0 (the `Mono13` text kind).
const MONO_SIZE: f32 = 13.0;

/// The unchecked switch track (07 §10.4): the RGB average of `on_background` and `primary` at 40 % alpha.
pub fn switch_track(tokens: &Tokens) -> Hsla {
    let a = tokens.on_background.to_rgb();
    let b = tokens.primary.to_rgb();
    Hsla::from(Rgba {
        r: (a.r + b.r) / 2.,
        g: (a.g + b.g) / 2.,
        b: (a.b + b.b) / 2.,
        a: 0.4,
    })
}

/// Writes the token-derived colours, fonts, radii and the always-visible scrollbar mode into `theme`.
/// `scale` is the Appearance scale setting (1.0 = 100 %).
pub fn apply(theme: &mut Theme, t: &Tokens, scale: f32) {
    let c = &mut theme.colors;
    c.background = t.background;
    c.popover = t.background;
    c.input = t.background;
    c.list = t.background;
    c.table = t.background;

    c.foreground = t.on_background;
    c.popover_foreground = t.on_background;
    c.muted_foreground = t.on_background_secondary;

    c.primary = t.primary;
    c.button_primary = t.primary;
    c.primary_foreground = t.on_primary;
    c.button_primary_foreground = t.on_primary;

    c.danger = t.error;
    c.button_danger = t.error;
    c.danger_foreground = t.on_error;

    c.warning = t.secondary;
    c.warning_foreground = t.on_secondary;

    c.accent = t.primary_variant;
    c.ring = t.primary_variant;
    c.caret = t.primary_variant;
    c.link = t.primary_variant;
    c.drag_border = t.primary_variant;

    c.selection = t.primary.alpha(0.4);
    c.border = t.field_border();

    c.list_active = t.background_selected;
    c.table_active = t.background_selected;
    c.list_hover = t.hover_layer();

    c.overlay = t.dialog_overlay;

    c.scrollbar_thumb = t.normal_scrollbar;
    c.scrollbar_thumb_hover = t.hover_scrollbar;
    c.scrollbar = transparent_black();

    c.switch = switch_track(t);
    c.switch_thumb = t.on_secondary;

    c.title_bar = t.surface;
    c.status_bar = t.surface;
    c.tab_bar = t.surface;
    c.sidebar = t.surface;

    theme.font_family = FONT_UI.into();
    // The rem base: every `dp()` length scales through it (spec 21 §3.5).
    theme.font_size = px(BASE_REM * scale);
    theme.mono_font_family = FONT_MONO.into();
    theme.mono_font_size = px(MONO_SIZE * scale);
    theme.radius = px(RADIUS * scale);
    theme.radius_lg = px(RADIUS_LG * scale);
    // Gitnuro scrollbars are always visible (07 §10.6).
    theme.scrollbar_mode = ScrollbarMode::Always;
}
