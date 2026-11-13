//! The single line-diff engine of the crate (§4.3, WP-B0d).
//!
//! `diff.rs` (display), `stage.rs` (stage/unstage/discard of selections) and the staging oracle test all build on
//! this module; nothing in the crate uses gix's `UnifiedDiff`. The line numbers the GUI selects (`LineRef`) are the
//! [`Op`] indexes + 1, so staging replays exactly the edit script that was displayed.
//!
//! Performance: images are byte slices plus one `Range` per line, tokens are interned slices of the image (or of one
//! normalised arena per side in the whitespace-ignoring modes), so there is no per-line allocation anywhere.

use std::ops::Range;

use bstr::ByteSlice;
use gix::diff::blob::{
    Algorithm, Diff, IndentHeuristic, IndentLevel, InternedInput, SliderHeuristic, Token,
    diff_with_slider_heuristics,
};

use crate::{DiffAlgorithm, Eol, EolStyle, HunkHeader, WhitespaceMode};

/// The line algorithm of a diff or of a partial operation: the explicitly requested one (`DiffOptions::algorithm`,
/// `SelectionRequest::algorithm`), else `configured`, what Git uses for the path (the diff driver's `algorithm`, else
/// `diff.algorithm`, default Myers; see `content::prepare_text`). The display and the staging path both resolve it
/// here, so a selection replays the edit script it was made on.
pub(crate) fn resolve_algorithm(
    requested: Option<DiffAlgorithm>,
    configured: Algorithm,
) -> Algorithm {
    match requested {
        Some(DiffAlgorithm::Myers) => Algorithm::Myers,
        Some(DiffAlgorithm::MyersMinimal) => Algorithm::MyersMinimal,
        Some(DiffAlgorithm::Histogram) => Algorithm::Histogram,
        None => configured,
    }
}

/// The lines of one side of a diff. Every range indexes into `bytes` and includes its terminator.
#[derive(Debug, Clone)]
pub(crate) struct LineImage<'a> {
    /// The whole content (git form).
    pub bytes: &'a [u8],
    /// One range per line, including its `\n` (and a preceding `\r`). Only the last line may lack a terminator.
    pub lines: Vec<Range<usize>>,
}

/// Split `bytes` into lines. The terminator is `\n`; a preceding `\r` stays part of the line, a lone `\r` is an
/// ordinary byte (Git does the same). An empty input has no lines; a trailing line without `\n` is a line.
pub(crate) fn split_lines(bytes: &[u8]) -> LineImage<'_> {
    // A guess that avoids most re-allocations for source code without a counting pass.
    let mut lines = Vec::with_capacity(bytes.len() / 32 + 1);
    let mut start = 0;
    while let Some(pos) = bytes[start..].find_byte(b'\n') {
        let end = start + pos + 1;
        lines.push(start..end);
        start = end;
    }
    if start < bytes.len() {
        lines.push(start..bytes.len());
    }
    LineImage { bytes, lines }
}

/// The number of lines [`split_lines`] finds in `bytes`, without building the ranges (limit checks before a split).
pub(crate) fn count_lines(bytes: &[u8]) -> usize {
    let terminated = bytes.iter().filter(|b| **b == b'\n').count();
    terminated + usize::from(bytes.last().is_some_and(|b| *b != b'\n'))
}

impl<'a> LineImage<'a> {
    /// The number of lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Line `idx` (0-based) including its terminator.
    ///
    /// Panics if `idx` is out of range, which is a bug of the caller (indexes come from [`edit_script`]).
    pub fn line(&self, idx: u32) -> &'a [u8] {
        &self.bytes[self.lines[idx as usize].clone()]
    }

    /// Line `idx` (0-based) without its terminator (`\r\n` or `\n`).
    pub fn content(&self, idx: u32) -> &'a [u8] {
        let line = self.line(idx);
        let cut = match eol_of(line) {
            Eol::CrLf => 2,
            Eol::Lf => 1,
            Eol::None => 0,
        };
        &line[..line.len() - cut]
    }

    /// The terminator of line `idx` (0-based).
    pub fn eol(&self, idx: u32) -> Eol {
        eol_of(self.line(idx))
    }

    /// `true` if the content is not empty and its last line has no `\n` (`\ No newline at end of file`).
    pub fn missing_final_newline(&self) -> bool {
        self.bytes.last().is_some_and(|b| *b != b'\n')
    }

    /// The number of lines ending in `\r\n` and of lines ending in a bare `\n`.
    pub fn eol_counts(&self) -> (usize, usize) {
        self.lines.iter().fold((0, 0), |(crlf, lf), range| {
            match eol_of(&self.bytes[range.clone()]) {
                Eol::CrLf => (crlf + 1, lf),
                Eol::Lf => (crlf, lf + 1),
                Eol::None => (crlf, lf),
            }
        })
    }

    /// The majority line-ending style: `Mixed` if both styles occur and the minority is above 5 % of the terminated
    /// lines, `None` if no line has a terminator.
    pub fn eol_style(&self) -> EolStyle {
        let (crlf, lf) = self.eol_counts();
        let minority = crlf.min(lf);
        if crlf == 0 && lf == 0 {
            EolStyle::None
        } else if minority > 0 && minority * 20 > crlf + lf {
            EolStyle::Mixed
        } else if crlf > lf {
            EolStyle::CrLf
        } else {
            EolStyle::Lf
        }
    }
}

