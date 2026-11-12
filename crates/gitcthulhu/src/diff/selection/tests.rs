use core::prelude::v1::test;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Entity, FocusHandle,
    InputEvent as _, InteractiveElement as _, IntoElement as _, ListAlignment, ListOffset,
    ListState, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Styled as _, StyledText, TestAppContext, Window, div, list,
    point, px,
};

use super::range::{Edge, source_offset};
use super::{
    DIFF_CONTEXT, DiffSelection, SIDE_LOCK_MOVES, SelKey, SelSide, set_active, take_clipboard,
};
use crate::backend::{CancelToken, DiffContent, DiffTarget, LineKind, RelaPath, Repo};
use crate::diff::model::{DiffDoc, DiffMode, DiffRow, MAX_DISPLAY_BYTES, build_doc};
use crate::menus::MenuHost;
use crate::menus::MenuItem;
use crate::menus::diff_selection::{ids as menu_ids, selection_menu};
use crate::test_support::fixture::Fixture;
use crate::test_support::geometry::{bounds, init_visual, open_probe};
use crate::widgets::ObservedExt as _;

const GUTTER: f32 = 80.;
const W: f32 = 900.;
const H: f32 = 300.;
const TALL: f32 = 700.;

/// The unstaged diff of `path` from `old` to `new`, built into a document in `mode`.
fn fixture_doc(path: &str, old: &str, new: &str, mode: DiffMode) -> Arc<DiffDoc> {
    let fixture = Fixture::new();
    fixture.write(path, old);
    fixture.commit_all("base");
    fixture.write(path, new);
    let repo = Repo::open(&fixture.runtime(), fixture.path()).expect("the fixture opens");
    let cancel = CancelToken::new();
    let target = DiffTarget::Unstaged {
        path: RelaPath::from(path),
        previous_path: None,
    };
    let diff = crate::backend::diff::file_diff(&repo, &target, false, &cancel)
        .expect("the diff can be read");
    assert!(matches!(diff.content, DiffContent::Text(_)), "a text diff");
    Arc::new(build_doc(Arc::new(diff), mode, 1, &cancel))
}

fn source_lines(doc: &DiffDoc) -> Vec<String> {
    let DiffContent::Text(text) = &doc.diff.content else {
        panic!("a text diff");
    };
    text.lines
        .iter()
        .map(|line| text.line_text(line).to_owned())
        .collect()
}

fn kinds(doc: &DiffDoc) -> Vec<LineKind> {
    let DiffContent::Text(text) = &doc.diff.content else {
        panic!("a text diff");
    };
    text.lines.iter().map(|line| line.kind).collect()
}

/// A text cell's half of a row: gutter, 16 dp padding, the cell (like G7a's rows, 09 §1.3).
fn half(
    doc: &DiffDoc,
    selection: &DiffSelection,
    line: Option<u32>,
    side: SelSide,
    window: &mut Window,
    cx: &mut App,
) -> gpui_kit::Div {
    let row = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w_0()
        .child(div().flex_none().w(px(GUTTER)));
    match line {
        Some(line) => row.child(
            div()
                .flex_1()
                .min_w_0()
                .pl(px(16.))
                .child(selection.text_cell(
                    SelKey { line, side },
                    StyledText::new(doc.texts[line as usize].clone()),
                    window,
                    cx,
                )),
        ),
        None => row,
    }
}

fn render_row(
    doc: &DiffDoc,
    selection: &DiffSelection,
    ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match doc.rows[ix] {
        DiffRow::HunkHeader { hunk } => div()
            .w_full()
            .h(px(24.))
            .child(format!("hunk {hunk}"))
            .observed(("row", ix)),
        DiffRow::Line { line } => half(doc, selection, Some(line), SelSide::Unified, window, cx)
            .w_full()
            .observed(("row", ix)),
        DiffRow::Pair { left, right, .. } => div()
            .w_full()
            .flex()
            .flex_row()
            .child(half(doc, selection, left, SelSide::Old, window, cx).observed(("left", ix)))
            .child(div().flex_none().w(px(4.)))
            .child(half(doc, selection, right, SelSide::New, window, cx).observed(("right", ix)))
            .observed(("row", ix)),
    }
}

