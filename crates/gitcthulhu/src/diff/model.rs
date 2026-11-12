//! The diff document: rows for unified and split mode, display texts, syntax tokens and line-number strings, built
//! in the background from a `FileDiff` (owned by G7a after G0b; spec 21 §4.1, 09 §1.3-§1.4).

use std::sync::Arc;

use gpui_kit::SharedString;
use parking_lot::Mutex;

use super::highlight::{Highlighter, LineTokens};
use crate::backend::{ByteSlice as _, CancelToken, ChangeKind, DiffContent, FileDiff, LineKind};

/// Display texts longer than this are cut (copy uses the full line; deviation 23).
pub const MAX_DISPLAY_BYTES: usize = 2_000;

/// Unified or side-by-side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffMode {
    Unified,
    Split,
}

/// One row of the diff list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRow {
    /// The header of hunk `hunk` (index into `TextDiff::hunks`).
    HunkHeader { hunk: u32 },
    /// Unified mode: line `line` (index into `TextDiff::lines`).
    Line { line: u32 },
    /// Split mode: the old-side and new-side lines of one row (`None` = grey filler).
    Pair {
        left: Option<u32>,
        right: Option<u32>,
        hunk: u32,
    },
}

/// Line-number strings, created lazily for visible rows (never `format!` per frame).
#[derive(Default)]
pub struct NumberCache {
    strings: Mutex<Vec<Option<SharedString>>>,
}

impl NumberCache {
    /// The string of line number `n`.
    pub fn get(&self, n: u32) -> SharedString {
        let mut strings = self.strings.lock();
        let ix = n as usize;
        if strings.len() <= ix {
            strings.resize(ix + 1, None);
        }
        strings[ix]
            .get_or_insert_with(|| SharedString::from(n.to_string()))
            .clone()
    }
}

/// A diff prepared for rendering.
pub struct DiffDoc {
    pub diff: Arc<FileDiff>,
    pub mode: DiffMode,
    pub rows: Arc<Vec<DiffRow>>,
    /// Per hunk: digits of the largest line number (09 §1.3 `highestLineNumberLength`).
    pub gutter_digits: Arc<Vec<u8>>,
    /// Display text per `DiffLine`: tabs as 4 spaces, cut at 2,000 bytes.
    pub texts: Arc<Vec<SharedString>>,
    /// Syntax tokens per `DiffLine` (empty beyond the highlighter's limits).
    pub syntax: Arc<Vec<LineTokens>>,
    pub numbers: Arc<NumberCache>,
    pub generation: u64,
}

fn display_text(line: &str) -> SharedString {
    let mut text = line.replace('\t', "    ");
    if text.len() > MAX_DISPLAY_BYTES {
        let mut cut = MAX_DISPLAY_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    SharedString::from(text)
}

fn digits(n: u32) -> u8 {
    let mut d = 1;
    let mut n = n / 10;
    while n > 0 {
        d += 1;
        n /= 10;
    }
    d
}

/// Builds the document (background). Split mode is not offered for added or deleted files (forced Unified).
/// Split pairing (09 §1.4): context lines on both sides; a run of removed lines and the added lines after it are
/// paired top-aligned, padded with `None`.
pub fn build_doc(
    diff: Arc<FileDiff>,
    mode: DiffMode,
    generation: u64,
    cancel: &CancelToken,
) -> DiffDoc {
    let mode = match diff.kind {
        ChangeKind::Added | ChangeKind::Deleted | ChangeKind::Untracked => DiffMode::Unified,
        _ => mode,
    };
    let (rows, gutter_digits, texts, syntax) = match &diff.content {
        DiffContent::Text(text) => {
            let texts: Vec<SharedString> = text
                .lines
                .iter()
                .map(|l| display_text(text.line_text(l)))
                .collect();
            let path = diff.path.to_str_lossy();
            let plain: Vec<&str> = texts.iter().map(|t| t.as_ref()).collect();
            let syntax = Highlighter::global().highlight(&path, &plain, cancel);
            let mut rows = Vec::new();
            let mut gutter = Vec::with_capacity(text.hunks.len());
            for (h, hunk) in text.hunks.iter().enumerate() {
                let h = h as u32;
                let lines = &text.lines[hunk.lines.start as usize..hunk.lines.end as usize];
                let max = lines
                    .iter()
                    .flat_map(|l| [l.old_lineno, l.new_lineno])
                    .flatten()
                    .max()
                    .unwrap_or(0);
                gutter.push(digits(max));
                rows.push(DiffRow::HunkHeader { hunk: h });
                match mode {
                    DiffMode::Unified => {
                        rows.extend(hunk.lines.clone().map(|line| DiffRow::Line { line }));
                    }
                    DiffMode::Split => pair_lines(text, hunk.lines.clone(), h, &mut rows),
                }
            }
            (rows, gutter, texts, syntax)
        }
        _ => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
    };
    DiffDoc {
        diff,
        mode,
        rows: Arc::new(rows),
        gutter_digits: Arc::new(gutter_digits),
        texts: Arc::new(texts),
        syntax: Arc::new(syntax),
        numbers: Arc::new(NumberCache::default()),
        generation,
    }
}

fn pair_lines(
    text: &crate::backend::TextDiff,
    range: std::ops::Range<u32>,
    hunk: u32,
    rows: &mut Vec<DiffRow>,
) {
    let mut removed: Vec<u32> = Vec::new();
    let mut added: Vec<u32> = Vec::new();
    let flush = |removed: &mut Vec<u32>, added: &mut Vec<u32>, rows: &mut Vec<DiffRow>| {
        let n = removed.len().max(added.len());
        for i in 0..n {
            rows.push(DiffRow::Pair {
                left: removed.get(i).copied(),
                right: added.get(i).copied(),
                hunk,
            });
        }
        removed.clear();
        added.clear();
    };
    for ix in range {
        match text.lines[ix as usize].kind {
            LineKind::Removed => {
                if !added.is_empty() {
                    flush(&mut removed, &mut added, rows);
                }
                removed.push(ix);
            }
            LineKind::Added => added.push(ix),
            _ => {
                flush(&mut removed, &mut added, rows);
                rows.push(DiffRow::Pair {
                    left: Some(ix),
                    right: Some(ix),
                    hunk,
                });
            }
        }
    }
    flush(&mut removed, &mut added, rows);
}
