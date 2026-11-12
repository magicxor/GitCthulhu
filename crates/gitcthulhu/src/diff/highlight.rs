//! Syntax highlighting of diff lines (owned by G7b after G0b; spec 21 §4.1, §3.2): syntect tokens mapped onto
//! Gitnuro's three syntax colours. The milestone-1 stub returns no tokens; the interface is final.

use std::ops::Range;
use std::sync::OnceLock;

use smallvec::SmallVec;

use crate::backend::CancelToken;

/// The token classes Gitnuro colours (§3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxToken {
    Keyword,
    Comment,
    Annotation,
}

/// The tokens of one display line: (byte range in the display text, class).
pub type LineTokens = SmallVec<[(Range<u32>, SyntaxToken); 4]>;

/// The highlighter (syntect syntax set + two-face definitions, loaded once). `Send + Sync`.
pub struct Highlighter {
    _private: (),
}

impl Highlighter {
    /// The process-wide highlighter (lazily initialised).
    pub fn global() -> &'static Highlighter {
        static GLOBAL: OnceLock<Highlighter> = OnceLock::new();
        GLOBAL.get_or_init(|| Highlighter { _private: () })
    }

    /// Blocking (background). One side's display lines in file order, the state carried across lines; returns one
    /// entry per input line, empty beyond the limits (first 500 changed lines, then up to 4,000; none for lines
    /// over 1,000 bytes). Milestone-1 stub: all empty.
    pub fn highlight(&self, _path: &str, lines: &[&str], _cancel: &CancelToken) -> Vec<LineTokens> {
        vec![LineTokens::new(); lines.len()]
    }
}