fn eol_of(line: &[u8]) -> Eol {
    if line.ends_with(b"\r\n") {
        Eol::CrLf
    } else if line.ends_with(b"\n") {
        Eol::Lf
    } else {
        Eol::None
    }
}

/// `b"\r\n"` if more lines end in CRLF than in LF, else `b"\n"`. Used by the glue rule of the staging algorithms
/// (§4.4) when a line without terminator gets a successor.
pub(crate) fn dominant_eol(img: &LineImage<'_>) -> &'static [u8] {
    let (crlf, lf) = img.eol_counts();
    if crlf > lf { b"\r\n" } else { b"\n" }
}

/// One step of an edit script. Indexes are 0-based line indexes into the old / new [`LineImage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// Old line `old` and new line `new` are the same line (equal bytes, or equal after whitespace normalisation).
    Equal {
        /// Index into the old image.
        old: u32,
        /// Index into the new image.
        new: u32,
    },
    /// Old line `old` was removed.
    Delete {
        /// Index into the old image.
        old: u32,
    },
    /// New line `new` was added.
    Insert {
        /// Index into the new image.
        new: u32,
    },
}

/// Compute the edit script that turns `old` into `new`.
///
/// This is `gix::diff::blob::diff_with_slider_heuristics(algo, &input)` over interned line tokens, i.e. Git's
/// algorithms plus Git's indent heuristic. With `ws == None` the tokens are the raw lines including their terminators
/// (so a missing final newline or a CRLF/LF change is a change, like in Git). Otherwise each line is interned from a
/// normalised copy in which Git's whitespace bytes (space, `\t`, `\r`, `\n`) are treated as follows, matching
/// `git diff --ignore-space-at-eol` / `-b` / `-w`:
/// - `IgnoreAtEol`: trailing whitespace is dropped;
/// - `IgnoreChange`: trailing whitespace is dropped and every other run of whitespace becomes one space;
/// - `IgnoreAll`: all whitespace is dropped.
///
/// The images keep their bytes; only the comparison changes. As in Git, the indent heuristic still measures the
/// original lines in these modes (so the same `diff_with_slider_heuristics` steps run, with a position-based
/// heuristic).
///
/// Known difference (spec §13): with `Histogram` *and* a whitespace mode, Git's xhistogram sometimes picks different,
/// even non-minimal, blocks (about 3 % of generated cases); this engine stays minimal. It cannot be removed by sharing
/// the engine (both the display and the staging path run this function), and it is harmless: it only affects what is
/// displayed, because partial operations are refused in whitespace modes (`PartialBlock::WhitespaceIgnored`, §4.2) and
/// staging always uses `WhitespaceMode::None`, where Myers and Histogram both match Git exactly. Porting xhistogram's
/// quirks is not worth it.
///
/// The ops cover both images completely and in order;
/// within a change block all `Delete`s precede all `Insert`s.
///
/// Precondition: both images have fewer than `i32::MAX` lines (callers enforce `DiffOptions::max_lines` first).
pub(crate) fn edit_script(
    old: &LineImage<'_>,
    new: &LineImage<'_>,
    algo: Algorithm,
    ws: WhitespaceMode,
) -> Vec<Op> {
    let diff = match ws {
        WhitespaceMode::None => {
            let input = intern(
                old.lines.iter().map(|r| &old.bytes[r.clone()]),
                new.lines.iter().map(|r| &new.bytes[r.clone()]),
                old.len(),
                new.len(),
            );
            diff_with_slider_heuristics(algo, &input)
        }
        WhitespaceMode::IgnoreAtEol | WhitespaceMode::IgnoreChange | WhitespaceMode::IgnoreAll => {
            let old_norm = Normalised::new(old, ws);
            let new_norm = Normalised::new(new, ws);
            let input = intern(old_norm.tokens(), new_norm.tokens(), old.len(), new.len());
            let mut diff = Diff::compute(algo, &input);
            // Git's indent heuristic measures the original lines (`get_indent(&xdf->recs[i])`), not the normalised
            // ones: `-b` collapses and `-w` drops the indentation the heuristic needs. Feed it the raw indentation
            // per position instead.
            let heuristic = RawIndentHeuristic::new(old, new, &input.before);
            diff.postprocess_with(&input.before, &input.after, heuristic);
            diff
        }
    };
    ops_from_diff(&diff, old.len() as u32, new.len() as u32)
}

