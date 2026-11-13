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
