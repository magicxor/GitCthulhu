//! Layout constants of the status pane (owned by G5 after G0b; 08 §4). All values are dp.

/// Height of the commit box at the bottom of the pane (08 §4.1).
pub const COMMIT_BOX_H: f32 = 192.; // 08 §4.1
/// Gap below each changes list (`padding bottom 4`, 08 §4.1).
pub const LIST_GAP: f32 = 4.; // 08 §4.1
/// Height of the optional search strip: a 38 dp field plus 4 dp padding on both sides (08 §4.3).
pub const SEARCH_STRIP_H: f32 = 46.; // 08 §4.3
/// Start padding of a list title in the header (08 §4.2).
pub const HEADER_TITLE_PAD_START: f32 = 16.; // 08 §4.2
/// End padding of a list title in the header (08 §4.2).
pub const HEADER_TITLE_PAD_END: f32 = 8.; // 08 §4.2
/// Start margin of the header action button (08 §4.2).
pub const HEADER_ACTION_MARGIN_START: f32 = 4.; // 08 §4.2
/// End margin of the header action button (08 §4.2).
pub const HEADER_ACTION_MARGIN_END: f32 = 16.; // 08 §4.2
/// Horizontal padding inside the header action button (08 §4.2).
pub const HEADER_ACTION_PAD_H: f32 = 4.; // 08 §4.2
/// Start padding of the icon of the header action button (08 §4.2).
pub const HEADER_ACTION_ICON_PAD_START: f32 = 12.; // 08 §4.2
/// End padding of the icon of the header action button (08 §4.2).
pub const HEADER_ACTION_ICON_PAD_END: f32 = 8.; // 08 §4.2
/// Vertical padding of the label of the header action button (08 §4.2).
pub const HEADER_ACTION_LABEL_PAD_V: f32 = 4.; // 08 §4.2
/// End padding of the label of the header action button (08 §4.2).
pub const HEADER_ACTION_LABEL_PAD_END: f32 = 12.; // 08 §4.2
/// Height of the commit box's checkbox row (08 §4.6).
pub const CHECKBOX_ROW_H: f32 = 28.; // 08 §4.6
/// Height of the Commit / Amend / state buttons (08 §4.6).
pub const BUTTON_H: f32 = 36.; // 08 §4.6
/// Gap above the Commit button in the SAFE layout (08 §4.6).
pub const COMMIT_BUTTON_GAP_TOP: f32 = 2.; // 08 §4.6
/// Gap above the state button row of merge, rebase, cherry-pick and revert (08 §4.6).
pub const STATE_ROW_PAD_TOP: f32 = 8.; // 08 §4.6
/// Padding between the two state buttons on each side of their seam (08 §4.6).
pub const STATE_BUTTON_PAD: f32 = 4.; // 08 §4.6
/// Start padding of the message field's label and text (21 §4.6).
pub const MESSAGE_PAD_H: f32 = 16.; // 21 §4.6
/// Y offset of the label while it floats above the text (21 §4.6).
pub const MESSAGE_LABEL_TOP_FLOATING: f32 = 8.; // 21 §4.6
/// Y offset of the label while the field is empty and unfocused (21 §4.6).
pub const MESSAGE_LABEL_TOP_RESTING: f32 = 20.; // 21 §4.6
/// Y offset where the message text starts (21 §4.6).
pub const MESSAGE_TEXT_TOP: f32 = 28.; // 21 §4.6
/// Font size of the floating label in dp (21 §4.6).
pub const MESSAGE_LABEL_FLOATING_SIZE: f32 = 12.; // 21 §4.6
/// Height of the indeterminate progress bar at the top of the pane (08 §4.1).
pub const PROGRESS_H: f32 = 4.; // 08 §4.1
/// Delay of the debounced draft save in milliseconds (08 §4.6, 05 §2.16).
pub const DRAFT_SAVE_MS: u64 = 1000; // 05 §2.16
