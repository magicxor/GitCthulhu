//! Syntax highlighting of diff lines (G7b; spec 21 §4.1, §3.2, 09 §1.6, deviation 24).
//!
//! syntect parses with bat's syntaxes from `two-face` (ported from the reference checkout's
//! `but-gpui/src/backend/highlight.rs`). Lines are tagged with token classes rather than colours, so the view colours
//! them from the active theme: the scope classes of the port (comment, string, constant, keyword, function, type,
//! tag) plus annotations are mapped onto Gitnuro's three syntax colours ([`SyntaxToken`]). The syntect theme used
//! here only maps scopes to classes, smuggled through the red channel of the foreground colour.
//!
//! Limits (21 §4.1, 04 §4.1): the first [`FIRST_PASS_CHANGED_LINES`] changed lines are highlighted immediately
//! ([`Pass::First`]), the rest up to [`MAX_CHANGED_LINES`] changed lines in a second pass ([`Pass::Full`]); lines
//! longer than [`MAX_LINE_BYTES`] are never highlighted. Highlighting runs on a background thread and stops early
//! when its [`CancelToken`] is cancelled.

use std::ops::Range;
use std::str::FromStr as _;
use std::sync::OnceLock;

use gpui_kit::Hsla;
use smallvec::SmallVec;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::backend::{CancelToken, LineKind};
use crate::theme::Tokens;

#[cfg(test)]
mod tests;

/// The token classes Gitnuro colours (§3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxToken {
    /// `diff_keyword`.
    Keyword,
    /// `diff_comment`.
    Comment,
    /// `diff_annotation` (annotations, attributes, decorators, type and tag names).
    Annotation,
}

impl SyntaxToken {
    /// The foreground colour of this class in `tokens`: `diff_keyword`, `diff_comment` or `diff_annotation`.
    pub fn color(self, tokens: &Tokens) -> Hsla {
        match self {
            SyntaxToken::Keyword => tokens.diff_keyword,
            SyntaxToken::Comment => tokens.diff_comment,
            SyntaxToken::Annotation => tokens.diff_annotation,
        }
    }
}

/// The tokens of one display line: (byte range in the display text, class).
pub type LineTokens = SmallVec<[(Range<u32>, SyntaxToken); 4]>;

/// Changed lines highlighted by [`Pass::First`] (04 §4.1 `FIRST_HIGHLIGHT_LIMIT`).
pub const FIRST_PASS_CHANGED_LINES: usize = 500;
/// Changed lines highlighted at most, by [`Pass::Full`] (04 §4.1 `HIGHLIGHT_LIMIT`).
pub const MAX_CHANGED_LINES: usize = 4_000;
/// Lines longer than this (in bytes) are never highlighted, as in minified files.
pub const MAX_LINE_BYTES: usize = 1_000;
/// Text parsed by [`Pass::First`] at most, context lines included: a full-file view of a large file with few changes
/// would otherwise parse every context line (twice) before the document can be shown.
pub const FIRST_PASS_BYTES: usize = 64 * 1024;
/// Text parsed by [`Pass::Full`] and [`Highlighter::highlight`] at most (04 §4.1 `WHOLE_HIGHLIGHT_LIMIT`).
pub const MAX_PASS_BYTES: usize = 512 * 1024;

/// Which of the two highlighting passes to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// Up to [`FIRST_PASS_CHANGED_LINES`] changed lines and [`FIRST_PASS_BYTES`]: cheap enough to run while the diff
    /// is built.
    First,
    /// Up to [`MAX_CHANGED_LINES`] changed lines and [`MAX_PASS_BYTES`]: the second pass, run after the first pass
    /// left lines plain.
    Full,
}

impl Pass {
    fn budget(self) -> usize {
        match self {
            Pass::First => FIRST_PASS_CHANGED_LINES,
            Pass::Full => MAX_CHANGED_LINES,
        }
    }

    fn bytes(self) -> usize {
        match self {
            Pass::First => FIRST_PASS_BYTES,
            Pass::Full => MAX_PASS_BYTES,
        }
    }
}

/// The result of one pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Highlighted {
    /// One entry per input line; empty beyond the pass's limit, for long lines and for unknown file types.
    pub tokens: Vec<LineTokens>,
    /// No further pass would add tokens: everything within [`MAX_CHANGED_LINES`] is highlighted (or the file type
    /// has no syntax). `false` after [`Pass::First`] stopped at its limit, or after a cancellation.
    pub complete: bool,
}

/// Scope classes of the syntect theme, in the port's order (a selector's class is its index + 1 in the red
/// channel). Additions to the port for Gitnuro's look: `Annotation` (`@Override`, `#[derive]`, decorators) and
/// `Operator` (symbolic operators stay uncoloured, as in Gitnuro's word matcher); `storage.type`
/// (`let`, `struct`, `class`, `int`) counts as a keyword, as in Gitnuro's keyword lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Comment,
    String,
    Constant,
    Keyword,
    Function,
    Type,
    Tag,
    Annotation,
    Operator,
}

