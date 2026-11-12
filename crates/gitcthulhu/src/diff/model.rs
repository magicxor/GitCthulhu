//! The diff document: rows for unified and split mode, display texts, syntax tokens and line-number strings, built
//! in the background from a `FileDiff` (owned by G7a; spec 21 §4.1, 09 §1.3-§1.4).

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::SharedString;
use parking_lot::Mutex;
use smallvec::SmallVec;

use super::highlight::{Highlighter, LineTokens};
use crate::backend::{
    ByteSlice as _, CancelToken, ChangeKind, DiffContent, DiffLine, DiffTarget, FileDiff,
    HunkHeader, LineKind, LineRef, LineSelection, PartialAvailability, SelectionRequest, TextDiff,
};

/// Display texts longer than this are cut around the change (copy uses the full line; deviation 23).
pub const MAX_DISPLAY_BYTES: usize = 2_000;

/// Context kept before the first change when a long line is cut (bytes).
const CUT_LEAD: usize = 200;

/// Marks the cut ends of a long display line.
const ELLIPSIS: &str = "\u{2026}";

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

/// Intra-line change ranges of one display line (byte ranges into its display text).
pub type InlineSpans = SmallVec<[Range<u32>; 2]>;

/// A diff prepared for rendering.
pub struct DiffDoc {
    pub diff: Arc<FileDiff>,
    pub mode: DiffMode,
    pub rows: Arc<Vec<DiffRow>>,
    /// Per hunk: digits of the largest line number (09 §1.3 `highestLineNumberLength`).
    pub gutter_digits: Arc<Vec<u8>>,
    /// Display text per `DiffLine`: tabs as 4 spaces, cut at 2,000 bytes around the change.
    pub texts: Arc<Vec<SharedString>>,
    /// Syntax tokens per `DiffLine` (empty beyond the highlighter's limits).
    pub syntax: Arc<Vec<LineTokens>>,
    /// Intra-line change ranges per `DiffLine`, mapped onto the display text (empty when the whole line changed).
    pub inline: Arc<Vec<InlineSpans>>,
    /// Header text per hunk (`@@ -a,b +c,d @@`, 09 §1.1 count rules).
    pub headers: Arc<Vec<SharedString>>,
    pub numbers: Arc<NumberCache>,
    pub generation: u64,
}