/// The indent heuristic over the raw lines of both images, addressed by position.
///
/// imara's [`IndentHeuristic`] only looks at tokens to ask for their indentation, so it is handed a slice of
/// position tokens (`Token(i)` for line `i` of the side being compacted, old lines first) instead of the normalised
/// tokens, and the indentation of each position comes from the original bytes.
struct RawIndentHeuristic<'a> {
    /// Position tokens: `0..old_len` for the old side, then `old_len..old_len + new_len` for the new side.
    positions: Vec<Token>,
    /// Raw indentation per position token.
    levels: Vec<IndentLevel>,
    old_len: usize,
    /// The normalised old tokens, to recognise which side postprocessing is compacting.
    old_tokens: &'a [Token],
}

impl<'a> RawIndentHeuristic<'a> {
    fn new(old: &LineImage<'_>, new: &LineImage<'_>, old_tokens: &'a [Token]) -> Self {
        let levels: Vec<IndentLevel> = old
            .lines
            .iter()
            .map(|r| &old.bytes[r.clone()])
            .chain(new.lines.iter().map(|r| &new.bytes[r.clone()]))
            .map(|line| IndentLevel::for_ascii_line(line.iter().copied(), 8))
            .collect();
        RawIndentHeuristic {
            positions: (0..levels.len() as u32).map(Token).collect(),
            levels,
            old_len: old.len(),
            old_tokens,
        }
    }
}

impl SliderHeuristic for RawIndentHeuristic<'_> {
    fn best_slider_end(&mut self, tokens: &[Token], hunk: Range<u32>, earliest_end: u32) -> u32 {
        let side = if std::ptr::eq(tokens, self.old_tokens) {
            &self.positions[..self.old_len]
        } else {
            &self.positions[self.old_len..]
        };
        debug_assert_eq!(
            side.len(),
            tokens.len(),
            "one position per token of the side"
        );
        let levels = &self.levels;
        IndentHeuristic::new(|token: Token| levels[token.0 as usize]).best_slider_end(
            side,
            hunk,
            earliest_end,
        )
    }
}

fn intern<'t>(
    before: impl Iterator<Item = &'t [u8]>,
    after: impl Iterator<Item = &'t [u8]>,
    before_len: usize,
    after_len: usize,
) -> InternedInput<&'t [u8]> {
    let mut input = InternedInput::default();
    input.reserve(before_len as u32, after_len as u32);
    input.update_before(before);
    input.update_after(after);
    input
}

fn ops_from_diff(diff: &Diff, old_len: u32, new_len: u32) -> Vec<Op> {
    // Every new line yields one op (Equal or Insert), plus one Delete per removed old line.
    let mut ops = Vec::with_capacity(new_len as usize + diff.count_removals() as usize);
    let (mut old, mut new) = (0u32, 0u32);
    let equal_until = |ops: &mut Vec<Op>, old: &mut u32, new: &mut u32, old_end: u32| {
        while *old < old_end {
            ops.push(Op::Equal {
                old: *old,
                new: *new,
            });
            *old += 1;
            *new += 1;
        }
    };
    for hunk in diff.hunks() {
        equal_until(&mut ops, &mut old, &mut new, hunk.before.start);
        debug_assert_eq!(
            new, hunk.after.start,
            "both sides advance together over equal lines"
        );
        ops.extend(hunk.before.clone().map(|old| Op::Delete { old }));
        ops.extend(hunk.after.clone().map(|new| Op::Insert { new }));
        old = hunk.before.end;
        new = hunk.after.end;
    }
    equal_until(&mut ops, &mut old, &mut new, old_len);
    debug_assert_eq!(new, new_len, "the trailing equal lines end both images");
    ops
}

/// All normalised lines of one image in one arena, so interning needs no per-line allocation.
struct Normalised {
    arena: Vec<u8>,
    ends: Vec<usize>,
}

/// Git's `XDL_ISSPACE` (git's own `isspace`: space, `\t`, `\n`, `\r`).
fn is_git_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

impl Normalised {
    fn new(img: &LineImage<'_>, ws: WhitespaceMode) -> Self {
        let mut arena = Vec::with_capacity(img.bytes.len());
        let mut ends = Vec::with_capacity(img.len());
        for range in &img.lines {
            let line = &img.bytes[range.clone()];
            // The terminator counts as trailing whitespace in all three modes.
            let trimmed_len =
                line.len() - line.iter().rev().take_while(|b| is_git_space(**b)).count();
            let line = &line[..trimmed_len];
            match ws {
                WhitespaceMode::None | WhitespaceMode::IgnoreAtEol => arena.extend_from_slice(line),
                WhitespaceMode::IgnoreChange => {
                    let mut in_space = false;
                    for &b in line {
                        if is_git_space(b) {
                            if !in_space {
                                arena.push(b' ');
                            }
                            in_space = true;
                        } else {
                            arena.push(b);
                            in_space = false;
                        }
                    }
                }
                WhitespaceMode::IgnoreAll => {
                    arena.extend(line.iter().copied().filter(|b| !is_git_space(*b)));
                }
            }
            ends.push(arena.len());
        }
        Normalised { arena, ends }
    }