impl Class {
    /// §3.2: Keyword -> keyword; Comment -> comment; Type, Tag, annotations -> annotation; String, Constant and
    /// Function keep the default text colour.
    fn token(self) -> Option<SyntaxToken> {
        match self {
            Class::Keyword => Some(SyntaxToken::Keyword),
            Class::Comment => Some(SyntaxToken::Comment),
            Class::Type | Class::Tag | Class::Annotation => Some(SyntaxToken::Annotation),
            Class::String | Class::Constant | Class::Function | Class::Operator => None,
        }
    }
}

const CLASSES: [(Class, &str); 9] = [
    (Class::Comment, "comment"),
    (Class::String, "string, markup.raw"),
    (
        Class::Constant,
        "constant.numeric, constant.language, constant.character, constant.other",
    ),
    (
        Class::Keyword,
        "keyword, storage.modifier, storage.type, variable.language",
    ),
    (
        Class::Function,
        "entity.name.function, support.function, meta.function-call variable.function",
    ),
    (
        Class::Type,
        "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, support.type, support.class",
    ),
    (
        Class::Tag,
        "entity.name.tag, entity.other.attribute-name, markup.heading",
    ),
    (
        Class::Annotation,
        "meta.annotation, meta.attribute, storage.type.annotation, variable.annotation, punctuation.definition.annotation, entity.name.function.decorator, punctuation.definition.decorator",
    ),
    (
        Class::Operator,
        "keyword.operator.assignment, keyword.operator.arithmetic, keyword.operator.comparison, keyword.operator.bitwise, keyword.operator.range, keyword.operator.ternary, keyword.operator.accessor, keyword.operator.member, keyword.operator.dereference, keyword.operator.namespace, keyword.operator.arrow",
    ),
];

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| Theme {
        settings: ThemeSettings {
            foreground: Some(Color::BLACK),
            ..Default::default()
        },
        scopes: CLASSES
            .iter()
            .enumerate()
            .filter_map(|(ix, (_, selector))| {
                Some(ThemeItem {
                    scope: ScopeSelectors::from_str(selector).ok()?,
                    style: StyleModifier {
                        foreground: Some(Color {
                            r: ix as u8 + 1,
                            g: 0,
                            b: 0,
                            a: 255,
                        }),
                        ..Default::default()
                    },
                })
            })
            .collect(),
        ..Default::default()
    })
}

/// The syntax for `path`: by extension (with a few aliases), then by the whole file name (`Makefile`), then by the
/// first line (shebangs).
fn syntax_for(path: &str, first_line: Option<&str>) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let extension = name.rsplit_once('.').map_or(name, |(_, ext)| ext);
    // Extensions no syntax claims, read as one that has a syntax.
    let extension = match extension {
        "mjs" | "cjs" => "js",
        "jsonc" | "hjson" => "json",
        "mdx" => "md",
        "kts" => "kt",
        other => other,
    };
    set.find_syntax_by_extension(extension)
        .or_else(|| set.find_syntax_by_extension(name))
        .or_else(|| first_line.and_then(|line| set.find_syntax_by_first_line(line)))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// One highlighting state: a syntect parser that carries its state from line to line.
struct LineState {
    lines: HighlightLines<'static>,
    buffer: String,
}

impl LineState {
    fn new(syntax: &'static SyntaxReference) -> Self {
        Self {
            lines: HighlightLines::new(syntax, theme()),
            buffer: String::new(),
        }
    }

    /// The tokens of `line`, continuing the state of the lines fed before.
    fn line(&mut self, line: &str) -> LineTokens {
        self.buffer.clear();
        self.buffer.push_str(line);
        self.buffer.push('\n');
        let Ok(regions) = self.lines.highlight_line(&self.buffer, syntaxes()) else {
            return LineTokens::new();
        };
        let mut tokens = LineTokens::new();
        let mut offset = 0usize;
        for (style, text) in regions {
            let start = offset;
            offset += text.len();
            let end = offset.min(line.len());
            let Some(token) = style
                .foreground
                .r
                .checked_sub(1)
                .and_then(|ix| CLASSES.get(ix as usize))
                .and_then(|(class, _)| class.token())
            else {
                continue;
            };
            if start >= end {
                continue;
            }
            let (start, end) = (start as u32, end as u32);
            match tokens.last_mut() {
                Some((range, last)) if *last == token && range.end == start => range.end = end,
                _ => tokens.push((start..end, token)),
            }
        }
        tokens
    }
}

/// The highlighter (syntect syntax set + two-face definitions, loaded once on first use). `Send + Sync`.
pub struct Highlighter {
    _private: (),
}

