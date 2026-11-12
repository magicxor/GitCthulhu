//! The read-only diff of the file history's right area (09 §2.2: "same renderer as §1.3/1.4, no header, no
//! actions"): hunk headers, unified lines and split pairs over a [`DiffDoc`], with intra-line backgrounds and syntax
//! colours. There are no hunk buttons, hover chips or line menus; text cells go through [`DiffSelection`].

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, StyledText, TestSupportExt as _, Window, div,
};

use super::ids;
use crate::backend::{DiffContent, DiffLine, LineKind, TextDiff};
use crate::blame::token_color;
use crate::diff::model::{DiffDoc, DiffRow};
use crate::diff::selection::{DiffSelection, SelKey, SelSide};
use crate::metrics::dp;
use crate::metrics::history as m;
use crate::theme::{TextStyleExt as _, Tokens};

/// What the rows of one frame share.
pub(super) struct DiffEnv {
    pub doc: Arc<DiffDoc>,
    /// Hunk header texts, per hunk.
    pub headers: Arc<Vec<SharedString>>,
    pub selection: Rc<DiffSelection>,
    pub tokens: Tokens,
}

/// The text of a diff document, if it is a text diff.
pub(super) fn text_of(doc: &DiffDoc) -> Option<&Arc<TextDiff>> {
    match &doc.diff.content {
        DiffContent::Text(text) => Some(text),
        _ => None,
    }
}

/// Row `ix` of the document.
pub(super) fn render_row(
    env: &DiffEnv,
    ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(text) = text_of(&env.doc) else {
        return div().into_any_element();
    };
    let Some(row) = env.doc.rows.get(ix).copied() else {
        return div().into_any_element();
    };
    let t = &env.tokens;
    match row {
        DiffRow::HunkHeader { hunk } => hunk_header(env, hunk, ix),
        DiffRow::Line { line } => {
            let l = text.lines[line as usize];
            let digits = digits_of_line(&env.doc, text, line);
            let bg = line_bg(t, l.kind);
            let numbers = div()
                .flex_none()
                .flex()
                .child(number(env, l.old_lineno, &l, digits))
                .child(number(env, l.new_lineno, &l, digits));
            div()
                .id((ids::HISTORY_DIFF_ROW, ix))
                .w_full()
                .flex()
                .items_start()
                .bg(bg)
                .text_color(t.on_background)
                .child(numbers)
                .child(text_cell(env, text, line, SelSide::Unified, window, cx))
                .test_support()
                .into_any_element()
        }
        DiffRow::Pair { left, right, hunk } => {
            let digits = env
                .doc
                .gutter_digits
                .get(hunk as usize)
                .copied()
                .unwrap_or(1) as usize;
            div()
                .id((ids::HISTORY_DIFF_ROW, ix))
                .w_full()
                .flex()
                .bg(t.secondary_surface)
                .text_color(t.on_background)
                .child(side(env, text, left, SelSide::Old, digits, window, cx))
                .child(
                    div()
                        .flex_none()
                        .w(dp(m::SPLIT_DIVIDER_W))
                        .bg(t.secondary_surface),
                )
                .child(side(env, text, right, SelSide::New, digits, window, cx))
                .test_support()
                .into_any_element()
        }
    }
}

fn line_bg(t: &Tokens, kind: LineKind) -> Hsla {
    match kind {
        LineKind::Added => t.diff_line_added,
        LineKind::Removed => t.diff_line_removed,
        LineKind::Context => t.background,
    }
}

/// The number of digits of the hunk that contains diff line `line` (09 §1.3 `highestLineNumberLength`).
fn digits_of_line(doc: &DiffDoc, text: &TextDiff, line: u32) -> usize {
    let hunk = text.hunks.partition_point(|h| h.lines.end <= line);
    doc.gutter_digits.get(hunk).copied().unwrap_or(1) as usize
}

/// `@@ -a,b +c,d @@` (a count of 1 is written as just the start).
pub(super) fn hunk_header_text(header: &crate::backend::HunkHeader) -> String {
    fn range(start: u32, count: u32) -> String {
        if count == 1 {
            start.to_string()
        } else {
            format!("{start},{count}")
        }
    }
    format!(
        "@@ -{} +{} @@",
        range(header.old_start, header.old_lines),
        range(header.new_start, header.new_lines)
    )
}

fn hunk_header(env: &DiffEnv, hunk: u32, ix: usize) -> AnyElement {
    let t = &env.tokens;
    let header = env.headers.get(hunk as usize).cloned().unwrap_or_default();
    div()
        .id((ids::HISTORY_DIFF_ROW, ix))
        .w_full()
        .flex()
        .items_center()
        .px(dp(m::HUNK_PAD_H))
        .py(dp(m::HUNK_PAD_V))
        .bg(t.secondary_surface)
        .text_body1()
        .text_color(t.on_background)
        .child(header)
        .test_support()
        .into_any_element()
}

/// One line-number column: the number (right-aligned in `digits` digits) or blank.
fn number(env: &DiffEnv, n: Option<u32>, line: &DiffLine, digits: usize) -> AnyElement {
    let t = &env.tokens;
    let remarked = line.kind != LineKind::Context;
    div()
        .flex_none()
        .w(dp(m::NUMBER_PAD + digits as f32 * m::DIGIT_W))
        .pl(dp(m::NUMBER_PAD_START))
        .pr(dp(m::NUMBER_PAD_END))
        .text_mono13()
        .text_right()
        .whitespace_nowrap()
        .text_color(if remarked {
            t.on_background
        } else {
            t.on_background_secondary
        })
        .children(n.map(|n| env.doc.numbers.get(n)))
        .into_any_element()
}

