//! Selection endpoints as content keys, the selected range, and the text it copies.
//!
//! An endpoint is a byte offset in the display text of one diff line on one side, packed into a gpui-base content
//! key. Keys are resolved when the pointer moves and stay valid while the row they point into is scrolled away, so a
//! selection survives virtualisation; the copied text is read from the source lines of the `TextDiff`, never from
//! the painted rows.

use std::borrow::Cow;
use std::ops::Range;

use gpui_kit::base::TextSelectionContentKey;

use super::{SelKey, SelSide};
use crate::backend::{DiffContent, LineKind};
use crate::diff::model::{DiffDoc, DiffMode};

/// The marker the display text carries at each end where a long line was cut (`model::display_line`).
const WINDOW_MARKER: &str = "\u{2026}";
const SIDE_SHIFT: u32 = 30;
const OFFSET_MASK: u64 = (1 << SIDE_SHIFT) - 1;

/// A selection endpoint: an offset in the display text of a diff line, on one side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Pos {
    pub line: u32,
    pub side: SelSide,
    pub offset: u32,
}

impl Pos {
    /// The order of endpoints: by line, then offset. Both sides of a context line are the same text, so the side does
    /// not take part.
    fn order(self) -> (u32, u32) {
        (self.line, self.offset)
    }
}

fn side_bits(side: SelSide) -> u64 {
    match side {
        SelSide::Old => 0,
        SelSide::New => 1,
        SelSide::Unified => 2,
    }
}

/// Packs an endpoint into a content key (line, side, offset; ordered like the document).
pub(super) fn encode(key: SelKey, offset: usize) -> TextSelectionContentKey {
    let offset = (offset as u64).min(OFFSET_MASK);
    TextSelectionContentKey::new(
        (u64::from(key.line) << 32) | (side_bits(key.side) << SIDE_SHIFT) | offset,
    )
}

/// The endpoint of a content key made by [`encode`].
pub(super) fn decode(key: TextSelectionContentKey) -> Option<Pos> {
    let value = key.value();
    let side = match (value >> SIDE_SHIFT) & 0b11 {
        0 => SelSide::Old,
        1 => SelSide::New,
        2 => SelSide::Unified,
        _ => return None,
    };
    Some(Pos {
        line: (value >> 32) as u32,
        side,
        offset: (value & OFFSET_MASK) as u32,
    })
}

/// What is selected in a diff document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Selection {
    #[default]
    None,
    /// From `start` to `end` (ordered, not empty). `side`: split mode with the opposite side excluded (09 §1.4);
    /// `None` takes every line between the endpoints.
    Range {
        start: Pos,
        end: Pos,
        side: Option<SelSide>,
    },
    /// Select all: every line of the document.
    All,
}

impl Selection {
    /// The selection between two endpoints of `doc` (`None` when they coincide or do not belong to it).
    pub fn between(doc: &DiffDoc, a: Pos, b: Pos, locked: Option<SelSide>) -> Selection {
        let lines = doc.texts.len() as u32;
        if a.line >= lines || b.line >= lines {
            return Selection::None;
        }
        let (start, end) = if a.order() <= b.order() {
            (a, b)
        } else {
            (b, a)
        };
        if start.order() == end.order() {
            return Selection::None;
        }
        let side = match doc.mode {
            DiffMode::Unified => None,
            DiffMode::Split => locked,
        };
        Selection::Range { start, end, side }
    }

    /// Whether anything is selected.
    pub fn is_some(&self) -> bool {
        !matches!(self, Selection::None)
    }

    /// The selected part of the run `key` whose display text is `len` bytes long.
    pub fn run_range(&self, key: SelKey, text: &str) -> Option<Range<usize>> {
        let len = text.len();
        let range = match *self {
            Selection::None => return None,
            Selection::All => 0..len,
            Selection::Range { start, end, side } => {
                if side.is_some_and(|side| side != key.side)
                    || key.line < start.line
                    || key.line > end.line
                {
                    return None;
                }
                let from = if key.line == start.line {
                    start.offset as usize
                } else {
                    0
                };
                let to = if key.line == end.line {
                    end.offset as usize
                } else {
                    len
                };
                floor_boundary(text, from)..floor_boundary(text, to)
            }
        };
        (range.start < range.end).then_some(range)
    }

