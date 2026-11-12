//! Diff row rendering: hunk headers, lines, split pairs and the hover chip (owned by G7a; 09 §1.3, §1.4, spec
//! 21 §4.1). Text cells go through `DiffSelection::text_cell` (G7b). Rows are plain elements built for the visible
//! range only; the only per-row allocation besides the element tree is the highlight vector of the text.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, HighlightStyle, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Point,
    StatefulInteractiveElement as _, Styled as _, StyledText, TestSupportExt as _, Window, div,
};
use smallvec::SmallVec;

use super::highlight::SyntaxToken;
use super::model::{DiffDoc, DiffRow, Uncommitted};
use super::selection::{SelKey, SelSide};
use super::{DiffView, SelectionOp, ids, line_request, run_selection};
use crate::backend::{DiffLine, DiffTarget, LineKind};
use crate::icons::{AppIcon, icon};
use crate::menus::{self, MenuKind, MenuTarget};
use crate::metrics::{diff as m, dp};
use crate::tab::ops::files::SelectionUnit;
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::widgets::SecondaryButton;

/// Hover group of a text cell (the chip shows while its own cell is hovered; nested groups resolve to the nearest).
const TEXT_GROUP: &str = "diff-text-cell";

/// Renders row `ix` of the shown document.
pub(super) fn render_row(
    view: &mut DiffView,
    ix: usize,
    window: &mut Window,
    cx: &mut Context<DiffView>,
) -> AnyElement {
    let Some(doc) = view.loaded.as_ref().map(|l| l.doc.clone()) else {
        return div().into_any_element();
    };
    let Some(row) = doc.rows.get(ix).copied() else {
        return div().into_any_element();
    };
    match row {
        DiffRow::HunkHeader { hunk } => hunk_header(view, &doc, ix, hunk, cx),
        DiffRow::Line { line } => unified_row(view, &doc, ix, line, window, cx),
        DiffRow::Pair { left, right, hunk } => {
            split_row(view, &doc, ix, left, right, hunk, window, cx)
        }
    }
}

/// The background of a line row (09 §1.3): the gutters share it.
pub(super) fn line_background(kind: LineKind, tokens: &Tokens) -> Hsla {
    match kind {
        LineKind::Added => tokens.diff_line_added,
        LineKind::Removed => tokens.diff_line_removed,
        LineKind::Context => tokens.background,
    }
}

fn number_color(kind: LineKind, tokens: &Tokens) -> Hsla {
    match kind {
        LineKind::Context => tokens.on_background_secondary,
        LineKind::Added | LineKind::Removed => tokens.on_background,
    }
}

fn hunk_header(
    view: &DiffView,
    doc: &Arc<DiffDoc>,
    ix: usize,
    hunk: u32,
    cx: &mut Context<DiffView>,
) -> AnyElement {
    let tokens = *cx.tokens();
    let title = doc.headers.get(hunk as usize).cloned().unwrap_or_default();
    let controls = view.frame.controls;
    let button = |id: &'static str, label: &'static str, bg: Hsla, op: SelectionOp| {
        let ctx = view.ctx.clone();
        let doc = doc.clone();
        div().mx(dp(m::HUNK_BUTTON_MARGIN_H)).child(
            SecondaryButton::new((id, ix), label)
                .background(bg)
                .on_click(move |window, cx| {
                    if let Some(req) = super::hunk_request(&doc, hunk) {
                        run_selection(&ctx, op, req, SelectionUnit::Hunk, window, cx);
                    }
                }),
        )
    };
    div()
        .id((ids::HUNK, ix))
        .w_full()
        .flex()
        .items_center()
        .bg(tokens.secondary_surface)
        .px(dp(m::HUNK_PAD_H))
        .py(dp(m::HUNK_PAD_V))
        .child(
            div()
                .text_body1()
                .text_color(tokens.on_background)
                .whitespace_nowrap()
                .child(title),
        )
        .child(div().flex_1())
        .when(controls.hunk_buttons, |row| match controls.list {
            Some(Uncommitted::Unstaged) => row
                .child(button(
                    ids::HUNK_DISCARD,
                    "Discard hunk",
                    tokens.error,
                    SelectionOp::Discard,
                ))
                .child(button(
                    ids::HUNK_STAGE,
                    "Stage hunk",
                    tokens.primary,
                    SelectionOp::Stage,
                )),
            Some(Uncommitted::Staged) => row.child(button(
                ids::HUNK_UNSTAGE,
                "Unstage hunk",
                tokens.error,
                SelectionOp::Unstage,
            )),
            None => row,
        })
        .test_support()
        .into_any_element()
}