struct Harness {
    handle: AnyWindowHandle,
    focus: FocusHandle,
    selection: Rc<RefCell<DiffSelection>>,
    list: ListState,
    host: Entity<MenuHost>,
    doc: Arc<DiffDoc>,
}

fn mount(cx: &mut TestAppContext, doc: Arc<DiffDoc>) -> Harness {
    mount_sized(cx, doc, H)
}

fn mount_sized(cx: &mut TestAppContext, doc: Arc<DiffDoc>, height: f32) -> Harness {
    mount_with(cx, doc, height, false)
}

/// `keyed`: the diff root binds the actions with `DiffSelection::key_actions` and is focused by a selection drag
/// (`focus_on_drag`) instead of up front.
fn mount_with(cx: &mut TestAppContext, doc: Arc<DiffDoc>, height: f32, keyed: bool) -> Harness {
    init_visual(cx);
    cx.update(super::register);
    let host = cx.update(MenuHost::install);
    let selection = cx.update(|cx| {
        let mut selection = DiffSelection::new(cx);
        selection.reset(&doc, cx);
        Rc::new(RefCell::new(selection))
    });
    let focus = cx.update(|cx| cx.focus_handle());
    let list_state = ListState::new(doc.rows.len(), ListAlignment::Top, px(200.));
    let (handle, _) = open_probe(cx, W, height, {
        let (selection, state, focus, host, doc) = (
            selection.clone(),
            list_state.clone(),
            focus.clone(),
            host.clone(),
            doc.clone(),
        );
        move |_, _| {
            let (selection, doc) = (selection.clone(), doc.clone());
            let root = div().relative();
            let root = if keyed {
                selection.borrow().key_actions(root)
            } else {
                root
            };
            root.size_full()
                .key_context(DIFF_CONTEXT)
                .track_focus(&focus)
                .font_family(crate::fonts::FONT_MONO)
                .text_size(px(13.))
                .child(
                    list(state.clone(), move |ix, window, cx| {
                        render_row(&doc, &selection.borrow(), ix, window, cx)
                    })
                    .size_full(),
                )
                .child(host.clone())
                .into_any_element()
        }
    });
    if keyed {
        cx.update(|cx| selection.borrow().focus_on_drag(&focus, cx));
    }
    cx.update_window(handle, |_, window, cx| {
        if !keyed {
            window.focus(&focus, cx);
        }
        window.render_frame(cx);
    })
    .expect("window");
    Harness {
        handle,
        focus,
        selection,
        list: list_state,
        host,
        doc,
    }
}

/// The window position just inside character `index` of the painted run `key` (mid-row).
fn char_point(h: &Harness, key: SelKey, index: usize) -> Point<Pixels> {
    let selection = h.selection.borrow();
    let inner = selection.shared.borrow();
    let run = inner
        .frame
        .runs
        .iter()
        .find(|run| run.key == key)
        .unwrap_or_else(|| panic!("{key:?} is painted"));
    let position = run
        .layout
        .position_for_index(index)
        .expect("the index is in the run");
    point(
        position.x + px(1.),
        position.y + run.layout.line_height() / 2.,
    )
}

/// The bounds of the painted run `key`.
fn run_bounds(h: &Harness, key: SelKey) -> Bounds<Pixels> {
    let selection = h.selection.borrow();
    let inner = selection.shared.borrow();
    inner
        .frame
        .runs
        .iter()
        .find(|run| run.key == key)
        .map(|run| run.bounds)
        .unwrap_or_else(|| panic!("{key:?} is painted"))
}

fn line_at(doc: &DiffDoc, row: usize) -> u32 {
    match doc.rows[row] {
        DiffRow::Line { line } => line,
        other => panic!("row {row} is {other:?}"),
    }
}

