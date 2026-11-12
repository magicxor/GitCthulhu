//! The blame document: the backend's [`BlameResult`] plus the strings the rows need (spec 09 §2.1). Pure data, no
//! UI types except `SharedString`, so it is unit-tested without a window.

use std::sync::Arc;

use gpui_kit::SharedString;

use crate::backend::{BlameChunk, BlameResult, ByteSlice as _, ObjectId};
use crate::diff::highlight::LineTokens;
use crate::metrics::blame as m;

/// The commit cell's message of lines that are not committed yet (09 §2.1).
pub const UNCOMMITTED_LABEL: &str = "Uncommitted change";

/// What the commit cell shows for one commit of the blame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitLabel {
    /// The commit (the click target).
    pub id: ObjectId,
    /// Author name (09 §2.1: body2).
    pub author: SharedString,
    /// The commit's first message line (09 §2.1: caption).
    pub summary: SharedString,
}

/// A blame ready for rendering. Cheap to clone for the virtual list's closure: everything is behind `Arc`. Built
/// in the background ([`BlameDoc::new`] is O(lines)); rows only clone its strings (21 §4.1 perf rules: no
/// `format!` or text processing per frame).
#[derive(Clone)]
pub struct BlameDoc {
    /// The backend result.
    pub result: Arc<BlameResult>,
    /// One label per `result.commits` entry.
    pub labels: Arc<Vec<CommitLabel>>,
    /// Digits of the largest line number: the gutter width (09 §2.1).
    pub digits: usize,
    /// Display text per line ([`display_line`]).
    pub texts: Arc<Vec<SharedString>>,
    /// Line-number strings, `numbers[i]` = `i + 1`.
    pub numbers: Arc<Vec<SharedString>>,
    /// Syntax tokens per line (display text); empty until the highlighter finished.
    pub syntax: Arc<Vec<LineTokens>>,
}

impl BlameDoc {
    /// The document of `result` (no syntax tokens yet). Blocking work: call it off the UI thread.
    pub fn new(result: BlameResult) -> Self {
        let labels = result
            .commits
            .iter()
            .map(|c| CommitLabel {
                id: c.id,
                author: SharedString::from(c.author.person.name.to_str_lossy().into_owned()),
                summary: SharedString::from(c.summary.clone()),
            })
            .collect();
        let digits = digits(result.lines.len());
        let texts = result
            .lines
            .iter()
            .map(|r| {
                display_line(
                    result
                        .text
                        .get(r.start as usize..r.end as usize)
                        .unwrap_or(""),
                )
            })
            .collect();
        let numbers = (1..=result.lines.len())
            .map(|n| SharedString::from(n.to_string()))
            .collect();
        Self {
            result: Arc::new(result),
            labels: Arc::new(labels),
            digits,
            texts: Arc::new(texts),
            numbers: Arc::new(numbers),
            syntax: Arc::new(Vec::new()),
        }
    }

    /// The display text of line `line` (0-based; empty out of range).
    pub fn display(&self, line: usize) -> SharedString {
        self.texts.get(line).cloned().unwrap_or_default()
    }

    /// The line-number string of line `line` (0-based).
    pub fn number(&self, line: usize) -> SharedString {
        self.numbers.get(line).cloned().unwrap_or_default()
    }

    /// Number of lines of the file.
    pub fn line_count(&self) -> usize {
        self.result.lines.len()
    }

    /// The source text of line `line` (0-based), without its terminator.
    pub fn line_text(&self, line: usize) -> &str {
        self.result
            .lines
            .get(line)
            .and_then(|r| self.result.text.get(r.start as usize..r.end as usize))
            .unwrap_or("")
    }

    /// The chunk that contains line `line` (chunks are in file order and cover the file).
    pub fn chunk_of(&self, line: usize) -> Option<&BlameChunk> {
        let chunks = &self.result.chunks;
        let ix = chunks.partition_point(|c| (c.start_line as usize) <= line);
        let chunk = chunks.get(ix.checked_sub(1)?)?;
        (line < chunk.start_line as usize + chunk.len as usize).then_some(chunk)
    }

    /// The commit that last changed line `line`; `None` for lines that are not committed (or out of range).
    pub fn commit_of(&self, line: usize) -> Option<&CommitLabel> {
        let ix = self.chunk_of(line)?.commit?;
        self.labels.get(ix as usize)
    }

    /// All lines of the file joined with `\n` (select all and copy).
    pub fn full_text(&self) -> String {
        let mut out = String::with_capacity(self.result.text.len() + self.line_count());
        for line in 0..self.line_count() {
            if line > 0 {
                out.push('\n');
            }
            out.push_str(self.line_text(line));
        }
        out
    }
}

/// Digits of `n` (at least 1).
pub fn digits(n: usize) -> usize {
    let mut d = 1;
    let mut n = n / 10;
    while n > 0 {
        d += 1;
        n /= 10;
    }
    d
}

/// The display text of a source line: tabs become four spaces (09 §2.1, no tab stops) and the text is cut at
/// [`m::MAX_LINE_BYTES`] on a character boundary.
pub fn display_line(line: &str) -> SharedString {
    let mut text = if line.contains('\t') {
        line.replace('\t', &" ".repeat(m::TAB_SPACES))
    } else {
        line.to_owned()
    };
    if text.len() > m::MAX_LINE_BYTES {
        let mut cut = m::MAX_LINE_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    SharedString::from(text)
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn digits_of_line_counts() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(999), 3);
        assert_eq!(digits(1000), 4);
    }

    #[test]
    fn display_expands_tabs_and_cuts_long_lines() {
        assert_eq!(&*display_line("\tfn x()"), "    fn x()");
        let long = "é".repeat(m::MAX_LINE_BYTES);
        let shown = display_line(&long);
        assert!(shown.len() <= m::MAX_LINE_BYTES);
        assert!(shown.chars().all(|c| c == 'é'), "cut on a boundary");
    }
}