impl DiffDoc {
    /// The text diff, if the content is text.
    pub fn text(&self) -> Option<&Arc<TextDiff>> {
        match &self.diff.content {
            DiffContent::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The hunk a row belongs to.
    pub fn row_hunk(&self, row: &DiffRow) -> Option<u32> {
        match *row {
            DiffRow::HunkHeader { hunk } | DiffRow::Pair { hunk, .. } => Some(hunk),
            DiffRow::Line { line } => {
                // Hunks are ordered by their line ranges: binary search.
                let text = self.text()?;
                let after = text.hunks.partition_point(|h| h.lines.start <= line);
                let hunk = after.checked_sub(1)?;
                text.hunks[hunk]
                    .lines
                    .contains(&line)
                    .then_some(hunk as u32)
            }
        }
    }

    /// The line of `ix` (index into `TextDiff::lines`).
    pub fn line(&self, ix: u32) -> Option<&DiffLine> {
        self.text()?.lines.get(ix as usize)
    }

    /// The scroll anchor of row `ix`: a stable description that survives mode switches and reloads.
    pub fn anchor(&self, ix: usize) -> Option<Anchor> {
        let text = self.text()?;
        let line_ix = match *self.rows.get(ix)? {
            DiffRow::HunkHeader { hunk } => {
                let hunk = text.hunks.get(hunk as usize)?;
                let line = text.lines.get(hunk.lines.start as usize)?;
                return Some(Anchor {
                    old: line.old_lineno,
                    new: line.new_lineno,
                    header: true,
                });
            }
            DiffRow::Line { line } => line,
            DiffRow::Pair { left, right, .. } => left.or(right)?,
        };
        let line = text.lines.get(line_ix as usize)?;
        Some(Anchor {
            old: line.old_lineno,
            new: line.new_lineno,
            header: false,
        })
    }

    /// The row that shows `anchor` best, or `None` when nothing matches. The old-side number is preferred: for
    /// uncommitted diffs the old side (index or HEAD) rarely changes while the worktree is edited, so the anchor
    /// follows the content. Added lines (no old number) match by their new number. A first pass requires the same
    /// line kind, a second accepts any line with the number. A header anchor returns the header row of the hunk the
    /// line opens.
    pub fn find_anchor(&self, anchor: &Anchor) -> Option<usize> {
        let text = self.text()?;
        let matches = |line: u32, strict: bool| {
            let Some(l) = text.lines.get(line as usize) else {
                return false;
            };
            match anchor.old {
                Some(old) => {
                    l.old_lineno == Some(old)
                        && (!strict || l.new_lineno.is_some() == anchor.new.is_some())
                }
                None => l.new_lineno == anchor.new && (!strict || l.old_lineno.is_none()),
            }
        };
        let find = |strict: bool| {
            self.rows.iter().position(|row| match *row {
                DiffRow::HunkHeader { .. } => false,
                DiffRow::Line { line } => matches(line, strict),
                DiffRow::Pair { left, right, .. } => {
                    left.is_some_and(|l| matches(l, strict))
                        || right.is_some_and(|r| matches(r, strict))
                }
            })
        };
        let row = find(true).or_else(|| find(false))?;
        if anchor.header && row > 0 && matches!(self.rows[row - 1], DiffRow::HunkHeader { .. }) {
            return Some(row - 1);
        }
        Some(row)
    }
}

/// A position in a diff that does not depend on the row layout (line numbers of the top line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// The anchor row was the hunk header above this line.
    pub header: bool,
}

/// Number of decimal digits of `n` (at least 1).
pub fn digits(n: u32) -> u8 {
    let mut d = 1;
    let mut n = n / 10;
    while n > 0 {
        d += 1;
        n /= 10;
    }
    d
}

/// `@@ -a,b +c,d @@` with Git's count rules: a count of 1 is written as the start only, a count of 0 keeps the
/// start (which Git already sets to the line before, 09 §1.1).
pub fn hunk_title(header: &HunkHeader) -> String {
    fn side(start: u32, lines: u32) -> String {
        if lines == 1 {
            start.to_string()
        } else {
            format!("{start},{lines}")
        }
    }
    format!(
        "@@ -{} +{} @@",
        side(header.old_start, header.old_lines),
        side(header.new_start, header.new_lines)
    )
}

/// The display text of a source line and its intra-line spans mapped onto it: tabs become 4 spaces (09 §1.3);
/// lines over [`MAX_DISPLAY_BYTES`] are cut to a window that starts shortly before the first change (deviation 23),
/// with an ellipsis at each cut end. Spans covering the whole line are dropped (Gitnuro highlights only lines with
/// more than one segment).
pub fn display_line(source: &str, spans: &[Range<u32>]) -> (SharedString, InlineSpans) {
    let tabs = source.bytes().filter(|b| *b == b'\t').count();
    let mut text = if tabs == 0 {
        source.to_owned()
    } else {
        source.replace('\t', "    ")
    };
    // Source offset -> display offset (before the cut): every tab before the offset adds 3 bytes.
    let map = |offset: u32| -> u32 {
        let offset = (offset as usize).min(source.len());
        if tabs == 0 {
            return offset as u32;
        }
        let before = source.as_bytes()[..offset]
            .iter()
            .filter(|b| **b == b'\t')
            .count();
        (offset + 3 * before) as u32
    };
    let whole = spans.len() == 1 && spans[0].start == 0 && spans[0].end as usize >= source.len();
    let mut mapped: InlineSpans = if whole {
        InlineSpans::new()
    } else {
        spans
            .iter()
            .filter(|s| s.start < s.end)
            .map(|s| map(s.start)..map(s.end))
            .collect()
    };
    if text.len() > MAX_DISPLAY_BYTES {
        let first_change = mapped.first().map_or(0, |s| s.start as usize);
        let mut start = first_change
            .saturating_sub(CUT_LEAD)
            .min(text.len() - MAX_DISPLAY_BYTES);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (start + MAX_DISPLAY_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let prefix = if start > 0 { ELLIPSIS } else { "" };
        let suffix = if end < text.len() { ELLIPSIS } else { "" };
        let window_len = end - start;
        let lead = prefix.len();
        mapped = mapped
            .into_iter()
            .filter_map(|s| {
                let a = (s.start as usize).saturating_sub(start).min(window_len);
                let b = (s.end as usize).saturating_sub(start).min(window_len);
                (a < b).then(|| (a + lead) as u32..(b + lead) as u32)
            })
            .collect();
        text = format!("{prefix}{}{suffix}", &text[start..end]);
    }
    (SharedString::from(text), mapped)
}

/// The modes a diff of this kind supports: split is not offered for added or deleted files (09 §1.4).
pub fn effective_mode(kind: ChangeKind, mode: DiffMode) -> DiffMode {
    match kind {
        ChangeKind::Added
        | ChangeKind::Deleted
        | ChangeKind::Untracked
        | ChangeKind::IntentToAdd => DiffMode::Unified,
        _ => mode,
    }
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
    let mode = effective_mode(diff.kind, mode);
    let (texts, inline, syntax, headers, gutter_digits) = match &diff.content {
        DiffContent::Text(text) => {
            let mut texts = Vec::with_capacity(text.lines.len());
            let mut inline = Vec::with_capacity(text.lines.len());
            for line in &text.lines {
                let (t, spans) = display_line(text.line_text(line), text.inline_spans(line));
                texts.push(t);
                inline.push(spans);
            }
            let syntax = highlight_sides(&diff, text, &texts, cancel);
            let headers = text
                .hunks
                .iter()
                .map(|h| SharedString::from(hunk_title(&h.header)))
                .collect();
            let gutter = text
                .hunks
                .iter()
                .map(|hunk| {
                    let max = text
                        .hunk_lines(hunk)
                        .iter()
                        .flat_map(|l| [l.old_lineno, l.new_lineno])
                        .flatten()
                        .max()
                        .unwrap_or(0);
                    digits(max)
                })
                .collect();
            (texts, inline, syntax, headers, gutter)
        }
        _ => (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()),
    };
    let rows = match &diff.content {
        DiffContent::Text(text) => build_rows(text, mode),
        _ => Vec::new(),
    };
    DiffDoc {
        diff,
        mode,
        rows: Arc::new(rows),
        gutter_digits: Arc::new(gutter_digits),
        texts: Arc::new(texts),
        syntax: Arc::new(syntax),
        inline: Arc::new(inline),
        headers: Arc::new(headers),
        numbers: Arc::new(NumberCache::default()),
        generation,
    }
}

/// Syntax tokens per `DiffLine`, highlighted per side (spec 21 §4.1): the old side (context + removed lines, old
/// path) and the new side (context + added lines, new path), each in file order so the highlighter's state follows
/// one file instead of interleaved removed and added lines. Context lines take the new side's tokens; a side
/// without changed lines is not highlighted on its own. Entries missing from a cancelled run stay empty.
fn highlight_sides(
    diff: &FileDiff,
    text: &TextDiff,
    texts: &[SharedString],
    cancel: &CancelToken,
) -> Vec<LineTokens> {
    let mut out = vec![LineTokens::new(); text.lines.len()];
    let has_removed = text.lines.iter().any(|l| l.kind == LineKind::Removed);
    let has_added = text.lines.iter().any(|l| l.kind == LineKind::Added);
    let new_path = diff.path.to_str_lossy();
    let old_path = diff
        .previous_path
        .as_ref()
        .map_or_else(|| new_path.clone(), |p| p.to_str_lossy());
    let mut side = |changed: LineKind, path: &str, take_context: bool| {
        let ix: Vec<usize> = text
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == changed || l.kind == LineKind::Context)
            .map(|(i, _)| i)
            .collect();
        let lines: Vec<&str> = ix
            .iter()
            .map(|&i| texts.get(i).map_or("", |t| t.as_ref()))
            .collect();
        let tokens = Highlighter::global().highlight(path, &lines, cancel);
        for (i, t) in ix.into_iter().zip(tokens) {
            if take_context || text.lines[i].kind == changed {
                out[i] = t;
            }
        }
    };
    let new_side = has_added || !has_removed;
    if new_side {
        side(LineKind::Added, &new_path, true);
    }
    if has_removed && !cancel.is_cancelled() {
        side(LineKind::Removed, &old_path, !new_side);
    }
    out
}

/// The same document in another mode: only the rows are rebuilt; texts, tokens and number strings are shared.
pub fn rebuild_mode(doc: &DiffDoc, mode: DiffMode, generation: u64) -> DiffDoc {
    let mode = effective_mode(doc.diff.kind, mode);
    let rows = match &doc.diff.content {
        DiffContent::Text(text) => build_rows(text, mode),
        _ => Vec::new(),
    };
    DiffDoc {
        diff: doc.diff.clone(),
        mode,
        rows: Arc::new(rows),
        gutter_digits: doc.gutter_digits.clone(),
        texts: doc.texts.clone(),
        syntax: doc.syntax.clone(),
        inline: doc.inline.clone(),
        headers: doc.headers.clone(),
        numbers: doc.numbers.clone(),
        generation,
    }
}

fn build_rows(text: &TextDiff, mode: DiffMode) -> Vec<DiffRow> {
    let mut rows = Vec::with_capacity(text.lines.len() + text.hunks.len());
    for (h, hunk) in text.hunks.iter().enumerate() {
        let h = h as u32;
        rows.push(DiffRow::HunkHeader { hunk: h });
        match mode {
            DiffMode::Unified => rows.extend(hunk.lines.clone().map(|line| DiffRow::Line { line })),
            DiffMode::Split => pair_lines(text, hunk.lines.clone(), h, &mut rows),
        }
    }
    rows
}

fn pair_lines(text: &TextDiff, range: Range<u32>, hunk: u32, rows: &mut Vec<DiffRow>) {
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
            LineKind::Context => {
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

/// Which list of uncommitted changes a diff shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uncommitted {
    Staged,
    Unstaged,
}

/// The uncommitted list of a target (`None` for commit diffs).
pub fn uncommitted(target: &DiffTarget) -> Option<Uncommitted> {
    match target {
        DiffTarget::Staged { .. } => Some(Uncommitted::Staged),
        DiffTarget::Unstaged { .. } => Some(Uncommitted::Unstaged),
        DiffTarget::Commit { .. } => None,
    }
}

/// What the staging controls of a document may show (09 §1.3, §4.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Controls {
    /// Stage/Unstage/Discard hunk buttons: uncommitted, repository SAFE, kind Modified, partial staging allowed.
    pub hunk_buttons: bool,
    /// Per-line chip: uncommitted, kind Modified, partial allowed (not gated by SAFE).
    pub line_chip: bool,
    /// "Discard line" menu: as the chip, unstaged only.
    pub discard_line: bool,
    /// The list the diff shows.
    pub list: Option<Uncommitted>,
}

/// The staging controls of `diff` given whether the repository is SAFE (no operation in progress).
pub fn controls(diff: &FileDiff, repo_safe: bool) -> Controls {
    let list = uncommitted(&diff.target);
    let partial = match &diff.content {
        DiffContent::Text(text) => text.partial == PartialAvailability::Allowed,
        _ => false,
    };
    let lines = list.is_some() && diff.kind == ChangeKind::Modified && partial;
    Controls {
        hunk_buttons: lines && repo_safe,
        line_chip: lines,
        discard_line: lines && list == Some(Uncommitted::Unstaged),
        list,
    }
}

/// The request for a selection of `diff` (stale check against the shown sides, same algorithm as the diff).
pub fn selection_request(diff: &FileDiff, selection: LineSelection) -> SelectionRequest {
    SelectionRequest {
        path: diff.path.clone(),
        previous_path: diff.previous_path.clone(),
        base_old: diff.old.map(|s| s.id),
        base_new: diff.new.map(|s| s.id),
        algorithm: None,
        selection,
    }
}

/// The line reference of a changed line (`None` for context lines).
pub fn line_ref(line: &DiffLine) -> Option<LineRef> {
    match line.kind {
        LineKind::Added => line
            .new_lineno
            .map(|new_lineno| LineRef::Added { new_lineno }),
        LineKind::Removed => line
            .old_lineno
            .map(|old_lineno| LineRef::Removed { old_lineno }),
        LineKind::Context => None,
    }
}

/// Whether two loads show the same content (same sides, kind and paths): a reload of the same content keeps the
/// document and the scroll position (spec 21 §4.1).
pub fn same_content(a: &FileDiff, b: &FileDiff) -> bool {
    a.old == b.old
        && a.new == b.new
        && a.kind == b.kind
        && a.path == b.path
        && a.previous_path == b.previous_path
        && std::mem::discriminant(&a.content) == std::mem::discriminant(&b.content)
}