fn mouse_down(window: &mut Window, cx: &mut App, at: Point<Pixels>, button: MouseButton) {
    window.dispatch_event(
        MouseDownEvent {
            button,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn mouse_move(window: &mut Window, cx: &mut App, at: Point<Pixels>) {
    window.dispatch_event(
        MouseMoveEvent {
            position: at,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn mouse_up(window: &mut Window, cx: &mut App, at: Point<Pixels>, button: MouseButton) {
    window.dispatch_event(
        MouseUpEvent {
            button,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

/// Drags from `from` to `to` in `moves` move events.
fn drag(window: &mut Window, cx: &mut App, from: Point<Pixels>, to: Point<Pixels>, moves: u32) {
    mouse_down(window, cx, from, MouseButton::Left);
    for step in 1..=moves {
        let f = step as f32 / moves as f32;
        mouse_move(
            window,
            cx,
            point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f),
        );
    }
    mouse_up(window, cx, to, MouseButton::Left);
}

fn many_lines(n: usize) -> String {
    (0..n)
        .map(|i| format!("line {i:03} with some text\n"))
        .collect()
}

#[gpui_kit::test]
fn select_across_virtualised_rows_copies_full_lines(cx: &mut TestAppContext) {
    // 300 added lines after one context line: far more rows than the 300 px window shows.
    let new = format!("first\n{}", many_lines(300));
    let doc = fixture_doc("notes.txt", "first\n", &new, DiffMode::Unified);
    let h = mount(cx, doc.clone());
    let lines = source_lines(&doc);
    let first = line_at(&doc, 3);
    let anchor_key = SelKey {
        line: first,
        side: SelSide::Unified,
    };
    cx.update_window(h.handle, |_, window, cx| {
        let from = char_point(&h, anchor_key, 5);
        mouse_down(window, cx, from, MouseButton::Left);
        mouse_move(window, cx, point(from.x + px(30.), from.y + px(20.)));
        // Scroll far down while the button is held: the anchor's row is no longer painted.
        h.list.scroll_to(ListOffset {
            item_ix: 250,
            offset_in_item: px(0.),
        });
        window.render_frame(cx);
        assert!(
            !h.selection.borrow().shared.borrow().frame.runs.iter().any(|run| run.key == anchor_key),
            "the anchor row is virtualised away"
        );
        let last = line_at(&doc, 255);
        let last_key = SelKey {
            line: last,
            side: SelSide::Unified,
        };
        let to = char_point(&h, last_key, 9);
        mouse_move(window, cx, to);
        mouse_up(window, cx, to, MouseButton::Left);

        let selection = h.selection.borrow();
        assert!(selection.has_selection(cx));
        let mut expected = lines[first as usize][5..].to_owned();
        for line in &lines[first as usize + 1..last as usize] {
            expected.push('\n');
            expected.push_str(line);
        }
        expected.push('\n');
        expected.push_str(&lines[last as usize][..9]);
        assert_eq!(
            selection.selected_text(cx).as_deref(),
            Some(expected.as_str()),
            "the selection copies whole source lines in between, including rows never painted together"
        );
        assert_eq!(expected.lines().count(), (last - first + 1) as usize);
    })
    .expect("window");

    // Ctrl+C in the Diff context copies the same text.
    take_clipboard();
    cx.update_window(h.handle, |_, window, cx| {
        window.press("ctrl-c", cx);
    })
    .expect("window");
    let copied = take_clipboard();
    let expected = cx.update(|cx| h.selection.borrow().selected_text(cx));
    assert_eq!(copied, vec![expected.expect("text is selected")]);
}

#[gpui_kit::test]
fn copy_uses_source_not_display_cut(cx: &mut TestAppContext) {
    // Not periodic, so a window of it occurs once in the line.
    let numbers: Vec<String> = (0..2_000).map(|n| n.to_string()).collect();
    let long = format!("\tlet value = [{}];", numbers.join(","));
    let new = format!("start\n{long}\nend\n");
    let doc = fixture_doc("long.rs", "start\nend\n", &new, DiffMode::Unified);
    let lines = source_lines(&doc);
    let long_ix = lines
        .iter()
        .position(|line| *line == long)
        .expect("the long line is in the diff") as u32;
    let display = doc.texts[long_ix as usize].clone();
    assert!(
        display.len() <= MAX_DISPLAY_BYTES + "\u{2026}".len() && display.starts_with("    let"),
        "the display text is tab-expanded and cut"
    );

    // The pure mapping: tabs, the cut end, and an unknown transformation.
    assert_eq!(source_offset(&long, &display, 0, Edge::Start), 0);
    assert_eq!(
        source_offset(&long, &display, 4, Edge::Start),
        1,
        "after the tab"
    );
    assert_eq!(
        source_offset(&long, &display, 1, Edge::Start),
        0,
        "inside the tab, its start"
    );
    assert_eq!(
        source_offset(&long, &display, 3, Edge::End),
        1,
        "inside the tab, its end"
    );
    assert_eq!(
        source_offset(&long, &display, 7, Edge::End),
        4,
        "\"let\" ends at 4"
    );
    assert_eq!(
        source_offset(&long, &display, display.len(), Edge::End),
        long.len(),
        "the visible end of a cut line takes the rest of the source line"
    );
    let windowed = &display[100..];
    assert_eq!(
        source_offset(&long, windowed, 0, Edge::Start),
        0,
        "the visible start of a window cut takes the line from its beginning"
    );
    assert_eq!(
        source_offset(&long, windowed, 10, Edge::Start),
        100 - 3 + 10
    );
    assert_eq!(source_offset(&long, "…unknown", 3, Edge::Start), 0);
    assert_eq!(source_offset(&long, "…unknown", 3, Edge::End), long.len());

    // Through the UI: from the start of the long line to the start of the next one.
    let h = mount_sized(cx, doc, TALL);
    cx.update_window(h.handle, |_, window, cx| {
        let key = SelKey {
            line: long_ix,
            side: SelSide::Unified,
        };
        let next = SelKey {
            line: long_ix + 1,
            side: SelSide::Unified,
        };
        let start = run_bounds(&h, key);
        let from = point(start.left() - px(8.), start.top() + px(4.));
        let to = char_point(&h, next, 0);
        drag(window, cx, from, point(to.x - px(1.), to.y), 3);
        let text = h.selection.borrow().selected_text(cx).expect("selected");
        assert_eq!(
            text,
            format!("{long}\n"),
            "the full source line, tab and all"
        );
        assert!(
            text.len() > MAX_DISPLAY_BYTES * 3,
            "longer than the display cut"
        );
    })
    .expect("window");
}

fn split_doc() -> Arc<DiffDoc> {
    let old = "ctx a\nold 1\nold 2\nold 3\nold 4\nctx b\n";
    let new = "ctx a\nnew 1\nnew 2\nnew 3\nnew 4\nctx b\n";
    fixture_doc("pairs.txt", old, new, DiffMode::Split)
}

/// The rows holding pairs of a removed and an added line, in order.
fn pair_rows(doc: &DiffDoc) -> Vec<(usize, u32, u32)> {
    let kinds = kinds(doc);
    doc.rows
        .iter()
        .enumerate()
        .filter_map(|(ix, row)| match *row {
            DiffRow::Pair {
                left: Some(l),
                right: Some(r),
                ..
            } if kinds[l as usize] == LineKind::Removed => Some((ix, l, r)),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn split_selection_locks_side_after_5_moves(cx: &mut TestAppContext) {
    let doc = split_doc();
    assert_eq!(doc.mode, DiffMode::Split);
    let pairs = pair_rows(&doc);
    assert_eq!(pairs.len(), 4);
    let h = mount(cx, doc);
    let old_key = |line| SelKey {
        line,
        side: SelSide::Old,
    };
    let new_key = |line| SelKey {
        line,
        side: SelSide::New,
    };
    cx.update_window(h.handle, |_, window, cx| {
        let from = char_point(&h, old_key(pairs[0].1), 0);
        let target = run_bounds(&h, new_key(pairs[2].2));
        let to = point(target.left() + px(20.), target.top() + px(4.));

        // More than five moves: the press side (old) stays selectable, the new side is excluded.
        drag(window, cx, from, to, SIDE_LOCK_MOVES + 3);
        let text = h.selection.borrow().selected_text(cx).expect("selected");
        assert!(
            text.starts_with("old 1") && text.contains("old 2") && !text.contains("new"),
            "locked to the old side: {text:?}"
        );
        {
            let selection = h.selection.borrow();
            let inner = selection.shared.borrow();
            assert_eq!(inner.gesture.locked, Some(SelSide::Old));
            assert!(
                inner
                    .painted_selection
                    .run_range(new_key(pairs[0].2), "new 1")
                    .is_none(),
                "the new side paints no selection"
            );
            assert!(
                inner
                    .painted_selection
                    .run_range(old_key(pairs[1].1), "old 2")
                    .is_some()
            );
        }

        // Five moves or fewer: both sides stay selectable, and the release keeps them so.
        drag(window, cx, from, to, 3);
        {
            let selection = h.selection.borrow();
            assert_eq!(selection.shared.borrow().gesture.locked, None);
            let text = selection.selected_text(cx).expect("selected");
            assert!(
                text.contains("old 4")
                    && text.contains("new 2")
                    && text.ends_with(
                        "
ne"
                    ),
                "both sides take part: {text:?}"
            );
        }

        // A press resets the lock and, without a drag, selects nothing.
        drag(window, cx, from, to, SIDE_LOCK_MOVES + 1);
        assert_eq!(
            h.selection.borrow().shared.borrow().gesture.locked,
            Some(SelSide::Old)
        );
        mouse_down(window, cx, to, MouseButton::Left);
        assert_eq!(h.selection.borrow().shared.borrow().gesture.locked, None);
        mouse_up(window, cx, to, MouseButton::Left);
        assert!(!h.selection.borrow().has_selection(cx));

        // Pressing on the new side locks to the new side.
        let from_new = char_point(&h, new_key(pairs[3].2), 0);
        let up = run_bounds(&h, old_key(pairs[1].1));
        drag(
            window,
            cx,
            from_new,
            point(up.left() + px(2.), up.top() + px(2.)),
            SIDE_LOCK_MOVES + 2,
        );
        let text = h.selection.borrow().selected_text(cx).expect("selected");
        assert!(
            text.contains("new 2") && text.contains("new 3") && !text.contains("old"),
            "locked to the new side: {text:?}"
        );
    })
    .expect("window");
}

fn ids_and_labels(items: &[MenuItem]) -> Vec<(String, String, bool, bool)> {
    items
        .iter()
        .map(|item| match item {
            MenuItem::Action {
                id,
                icon,
                label,
                enabled,
                ..
            } => (id.to_string(), label.to_string(), *enabled, icon.is_some()),
            MenuItem::Separator => ("---".into(), String::new(), false, false),
        })
        .collect()
}

#[gpui_kit::test]
fn selection_menu_items(cx: &mut TestAppContext) {
    let noop: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(|_, _| {});
    assert_eq!(
        ids_and_labels(&selection_menu(true, noop.clone(), noop.clone())),
        vec![
            (menu_ids::COPY.into(), "Copy".into(), true, true),
            (
                menu_ids::SELECT_ALL.into(),
                "Select All".into(),
                true,
                false
            ),
        ],
        "09 §4.7: Copy (copy icon) and Select All (no icon); Cut and Paste never apply"
    );
    assert!(!ids_and_labels(&selection_menu(false, noop.clone(), noop))[0].2);

    let doc = fixture_doc("menu.txt", "a\n", "a\nb one\nc two\n", DiffMode::Unified);
    let h = mount(cx, doc.clone());
    let lines = source_lines(&doc);
    cx.update_window(h.handle, |_, window, cx| {
        let b = SelKey {
            line: line_at(&doc, 2),
            side: SelSide::Unified,
        };
        let at = char_point(&h, b, 2);

        // Without a selection, a right-click opens no selection menu (it goes on to the diff-line menu).
        mouse_down(window, cx, at, MouseButton::Right);
        mouse_up(window, cx, at, MouseButton::Right);
        assert!(!h.host.read(cx).is_open());

        drag(window, cx, char_point(&h, b, 0), char_point(&h, b, 3), 2);
        assert_eq!(
            h.selection.borrow().selected_text(cx).as_deref(),
            Some(&lines[b.line as usize][..3])
        );
        mouse_down(window, cx, at, MouseButton::Right);
        mouse_up(window, cx, at, MouseButton::Right);
        assert!(
            h.host.read(cx).is_open(),
            "right-click on a selection opens the menu"
        );
        assert!(
            window
                .try_find(format!("menu-item-{}", menu_ids::COPY))
                .is_some()
        );
        assert!(
            h.selection.borrow().has_selection(cx),
            "the right-click keeps the selection"
        );
        take_clipboard();
        window.click(format!("menu-item-{}", menu_ids::COPY), cx);
        assert_eq!(
            take_clipboard(),
            vec![lines[b.line as usize][..3].to_owned()]
        );
        assert!(!h.host.read(cx).is_open());

        // The press on the menu ended the window selection; select again for Select All.
        drag(window, cx, char_point(&h, b, 0), char_point(&h, b, 3), 2);
        mouse_down(window, cx, at, MouseButton::Right);
        mouse_up(window, cx, at, MouseButton::Right);
        window.click(format!("menu-item-{}", menu_ids::SELECT_ALL), cx);
        assert_eq!(
            h.selection.borrow().selected_text(cx),
            Some(lines.join("\n")),
            "Select All takes the whole document"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn ctrl_a_selects_document(cx: &mut TestAppContext) {
    let new = format!("first\n{}", many_lines(120));
    let doc = fixture_doc("all.txt", "first\nlast\n", &new, DiffMode::Unified);
    let h = mount(cx, doc.clone());
    let lines = source_lines(&doc);
    cx.update_window(h.handle, |_, window, cx| {
        assert!(!h.selection.borrow().has_selection(cx));
        window.press("ctrl-a", cx);
        let selection = h.selection.borrow();
        assert!(selection.has_selection(cx));
        assert_eq!(selection.selected_text(cx), Some(lines.join("\n")));
        let inner = selection.shared.borrow();
        let run = &inner.frame.runs[0];
        assert_eq!(
            inner.painted_selection.run_range(run.key, &run.text),
            (!run.text.is_empty()).then(|| 0..run.text.len()),
            "every painted run shows the selection"
        );
    })
    .expect("window");
    take_clipboard();
    cx.update_window(h.handle, |_, window, cx| window.press("ctrl-c", cx))
        .expect("window");
    assert_eq!(take_clipboard(), vec![lines.join("\n")]);
    // A click elsewhere clears select-all.
    cx.update_window(h.handle, |_, window, cx| {
        let key = SelKey {
            line: line_at(&doc, 2),
            side: SelSide::Unified,
        };
        let at = char_point(&h, key, 1);
        mouse_down(window, cx, at, MouseButton::Left);
        mouse_up(window, cx, at, MouseButton::Left);
        assert!(!h.selection.borrow().has_selection(cx));
    })
    .expect("window");
}

#[gpui_kit::test]
fn double_click_selects_a_word(cx: &mut TestAppContext) {
    let doc = fixture_doc(
        "words.txt",
        "a\n",
        "a\nalpha beta_gamma delta\n",
        DiffMode::Unified,
    );
    let h = mount(cx, doc.clone());
    cx.update_window(h.handle, |_, window, cx| {
        let key = SelKey {
            line: line_at(&doc, 2),
            side: SelSide::Unified,
        };
        let at = char_point(&h, key, 8);
        for count in 1..=2 {
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers: Modifiers::default(),
                    click_count: count,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers: Modifiers::default(),
                    click_count: count,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        }
        assert_eq!(
            h.selection.borrow().selected_text(cx).as_deref(),
            Some("beta_gamma")
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn reset_clears_on_a_new_document_and_keeps_the_same_one(cx: &mut TestAppContext) {
    let doc = fixture_doc("r.txt", "a\n", "a\nb\nc\n", DiffMode::Unified);
    let h = mount(cx, doc.clone());
    cx.update_window(h.handle, |_, window, cx| {
        window.press("ctrl-a", cx);
        assert!(h.selection.borrow().has_selection(cx));
        h.selection.borrow_mut().reset(&doc, cx);
        assert!(
            h.selection.borrow().has_selection(cx),
            "the same document keeps it"
        );
        let other = Arc::new(build_doc(
            doc.diff.clone(),
            DiffMode::Unified,
            2,
            &CancelToken::new(),
        ));
        h.selection.borrow_mut().reset(&other, cx);
        assert!(
            !h.selection.borrow().has_selection(cx),
            "a new document clears it"
        );
        assert!(Arc::ptr_eq(
            h.selection.borrow().doc().expect("a doc"),
            &other
        ));
    })
    .expect("window");
    // A drag selection is cleared too, also in the window.
    cx.update_window(h.handle, |_, window, cx| {
        window.render_frame(cx);
        let key = SelKey {
            line: line_at(&h.doc, 2),
            side: SelSide::Unified,
        };
        drag(
            window,
            cx,
            char_point(&h, key, 0),
            char_point(&h, key, 1),
            2,
        );
        assert!(h.selection.borrow().has_selection(cx));
        h.selection.borrow_mut().clear(cx);
        assert!(!h.selection.borrow().has_selection(cx));
        assert!(gpui_kit::base::TextSelection::selected_text(window, cx).is_empty());
    })
    .expect("window");
}

#[gpui_kit::test]
fn text_cell_is_layout_transparent(cx: &mut TestAppContext) {
    // The cell lays out exactly like the bare StyledText (G7a's row geometry, 09 §1.3, is unchanged), and wraps
    // to the pane width.
    let text = "word ".repeat(400);
    let doc = fixture_doc(
        "wrap.txt",
        "a\n",
        &format!("a\n{text}\n"),
        DiffMode::Unified,
    );
    init_visual(cx);
    let selection = cx.update(|cx| {
        let mut selection = DiffSelection::new(cx);
        selection.reset(&doc, cx);
        Rc::new(selection)
    });
    let line = line_at(&doc, 2);
    let (handle, _) = open_probe(cx, 600., 400., {
        move |window, cx| {
            let display = doc.texts[line as usize].clone();
            div()
                .w(px(600.))
                .font_family(crate::fonts::FONT_MONO)
                .text_size(px(13.))
                .child(
                    div()
                        .w_full()
                        .child(StyledText::new(display.clone()))
                        .observed("bare"),
                )
                .child(
                    div()
                        .w_full()
                        .child(selection.text_cell(
                            SelKey {
                                line,
                                side: SelSide::Unified,
                            },
                            StyledText::new(display),
                            window,
                            cx,
                        ))
                        .observed("cell"),
                )
                .into_any_element()
        }
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let (bare, cell) = (bounds(window, "bare"), bounds(window, "cell"));
        assert_eq!(bare.size, cell.size);
        assert!(cell.size.width <= px(600.));
        assert!(cell.size.height > px(40.), "the long line wraps");
    })
    .expect("window");
}

#[gpui_kit::test]
fn key_actions_act_on_the_focused_diff_and_propagate(cx: &mut TestAppContext) {
    let doc = fixture_doc(
        "keys.txt",
        "a
",
        "a
b one
c two
",
        DiffMode::Unified,
    );
    let h = mount_with(cx, doc.clone(), H, true);
    let lines = source_lines(&doc);
    let other = cx.update(|cx| {
        let mut other = DiffSelection::new(cx);
        other.reset(&doc, cx);
        other
    });
    cx.update_window(h.handle, |_, window, cx| {
        assert!(!h.focus.is_focused(window), "not focused before the drag");
        let key = SelKey {
            line: line_at(&doc, 2),
            side: SelSide::Unified,
        };
        drag(
            window,
            cx,
            char_point(&h, key, 0),
            char_point(&h, key, 3),
            2,
        );
        assert!(
            h.focus.is_focused(window),
            "a selection drag focuses the diff"
        );

        // Another diff selection was painted last: the global fallback would act on it, the bound actions do not.
        set_active(&other.shared);
        window.press("ctrl-a", cx);
        assert_eq!(
            h.selection.borrow().selected_text(cx),
            Some(lines.join(
                "
"
            ))
        );
        assert!(!other.has_selection(cx));
    })
    .expect("window");
    take_clipboard();
    cx.update_window(h.handle, |_, window, cx| window.press("ctrl-c", cx))
        .expect("window");
    assert_eq!(
        take_clipboard(),
        vec![lines.join(
            "
"
        )]
    );

    // Without a selection the keystroke goes on (to gpui-base Root's copy, which has nothing to copy either).
    cx.update_window(h.handle, |_, window, cx| {
        h.selection.borrow_mut().clear(cx);
        window.render_frame(cx);
        window.press("ctrl-c", cx);
    })
    .expect("window");
    assert!(take_clipboard().is_empty());
}
