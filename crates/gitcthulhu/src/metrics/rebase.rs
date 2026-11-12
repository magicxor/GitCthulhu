//! Layout constants of the interactive rebase editor (owned by G9 after G0b; 08 §6).

/// Title ("Rebase interactive") padding at the start (08 §6).
pub const TITLE_PAD_START: f32 = 16.;
/// Title padding at the top (08 §6).
pub const TITLE_PAD_TOP: f32 = 16.;
/// Title padding at the bottom (08 §6).
pub const TITLE_PAD_BOTTOM: f32 = 8.;
/// Horizontal padding of a step row (08 §6).
pub const ROW_PAD_H: f32 = 16.;
/// Vertical padding of a step row (08 §6).
pub const ROW_PAD_V: f32 = 8.;
/// Width of the action dropdown; Gitnuro's 120 dp button under the 180 dp dropdown is a quirk, the visible
/// width is 180 (08 §6, §9.9).
pub const ACTION_W: f32 = 180.;
/// Minimum height of the message field (08 §6).
pub const MESSAGE_MIN_H: f32 = 40.;
/// Gap between the action dropdown and the message field (08 §6).
pub const MESSAGE_PAD_START: f32 = 8.;
/// Border of the message field (08 §6).
pub const MESSAGE_BORDER: f32 = 2.;
/// Horizontal text padding inside the message field's border (the border itself is drawn over the 12 dp content
/// padding of the outlined field, 07 §10.1).
pub const MESSAGE_PAD_H: f32 = 10.;
/// Vertical text padding inside the message field's border (Gitnuro's 8 dp with the 2 dp border drawn over it,
/// 07 §10.1); single lines are centred in the 40 dp minimum height.
pub const MESSAGE_PAD_V: f32 = 6.;
/// Border of the box around the action dropdown (`onBackgroundSecondary` at 20 %, drawn over the dropdown's own
/// border, 08 §6 ActionDropdown).
pub const ACTION_BORDER: f32 = 2.;
/// Most lines the message editor grows to before it scrolls.
pub const MESSAGE_MAX_LINES: usize = 12;
/// Bottom padding of the button row (08 §6).
pub const FOOTER_PAD_BOTTOM: f32 = 16.;
/// Padding after the Cancel button (08 §6).
pub const CANCEL_PAD_END: f32 = 8.;
/// Padding after the "Complete rebase" button (08 §6).
pub const COMPLETE_PAD_END: f32 = 16.;
/// Corner radius clipping the editor (08 §6).
pub const CLIP_RADIUS: f32 = 4.;
/// Pixels of auto-scroll per pointer move per pixel the pointer is beyond the list edge (08 §6).
pub const AUTOSCROLL_GAIN: f32 = 1.;