    /// The source text of the selection: the selected part of each line (whole lines in between, also those never
    /// painted), read from the diff's source lines, not the cut display texts; joined with '\n'.
    pub fn text(&self, doc: &DiffDoc) -> Option<String> {
        let DiffContent::Text(diff) = &doc.diff.content else {
            return None;
        };
        let (first, last, side, start, end) = match *self {
            Selection::None => return None,
            Selection::All => {
                let last = diff.lines.len().checked_sub(1)? as u32;
                (0, last, None, None, None)
            }
            Selection::Range { start, end, side } => {
                (start.line, end.line, side, Some(start), Some(end))
            }
        };
        let mut out = String::new();
        let mut first_line = true;
        for ix in first..=last {
            let Some(line) = diff.lines.get(ix as usize) else {
                break;
            };
            let shown = match side {
                Some(SelSide::Old) => line.kind != LineKind::Added,
                Some(SelSide::New) => line.kind != LineKind::Removed,
                Some(SelSide::Unified) | None => true,
            };
            if !shown {
                continue;
            }
            let source = diff.line_text(line);
            let display = doc.texts.get(ix as usize).map_or("", |t| t.as_ref());
            let from = match start {
                Some(start) if start.line == ix => {
                    source_offset(source, display, start.offset as usize, Edge::Start)
                }
                _ => 0,
            };
            let to = match end {
                Some(end) if end.line == ix => {
                    source_offset(source, display, end.offset as usize, Edge::End)
                }
                _ => source.len(),
            };
            if !first_line {
                out.push('\n');
            }
            first_line = false;
            if from < to {
                out.push_str(&source[from..to]);
            }
        }
        Some(out)
    }
}

fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Which end of a selection an offset is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    Start,
    End,
}

/// The display text of a source line before cutting: tabs as 4 spaces (09 §1.3).
fn expand_tabs(source: &str) -> Cow<'_, str> {
    if source.contains('\t') {
        Cow::Owned(source.replace('\t', "    "))
    } else {
        Cow::Borrowed(source)
    }
}

/// Maps a byte offset in the display text of a line to the source line. The display text is the tab-expanded source,
/// possibly cut to a window (deviation 23): a selection that reaches the visible end of a cut line takes the rest of
/// the source line, one that starts at its visible start takes the line from its beginning. An unknown display
/// transformation selects the whole source line.
pub(super) fn source_offset(source: &str, display: &str, offset: usize, edge: Edge) -> usize {
    if edge == Edge::Start && offset == 0 {
        return 0;
    }
    if edge == Edge::End && offset >= display.len() {
        return source.len();
    }
    let expanded = expand_tabs(source);
    let locate = |text: &str| {
        if expanded.starts_with(text) {
            Some(0)
        } else {
            expanded.find(text)
        }
    };
    // A display cut to a window (deviation 23) carries an ellipsis marker at each cut end; the markers are not
    // part of the source, so the window is located without them and the offset is shifted past the leading one.
    let (front, lead) = if let Some(front) = locate(display) {
        (front, 0)
    } else {
        let lead = if display.starts_with(WINDOW_MARKER) {
            WINDOW_MARKER.len()
        } else {
            0
        };
        let core = &display[lead..];
        let core = core.strip_suffix(WINDOW_MARKER).unwrap_or(core);
        match locate(core).filter(|_| !core.is_empty()) {
            Some(front) => (front, lead),
            None => {
                return match edge {
                    Edge::Start => 0,
                    Edge::End => source.len(),
                };
            }
        }
    };
    let target = front + offset.saturating_sub(lead).min(display.len() - lead);
    let mut position = 0;
    for (ix, ch) in source.char_indices() {
        let width = if ch == '\t' { 4 } else { ch.len_utf8() };
        if target < position + width {
            // Inside a tab's spaces: the nearer side of the tab.
            return if ch == '\t' && target - position >= width / 2 {
                ix + 1
            } else {
                ix
            };
        }
        position += width;
    }
    source.len()
}
