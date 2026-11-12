//! Sizes and timings of the AI additions (spec 21 §5.3, §2.10).

/// Height of the "Generate" button in the commit box's checkbox row (21 §5.3).
pub const BUTTON_H: f32 = 24.;
/// Horizontal padding of the button.
pub const BUTTON_PAD_H: f32 = 8.;
/// Size of the button's icon.
pub const BUTTON_ICON: f32 = 16.;
/// Gap between the icon and the label.
pub const BUTTON_ICON_GAP: f32 = 4.;
/// Streamed text is applied to the message field at most this often (21 §2.10).
pub const STREAM_APPLY_MS: u64 = 30;
/// A key typed or pasted into the API key field is stored after this quiet period.
pub const KEY_DEBOUNCE_MS: u64 = 500;
/// Width of the "Stored" badge next to the key field is content; this is its horizontal padding.
pub const BADGE_PAD_H: f32 = 8.;
/// Vertical padding of the "Stored" badge.
pub const BADGE_PAD_V: f32 = 2.;
/// Gap between the controls of one settings row.
pub const ROW_CONTROL_GAP: f32 = 8.;