impl Highlighter {
    /// The process-wide highlighter. Cheap: the syntax definitions load on the first highlighting call, which runs
    /// on a background thread.
    pub fn global() -> &'static Highlighter {
        static GLOBAL: OnceLock<Highlighter> = OnceLock::new();
        GLOBAL.get_or_init(|| Highlighter { _private: () })
    }

    /// Whether `path` has a syntax (by name; files without one are matched by their first line when highlighted).
    /// Blocking on the first call (it loads the syntax definitions): call it in the background, like the rest.
    pub fn supports(&self, path: &str) -> bool {
        syntax_for(path, None).is_some()
    }

    /// Blocking (background). One side's display lines in file order, the state carried across lines; returns one
    /// entry per input line. Every line counts against the limit of [`MAX_CHANGED_LINES`] (both passes in one call),
    /// and at most [`MAX_PASS_BYTES`] of text are parsed; lines over [`MAX_LINE_BYTES`] and lines after a
    /// cancellation stay empty. Use [`Self::highlight_diff`] for the mixed lines of a unified diff and the two-pass
    /// schedule.
    pub fn highlight(&self, path: &str, lines: &[&str], cancel: &CancelToken) -> Vec<LineTokens> {
        let mut tokens = vec![LineTokens::new(); lines.len()];
        let Some(syntax) = syntax_for(path, lines.first().copied()) else {
            return tokens;
        };
        let mut state = LineState::new(syntax);
        let mut bytes = 0usize;
        for (ix, line) in lines.iter().enumerate().take(MAX_CHANGED_LINES) {
            if cancel.is_cancelled() {
                break;
            }
            if line.len() > MAX_LINE_BYTES {
                continue;
            }
            bytes += line.len() + 1;
            if bytes > MAX_PASS_BYTES {
                break;
            }
            tokens[ix] = state.line(line);
        }
        tokens
    }

    /// Blocking (background). The lines of a text diff in `TextDiff::lines` order with their kinds. The old side
    /// (context and removed lines) and the new side (context and added lines) each keep their own state, so a string
    /// a removed line opens does not run on into the added lines; context lines take the new side's tokens. Only
    /// added and removed lines count against the pass's limit ([`Pass::First`]: 500, [`Pass::Full`]: 4,000); the
    /// lines from the first changed line beyond it on stay empty. Every parsed line counts against the pass's byte
    /// budget ([`FIRST_PASS_BYTES`], [`MAX_PASS_BYTES`]), which bounds full-file views of large files. `path` picks the
    /// new side's syntax; the old side follows `old_path` (the pre-rename path) when there is one, so a rename from
    /// `a.js` to `a.ts` colours each side with its own grammar.
    pub fn highlight_diff(
        &self,
        path: &str,
        old_path: Option<&str>,
        lines: &[(&str, LineKind)],
        pass: Pass,
        cancel: &CancelToken,
    ) -> Highlighted {
        let mut tokens = vec![LineTokens::new(); lines.len()];
        let first_line = lines.first().map(|(text, _)| *text);
        let new_syntax = syntax_for(path, first_line);
        let old_syntax = match old_path {
            Some(old_path) if old_path != path => syntax_for(old_path, first_line),
            _ => new_syntax,
        };
        if new_syntax.is_none() && old_syntax.is_none() {
            return Highlighted {
                tokens,
                complete: true,
            };
        }
        let (mut old, mut new) = (
            old_syntax.map(LineState::new),
            new_syntax.map(LineState::new),
        );
        let budget = pass.budget();
        let mut changed = 0usize;
        let mut bytes = 0usize;
        for (ix, (text, kind)) in lines.iter().enumerate() {
            if cancel.is_cancelled() {
                return Highlighted {
                    tokens,
                    complete: false,
                };
            }
            if *kind != LineKind::Context {
                if changed == budget {
                    // The limit: everything from here on stays plain; a further pass helps only after the first.
                    return Highlighted {
                        tokens,
                        complete: pass == Pass::Full,
                    };
                }
                changed += 1;
            }
            if text.len() > MAX_LINE_BYTES {
                continue;
            }
            bytes += text.len() + 1;
            if bytes > pass.bytes() {
                return Highlighted {
                    tokens,
                    complete: pass == Pass::Full,
                };
            }
            // A side without a syntax stays plain.
            let old_tokens = |old: &mut Option<LineState>| old.as_mut().map(|s| s.line(text));
            let new_tokens = |new: &mut Option<LineState>| new.as_mut().map(|s| s.line(text));
            tokens[ix] = match kind {
                LineKind::Removed => old_tokens(&mut old),
                LineKind::Added => new_tokens(&mut new),
                LineKind::Context => {
                    old_tokens(&mut old);
                    new_tokens(&mut new)
                }
            }
            .unwrap_or_default();
        }
        Highlighted {
            tokens,
            complete: true,
        }
    }
}
