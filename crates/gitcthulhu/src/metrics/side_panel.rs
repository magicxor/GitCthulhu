//! Layout constants of the side panel (owned by G3 after G0b; 07 §6).
//!
//! All values are dp (07 §0): `dp(v)` is `v` logical pixels at scale 1. Row heights come from
//! [`crate::metrics::line_heights`] (`side_item`: 36 spaced, 34 compact).

/// Left margin of the filter field (07 §6.2: 8 dp left, 0 dp right).
pub const FILTER_MARGIN_START: f32 = 8.; // 07 §6.2
/// Height of the filter field (07 §6.2).
pub const FILTER_H: f32 = 38.; // 07 §6.2
/// Padding before the filter's leading search icon, inside the 2 dp border: Compose draws the border over the
/// 12 dp decoration padding (07 §10.1), GPUI lays it out, so the icon starts 12 dp from the field's edge.
pub const FILTER_ICON_PAD_START: f32 = 12. - super::FIELD_BORDER; // 07 §10.1
/// Space between the filter's leading icon and the text.
pub const FILTER_ICON_PAD_END: f32 = 8.; // 07 §10.1 (Spacer 8)
/// Space between the filter's text and the trailing clear button.
pub const FILTER_CLEAR_PAD_START: f32 = 8.; // 07 §10.1 (Spacer 8)
/// Padding after the filter's trailing clear button (12 dp from the field's edge, inside the 2 dp border).
pub const FILTER_CLEAR_PAD_END: f32 = 12. - super::FIELD_BORDER; // 07 §10.1
/// Gap between the filter and the list (`padding(top = 4dp)` of the lazy column).
pub const LIST_TOP_PAD: f32 = 4.; // 07 §6.2

/// Icons of headers and rows.
pub const ICON: f32 = 16.; // 07 §6.3, §6.4
/// Horizontal padding of the header chevron (x = 8..24).
pub const HEADER_CHEVRON_PAD_H: f32 = 8.; // 07 §6.3
/// Horizontal padding of the header title (the text starts at x = 56).
pub const HEADER_TITLE_PAD_H: f32 = 8.; // 07 §6.3
/// Padding after the header's count or "+" button.
pub const HEADER_TRAILING_PAD_END: f32 = 16.; // 07 §6.3

/// Padding before a row's icon (the icon starts at x = 32).
pub const ROW_ICON_PAD_START: f32 = 32.; // 07 §6.4
/// Padding between a row's icon and its text (the text starts at x = 56).
pub const ROW_ICON_PAD_END: f32 = 8.; // 07 §6.4
/// Extra left padding of remote branches (icon x = 56, text x = 80).
pub const REMOTE_BRANCH_EXTRA_PAD: f32 = 24.; // 07 §6.5
/// Horizontal padding of trailing row content ("HEAD", the submodule state letter, ahead/behind).
pub const ROW_TRAILING_PAD_H: f32 = 16.; // 07 §6.5
/// Space between two trailing items of one row (ahead/behind and "HEAD").
pub const ROW_TRAILING_GAP: f32 = 8.;

/// Offset of a header's section icon: chevron box (8 + 16 + 8) before it.
pub const HEADER_ICON_X: f32 = 2. * HEADER_CHEVRON_PAD_H + ICON; // 07 §6.1: x = 32
/// Offset of a header's title.
pub const HEADER_TEXT_X: f32 = HEADER_ICON_X + ICON + HEADER_TITLE_PAD_H; // 07 §6.1: x = 56
/// Offset of a branch, tag, stash, submodule or remote row's icon.
pub const ROW_ICON_X: f32 = ROW_ICON_PAD_START; // 07 §6.1
/// Offset of such a row's text.
pub const ROW_TEXT_X: f32 = ROW_ICON_PAD_START + ICON + ROW_ICON_PAD_END; // 07 §6.1: x = 56
/// Offset of a remote branch's icon.
pub const REMOTE_BRANCH_ICON_X: f32 = REMOTE_BRANCH_EXTRA_PAD + ROW_ICON_X; // 07 §6.1: x = 56
/// Offset of a remote branch's text.
pub const REMOTE_BRANCH_TEXT_X: f32 = REMOTE_BRANCH_EXTRA_PAD + ROW_TEXT_X; // 07 §6.1: x = 80