    fn tokens(&self) -> impl Iterator<Item = &[u8]> {
        let starts = std::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(self.ends.iter().copied())
            .map(|(start, end)| &self.arena[start..end])
    }
}

/// Git's hunk grouping over a complete edit script.
///
/// Every change block gets `context` lines before and after; two blocks whose gap of `Equal` ops is ≤ `2·context`
/// end up in one hunk (Git's `xdl_get_hunk` rule). Returns each hunk's header and its index range into `ops`.
/// Header starts are 1-based; a side without lines in the hunk has the number of the line before it as start (0 at
/// the top of the file), like Git.
///
/// `context == u32::MAX` is the full-file view: exactly one hunk spanning all ops, even if nothing changed (none if
/// `ops` is empty). Otherwise an edit script without changes has no hunks.
pub(crate) fn group_hunks(ops: &[Op], context: u32) -> Vec<(HunkHeader, Range<usize>)> {
    if context == u32::MAX {
        if ops.is_empty() {
            return Vec::new();
        }
        let (old_lines, new_lines) = ops.iter().fold((0u32, 0u32), |(o, n), op| match op {
            Op::Equal { .. } => (o + 1, n + 1),
            Op::Delete { .. } => (o + 1, n),
            Op::Insert { .. } => (o, n + 1),
        });
        let header = HunkHeader {
            old_start: u32::from(old_lines > 0),
            old_lines,
            new_start: u32::from(new_lines > 0),
            new_lines,
        };
        return vec![(header, 0..ops.len())];
    }

    let context = context as usize;
    let max_gap = context.saturating_mul(2);
    let mut hunks = Vec::new();
    let mut pending: Option<Group> = None;
    // Number of old / new lines before op `i`.
    let (mut old_pos, mut new_pos) = (0usize, 0usize);
    let mut i = 0;
    while i < ops.len() {
        if let Op::Equal { .. } = ops[i] {
            old_pos += 1;
            new_pos += 1;
            i += 1;
            continue;
        }
        let block = Group {
            start_op: i,
            start_old: old_pos,
            start_new: new_pos,
            end_op: 0,
            end_old: 0,
            end_new: 0,
        };
        while i < ops.len() {
            match ops[i] {
                Op::Equal { .. } => break,
                Op::Delete { .. } => old_pos += 1,
                Op::Insert { .. } => new_pos += 1,
            }
            i += 1;
        }
        let (end_op, end_old, end_new) = (i, old_pos, new_pos);
        match pending.as_mut() {
            Some(group) if block.start_op - group.end_op <= max_gap => {
                group.end_op = end_op;
                group.end_old = end_old;
                group.end_new = end_new;
            }
            _ => {
                if let Some(group) = pending.take() {
                    hunks.push(group.finish(context, ops.len()));
                }
                pending = Some(Group {
                    end_op,
                    end_old,
                    end_new,
                    ..block
                });
            }
        }
    }
    if let Some(group) = pending {
        hunks.push(group.finish(context, ops.len()));
    }
    hunks
}

/// The changes of one hunk before context is added: op range plus the line positions at both ends.
struct Group {
    start_op: usize,
    start_old: usize,
    start_new: usize,
    end_op: usize,
    end_old: usize,
    end_new: usize,
}

