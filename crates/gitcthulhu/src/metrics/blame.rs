//! Layout constants of the blame view (owned by G8 after G0b; 09 §2.1). All values are dp.

/// Header bar height of the blame view and of the file history view (09 §2.1, §2.2).
pub const HEADER_H: f32 = 40.; // 09 §2.1
/// Header bar padding at both ends (09 §2.1).
pub const HEADER_PAD: f32 = 8.; // 09 §2.1
/// Horizontal padding of the path text in the header (09 §2.1).
pub const HEADER_TITLE_PAD_H: f32 = 16.; // 09 §2.1
/// Size of the close icon in the header and the minimized bar (09 §2.1: `close.svg`, 24 dp).
pub const CLOSE_ICON: f32 = 24.; // 09 §2.1
/// Height of one blame row. Gitnuro's row is ~33 dp from font metrics (09 §2.1); the list is virtualised, so the
/// height is fixed at 34 dp (21 §4.0).
pub const ROW_H: f32 = 34.; // 09 §2.1, 21 §4.0
/// Width of the commit cell (author and message), also the width of the gutter-to-code separator block.
pub const COMMIT_CELL_W: f32 = 200.; // 09 §2.1
/// Left padding of the author and the message in the commit cell.
pub const COMMIT_TEXT_PAD_START: f32 = 16.; // 09 §2.1
/// Horizontal padding of the line-number gutter.
pub const GUTTER_PAD_H: f32 = 4.; // 09 §2.1
/// Horizontal padding of the code column.
pub const CODE_PAD_H: f32 = 16.; // 09 §2.1
/// Height of the minimized blame bar.
pub const MINIMIZED_H: f32 = 52.; // 09 §2.1
/// Horizontal padding of the minimized bar's text column and of its close button.
pub const MINIMIZED_PAD_H: f32 = 16.; // 09 §2.1
/// Code text is displayed with tabs as this many spaces (09 §2.1, no tab stops).
pub const TAB_SPACES: usize = 4; // 09 §2.1
/// Lines longer than this are cut for display (deviation 23: minified files must stay smooth); copying uses the
/// full line.
pub const MAX_LINE_BYTES: usize = 2_000; // 21 §6 #23
/// Advance of the mono font as a fraction of its size (Geist Mono: 600 of 1000 units), the fallback when the font
/// cannot be measured.
pub const MONO_ADVANCE_EM: f32 = 0.6; // 21 §3.4
/// Size of the code and gutter text (`body1`, 14 sp).
pub const CODE_TEXT: f32 = 14.; // 09 §2.1
/// Selection background alpha (09 §0.3: primary at 0.4).
pub const SELECTION_ALPHA: f32 = 0.4; // 09 §0.3