/// One half of a split row; `None` is the grey filler.
fn side(
    env: &DiffEnv,
    text: &TextDiff,
    line: Option<u32>,
    side: SelSide,
    digits: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let t = &env.tokens;
    let Some(ix) = line else {
        return div()
            .flex_1()
            .min_w_0()
            .bg(t.secondary_surface)
            .into_any_element();
    };
    let l = text.lines[ix as usize];
    let n = match side {
        SelSide::Old => l.old_lineno,
        _ => l.new_lineno,
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_start()
        .bg(line_bg(t, l.kind))
        .child(number(env, n, &l, digits))
        .child(text_cell(env, text, ix, side, window, cx))
        .into_any_element()
}

fn text_cell(
    env: &DiffEnv,
    text: &TextDiff,
    line: u32,
    side: SelSide,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let t = &env.tokens;
    let l = text.lines[line as usize];
    let shown = env
        .doc
        .texts
        .get(line as usize)
        .cloned()
        .unwrap_or_default();
    let background = match l.kind {
        LineKind::Added => Some(t.diff_content_added),
        LineKind::Removed => Some(t.diff_content_removed),
        LineKind::Context => None,
    };
    let source = text.line_text(&l);
    let inline: Vec<Range<usize>> = if background.is_some() {
        text.inline_spans(&l)
            .iter()
            .map(|r| {
                to_display(source, r.start as usize, shown.len())
                    ..to_display(source, r.end as usize, shown.len())
            })
            .collect()
    } else {
        Vec::new()
    };
    let tokens: Vec<(Range<usize>, Hsla)> = env
        .doc
        .syntax
        .get(line as usize)
        .map(|tokens| {
            tokens
                .iter()
                .map(|(r, kind)| (r.start as usize..r.end as usize, token_color(*kind, t)))
                .collect()
        })
        .unwrap_or_default();
    let runs = merge_highlights(&shown, &inline, background, &tokens);
    let styled = StyledText::new(shown).with_highlights(runs);
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .pl(dp(m::TEXT_PAD_START))
        .text_mono13()
        .child(
            env.selection
                .text_cell(SelKey { line, side }, styled, window, cx),
        )
        .into_any_element()
}

/// A byte offset of the source line mapped into its display text (tabs are four spaces there), clamped to `max`.
pub(super) fn to_display(source: &str, offset: usize, max: usize) -> usize {
    let end = offset.min(source.len());
    let tabs = source
        .as_bytes()
        .get(..end)
        .map_or(0, |b| b.iter().filter(|c| **c == b'\t').count());
    (end + tabs * (crate::metrics::blame::TAB_SPACES - 1)).min(max)
}

/// Combines the intra-line background spans and the syntax foregrounds into non-overlapping runs. (`combine_highlights`
/// decides in hash order where two layers set the same field, so the runs are split by hand.) Offsets that are not
/// on character boundaries of `text` are dropped.
pub(super) fn merge_highlights(
    text: &str,
    backgrounds: &[Range<usize>],
    background: Option<Hsla>,
    foregrounds: &[(Range<usize>, Hsla)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    if backgrounds.is_empty() && foregrounds.is_empty() {
        return Vec::new();
    }
    let ok = |o: usize| o <= text.len() && text.is_char_boundary(o);
    let mut cuts: Vec<usize> = backgrounds
        .iter()
        .cloned()
        .chain(foregrounds.iter().map(|(r, _)| r.clone()))
        .flat_map(|r| [r.start, r.end])
        .filter(|o| ok(*o))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let bg = background.filter(|_| backgrounds.iter().any(|r| r.start <= a && b <= r.end));
        let fg = foregrounds
            .iter()
            .find(|(r, _)| r.start <= a && b <= r.end)
            .map(|(_, c)| *c);
        if bg.is_some() || fg.is_some() {
            out.push((
                a..b,
                HighlightStyle {
                    color: fg,
                    background_color: bg,
                    ..HighlightStyle::default()
                },
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use gpui_kit::{black, red, white};

    use super::*;

    #[test]
    fn hunk_header_counts() {
        let h = crate::backend::HunkHeader {
            old_start: 12,
            old_lines: 7,
            new_start: 12,
            new_lines: 1,
        };
        assert_eq!(hunk_header_text(&h), "@@ -12,7 +12 @@");
        let empty = crate::backend::HunkHeader {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: 3,
        };
        assert_eq!(hunk_header_text(&empty), "@@ -0,0 +1,3 @@");
    }

    #[test]
    fn tab_offsets_map_into_display_text() {
        assert_eq!(to_display("\tab", 1, 100), 4);
        assert_eq!(to_display("\tab", 3, 100), 6);
        assert_eq!(to_display("\tab", 3, 5), 5, "clamped");
    }

    #[test]
    fn layers_are_split_into_disjoint_runs() {
        let inline = vec![2..6, 6..6];
        let runs = merge_highlights(
            "abcdefgh",
            &inline,
            Some(black()),
            &[(0..4, red()), (5..8, white())],
        );
        let shape: Vec<(Range<usize>, bool, bool)> = runs
            .iter()
            .map(|(r, s)| (r.clone(), s.color.is_some(), s.background_color.is_some()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (0..2, true, false),
                (2..4, true, true),
                (4..5, false, true),
                (5..6, true, true),
                (6..8, true, false),
            ]
        );
        assert!(runs.windows(2).all(|w| w[0].0.end <= w[1].0.start));
    }

    #[test]
    fn no_layers_no_runs() {
        assert!(merge_highlights("abc", &[], None, &[]).is_empty());
    }
}
