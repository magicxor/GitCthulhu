//! The scenario language: one step per line, words separated by spaces, `"double quotes"` group words, `#` starts
//! a comment (at the start of a line or after a space). Blank lines are skipped.
//!
//! Parsing only tokenizes; the first words of a line name a verb of the verb table (`qa::verbs`), the rest are its
//! arguments. Arguments may use `$SANDBOX` (the sandbox directory) and `$REPOS` (the fixture directory).

use std::path::Path;

/// One non-empty line of a scenario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// 1-based line number in the file.
    pub number: usize,
    /// The line without comment, trimmed (for the report).
    pub text: String,
    /// The words of the line.
    pub tokens: Vec<String>,
}

/// A scenario line that cannot be tokenized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number.
    pub number: usize,
    /// What is wrong.
    pub message: String,
}

/// Tokenizes a scenario; collects every error instead of stopping at the first.
pub fn parse(text: &str) -> Result<Vec<Line>, Vec<ParseError>> {
    let mut lines = Vec::new();
    let mut errors = Vec::new();
    for (ix, raw) in text.lines().enumerate() {
        let number = ix + 1;
        match tokenize(raw) {
            Ok(tokens) if tokens.is_empty() => {}
            Ok(tokens) => lines.push(Line {
                number,
                text: strip_comment(raw).trim().to_owned(),
                tokens,
            }),
            Err(message) => errors.push(ParseError { number, message }),
        }
    }
    if errors.is_empty() {
        Ok(lines)
    } else {
        Err(errors)
    }
}

/// The part of `raw` before a comment (a `#` outside quotes, at the line start or after a space).
fn strip_comment(raw: &str) -> &str {
    let mut quoted = false;
    let mut previous_space = true;
    for (ix, c) in raw.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted && previous_space => return &raw[..ix],
            _ => {}
        }
        previous_space = c.is_whitespace();
    }
    raw
}

fn tokenize(raw: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    for c in strip_comment(raw).chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err("unterminated double quote".into());
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

/// Replaces `$SANDBOX` and `$REPOS` in an argument.
pub fn expand(token: &str, sandbox: &Path) -> String {
    let root = sandbox.display().to_string();
    token
        .replace("$SANDBOX", &root)
        .replace("$REPOS", &sandbox.join("repos").display().to_string())
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn splits_words_and_quoted_groups() {
        let lines = parse("open repository \"C:\\Some Dir\\repo\"\nshot main-dark\n")
            .expect("valid scenario");
        assert_eq!(lines.len(), 2, "two steps");
        assert_eq!(
            lines[0].tokens,
            ["open", "repository", "C:\\Some Dir\\repo"],
            "quotes group a path with spaces"
        );
        assert_eq!(lines[1].number, 2, "line numbers are 1-based");
    }

    #[test]
    fn skips_blank_lines_and_comments() {
        let lines = parse("# header\n\n  # indented\nwait idle # trailing\nshot a#b\n")
            .expect("valid scenario");
        assert_eq!(lines.len(), 2, "comment-only lines vanish");
        assert_eq!(
            lines[0].tokens,
            ["wait", "idle"],
            "trailing comments are cut"
        );
        assert_eq!(lines[0].text, "wait idle", "the report text has no comment");
        assert_eq!(
            lines[1].tokens,
            ["shot", "a#b"],
            "a # inside a word is data"
        );
    }

    #[test]
    fn a_hash_inside_quotes_is_data() {
        let lines = parse("assert toast \"fix #12\"").expect("valid scenario");
        assert_eq!(
            lines[0].tokens,
            ["assert", "toast", "fix #12"],
            "quoted hash"
        );
    }

    #[test]
    fn an_empty_quoted_word_is_kept() {
        let lines = parse("open dialog error \"\" message").expect("valid scenario");
        assert_eq!(lines[0].tokens.len(), 5, "the empty word counts");
    }

    #[test]
    fn an_unterminated_quote_is_reported_with_its_line() {
        let errors = parse("ok\nshot \"oops\n").expect_err("bad quote");
        assert_eq!(errors.len(), 1, "one error");
        assert_eq!(errors[0].number, 2, "the offending line");
    }

    #[test]
    fn expands_sandbox_variables() {
        let root = Path::new("/sbx");
        assert!(
            expand("$REPOS/rich", root).starts_with(&root.join("repos").display().to_string()),
            "$REPOS is <sandbox>/repos"
        );
        assert!(
            expand("$SANDBOX", root).ends_with("sbx"),
            "$SANDBOX is the root"
        );
        assert_eq!(expand("plain", root), "plain", "other words stay");
    }
}