/// A line-number column: `12 + digits × advance` wide (09 §1.3), number right-aligned (Gitnuro pads with spaces).
fn number_cell(
    view: &DiffView,
    id: ElementId,
    number: Option<u32>,
    digits: u8,
    color: Hsla,
    doc: &DiffDoc,
) -> AnyElement {
    let width = view.frame.number_pad + view.frame.advance * f32::from(digits);
    div()
        .id(id)
        .flex_none()
        .w(width)
        .pl(dp(m::NUMBER_PAD_START))
        .pr(dp(m::NUMBER_PAD_END))
        .flex()
        .justify_end()
        .whitespace_nowrap()
        .text_color(color)
        .when_some(number, |cell, n| cell.child(doc.numbers.get(n)))
        .test_support()
        .into_any_element()
}

/// Combined highlights of a display line: intra-line change backgrounds and syntax foregrounds, split into
/// non-overlapping runs by hand (combining overlapping layers picks a winner in hash order).
pub(super) fn highlights(
    text: &str,
    inline: &[Range<u32>],
    syntax: &[(Range<u32>, SyntaxToken)],
    change_bg: Option<Hsla>,
    tokens: &Tokens,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let inline = if change_bg.is_some() { inline } else { &[] };
    if inline.is_empty() && syntax.is_empty() {
        return Vec::new();
    }
    let floor = |mut at: usize| {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let mut cuts: SmallVec<[usize; 16]> = SmallVec::new();
    for r in inline.iter().chain(syntax.iter().map(|(r, _)| r)) {
        cuts.push(floor(r.start as usize));
        cuts.push(floor(r.end as usize));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::with_capacity(cuts.len());
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let covers = |r: &Range<u32>| r.start as usize <= a && b <= r.end as usize;
        let background_color = change_bg.filter(|_| inline.iter().any(covers));
        let color = syntax
            .iter()
            .find(|(r, _)| covers(r))
            .map(|(_, token)| match token {
                SyntaxToken::Keyword => tokens.diff_keyword,
                SyntaxToken::Comment => tokens.diff_comment,
                SyntaxToken::Annotation => tokens.diff_annotation,
            });
        if background_color.is_none() && color.is_none() {
            continue;
        }
        out.push((
            a..b,
            HighlightStyle {
                color,
                background_color,
                ..HighlightStyle::default()
            },
        ));
    }
    out
}

/// Which chip a text cell shows, if any.
#[derive(Clone, Copy)]
struct Chip {
    id: &'static str,
    op: SelectionOp,
    role: AppIcon,
    color: Hsla,
}

fn chip_for(view: &DiffView, line: &DiffLine, tokens: &Tokens) -> Option<Chip> {
    let controls = view.frame.controls;
    if !controls.line_chip || line.kind == LineKind::Context {
        return None;
    }
    match controls.list? {
        Uncommitted::Unstaged => Some(Chip {
            id: ids::CHIP,
            op: SelectionOp::Stage,
            role: AppIcon::Add,
            color: tokens.primary,
        }),
        Uncommitted::Staged => Some(Chip {
            id: ids::CHIP,
            op: SelectionOp::Unstage,
            role: AppIcon::Remove,
            color: tokens.error,
        }),
    }
}

/// A text cell: 16 dp start padding holding the hover chip, then the selectable text (wraps to the cell width).
#[allow(clippy::too_many_arguments)]
fn text_cell(
    view: &DiffView,
    doc: &Arc<DiffDoc>,
    ix: usize,
    line_ix: u32,
    side: SelSide,
    text_id: &'static str,
    chip: Option<Chip>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let tokens = *cx.tokens();
    let li = line_ix as usize;
    let text = doc.texts.get(li).cloned().unwrap_or_default();
    let kind = doc.line(line_ix).map_or(LineKind::Context, |l| l.kind);
    let change_bg = match kind {
        LineKind::Added => Some(tokens.diff_content_added),
        LineKind::Removed => Some(tokens.diff_content_removed),
        LineKind::Context => None,
    };
    let runs = highlights(
        &text,
        doc.inline.get(li).map_or(&[][..], |s| &s[..]),
        doc.syntax.get(li).map_or(&[][..], |s| &s[..]),
        change_bg,
        &tokens,
    );
    let styled = StyledText::new(text).with_highlights(runs);
    let content = view.selection.text_cell(
        SelKey {
            line: line_ix,
            side,
        },
        styled,
        window,
        cx,
    );
    let chip_element = chip.map(|chip| {
        let ctx = view.ctx.clone();
        let doc = doc.clone();
        div()
            .id((chip.id, ix))
            .absolute()
            .left_0()
            .top_0()
            .size(dp(m::CHIP))
            .rounded(dp(m::CHIP_RADIUS))
            .bg(chip.color)
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .opacity(0.)
            .group_hover(TEXT_GROUP, |s| s.opacity(1.))
            .on_click(move |_, window, cx| {
                if let Some(req) = line_request(&doc, line_ix) {
                    run_selection(&ctx, chip.op, req, SelectionUnit::Line, window, cx);
                }
            })
            .child(icon(chip.role, m::CHIP, tokens.on_primary))
            .test_support()
    });
    div()
        .id((text_id, ix))
        .group(TEXT_GROUP)
        .relative()
        .flex_1()
        .min_w_0()
        .pl(dp(m::TEXT_PAD_START))
        .text_color(tokens.on_background)
        .child(content)
        .children(chip_element)
        .test_support()
        .into_any_element()
}

/// Right-click on a changed line: the diff line menu (09 §4.6), unless text is selected (G7b's menu then).
fn line_menu_listener(
    line_ix: u32,
    cx: &mut Context<DiffView>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(
        move |this: &mut DiffView, ev: &MouseDownEvent, window, cx| {
            this.open_line_menu(line_ix, ev.position, window, cx);
        },
    )
}

impl DiffView {
    fn open_line_menu(
        &mut self,
        line_ix: u32,
        position: Point<gpui_kit::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selection.has_selection(cx) {
            return;
        }
        let Some(loaded) = &self.loaded else {
            return;
        };
        let doc = loaded.doc.clone();
        let discard = if self.frame.controls.discard_line {
            line_request(&doc, line_ix).map(|req| {
                let ctx = self.ctx.clone();
                Rc::new(move |window: &mut Window, cx: &mut App| {
                    run_selection(
                        &ctx,
                        SelectionOp::Discard,
                        req.clone(),
                        SelectionUnit::Line,
                        window,
                        cx,
                    );
                }) as Rc<dyn Fn(&mut Window, &mut App)>
            })
        } else {
            None
        };
        let commit = match &doc.diff.target {
            DiffTarget::Commit { commit, .. } => Some(*commit),
            DiffTarget::Staged { .. } | DiffTarget::Unstaged { .. } => None,
        };
        let target = MenuTarget {
            kind: MenuKind::DiffLine,
            tab: self.ctx.clone(),
            branch: None,
            upstream: None,
            commit,
            path: Some(doc.diff.path.clone()),
            is_current: false,
            checked_out_elsewhere: false,
        };
        let items = menus::diff_line::diff_line_with(&target, discard, cx);
        menus::open_context_menu(window, cx, position, items);
    }
}

fn unified_row(
    view: &DiffView,
    doc: &Arc<DiffDoc>,
    ix: usize,
    line_ix: u32,
    window: &mut Window,
    cx: &mut Context<DiffView>,
) -> AnyElement {
    let tokens = *cx.tokens();
    let Some(line) = doc.line(line_ix).copied() else {
        return div().into_any_element();
    };
    let digits = doc
        .row_hunk(&DiffRow::Line { line: line_ix })
        .and_then(|h| doc.gutter_digits.get(h as usize).copied())
        .unwrap_or(1);
    let color = number_color(line.kind, &tokens);
    let chip = chip_for(view, &line, &tokens);
    let old = number_cell(
        view,
        (ids::NUM_OLD, ix).into(),
        line.old_lineno,
        digits,
        color,
        doc,
    );
    let new = number_cell(
        view,
        (ids::NUM_NEW, ix).into(),
        line.new_lineno,
        digits,
        color,
        doc,
    );
    let text = text_cell(
        view,
        doc,
        ix,
        line_ix,
        SelSide::Unified,
        ids::TEXT,
        chip,
        window,
        cx,
    );
    let menu = line_menu_listener(line_ix, cx);
    div()
        .id((ids::LINE, ix))
        .w_full()
        .min_h(dp(m::LINE_H))
        .flex()
        .items_start()
        .bg(line_background(line.kind, &tokens))
        .text_mono13()
        .on_mouse_down(MouseButton::Right, menu)
        .child(old)
        .child(new)
        .child(text)
        .test_support()
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn split_side(
    view: &DiffView,
    doc: &Arc<DiffDoc>,
    ix: usize,
    line_ix: Option<u32>,
    side: SelSide,
    digits: u8,
    window: &mut Window,
    cx: &mut Context<DiffView>,
) -> AnyElement {
    let tokens = *cx.tokens();
    let (number_id, text_id, chip_id, filler_id, side_id) = match side {
        SelSide::Old => (
            ids::NUM_OLD,
            ids::TEXT_OLD,
            ids::CHIP_OLD,
            ids::FILLER_OLD,
            ids::SIDE_OLD,
        ),
        SelSide::New | SelSide::Unified => (
            ids::NUM_NEW,
            ids::TEXT_NEW,
            ids::CHIP_NEW,
            ids::FILLER_NEW,
            ids::SIDE_NEW,
        ),
    };
    let Some((line_ix, line)) = line_ix.and_then(|l| doc.line(l).map(|line| (l, *line))) else {
        // The row's secondary-surface colour shows through (09 §1.4 grey filler).
        return div()
            .id((filler_id, ix))
            .flex_1()
            .min_w_0()
            .test_support()
            .into_any_element();
    };
    let number = match side {
        SelSide::Old => line.old_lineno,
        SelSide::New | SelSide::Unified => line.new_lineno,
    };
    let color = number_color(line.kind, &tokens);
    let chip = chip_for(view, &line, &tokens).map(|c| Chip { id: chip_id, ..c });
    let number = number_cell(view, (number_id, ix).into(), number, digits, color, doc);
    let text = text_cell(view, doc, ix, line_ix, side, text_id, chip, window, cx);
    let menu = line_menu_listener(line_ix, cx);
    div()
        .id((side_id, ix))
        .flex_1()
        .min_w_0()
        .flex()
        .items_start()
        .bg(line_background(line.kind, &tokens))
        .on_mouse_down(MouseButton::Right, menu)
        .child(number)
        .child(text)
        .test_support()
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn split_row(
    view: &DiffView,
    doc: &Arc<DiffDoc>,
    ix: usize,
    left: Option<u32>,
    right: Option<u32>,
    hunk: u32,
    window: &mut Window,
    cx: &mut Context<DiffView>,
) -> AnyElement {
    let tokens = *cx.tokens();
    let digits = doc.gutter_digits.get(hunk as usize).copied().unwrap_or(1);
    let left = split_side(view, doc, ix, left, SelSide::Old, digits, window, cx);
    let right = split_side(view, doc, ix, right, SelSide::New, digits, window, cx);
    div()
        .id((ids::LINE, ix))
        .w_full()
        .min_h(dp(m::LINE_H))
        .flex()
        .bg(tokens.secondary_surface)
        .text_mono13()
        .child(left)
        .child(
            div()
                .id((ids::SPLIT_DIVIDER, ix))
                .flex_none()
                .w(dp(m::SPLIT_DIVIDER_W))
                .bg(tokens.secondary_surface)
                .test_support(),
        )
        .child(right)
        .test_support()
        .into_any_element()
}
