//! Layout constants of the file history view (owned by G8 after G0b; 09 §2.2). All values are dp.

/// Width of the commit list on the left (09 §2.2).
pub const LIST_W: f32 = 300.; // 09 §2.2
/// Height of one commit row (16 + 40 + 16, 09 §2.2).
pub const ROW_H: f32 = 72.; // 09 §2.2
/// Vertical padding of a commit row.
pub const ROW_PAD_V: f32 = 16.; // 09 §2.2
/// Avatar diameter.
pub const AVATAR: f32 = 40.; // 09 §2.2
/// Horizontal padding around the avatar and at the right of the date.
pub const ROW_PAD_H: f32 = 16.; // 09 §2.2
/// Commits requested per page.
pub const PAGE_SIZE: usize = 100; // 21 §2.4
/// The next page is requested when the last visible row is this close to the end of the loaded rows.
pub const PAGE_AHEAD: usize = 30; // 21 §2.4

/// Hunk header: horizontal and vertical padding (09 §1.3).
pub const HUNK_PAD_H: f32 = 8.; // 09 §1.3
/// Hunk header vertical padding (09 §1.3).
pub const HUNK_PAD_V: f32 = 4.; // 09 §1.3
/// Line-number column: fixed part of the width (padding start 8 + end 4, 09 §1.3).
pub const NUMBER_PAD: f32 = 12.; // 09 §1.3
/// Line-number column: padding at the start.
pub const NUMBER_PAD_START: f32 = 8.; // 09 §1.3
/// Line-number column: padding at the end.
pub const NUMBER_PAD_END: f32 = 4.; // 09 §1.3
/// Advance of one digit of the 13 sp mono font (09 §1.3: 7.8 dp).
pub const DIGIT_W: f32 = 7.8; // 09 §1.3
/// Left padding of the diff text cell (09 §1.3).
pub const TEXT_PAD_START: f32 = 16.; // 09 §1.3
/// Width of the divider between the two sides of a split diff (09 §1.4).
pub const SPLIT_DIVIDER_W: f32 = 4.; // 09 §1.4
/// Estimated height of a diff row before it is measured (13 sp mono, line height 1.30).
pub const DIFF_ROW_ESTIMATE: f32 = 17.; // 21 §3.4
/// Overdraw of the diff list in dp (21 §4.1).
pub const DIFF_OVERDRAW: f32 = 400.; // 21 §4.1
