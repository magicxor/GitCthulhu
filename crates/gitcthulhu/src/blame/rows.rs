//! One blame row (09 §2.1): line-number gutter, commit cell, code column.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, HighlightStyle, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, div,
};

use super::doc::{BlameDoc, UNCOMMITTED_LABEL};
use super::ids;
use super::selection::{CodeLine, SelectionBridge};
use crate::diff::highlight::{LineTokens, SyntaxToken};
use crate::metrics::blame as m;
use crate::metrics::dp;
use crate::tab::TabCtx;
use crate::tab::ui::Selection;
use crate::theme::{TextStyleExt as _, Tokens};

/// What the rows of one frame share.
pub(super) struct RowEnv {
    pub tokens: Tokens,
    pub ctx: TabCtx,
    pub bridge: SelectionBridge,
    /// Width of the line-number gutter.
    pub gutter_w: Pixels,
    pub selection: Hsla,
    /// Right press on the code column.
    pub on_menu: Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>,
}

/// The highlight runs of one line: syntax tokens as foreground colours. Ranges that do not fit the text or are not
/// on character boundaries are dropped (the highlighter works on the same display text, so this is a guard only).
pub(super) fn highlights(
    tokens: Option<&LineTokens>,
    text: &str,
    colors: &Tokens,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(tokens) = tokens else {
        return Vec::new();
    };
    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::with_capacity(tokens.len());
    let mut at = 0;
    for (range, kind) in tokens.iter() {
        let (start, end) = (range.start as usize, range.end as usize);
        if start < at
            || start >= end
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            continue;
        }
        at = end;
        out.push((
            start..end,
            HighlightStyle {
                color: Some(token_color(*kind, colors)),
                ..HighlightStyle::default()
            },
        ));
    }
    out
}

/// The foreground of a syntax token class (21 §3.2).
pub(crate) fn token_color(kind: SyntaxToken, colors: &Tokens) -> Hsla {
    match kind {
        SyntaxToken::Keyword => colors.diff_keyword,
        SyntaxToken::Comment => colors.diff_comment,
        SyntaxToken::Annotation => colors.diff_annotation,
    }
}

/// Row `ix` of the blame.
pub(super) fn row(env: &RowEnv, doc: &BlameDoc, ix: usize) -> impl IntoElement + use<> {
    let t = &env.tokens;
    let commit = doc.commit_of(ix);
    let id = commit.map(|c| c.id);
    let author = commit.map(|c| c.author.clone());
    let summary = commit
        .map(|c| c.summary.clone())
        .unwrap_or_else(|| SharedString::new_static(UNCOMMITTED_LABEL));
    let text = doc.display(ix);
    let runs = highlights(doc.syntax.get(ix), &text, t);
    let ctx = env.ctx.clone();
    let on_menu = env.on_menu.clone();
    let hover = t.hover_layer();

    div()
        .w_full()
        .h(dp(m::ROW_H))
        .flex()
        .items_center()
        .bg(t.background)
        .text_color(t.on_background)
        // 1. Line number: mono 14 sp, secondary surface, padding h4, centred.
        .child(
            div()
                .id((ids::BLAME_GUTTER, ix))
                .flex_none()
                .h_full()
                .w(env.gutter_w)
                .flex()
                .items_center()
                .justify_end()
                .px(dp(m::GUTTER_PAD_H))
                .bg(t.secondary_surface)
                .text_mono14()
                .whitespace_nowrap()
                .child(doc.number(ix))
                .test_support(),
        )
        // 2. Commit cell: author and message, clickable.
        .child(
            div()
                .id((ids::BLAME_COMMIT, ix))
                .flex_none()
                .w(dp(m::COMMIT_CELL_W))
                .h_full()
                .flex()
                .flex_col()
                .justify_center()
                .pl(dp(m::COMMIT_TEXT_PAD_START))
                .bg(t.secondary_surface)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(move |_: &ClickEvent, _, cx| select_commit(&ctx, id, cx))
                .when_some(author, |cell, author| {
                    cell.child(div().truncate().text_body2().child(author))
                })
                .child(div().truncate().text_caption().child(summary))
                .test_support(),
        )
        // 3. Code: one line, clipped, syntax coloured, selectable.
        .child(
            div()
                .id((ids::BLAME_CODE, ix))
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .px(dp(m::CODE_PAD_H))
                .overflow_hidden()
                .text_mono14()
                .whitespace_nowrap()
                .on_mouse_down(MouseButton::Right, move |ev, window, cx| {
                    on_menu(ev, window, cx)
                })
                .child(CodeLine::new(
                    ix,
                    text,
                    runs,
                    env.bridge.clone(),
                    env.selection,
                ))
                .test_support(),
        )
}

/// Selects the commit (or the uncommitted changes) in the log and the right pane; the blame stays open.
fn select_commit(ctx: &TabCtx, id: Option<crate::backend::ObjectId>, cx: &mut App) {
    let selection = match id {
        Some(id) => Selection::Commit {
            id,
            is_stash: false,
            scroll_to: true,
        },
        None => Selection::UncommittedChanges,
    };
    ctx.ui.update(cx, |ui, cx| ui.select(selection, cx));
}
