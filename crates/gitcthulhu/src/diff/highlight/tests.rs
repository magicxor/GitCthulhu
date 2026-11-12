use core::prelude::v1::test;

use super::{
    FIRST_PASS_BYTES, FIRST_PASS_CHANGED_LINES, Highlighter, LineTokens, MAX_CHANGED_LINES,
    MAX_LINE_BYTES, MAX_PASS_BYTES, Pass, SyntaxToken,
};
use crate::backend::{CancelToken, LineKind};
use crate::theme::Tokens;

fn text_of<'a>(line: &'a str, tokens: &LineTokens, token: SyntaxToken) -> Vec<&'a str> {
    tokens
        .iter()
        .filter(|(_, t)| *t == token)
        .map(|(range, _)| &line[range.start as usize..range.end as usize])
        .collect()
}

#[test]
fn highlight_tokens_map_to_three_colours() {
    let lines = [
        "// a comment",
        "#[derive(Debug)]",
        "pub fn main() -> u32 { let s = \"fn\"; 42 }",
    ];
    let tokens = Highlighter::global().highlight("src/main.rs", &lines, &CancelToken::new());
    assert_eq!(tokens.len(), 3, "one entry per input line");

    assert_eq!(
        text_of(lines[0], &tokens[0], SyntaxToken::Comment),
        vec!["// a comment"],
        "a comment line is coloured as a whole"
    );
    assert!(
        !text_of(lines[1], &tokens[1], SyntaxToken::Annotation).is_empty(),
        "a Rust attribute gets the annotation colour: {:?}",
        tokens[1]
    );
    let keywords = text_of(lines[2], &tokens[2], SyntaxToken::Keyword);
    assert!(
        keywords.contains(&"pub") && keywords.contains(&"fn") && keywords.contains(&"let"),
        "keywords are found: {keywords:?}"
    );
    assert!(
        !keywords.contains(&"=") && !keywords.contains(&"->"),
        "symbolic operators keep the text colour: {keywords:?}"
    );
    // Strings and numbers keep the text colour: "fn" inside the string literal is not a keyword.
    let string_start = lines[2].find('"').expect("the line has a string") as u32;
    assert!(
        tokens[2]
            .iter()
            .all(|(range, _)| range.end <= string_start || range.start >= string_start + 4),
        "nothing inside the string literal is coloured: {:?}",
        tokens[2]
    );
    // The tokens never overlap and stay within the line.
    for (line, tokens) in lines.iter().zip(&tokens) {
        let mut end = 0;
        for (range, _) in tokens {
            assert!(
                range.start >= end && range.end as usize <= line.len() && range.start < range.end
            );
            end = range.end;
        }
    }

    // Exactly three colours, from the theme's diff tokens.
    for kind in [false, true] {
        let tokens = if kind {
            crate::theme::builtin::light()
        } else {
            crate::theme::builtin::dark()
        };
        assert_colors(&tokens);
    }
}

fn assert_colors(tokens: &Tokens) {
    assert_eq!(SyntaxToken::Keyword.color(tokens), tokens.diff_keyword);
    assert_eq!(SyntaxToken::Comment.color(tokens), tokens.diff_comment);
    assert_eq!(
        SyntaxToken::Annotation.color(tokens),
        tokens.diff_annotation
    );
}

#[test]
fn python_comments_use_hash() {
    // Gitnuro bug 09 §5.8: Python comments were matched with `//`; the real lexer knows `#`.
    let lines = ["# comment", "@decorator", "def f(): return None"];
    let tokens = Highlighter::global().highlight("tool.py", &lines, &CancelToken::new());
    assert_eq!(
        text_of(lines[0], &tokens[0], SyntaxToken::Comment),
        vec!["# comment"]
    );
    assert!(
        !text_of(lines[1], &tokens[1], SyntaxToken::Annotation).is_empty(),
        "a decorator gets the annotation colour: {:?}",
        tokens[1]
    );
    assert!(text_of(lines[2], &tokens[2], SyntaxToken::Keyword).contains(&"def"));
}