impl Group {
    fn finish(self, context: usize, num_ops: usize) -> (HunkHeader, Range<usize>) {
        // The `context` ops on either side are all `Equal`: a closer change block would have been merged.
        let lead = context.min(self.start_op);
        let trail = context.min(num_ops - self.end_op);
        let old_first = self.start_old - lead;
        let new_first = self.start_new - lead;
        let old_lines = self.end_old + trail - old_first;
        let new_lines = self.end_new + trail - new_first;
        let start = |first: usize, lines: usize| if lines == 0 { first } else { first + 1 };
        let header = HunkHeader {
            old_start: start(old_first, old_lines) as u32,
            old_lines: old_lines as u32,
            new_start: start(new_first, new_lines) as u32,
            new_lines: new_lines as u32,
        };
        (header, self.start_op - lead..self.end_op + trail)
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn lines_of<'a>(img: &LineImage<'a>) -> Vec<&'a bstr::BStr> {
        (0..img.len() as u32)
            .map(|i| img.line(i).as_bstr())
            .collect()
    }

    #[test]
    fn split_lines_empty_input_has_no_lines() {
        let img = split_lines(b"");
        assert_eq!(img.len(), 0, "nothing to split");
        assert!(!img.missing_final_newline(), "an empty file lacks nothing");
        assert_eq!(img.eol_style(), EolStyle::None, "no terminators at all");
    }

    #[test]
    fn split_lines_keeps_terminators_in_each_line() {
        let img = split_lines(b"a\nb\r\n\nc\rd\r\n");
        assert_eq!(
            lines_of(&img),
            ["a\n", "b\r\n", "\n", "c\rd\r\n"],
            "a lone \\r is no terminator, \\r\\n stays together"
        );
        assert_eq!(
            [img.eol(0), img.eol(1), img.eol(2), img.eol(3)],
            [Eol::Lf, Eol::CrLf, Eol::Lf, Eol::CrLf],
            "terminators are classified per line"
        );
        assert_eq!(img.content(1), b"b", "content drops the whole CRLF");
        assert_eq!(img.content(3), b"c\rd", "inner \\r is content");
        assert!(!img.missing_final_newline(), "ends in a newline");
    }

    #[test]
    fn split_lines_last_line_without_newline() {
        let img = split_lines(b"one\ntwo");
        assert_eq!(lines_of(&img), ["one\n", "two"], "the tail is its own line");
        assert_eq!(img.eol(1), Eol::None, "it has no terminator");
        assert_eq!(img.content(1), b"two", "content is the whole line");
        assert!(
            img.missing_final_newline(),
            "Git prints '\\ No newline at end of file'"
        );

        let img = split_lines(b"x\r");
        assert_eq!(
            lines_of(&img),
            ["x\r"],
            "a trailing lone \\r stays in the line"
        );
        assert_eq!(img.eol(0), Eol::None, "\\r alone terminates nothing");
    }

    #[test]
    fn count_lines_agrees_with_split_lines() {
        for input in [
            &b""[..],
            b"\n",
            b"a",
            b"a\n",
            b"a\r\nb",
            b"a\nb\n\n",
            b"x\r",
            b"\n\nlast",
        ] {
            assert_eq!(
                count_lines(input),
                split_lines(input).len(),
                "{:?}: one definition of a line",
                input.as_bstr()
            );
        }
    }

    #[test]
    fn split_lines_only_newlines() {
        let img = split_lines(b"\n\n\n");
        assert_eq!(
            lines_of(&img),
            ["\n", "\n", "\n"],
            "every newline is an empty line"
        );
    }

    #[test]
    fn dominant_eol_and_style() {
        let crlf = split_lines(b"a\r\nb\r\nc\n");
        assert_eq!(
            dominant_eol(&crlf),
            b"\r\n",
            "two CRLF lines beat one LF line"
        );
        assert_eq!(crlf.eol_style(), EolStyle::Mixed, "1 of 3 is above 5 %");

        let tie = split_lines(b"a\r\nb\n");
        assert_eq!(dominant_eol(&tie), b"\n", "LF wins ties");

        let none = split_lines(b"no terminator");
        assert_eq!(dominant_eol(&none), b"\n", "LF is the default");
        assert_eq!(none.eol_style(), EolStyle::None, "no terminated line");

        let mut text = b"x\r\n".repeat(40);
        text.extend_from_slice(b"y\n");
        let mostly_crlf = split_lines(&text);
        assert_eq!(
            mostly_crlf.eol_style(),
            EolStyle::CrLf,
            "1 of 41 is below 5 %"
        );
        assert_eq!(split_lines(b"a\nb\n").eol_style(), EolStyle::Lf, "pure LF");
    }

    fn myers(old: &[u8], new: &[u8]) -> Vec<Op> {
        edit_script(
            &split_lines(old),
            &split_lines(new),
            Algorithm::Myers,
            WhitespaceMode::None,
        )
    }

    #[test]
    fn edit_script_orders_deletes_before_inserts() {
        let ops = myers(b"a\nb\nc\n", b"a\nB\nc\nd\n");
        assert_eq!(
            ops,
            [
                Op::Equal { old: 0, new: 0 },
                Op::Delete { old: 1 },
                Op::Insert { new: 1 },
                Op::Equal { old: 2, new: 2 },
                Op::Insert { new: 3 },
            ],
            "a replaced line is a Delete then an Insert; the appended line is an Insert"
        );
    }

    #[test]
    fn edit_script_missing_final_newline_is_a_change() {
        let ops = myers(b"a\nb", b"a\nb\n");
        assert_eq!(
            ops,
            [
                Op::Equal { old: 0, new: 0 },
                Op::Delete { old: 1 },
                Op::Insert { new: 1 }
            ],
            "the terminator is part of the token"
        );
        assert_eq!(myers(b"", b""), [], "two empty images have no ops");
    }

    #[test]
    fn group_hunks_merge_rule() {
        let old: Vec<u8> = (1..=30)
            .flat_map(|i| format!("line {i}\n").into_bytes())
            .collect();
        let changed = |lines: &[usize]| -> Vec<u8> {
            (1..=30)
                .flat_map(|i| {
                    if lines.contains(&i) {
                        format!("changed {i}\n").into_bytes()
                    } else {
                        format!("line {i}\n").into_bytes()
                    }
                })
                .collect()
        };

        // Lines 5 and 12 changed: 6 equal lines in between, = 2·3 → one hunk.
        let ops = myers(&old, &changed(&[5, 12]));
        let headers: Vec<_> = group_hunks(&ops, 3).into_iter().map(|(h, _)| h).collect();
        assert_eq!(
            headers,
            [HunkHeader {
                old_start: 2,
                old_lines: 14,
                new_start: 2,
                new_lines: 14
            }],
            "a gap of exactly 2·context merges"
        );

        // Lines 5 and 13 changed: 7 equal lines in between → two hunks.
        let ops = myers(&old, &changed(&[5, 13]));
        let hunks = group_hunks(&ops, 3);
        let headers: Vec<_> = hunks.iter().map(|(h, _)| *h).collect();
        assert_eq!(
            headers,
            [
                HunkHeader {
                    old_start: 2,
                    old_lines: 7,
                    new_start: 2,
                    new_lines: 7
                },
                HunkHeader {
                    old_start: 10,
                    old_lines: 7,
                    new_start: 10,
                    new_lines: 7
                },
            ],
            "a gap of 2·context + 1 splits"
        );
        for (header, range) in &hunks {
            let old_lines = ops[range.clone()]
                .iter()
                .filter(|op| !matches!(op, Op::Insert { .. }))
                .count();
            assert_eq!(
                old_lines as u32, header.old_lines,
                "the op range matches the header"
            );
        }

        // Context 0 never merges distinct blocks.
        let ops = myers(&old, &changed(&[5, 6, 8]));
        assert_eq!(
            group_hunks(&ops, 0).len(),
            2,
            "adjacent lines form one block, a gap of 1 splits"
        );

        // Full-file view: one hunk over everything.
        let hunks = group_hunks(&ops, u32::MAX);
        assert_eq!(
            hunks,
            [(
                HunkHeader {
                    old_start: 1,
                    old_lines: 30,
                    new_start: 1,
                    new_lines: 30
                },
                0..ops.len()
            )],
            "u32::MAX is one hunk spanning all ops"
        );
        let unchanged = myers(&old, &old);
        assert_eq!(group_hunks(&unchanged, 3), [], "no change, no hunk");
        assert_eq!(
            group_hunks(&unchanged, u32::MAX).len(),
            1,
            "the full-file view still shows the file"
        );
        assert_eq!(
            group_hunks(&[], u32::MAX),
            [],
            "nothing to show for empty images"
        );
    }

    #[test]
    fn group_hunks_empty_side_starts_at_line_before() {
        let ops = myers(b"", b"a\nb\n");
        assert_eq!(
            group_hunks(&ops, 3),
            [(
                HunkHeader {
                    old_start: 0,
                    old_lines: 0,
                    new_start: 1,
                    new_lines: 2
                },
                0..2
            )],
            "a new file is '@@ -0,0 +1,2 @@'"
        );
        let ops = myers(b"a\nb\nc\n", b"a\nb\nX\nc\n");
        assert_eq!(
            group_hunks(&ops, 0)[0].0,
            HunkHeader {
                old_start: 2,
                old_lines: 0,
                new_start: 3,
                new_lines: 1
            },
            "a pure insertion after old line 2 has old start 2"
        );
        let ops = myers(b"a\nb\nc\n", b"b\nc\n");
        assert_eq!(
            group_hunks(&ops, 0)[0].0,
            HunkHeader {
                old_start: 1,
                old_lines: 1,
                new_start: 0,
                new_lines: 0
            },
            "deleting the first line has new start 0"
        );
    }

    #[test]
    fn whitespace_modes_normalise_tokens() {
        let old = b"a  b\nc\t\nd\r\ne f\n";
        let new = b"a b\nc\nd\n ef\n";
        let script = |ws| {
            let ops = edit_script(&split_lines(old), &split_lines(new), Algorithm::Myers, ws);
            ops.iter()
                .map(|op| match op {
                    Op::Equal { .. } => '=',
                    Op::Delete { .. } => '-',
                    Op::Insert { .. } => '+',
                })
                .collect::<String>()
        };
        assert_eq!(
            script(WhitespaceMode::None),
            "----++++",
            "every line differs in some whitespace"
        );
        assert_eq!(
            script(WhitespaceMode::IgnoreAtEol),
            "-+==-+",
            "trailing tab and CRLF vs LF are ignored, inner whitespace is not"
        );
        assert_eq!(
            script(WhitespaceMode::IgnoreChange),
            "===-+",
            "runs of whitespace compare equal, but 'e f' vs ' ef' does not"
        );
        assert_eq!(
            script(WhitespaceMode::IgnoreAll),
            "====",
            "all whitespace is ignored"
        );

        let missing_newline = edit_script(
            &split_lines(b"a\nb"),
            &split_lines(b"a\nb\n"),
            Algorithm::Myers,
            WhitespaceMode::IgnoreAtEol,
        );
        assert!(
            missing_newline
                .iter()
                .all(|op| matches!(op, Op::Equal { .. })),
            "like `git diff -w`, a missing final newline is whitespace"
        );

        let norm = Normalised::new(
            &split_lines(b" x \t y \r\n\n"),
            WhitespaceMode::IgnoreChange,
        );
        assert_eq!(
            norm.tokens().map(|t| t.as_bstr()).collect::<Vec<_>>(),
            [" x y", ""],
            "leading run collapses to one space, the trailing run and terminator vanish"
        );
    }

    /// `git diff --no-index` with an isolated configuration; returns the `@@` headers of its output.
    fn git_hunk_headers(
        dir: &std::path::Path,
        old: &[u8],
        new: &[u8],
        extra: &[&str],
    ) -> Vec<HunkHeader> {
        std::fs::write(dir.join("old"), old).expect("temp dir is writable");
        std::fs::write(dir.join("new"), new).expect("temp dir is writable");
        let empty_config = dir.join("gitconfig");
        std::fs::write(&empty_config, b"").expect("temp dir is writable");
        let out = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &empty_config)
            .env("HOME", dir)
            .env("XDG_CONFIG_HOME", dir)
            .args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "diff.indentHeuristic=true",
            ])
            .args([
                "diff",
                "--no-index",
                "--no-color",
                "--no-ext-diff",
                "--diff-algorithm=myers",
            ])
            .args(extra)
            .args(["old", "new"])
            .output()
            .expect("git is installed");
        assert!(
            matches!(out.status.code(), Some(0 | 1)),
            "git diff --no-index exits 0 or 1: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
            .lines()
            .filter_map(|line| line.strip_prefix(b"@@ -"))
            .map(|rest| {
                let rest = rest.to_str().expect("headers are ASCII");
                let mut sides = rest.split(' ');
                let parse = |side: &str| -> (u32, u32) {
                    let mut parts = side.split(',');
                    let start = parts
                        .next()
                        .and_then(|s| s.parse().ok())
                        .expect("start is a number");
                    let lines = parts
                        .next()
                        .map_or(1, |s| s.parse().expect("count is a number"));
                    (start, lines)
                };
                let (old_start, old_lines) = parse(sides.next().expect("old side"));
                let new_side = sides
                    .next()
                    .and_then(|s| s.strip_prefix('+'))
                    .expect("new side");
                let (new_start, new_lines) = parse(new_side);
                HunkHeader {
                    old_start,
                    old_lines,
                    new_start,
                    new_lines,
                }
            })
            .collect()
    }

    fn numbered(range: std::ops::RangeInclusive<u32>) -> Vec<u8> {
        range
            .flat_map(|i| format!("line {i}\n").into_bytes())
            .collect()
    }

    fn fixtures() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
        let base = numbered(1..=20);
        let modify_middle = {
            let mut v = numbered(1..=9);
            v.extend_from_slice(b"line ten\n");
            v.extend(numbered(11..=20));
            v
        };
        let gaps = {
            let mut v = numbered(1..=3);
            v.extend_from_slice(b"X4\n");
            v.extend(numbered(5..=10));
            v.extend_from_slice(b"X11\n");
            v.extend(numbered(12..=18));
            v.extend_from_slice(b"X19\n");
            v.extend(numbered(20..=20));
            v
        };
        let c_old = b"int f() {\n    a();\n}\n\nint g() {\n    b();\n}\n".to_vec();
        let c_new = b"int f() {\n    a();\n}\n\nint h() {\n    c();\n}\n\nint g() {\n    b();\n}\n"
            .to_vec();
        vec![
            ("modify_middle", base.clone(), modify_middle),
            (
                "insert_at_start",
                base.clone(),
                [b"new first\n".as_slice(), &base].concat(),
            ),
            ("delete_at_end", base.clone(), numbered(1..=17)),
            (
                "no_eol_old_add_after",
                b"a\nb".to_vec(),
                b"a\nb\nc\n".to_vec(),
            ),
            (
                "no_eol_new_only",
                b"a\nb\nc\n".to_vec(),
                b"a\nb\nc".to_vec(),
            ),
            (
                "crlf_lines_and_eol_change",
                b"one\r\ntwo\r\nthree\r\nfour\r\n".to_vec(),
                b"one\r\nTWO\r\nthree\nfour\r\n".to_vec(),
            ),
            ("indent_heuristic_slider", c_old, c_new),
            (
                "latin1_bytes",
                b"caf\xe9\nna\xefve\nend\n".to_vec(),
                b"caf\xe9\nna\xeff\nend\n".to_vec(),
            ),
            ("gaps_of_six_and_seven", base.clone(), gaps),
            ("new_file_and_rewrite", Vec::new(), b"x\ny\nz\n".to_vec()),
        ]
    }

    #[test]
    fn edit_script_matches_git_diff_on_fixtures() {
        let tmp = tempfile::tempdir().expect("temp dir");
        for (name, old, new) in fixtures() {
            let ops = myers(&old, &new);
            for context in [0u32, 3] {
                let ours: Vec<_> = group_hunks(&ops, context)
                    .into_iter()
                    .map(|(h, _)| h)
                    .collect();
                let git = git_hunk_headers(tmp.path(), &old, &new, &[&format!("-U{context}")]);
                assert_eq!(
                    ours, git,
                    "{name}: hunks with context {context} equal `git diff -U{context}`"
                );
            }
        }
    }

    fn whitespace_fixtures() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
        vec![
            (
                "mixed_whitespace_edits",
                b"keep\na  b\nc\t\nd\r\ne f\nlast\n".to_vec(),
                b"keep\na b\nc\nd\n ef\nlast\n".to_vec(),
            ),
            (
                "missing_final_newline_only",
                b"a\nb".to_vec(),
                b"a\nb\n".to_vec(),
            ),
            (
                "crlf_to_lf_everywhere",
                b"one\r\ntwo\r\nthree\r\n".to_vec(),
                b"one\ntwo\nthree\n".to_vec(),
            ),
            (
                "blank_and_whitespace_only_lines",
                b"x\n\n  \ny\n\t\nz\n".to_vec(),
                b"x\n \n\ny\nz\n\n".to_vec(),
            ),
            (
                "reindented_block_insert",
                b"fn a() {\n    if x {\n        one();\n    }\n}\n\nfn b() {\n    two();\n}\n".to_vec(),
                b"fn a() {\n  if x {\n    one();\n  }\n}\n\nfn c() {\n  three();\n}\n\nfn b() {\n  two();\n}\n"
                    .to_vec(),
            ),
            // The next two slide differently if the indent heuristic sees the normalised lines (`-b` collapses,
            // `-w` drops indentation) instead of the original ones, as Git does.
            (
                "slider_needs_raw_indent_b",
                b"}\nx\n\ny\n\n  x\n    x\n  x\ny\n".to_vec(),
                b"  y\n\ny\n\n  x\ny\n".to_vec(),
            ),
            (
                "slider_needs_raw_indent_w",
                b"}\n  y\n}\n\tx\ny \n".to_vec(),
                b"}\n}\n}\n{\ny\n}\n  y\n}\n\tx\ny \nx\ny\n".to_vec(),
            ),
        ]
    }

    #[test]
    fn whitespace_modes_match_git_diff() {
        let tmp = tempfile::tempdir().expect("temp dir");
        for (name, old, new) in whitespace_fixtures().into_iter().chain(fixtures()) {
            for (ws, flag) in [
                (WhitespaceMode::IgnoreAtEol, "--ignore-space-at-eol"),
                (WhitespaceMode::IgnoreChange, "-b"),
                (WhitespaceMode::IgnoreAll, "-w"),
            ] {
                let ops = edit_script(&split_lines(&old), &split_lines(&new), Algorithm::Myers, ws);
                let ours: Vec<_> = group_hunks(&ops, 0).into_iter().map(|(h, _)| h).collect();
                let git = git_hunk_headers(tmp.path(), &old, &new, &["-U0", flag]);
                assert_eq!(ours, git, "{name}: {ws:?} equals `git diff -U0 {flag}`");
            }
        }
    }

    /// Deterministic pseudo-random line soup (braces, blank lines, mixed indentation, trailing blanks), compared with
    /// Git for both algorithms. Catches slider and tie-breaking differences hand-written fixtures miss.
    #[test]
    fn edit_script_matches_git_diff_on_generated_cases() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let pool: &[&[u8]] = &[
            b"\n", b"{\n", b"}\n", b"  x\n", b"\tx\n", b"x\n", b"    x\n", b"y \n", b"y\n",
            b"  y\n",
        ];
        let mut seed: u64 = 0x1234_5678;
        let mut rnd = |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as usize) % n
        };
        for case in 0..16 {
            let old: Vec<&[u8]> = (0..5 + rnd(20)).map(|_| pool[rnd(pool.len())]).collect();
            let mut new = old.clone();
            for _ in 0..1 + rnd(4) {
                match rnd(3) {
                    0 => {
                        let at = rnd(new.len() + 1);
                        for _ in 0..1 + rnd(4) {
                            new.insert(at, pool[rnd(pool.len())]);
                        }
                    }
                    1 if !new.is_empty() => {
                        new.remove(rnd(new.len()));
                    }
                    _ if !new.is_empty() => {
                        let at = rnd(new.len());
                        new[at] = pool[rnd(pool.len())];
                    }
                    _ => {}
                }
            }
            let (old, new) = (old.concat(), new.concat());
            for (algo, ws, flags) in [
                (
                    Algorithm::Myers,
                    WhitespaceMode::None,
                    ["--diff-algorithm=myers", "--no-color"],
                ),
                (
                    Algorithm::Histogram,
                    WhitespaceMode::None,
                    ["--diff-algorithm=histogram", "--no-color"],
                ),
                (
                    Algorithm::Myers,
                    WhitespaceMode::IgnoreAll,
                    ["--diff-algorithm=myers", "-w"],
                ),
            ] {
                let ops = edit_script(&split_lines(&old), &split_lines(&new), algo, ws);
                let ours: Vec<_> = group_hunks(&ops, 0).into_iter().map(|(h, _)| h).collect();
                let mut args = vec!["-U0"];
                args.extend(flags);
                let git = git_hunk_headers(tmp.path(), &old, &new, &args);
                assert_eq!(
                    ours,
                    git,
                    "case {case} ({algo:?}, {ws:?}): old={:?} new={:?}",
                    old.as_bstr(),
                    new.as_bstr()
                );
            }
        }
    }
}
