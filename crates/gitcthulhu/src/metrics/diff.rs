//! Layout constants of the diff view (owned by G7a; Gitnuro 09 §1). All values in dp.

// header - 09 §1.2
/// Height of the diff header row.
pub const HEADER_H: f32 = 34.; // 09 §1.2
/// Header padding at the start.
pub const HEADER_PAD_START: f32 = 16.; // 09 §1.2
/// Header padding at the end.
pub const HEADER_PAD_END: f32 = 8.; // 09 §1.2
/// Gap between the children of the header's right group.
pub const HEADER_GROUP_GAP: f32 = 16.; // 09 §1.2 spacedBy 16
/// Padding after the file name.
pub const FILE_NAME_PAD_END: f32 = 16.; // 09 §1.2
/// A diff-type toggle (StateIcon box).
pub const TOGGLE: f32 = 32.; // 09 §1.2
/// Padding around the toggle icon.
pub const TOGGLE_PAD: f32 = 4.; // 09 §1.2
/// Toggle icon size.
pub const TOGGLE_ICON: f32 = 24.; // 09 §1.2
/// Gap between the hunks/full-file pair and the unified/split pair.
pub const TOGGLE_PAIR_GAP: f32 = 16.; // 09 §1.2 padding end 16
/// Close icon size.
pub const CLOSE_ICON: f32 = 24.; // 09 §1.2
/// Height of the path-only header shown while loading.
pub const LOADING_HEADER_H: f32 = 40.; // 09 §1.2 PathOnlyDiffHeader
/// Horizontal padding of the path-only header.
pub const LOADING_HEADER_PAD_H: f32 = 8.; // 09 §1.2
/// Horizontal padding of the path text in the path-only header.
pub const LOADING_PATH_PAD_H: f32 = 16.; // 09 §1.2
/// Height of the loading progress bar.
pub const PROGRESS_H: f32 = 4.; // 09 §0.3 LinearProgressIndicator
/// Delay before the loading header appears (fast loads never flash it).
pub const LOADING_DELAY_MS: u64 = 150;

// text rows - 09 §1.3, §1.4
/// Height of one unwrapped text line: mono 13 at line height 1.30 (21 §6 deviation 25).
pub const LINE_H: f32 = 13. * 1.30;
/// Hunk header padding, horizontal.
pub const HUNK_PAD_H: f32 = 8.; // 09 §1.3
/// Hunk header padding, vertical.
pub const HUNK_PAD_V: f32 = 4.; // 09 §1.3
/// Horizontal margin around each hunk button.
pub const HUNK_BUTTON_MARGIN_H: f32 = 16.; // 09 §1.3
/// Line-number padding at the start.
pub const NUMBER_PAD_START: f32 = 8.; // 09 §1.3
/// Line-number padding at the end.
pub const NUMBER_PAD_END: f32 = 4.; // 09 §1.3
/// Padding at the start of a text cell (the hover chip sits in it).
pub const TEXT_PAD_START: f32 = 16.; // 09 §1.3
/// Size of the per-line stage chip.
pub const CHIP: f32 = 16.; // 09 §1.3
/// Corner radius of the chip.
pub const CHIP_RADIUS: f32 = 2.; // 09 §1.3
/// Width of the split-view divider.
pub const SPLIT_DIVIDER_W: f32 = 4.; // 09 §1.4
/// Overdraw of the virtual list above and below the viewport.
pub const LIST_OVERDRAW: f32 = 400.; // 21 §4.1

// other content - 09 §1.3, §1.7, §1.8
/// Icon size of the "No content changes" view.
pub const NO_CHANGES_ICON: f32 = 96.; // 09 §1.3
/// Gap between icon and text of the "No content changes" view.
pub const NO_CHANGES_GAP: f32 = 16.; // 09 §1.3
/// Outer padding of an image side.
pub const IMAGE_PAD_OUTER: f32 = 24.; // 09 §1.7
/// Inner padding of an image side (towards the other side).
pub const IMAGE_PAD_INNER: f32 = 8.; // 09 §1.7
/// Spacer above a single added image.
pub const IMAGE_SINGLE_SPACER: f32 = 24.; // 09 §1.7
/// Width of the binary-file icon.
pub const BINARY_ICON: f32 = 400.; // 09 §1.7
/// Padding below the submodule title.
pub const SUBMODULE_TITLE_PAD_BOTTOM: f32 = 8.; // 09 §1.8
/// Padding above the "Open submodule" button.
pub const SUBMODULE_BUTTON_PAD_TOP: f32 = 8.; // 09 §1.8
