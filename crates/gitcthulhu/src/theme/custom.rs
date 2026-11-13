//! Custom theme JSON (owned by G11; spec 21 §3, 07 §13.1): Gitnuro's 28 colour keys plus `isLight`, colours as
//! ARGB hex strings (`"primary": "FF0070D8"`).
//!
//! [`parse`] reads the format (missing colour keys take the Dark value, unknown keys are ignored, malformed
//! values are errors) and [`export`] writes every key, which fixes Gitnuro's empty export (07 §13.1).

use gpui_kit::{Hsla, Rgba};

use super::{CustomThemeError, Tokens, builtin, parse_custom};

/// The camelCase names of the 28 colour keys, in Gitnuro's order.
pub fn colour_keys() -> impl Iterator<Item = &'static str> {
    builtin::DARK_ARGB.iter().map(|(key, _)| *key)
}

/// Whether `key` is one of the 28 colour keys.
pub fn is_colour_key(key: &str) -> bool {
    colour_keys().any(|k| k == key)
}

/// Parses an ARGB hex colour: optional `#`, 1 to 16 hex digits. Like Compose's `toLong(16)` the value is masked to
/// its low 32 bits, so a 6-digit value has alpha 0.
pub fn parse_argb(text: &str) -> Option<u32> {
    let digits = text.trim().trim_start_matches('#');
    if digits.is_empty() || digits.len() > 16 {
        return None;
    }
    u64::from_str_radix(digits, 16)
        .ok()
        .map(|v| (v & 0xFFFF_FFFF) as u32)
}

/// `0xAARRGGBB` of a colour.
pub fn to_argb(color: Hsla) -> u32 {
    let rgba = Rgba::from(color);
    let channel = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
    (channel(rgba.a) << 24) | (channel(rgba.r) << 16) | (channel(rgba.g) << 8) | channel(rgba.b)
}

/// Parses a custom theme JSON.
///
/// Errors: the text is not JSON, is not an object, or a colour key holds something other than a hex string, or
/// `isLight` is not a boolean.
pub fn parse(json: &str) -> Result<Tokens, CustomThemeError> {
    parse_custom(json)
}

/// The JSON of `tokens`: all 28 colour keys as 8-digit upper-case ARGB plus `isLight`, pretty-printed in Gitnuro's key order. The result
/// parses back to the same tokens (colours are quantised to 8 bits per channel).
pub fn export(tokens: &Tokens) -> String {
    let colours = [
        ("primary", tokens.primary),
        ("primaryVariant", tokens.primary_variant),
        ("onPrimary", tokens.on_primary),
        ("secondary", tokens.secondary),
        ("onSecondary", tokens.on_secondary),
        ("onBackground", tokens.on_background),
        ("onBackgroundSecondary", tokens.on_background_secondary),
        ("error", tokens.error),
        ("onError", tokens.on_error),
        ("background", tokens.background),
        ("backgroundSelected", tokens.background_selected),
        ("surface", tokens.surface),
        ("secondarySurface", tokens.secondary_surface),
        ("tertiarySurface", tokens.tertiary_surface),
        ("addFile", tokens.add_file),
        ("deletedFile", tokens.deleted_file),
        ("modifiedFile", tokens.modified_file),
        ("conflictingFile", tokens.conflicting_file),
        ("dialogOverlay", tokens.dialog_overlay),
        ("normalScrollbar", tokens.normal_scrollbar),
        ("hoverScrollbar", tokens.hover_scrollbar),
        ("diffLineAdded", tokens.diff_line_added),
        ("diffContentAdded", tokens.diff_content_added),
        ("diffLineRemoved", tokens.diff_line_removed),
        ("diffContentRemoved", tokens.diff_content_removed),
        ("diffKeyword", tokens.diff_keyword),
        ("diffAnnotation", tokens.diff_annotation),
        ("diffComment", tokens.diff_comment),
    ];
    let mut out = String::from("{\n");
    for (key, color) in colours {
        out.push_str(&format!("  \"{key}\": \"{:08X}\",\n", to_argb(color)));
    }
    out.push_str(&format!("  \"isLight\": {}\n}}", tokens.is_light));
    out
}