#[test]
fn unknown_file_types_stay_plain() {
    let lines = ["fn main() {}", "// not a comment"];
    let tokens = Highlighter::global().highlight("notes.unknownext", &lines, &CancelToken::new());
    assert!(tokens.iter().all(|t| t.is_empty()));
    assert!(!Highlighter::global().supports("notes.unknownext"));
    assert!(Highlighter::global().supports("a/b/c.rs"));
    assert!(Highlighter::global().supports("web/app.tsx"));
    assert!(Highlighter::global().supports("lib.mjs"));
    // No extension: the first line decides.
    let script = ["#!/bin/bash", "# comment"];
    let tokens = Highlighter::global().highlight("tools/run", &script, &CancelToken::new());
    assert_eq!(
        text_of(script[1], &tokens[1], SyntaxToken::Comment),
        vec!["# comment"]
    );
}

#[test]
fn highlight_state_carries_across_lines() {
    // The comment opened on the first line covers the second; a fresh highlighter would read it as code.
    let lines = [
        "/* start of a comment",
        "fn not_code() {",
        "end */ fn code() {}",
    ];
    let tokens = Highlighter::global().highlight("a.rs", &lines, &CancelToken::new());
    assert_eq!(
        text_of(lines[1], &tokens[1], SyntaxToken::Comment),
        vec!["fn not_code() {"],
        "the second line is still inside the comment"
    );
    assert!(
        text_of(lines[2], &tokens[2], SyntaxToken::Keyword).contains(&"fn"),
        "after the comment ends, code is code again: {:?}",
        tokens[2]
    );

    // In a diff, each side keeps its own state: a comment a removed line opens does not swallow the added lines.
    let diff = [
        ("/* removed opens a comment", LineKind::Removed),
        ("fn added() {}", LineKind::Added),
        ("fn context() {}", LineKind::Context),
    ];
    let result =
        Highlighter::global().highlight_diff("a.rs", &diff, Pass::Full, &CancelToken::new());
    assert!(result.complete);
    assert_eq!(
        text_of(diff[0].0, &result.tokens[0], SyntaxToken::Comment),
        vec![diff[0].0]
    );
    assert!(
        text_of(diff[1].0, &result.tokens[1], SyntaxToken::Keyword).contains(&"fn"),
        "the added line is highlighted with the new side's state: {:?}",
        result.tokens[1]
    );
    assert!(
        text_of(diff[2].0, &result.tokens[2], SyntaxToken::Keyword).contains(&"fn"),
        "context lines take the new side's tokens: {:?}",
        result.tokens[2]
    );
}

#[test]
fn highlight_limits_500_then_4000_and_1000_bytes() {
    let keyword_line = "let x = 1;";
    let long = format!("let y = \"{}\";", "a".repeat(MAX_LINE_BYTES));
    // 4,100 changed lines with context lines in between; line 3 is too long to highlight.
    let mut lines: Vec<(String, LineKind)> = Vec::new();
    for ix in 0..4_100 {
        if ix % 10 == 0 {
            lines.push((keyword_line.to_owned(), LineKind::Context));
        }
        let kind = if ix % 2 == 0 {
            LineKind::Removed
        } else {
            LineKind::Added
        };
        let text = if ix == 3 {
            long.clone()
        } else {
            keyword_line.to_owned()
        };
        lines.push((text, kind));
    }
    let input: Vec<(&str, LineKind)> = lines.iter().map(|(t, k)| (t.as_str(), *k)).collect();
    // The index of the n-th changed line (0-based).
    let changed_ix = |n: usize| {
        input
            .iter()
            .enumerate()
            .filter(|(_, (_, k))| *k != LineKind::Context)
            .nth(n)
            .map(|(ix, _)| ix)
            .expect("enough changed lines")
    };
    let highlighter = Highlighter::global();
    let cancel = CancelToken::new();

    let first = highlighter.highlight_diff("a.rs", &input, Pass::First, &cancel);
    assert_eq!(first.tokens.len(), input.len());
    assert!(!first.complete, "the first pass stops at 500 changed lines");
    assert!(!first.tokens[changed_ix(0)].is_empty());
    assert!(
        first.tokens[changed_ix(3)].is_empty(),
        "a line over {MAX_LINE_BYTES} bytes is never highlighted"
    );
    assert!(!first.tokens[changed_ix(FIRST_PASS_CHANGED_LINES - 1)].is_empty());
    assert!(
        first.tokens[changed_ix(FIRST_PASS_CHANGED_LINES)..]
            .iter()
            .all(|t| t.is_empty()),
        "nothing after the 500th changed line"
    );

    let full = highlighter.highlight_diff("a.rs", &input, Pass::Full, &cancel);
    assert!(full.complete, "the second pass is final");
    assert!(full.tokens[changed_ix(3)].is_empty());
    assert!(!full.tokens[changed_ix(FIRST_PASS_CHANGED_LINES)].is_empty());
    assert!(!full.tokens[changed_ix(MAX_CHANGED_LINES - 1)].is_empty());
    assert!(
        full.tokens[changed_ix(MAX_CHANGED_LINES)..]
            .iter()
            .all(|t| t.is_empty()),
        "nothing after the 4,000th changed line"
    );

    // A small diff is complete after the first pass.
    let small = highlighter.highlight_diff("a.rs", &input[..20], Pass::First, &cancel);
    assert!(small.complete);

    // The single-side entry point: 4,000 lines at most, one entry per line.
    let side: Vec<&str> = (0..MAX_CHANGED_LINES + 5).map(|_| keyword_line).collect();
    let tokens = highlighter.highlight("a.rs", &side, &cancel);
    assert_eq!(tokens.len(), side.len());
    assert!(!tokens[MAX_CHANGED_LINES - 1].is_empty());
    assert!(tokens[MAX_CHANGED_LINES..].iter().all(|t| t.is_empty()));
}

#[test]
fn highlight_cancel_returns_early() {
    let lines: Vec<&str> = (0..2_000).map(|_| "fn f() { let a = 1; }").collect();
    let cancel = CancelToken::new();
    cancel.cancel();
    let tokens = Highlighter::global().highlight("a.rs", &lines, &cancel);
    assert_eq!(tokens.len(), lines.len(), "still one entry per line");
    assert!(
        tokens.iter().all(|t| t.is_empty()),
        "nothing is highlighted after a cancellation"
    );

    let diff: Vec<(&str, LineKind)> = lines.iter().map(|l| (*l, LineKind::Added)).collect();
    let result = Highlighter::global().highlight_diff("a.rs", &diff, Pass::Full, &cancel);
    assert_eq!(result.tokens.len(), diff.len());
    assert!(!result.complete, "a cancelled pass is not complete");
    assert!(result.tokens.iter().all(|t| t.is_empty()));
}

#[test]
fn highlight_first_pass_is_bounded_by_bytes_in_full_file_views() {
    // A full-file view: thousands of context lines and one change at the end. The first pass must not parse the
    // whole file before the document is shown; the second pass reaches the change.
    let context = "let value = compute(1, 2, 3); // ok";
    let mut input: Vec<(&str, LineKind)> =
        (0..2_500).map(|_| (context, LineKind::Context)).collect();
    input.push(("let changed = 1;", LineKind::Added));
    let total: usize = input.iter().map(|(t, _)| t.len() + 1).sum();
    assert!(total > FIRST_PASS_BYTES && total < MAX_PASS_BYTES);
    let last = input.len() - 1;
    let cancel = CancelToken::new();

    let first = Highlighter::global().highlight_diff("a.rs", &input, Pass::First, &cancel);
    assert!(!first.complete, "the first pass stopped at its byte budget");
    assert!(!first.tokens[0].is_empty());
    let parsed = first.tokens.iter().take_while(|t| !t.is_empty()).count();
    assert!(
        parsed * (context.len() + 1) <= FIRST_PASS_BYTES && parsed > 1_000,
        "about {FIRST_PASS_BYTES} bytes are parsed: {parsed} lines"
    );
    assert!(first.tokens[last].is_empty());

    let full = Highlighter::global().highlight_diff("a.rs", &input, Pass::Full, &cancel);
    assert!(full.complete);
    assert!(
        !full.tokens[last].is_empty(),
        "the second pass reaches the change"
    );
}
