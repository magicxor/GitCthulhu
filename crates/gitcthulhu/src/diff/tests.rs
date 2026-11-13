use core::prelude::v1::test;

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, Modifiers, TestAppContext, px};

use super::model::{
    self, Anchor, DiffMode, DiffRow, MAX_DISPLAY_BYTES, Uncommitted, build_doc, controls,
    display_line, hunk_title,
};
use super::other::{fit, image_size, too_large_text};
use super::recorder::{self, Recorded};
use super::{DiffView, SelectionOp, ids};
use crate::backend::{
    CancelToken, ChangeKind, DiffContent, DiffTarget, FileDiff, HunkHeader, LineKind, LineRef,
    LineSelection, ObjectId, RelaPath, SizeUnit,
};
use crate::metrics::{self, diff as m};
use crate::settings::{self, DiffViewMode};
use crate::tab::ops::files::SelectionUnit;
use crate::tab::ui::{FileList, Selection};
use crate::tab::{RepoTab, RepoTabEvent};
use crate::test_support::geometry::bounds;
use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture};

// ───────────────────────────── helpers ─────────────────────────────

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// `src/lib.rs` with 40 committed lines; lines 5 and 30 changed in the worktree (two hunks).
fn modified_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.write("src/lib.rs", &numbered(40));
    fx.commit_all("init");
    let text: String = (1..=40)
        .map(|i| match i {
            5 => "line five changed\n".to_owned(),
            30 => "line thirty changed\n".to_owned(),
            _ => format!("line {i}\n"),
        })
        .collect();
    fx.write("src/lib.rs", &text);
    fx
}

fn view(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Entity<DiffView> {
    cx.update(|cx| tab.read(cx).views().diff.clone())
}

fn select(cx: &mut TestAppContext, tab: &Entity<RepoTab>, list: FileList, path: &str) {
    cx.update(|cx| {
        let ui = tab.read(cx).ctx().ui.clone();
        ui.update(cx, |ui, cx| {
            ui.click_file(list, RelaPath::from(path), Modifiers::default(), &[], cx)
        });
    });
    cx.run_until_parked();
}

fn draw(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.simulate_next_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
}

fn doc(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Arc<super::model::DiffDoc> {
    let view = view(tab, cx);
    cx.update(|cx| view.read(cx).doc().cloned().expect("a document is shown"))
}

fn set_settings(cx: &mut TestAppContext, edit: impl FnOnce(&mut settings::Settings)) {
    cx.update(|cx| settings::update(cx, edit));
    cx.run_until_parked();
}

fn head(fx: &Fixture) -> ObjectId {
    ObjectId::from_hex(fx.git(&["rev-parse", "HEAD"]).trim().as_bytes()).expect("hex id")
}

fn row_of_line(doc: &super::model::DiffDoc, new_lineno: u32) -> usize {
    let text = doc.text().expect("text");
    doc.rows
        .iter()
        .position(|r| {
            let lines = match *r {
                DiffRow::Line { line } => vec![line],
                DiffRow::Pair { left, right, .. } => left.into_iter().chain(right).collect(),
                DiffRow::HunkHeader { .. } => vec![],
            };
            lines
                .iter()
                .any(|l| text.lines[*l as usize].new_lineno == Some(new_lineno))
        })
        .expect("row of the line")
}

fn file_diff(fx: &Fixture, target: DiffTarget) -> FileDiff {
    let rt = fx.runtime();
    let undo = crate::backend::handle::UndoSettings {
        enabled: false,
        ..crate::backend::handle::UndoSettings::default()
    };
    let handle = crate::backend::handle::open_repo(&rt, fx.path(), &undo).expect("open");
    crate::backend::diff::file_diff(&handle.repo, &target, false, &CancelToken::new())
        .expect("diff")
}

fn unstaged(path: &str) -> DiffTarget {
    DiffTarget::Unstaged {
        path: RelaPath::from(path),
        previous_path: None,
    }
}

fn mono_advance(cx: &mut TestAppContext, handle: AnyWindowHandle) -> f32 {
    cx.update_window(handle, |_, window, _| {
        let ts = window.text_system();
        let id = ts.resolve_font(&gpui_kit::font(crate::fonts::FONT_MONO));
        ts.advance(id, metrics::dpx(13., window), '0')
            .expect("advance")
            .width
            .as_f32()
    })
    .expect("window")
}

// ───────────────────────────── model ─────────────────────────────

#[test]
fn display_line_expands_tabs_and_maps_spans() {
    let (text, spans) = display_line("\tab\tcd", &[1..3, 4..6]);
    assert_eq!(text.as_ref(), "    ab    cd");
    assert_eq!(
        spans.as_slice(),
        &[4..6, 10..12],
        "spans follow the tab expansion"
    );
    let (_, whole) = display_line("abc", &[Range { start: 0, end: 3 }]);
    assert!(
        whole.is_empty(),
        "a whole-line span is not highlighted (09 §1.3)"
    );
}

#[test]
fn display_line_cuts_long_lines_around_the_change() {
    let source = "x".repeat(5_000) + "CHANGE" + &"y".repeat(5_000);
    let (text, spans) = display_line(
        &source,
        &[Range {
            start: 5_000,
            end: 5_006,
        }],
    );
    assert!(text.len() <= MAX_DISPLAY_BYTES + 2 * "\u{2026}".len());
    assert!(text.starts_with('\u{2026}') && text.ends_with('\u{2026}'));
    let span = spans.first().expect("span kept").clone();
    assert_eq!(&text[span.start as usize..span.end as usize], "CHANGE");
}

#[test]
fn hunk_titles_follow_gits_count_rules() {
    let h = |a, b, c, d| HunkHeader {
        old_start: a,
        old_lines: b,
        new_start: c,
        new_lines: d,
    };
    assert_eq!(hunk_title(&h(12, 7, 12, 9)), "@@ -12,7 +12,9 @@");
    assert_eq!(hunk_title(&h(3, 1, 3, 1)), "@@ -3 +3 @@");
    assert_eq!(hunk_title(&h(0, 0, 1, 4)), "@@ -0,0 +1,4 @@");
    assert_eq!(model::digits(0), 1);
    assert_eq!(model::digits(9), 1);
    assert_eq!(model::digits(10), 2);
    assert_eq!(model::digits(12_345), 5);
}

#[test]
fn fit_scales_down_never_up() {
    assert_eq!(
        fit((10., 10.), (300., 200.)),
        (10., 10.),
        "small images keep their size"
    );
    assert_eq!(fit((4000., 3000.), (400., 600.)), (400., 300.));
    assert_eq!(fit((1000., 2000.), (400., 500.)), (250., 500.));
    assert_eq!(
        too_large_text(52, SizeUnit::Lines),
        "File too large to display (over 52 lines)"
    );
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b.extend_from_slice(b"\0\0\0\0IEND\xaeB`\x82");
    b
}

#[test]
fn image_sizes_from_headers() {
    assert_eq!(image_size(&png(640, 480)), Some((640, 480)));
    assert_eq!(image_size(b"GIF89a\x20\x00\x10\x00rest"), Some((32, 16)));
    let jpeg = [
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x30,
        0x00, 0x40, 0x03,
    ];
    assert_eq!(image_size(&jpeg), Some((64, 48)));
    assert_eq!(image_size(b"not an image"), None);
}

#[test]
fn split_pairing_and_hunk_buttons_visibility_matrix() {
    let fx = Fixture::new();
    fx.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n");
    fx.commit_all("init");
    // Three removed lines replaced by one added line.
    fx.write("a.txt", "1\n2\nnew\n6\n7\n8\n");
    let diff = Arc::new(file_diff(&fx, unstaged("a.txt")));
    let doc = build_doc(diff.clone(), DiffMode::Split, 1, &CancelToken::new());
    let pairs: Vec<(bool, bool)> = doc
        .rows
        .iter()
        .filter_map(|r| match r {
            DiffRow::Pair { left, right, .. } => Some((left.is_some(), right.is_some())),
            _ => None,
        })
        .collect();
    assert!(
        pairs
            .windows(3)
            .any(|w| w == [(true, true), (true, false), (true, false)]),
        "the shorter side is padded with fillers: {pairs:?}"
    );
    let unified = build_doc(diff.clone(), DiffMode::Unified, 1, &CancelToken::new());
    let text = unified.text().expect("text");
    assert_eq!(unified.rows.len(), text.hunks.len() + text.lines.len());

    // 09 §1.3 / §4.6 visibility matrix.
    let c = controls(&diff, true);
    assert!(c.hunk_buttons && c.line_chip && c.discard_line);
    assert_eq!(c.list, Some(Uncommitted::Unstaged));
    let c = controls(&diff, false);
    assert!(
        !c.hunk_buttons && c.line_chip,
        "not SAFE: only hunk buttons hide"
    );
    fx.git(&["add", "a.txt"]);
    let staged = file_diff(
        &fx,
        DiffTarget::Staged {
            path: RelaPath::from("a.txt"),
            previous_path: None,
        },
    );
    let c = controls(&staged, true);
    assert!(c.hunk_buttons && c.line_chip && !c.discard_line);
    fx.write("new.txt", "x\n");
    let untracked = file_diff(&fx, unstaged("new.txt"));
    assert_eq!(
        controls(&untracked, true),
        model::Controls {
            list: Some(Uncommitted::Unstaged),
            ..Default::default()
        }
    );
    assert_eq!(
        build_doc(Arc::new(untracked), DiffMode::Split, 1, &CancelToken::new()).mode,
        DiffMode::Unified,
        "split is not offered for added files"
    );
    fx.commit_all("second");
    let commit = file_diff(
        &fx,
        DiffTarget::Commit {
            commit: head(&fx),
            against: crate::backend::Against::FirstParent,
            path: RelaPath::from("a.txt"),
            previous_path: None,
        },
    );
    assert_eq!(controls(&commit, true), model::Controls::default());
}

#[test]
fn anchors_survive_mode_switches() {
    let fx = modified_fixture();
    let diff = Arc::new(file_diff(&fx, unstaged("src/lib.rs")));
    let unified = build_doc(diff, DiffMode::Unified, 1, &CancelToken::new());
    let split = model::rebuild_mode(&unified, DiffMode::Split, 2);
    let row = row_of_line(&unified, 31);
    let anchor = unified.anchor(row).expect("anchor");
    assert_eq!(
        anchor,
        Anchor {
            old: Some(31),
            new: Some(31),
            header: false
        }
    );
    let split_row = split.find_anchor(&anchor).expect("found");
    assert_eq!(split_row, row_of_line(&split, 31));
    let header = unified.anchor(0).expect("header anchor");
    assert!(header.header);
    assert_eq!(split.find_anchor(&header), Some(0));
}

// ───────────────────────────── layout (21 §8.2) ─────────────────────────────

#[gpui_kit::test]
fn header_layout_and_toggles(cx: &mut TestAppContext) {
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let pane = bounds(window, crate::tab::ids::CENTER_DIFF);
        let header = bounds(window, ids::HEADER);
        assert_eq!(
            header.origin, pane.origin,
            "header at the top of the diff pane"
        );
        assert!(
            (header.size.height.as_f32() - 34.).abs() < 0.5,
            "header 34 (09 §1.2)"
        );
        assert!((header.size.width - pane.size.width).abs() < px(0.5));
        for id in [
            ids::TOGGLE_HUNKS,
            ids::TOGGLE_FULL_FILE,
            ids::TOGGLE_UNIFIED,
            ids::TOGGLE_SPLIT,
        ] {
            let b = bounds(window, id);
            assert!((b.size.width.as_f32() - 32.).abs() < 0.5, "{id} width 32");
            assert!((b.size.height.as_f32() - 32.).abs() < 0.5, "{id} height 32");
            assert!(
                ((b.top() - header.top()).as_f32() - 1.).abs() < 0.5,
                "{id} centred in the 34 dp header"
            );
        }
        let hunks = bounds(window, ids::TOGGLE_HUNKS);
        let full = bounds(window, ids::TOGGLE_FULL_FILE);
        let unified = bounds(window, ids::TOGGLE_UNIFIED);
        let split = bounds(window, ids::TOGGLE_SPLIT);
        assert!(
            (full.left() - hunks.right()).abs() < px(0.5),
            "a pair touches"
        );
        assert!(
            (split.left() - unified.right()).abs() < px(0.5),
            "a pair touches"
        );
        assert!(
            (unified.left() - full.right() - px(16.)).abs() < px(0.5),
            "16 between the pairs"
        );
        let stage = bounds(window, ids::STAGE_FILE);
        assert!(
            (stage.left() - split.right() - px(16.)).abs() < px(0.5),
            "16 between toggles and Stage file"
        );
        let close = bounds(window, ids::CLOSE);
        assert!(
            (close.left() - stage.right() - px(16.)).abs() < px(0.5),
            "16 between Stage file and close"
        );
        assert!(
            (header.right() - close.right() - px(8.)).abs() < px(0.5),
            "padding end 8"
        );
        assert!(
            (close.size.width.as_f32() - 48.).abs() < 0.5,
            "IconButton 48 wide"
        );
        assert!(
            (stage.size.height.as_f32() - 24.).abs() < 1.,
            "SecondaryButton ≈ 24 high"
        );
        let path = bounds(window, ids::HEADER_PATH);
        assert!(
            ((path.left() - header.left()).as_f32() - 16.).abs() < 0.5,
            "padding start 16"
        );
        // Lines start right below the header (no padding, 09 §1.3).
        let first = bounds(window, (ids::HUNK, 0usize));
        assert!((first.top() - header.bottom()).abs() < px(0.5));
    })
    .expect("window");

    // Split toggle: global setting, rows rebuilt without reload.
    cx.update_window(handle, |_, window, cx| window.click(ids::TOGGLE_SPLIT, cx))
        .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| settings::get(cx).diff_view),
        DiffViewMode::Split
    );
    assert_eq!(doc(&tab, cx).mode, DiffMode::Split);
    set_settings(cx, |s| s.diff_view = DiffViewMode::Unified);
    assert_eq!(doc(&tab, cx).mode, DiffMode::Unified);

    // A commit diff has no Stage file button.
    fx.commit_all("change");
    cx.update(|cx| {
        let ui = tab.read(cx).ctx().ui.clone();
        ui.update(cx, |ui, cx| {
            ui.select(
                Selection::Commit {
                    id: head(&fx),
                    is_stash: false,
                    scroll_to: false,
                },
                cx,
            )
        });
    });
    select(cx, &tab, FileList::CommitFiles, "src/lib.rs");
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::HEADER).is_some());
        assert!(
            window.try_find(ids::STAGE_FILE).is_none(),
            "no stage button for commits"
        );
        assert!(window.try_find((ids::HUNK_STAGE, 0usize)).is_none());
    })
    .expect("window");
}

#[gpui_kit::test]
fn diff_replaces_log_and_shares_with_show_diff_with_log(cx: &mut TestAppContext) {
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(crate::tab::ids::CENTER_LOG).is_none(),
            "diff replaces the log"
        );
        let diff = bounds(window, ids::DIFF_VIEW);
        let pane = bounds(window, crate::tab::ids::CENTER_DIFF);
        assert_eq!(diff, pane);
    })
    .expect("window");
    set_settings(cx, |s| s.show_diff_with_log = true);
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let log = bounds(window, crate::tab::ids::CENTER_LOG);
        let diff = bounds(window, ids::DIFF_VIEW);
        assert!((log.size.height - diff.size.height).abs() < px(1.), "50/50");
        assert!(
            (diff.top() - log.bottom()).abs() < px(0.5),
            "log on top, no divider"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn rows_unified_and_split(cx: &mut TestAppContext) {
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    draw(cx, handle);
    let d = doc(&tab, cx);
    let text = d.text().expect("text").clone();
    assert_eq!(text.hunks.len(), 2, "two separate changes = two hunks");
    let removed_row = d
        .rows
        .iter()
        .position(|r| matches!(r, DiffRow::Line { line } if text.lines[*line as usize].old_lineno == Some(5) && text.lines[*line as usize].new_lineno.is_none()))
        .expect("removed line row");
    cx.update_window(handle, |_, window, _| {
        let row = bounds(window, (ids::LINE, removed_row));
        let list = bounds(window, ids::LIST);
        assert!(
            (row.size.width - list.size.width).abs() < px(0.5),
            "rows are full width"
        );
        assert!(
            (row.size.height.as_f32() - m::LINE_H).abs() < 1.0,
            "one text line"
        );
        assert!(window.try_find((ids::NUM_OLD, removed_row)).is_some());
        assert!(window.try_find((ids::TEXT, removed_row)).is_some());
        let header = bounds(window, (ids::HUNK, 0usize));
        assert!(
            (header.size.height.as_f32() - 32.).abs() < 1.,
            "hunk header with buttons ≈ 4 + 24 + 4 (09 §1.3): {}",
            header.size.height.as_f32()
        );
        let title_left = header.left();
        let first_line = bounds(window, (ids::LINE, 1usize));
        assert!(
            (first_line.top() - header.bottom()).abs() < px(0.5),
            "lines sit directly below the header"
        );
        assert!((first_line.left() - title_left).abs() < px(0.5));
        let text = bounds(window, (ids::TEXT, removed_row));
        let new_num = bounds(window, (ids::NUM_NEW, removed_row));
        assert!(
            (text.left() - new_num.right()).abs() < px(0.5),
            "text cell right after the new-number gutter"
        );
    })
    .expect("window");
    set_settings(cx, |s| s.diff_view = DiffViewMode::Split);
    draw(cx, handle);
    let d = doc(&tab, cx);
    assert_eq!(d.mode, DiffMode::Split);
    let pair = d
        .rows
        .iter()
        .position(|r| matches!(r, DiffRow::Pair { left: Some(l), right: Some(r), .. } if l != r))
        .expect("a removed/added pair");
    cx.update_window(handle, |_, window, _| {
        let old = bounds(window, (ids::TEXT_OLD, pair));
        let new = bounds(window, (ids::TEXT_NEW, pair));
        let divider = bounds(window, (ids::SPLIT_DIVIDER, pair));
        assert!(
            (divider.size.width.as_f32() - 4.).abs() < 0.5,
            "4 dp divider"
        );
        assert!(old.right() <= divider.left() + px(0.5) && divider.right() <= new.left() + px(0.5));
        assert!(window.try_find((ids::NUM_OLD, pair)).is_some());
        assert!(window.try_find((ids::NUM_NEW, pair)).is_some());
    })
    .expect("window");
}

#[gpui_kit::test]
fn split_pairs_filler_equal_heights(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "1\n2\n3\n4\n5\n6\n");
    fx.commit_all("init");
    let long = "w ".repeat(400);
    fx.write("a.txt", &format!("1\n{long}\n5\n6\n"));
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    set_settings(cx, |s| s.diff_view = DiffViewMode::Split);
    select(cx, &tab, FileList::Unstaged, "a.txt");
    draw(cx, handle);
    let d = doc(&tab, cx);
    let wrapped = d
        .rows
        .iter()
        .position(|r| matches!(r, DiffRow::Pair { left: Some(l), right: Some(r), .. } if l != r))
        .expect("the wrapped pair");
    let filler = d
        .rows
        .iter()
        .position(|r| matches!(r, DiffRow::Pair { right: None, .. }))
        .expect("a filler row");
    cx.update_window(handle, |_, window, _| {
        let row = bounds(window, (ids::LINE, wrapped));
        let old = bounds(window, (ids::SIDE_OLD, wrapped));
        let new = bounds(window, (ids::TEXT_NEW, wrapped));
        assert!(
            new.size.height.as_f32() > 2. * m::LINE_H,
            "the long line wraps"
        );
        assert!(
            (old.size.height - row.size.height).abs() < px(0.5),
            "both halves share the row height"
        );
        let filler_bounds = bounds(window, (ids::FILLER_NEW, filler));
        let filler_row = bounds(window, (ids::LINE, filler));
        assert!((filler_bounds.size.height - filler_row.size.height).abs() < px(0.5));
    })
    .expect("window");
}

#[gpui_kit::test]
fn gutter_width_per_hunk(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", &numbered(150));
    fx.commit_all("init");
    let text: String = (1..=150)
        .map(|i| match i {
            2 | 140 => format!("changed {i}\n"),
            _ => format!("line {i}\n"),
        })
        .collect();
    fx.write("a.txt", &text);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "a.txt");
    draw(cx, handle);
    let d = doc(&tab, cx);
    assert_eq!(d.gutter_digits.as_slice(), &[1, 3], "digits per hunk");
    let advance = mono_advance(cx, handle);
    let second_header = d
        .rows
        .iter()
        .position(|r| matches!(r, DiffRow::HunkHeader { hunk: 1 }))
        .expect("second hunk");
    let view = view(&tab, cx);
    cx.update(|cx| view.update(cx, |v, cx| v.scroll_to_row(second_header, cx)));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let one = bounds(window, (ids::NUM_OLD, second_header + 1));
        let expected = 12. + 3. * advance;
        assert!(
            (one.size.width.as_f32() - expected).abs() < 0.5,
            "3-digit gutter {} vs {expected}",
            one.size.width.as_f32()
        );
    })
    .expect("window");
    cx.update(|cx| view.update(cx, |v, cx| v.scroll_to_row(0, cx)));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let one = bounds(window, (ids::NUM_NEW, 1usize));
        let expected = 12. + advance;
        assert!(
            (one.size.width.as_f32() - expected).abs() < 0.5,
            "1-digit gutter"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn long_line_wraps_to_pane_width(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "short\n");
    fx.commit_all("init");
    fx.write("a.txt", &format!("short\n{}\n", "x".repeat(600)));
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "a.txt");
    draw(cx, handle);
    let d = doc(&tab, cx);
    let row = row_of_line(&d, 2);
    cx.update_window(handle, |_, window, _| {
        let list = bounds(window, ids::LIST);
        let line = bounds(window, (ids::LINE, row));
        let text = bounds(window, (ids::TEXT, row));
        assert!(
            (line.size.width - list.size.width).abs() < px(0.5),
            "not wider than the pane"
        );
        assert!(text.right() <= list.right() + px(0.5));
        assert!(
            line.size.height.as_f32() > 3. * m::LINE_H,
            "wrapped onto several lines"
        );
        let number = bounds(window, (ids::NUM_NEW, row));
        assert!(
            (number.top() - line.top()).abs() < px(0.5),
            "numbers align to the top"
        );
    })
    .expect("window");
}

// ───────────────────────────── behaviour ─────────────────────────────

#[gpui_kit::test]
fn hunk_buttons_visibility_and_requests(cx: &mut TestAppContext) {
    recorder::take();
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    draw(cx, handle);
    let d = doc(&tab, cx);
    cx.update_window(handle, |_, window, cx| {
        let discard = bounds(window, (ids::HUNK_DISCARD, 0usize));
        let stage = bounds(window, (ids::HUNK_STAGE, 0usize));
        let header = bounds(window, (ids::HUNK, 0usize));
        assert!(
            (stage.left() - discard.right() - px(32.)).abs() < px(0.5),
            "32 between the buttons"
        );
        assert!(
            (header.right() - stage.right() - px(24.)).abs() < px(0.5),
            "the last button ends 24 from the right edge"
        );
        assert!(window.try_find((ids::HUNK_UNSTAGE, 0usize)).is_none());
        window.click((ids::HUNK_STAGE, 0usize), cx);
    })
    .expect("window");
    cx.run_until_parked();
    let calls = recorder::take();
    let header = d.text().expect("text").hunks[0].header;
    match calls.as_slice() {
        [Recorded::Selection(SelectionOp::Stage, req, SelectionUnit::Hunk)] => {
            assert!(matches!(&req.selection, LineSelection::Hunks(h) if h == &vec![header]));
            assert_eq!(req.base_old, d.diff.old.map(|s| s.id));
            assert_eq!(req.base_new, d.diff.new.map(|s| s.id));
            assert_eq!(req.algorithm, None);
        }
        other => panic!("unexpected calls {other:?}"),
    }

    // While a merge-like operation is in progress the hunk buttons hide (09 §1.3); chips stay.
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.clone();
        model.update(cx, |m, cx| {
            let mut state = (**m.state.value.as_ref().expect("state")).clone();
            state.in_progress = Some(crate::backend::InProgress::Merge);
            m.state.value = Some(Arc::new(state));
            cx.notify();
        });
    });
    draw(cx, handle);
    let added = row_of_line(&d, 5);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find((ids::HUNK_STAGE, 0usize)).is_none());
        assert!(
            window.try_find((ids::HUNK, 0usize)).is_some(),
            "the header row stays"
        );
        assert!(
            window.try_find((ids::CHIP, added)).is_some(),
            "chips are not gated by SAFE"
        );
    })
    .expect("window");

    // Staged diff: Unstage hunk only.
    fx.git(&["add", "src/lib.rs"]);
    cx.update(|cx| {
        tab.update(cx, |t, cx| {
            t.reload(
                crate::backend::RefreshSet::STATUS | crate::backend::RefreshSet::REPO_STATE,
                cx,
            )
        })
    });
    cx.run_until_parked();
    select(cx, &tab, FileList::Staged, "src/lib.rs");
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find((ids::HUNK_UNSTAGE, 0usize)).is_some());
        assert!(window.try_find((ids::HUNK_STAGE, 0usize)).is_none());
        assert!(window.try_find((ids::HUNK_DISCARD, 0usize)).is_none());
        assert!(window.find(ids::STAGE_FILE).bounds().size.width > px(0.));
    })
    .expect("window");
}

#[gpui_kit::test]
fn line_chip_hover_and_click_builds_lineref(cx: &mut TestAppContext) {
    recorder::take();
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    draw(cx, handle);
    let d = doc(&tab, cx);
    let text = d.text().expect("text").clone();
    let added = row_of_line(&d, 5);
    let removed = d
        .rows
        .iter()
        .position(|r| matches!(r, DiffRow::Line { line } if text.lines[*line as usize].old_lineno == Some(5) && text.lines[*line as usize].new_lineno.is_none()))
        .expect("removed");
    let context = row_of_line(&d, 4);
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window.try_find((ids::CHIP, context)).is_none(),
            "no chip on context lines"
        );
        let chip = bounds(window, (ids::CHIP, added));
        let text_cell = bounds(window, (ids::TEXT, added));
        assert_eq!(
            chip.origin, text_cell.origin,
            "chip at the text cell's top-left"
        );
        assert!((chip.size.width.as_f32() - 16.).abs() < 0.5);
        assert!((chip.size.height.as_f32() - 16.).abs() < 0.5);
        window.hover((ids::TEXT, added), cx);
        window.render_frame(cx);
        window.click((ids::CHIP, added), cx);
        window.hover((ids::TEXT, removed), cx);
        window.render_frame(cx);
        window.click((ids::CHIP, removed), cx);
    })
    .expect("window");
    cx.run_until_parked();
    let refs: Vec<LineRef> = recorder::take()
        .into_iter()
        .filter_map(|c| match c {
            Recorded::Selection(SelectionOp::Stage, req, SelectionUnit::Line) => {
                match req.selection {
                    LineSelection::Lines(lines) => Some(lines),
                    LineSelection::Hunks(_) => None,
                }
            }
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        refs,
        vec![
            LineRef::Added { new_lineno: 5 },
            LineRef::Removed { old_lineno: 5 }
        ]
    );
}

#[gpui_kit::test]
fn discard_line_menu_conditions(cx: &mut TestAppContext) {
    recorder::take();
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    draw(cx, handle);
    let d = doc(&tab, cx);
    let added = row_of_line(&d, 5);
    let context = row_of_line(&d, 4);
    let item = gpui_kit::SharedString::from(format!(
        "menu-item-{}",
        crate::menus::diff_line::DISCARD_LINE
    ));
    cx.update_window(handle, |_, window, cx| {
        window.right_click((ids::LINE, context), cx);
        window.render_frame(cx);
        assert!(
            window.try_find(item.clone()).is_none(),
            "no menu on context lines"
        );
        window.right_click((ids::LINE, added), cx);
        window.render_frame(cx);
        window.click(item.clone(), cx);
    })
    .expect("window");
    cx.run_until_parked();
    match recorder::take().as_slice() {
        [Recorded::Selection(SelectionOp::Discard, req, SelectionUnit::Line)] => assert!(
            matches!(&req.selection, LineSelection::Lines(l) if l == &vec![LineRef::Added { new_lineno: 5 }])
        ),
        other => panic!("unexpected calls {other:?}"),
    }
    // Staged diffs have no line menu.
    fx.git(&["add", "src/lib.rs"]);
    cx.update(|cx| tab.update(cx, |t, cx| t.reload(crate::backend::RefreshSet::STATUS, cx)));
    cx.run_until_parked();
    select(cx, &tab, FileList::Staged, "src/lib.rs");
    draw(cx, handle);
    let d = doc(&tab, cx);
    let added = row_of_line(&d, 5);
    cx.update_window(handle, |_, window, cx| {
        crate::menus::close_menu(cx);
        window.render_frame(cx);
        window.right_click((ids::LINE, added), cx);
        window.render_frame(cx);
        assert!(
            window.try_find(item.clone()).is_none(),
            "no Discard line when staged"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn stage_file_button_stages_the_file(cx: &mut TestAppContext) {
    recorder::take();
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    draw(cx, handle);
    cx.update_window(handle, |_, window, cx| window.click(ids::STAGE_FILE, cx))
        .expect("window");
    cx.run_until_parked();
    assert!(
        matches!(recorder::take().as_slice(), [Recorded::File(true, p)] if p == &vec![RelaPath::from("src/lib.rs")])
    );
    let staged = cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        let status = model.status.value.clone().expect("status");
        status.staged.iter().any(|e| e.path == "src/lib.rs")
    });
    assert!(staged, "the real stage_paths ran");
    // The unstaged entry vanished: the selection was pruned and the diff closed.
    let view = view(&tab, cx);
    let shown = cx.update(|cx| view.read(cx).doc().is_some());
    assert!(
        !shown,
        "the diff closes when its file has no unstaged changes left"
    );
}

#[gpui_kit::test]
fn full_file_single_hunk(cx: &mut TestAppContext) {
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    draw(cx, handle);
    assert_eq!(doc(&tab, cx).text().expect("text").hunks.len(), 2);
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::TOGGLE_FULL_FILE, cx)
    })
    .expect("window");
    cx.run_until_parked();
    let d = doc(&tab, cx);
    let text = d.text().expect("text");
    assert_eq!(text.hunks.len(), 1, "one hunk spans the file (09 §1.5)");
    assert_eq!(
        text.lines.iter().filter(|l| l.new_lineno.is_some()).count(),
        40
    );
    assert!(cx.update(|cx| settings::get(cx).diff_full_file));
}

#[gpui_kit::test]
fn scroll_resets_on_file_change(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "a\n");
    fx.write("b.txt", "b\n");
    fx.commit_all("init");
    fx.write("a.txt", &numbered(300));
    fx.write("b.txt", &numbered(300));
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "a.txt");
    draw(cx, handle);
    let view = view(&tab, cx);
    cx.update(|cx| view.update(cx, |v, cx| v.scroll_to_row(150, cx)));
    draw(cx, handle);
    assert_eq!(
        cx.update(|cx| view.read(cx).list_state().logical_scroll_top().item_ix),
        150
    );
    select(cx, &tab, FileList::Unstaged, "b.txt");
    draw(cx, handle);
    let top = cx.update(|cx| view.read(cx).list_state().logical_scroll_top());
    assert_eq!(
        top.item_ix, 0,
        "another file starts at the top (09 §1.1, 7b10bc66)"
    );
    assert_eq!(doc(&tab, cx).diff.path, "b.txt");
}

#[gpui_kit::test]
fn mode_switch_keeps_row_index(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", &numbered(200));
    fx.commit_all("init");
    let text: String = (1..=200)
        .map(|i| {
            if i % 3 == 0 {
                format!("changed {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    fx.write("a.txt", &text);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "a.txt");
    draw(cx, handle);
    let unified = doc(&tab, cx);
    let row = row_of_line(&unified, 100);
    let view = view(&tab, cx);
    cx.update(|cx| view.update(cx, |v, cx| v.scroll_to_row(row, cx)));
    draw(cx, handle);
    set_settings(cx, |s| s.diff_view = DiffViewMode::Split);
    draw(cx, handle);
    let split = doc(&tab, cx);
    assert_eq!(split.mode, DiffMode::Split);
    let top = cx.update(|cx| view.read(cx).list_state().logical_scroll_top().item_ix);
    assert_eq!(
        top,
        row_of_line(&split, 100),
        "the same line stays at the top"
    );
    assert!(
        top < row,
        "split rows are fewer, so the index moved with the content"
    );
}

#[gpui_kit::test]
fn jump_to_row_25000_shows_it(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("big.txt", &numbered(50_000));
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "big.txt");
    draw(cx, handle);
    let d = doc(&tab, cx);
    assert_eq!(d.rows.len(), 50_001, "one hunk header + 50k added lines");
    let view = view(&tab, cx);
    cx.update(|cx| view.update(cx, |v, cx| v.scroll_to_row(25_000, cx)));
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let row = bounds(window, (ids::LINE, 25_000usize));
        let list = bounds(window, ids::LIST);
        assert!(
            (row.top() - list.top()).abs() < px(0.5),
            "row 25000 is at the top"
        );
        assert!(window.try_find((ids::LINE, 12usize)).is_none());
    })
    .expect("window");
}

#[gpui_kit::test]
fn keyboard_navigation_scrolls(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("big.txt", &numbered(2_000));
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "big.txt");
    draw(cx, handle);
    let view = view(&tab, cx);
    // Another closeable view opened after the diff.
    let ui = cx.update(|cx| tab.read(cx).ctx().ui.clone());
    cx.update(|cx| {
        ui.update(cx, |ui, cx| {
            ui.push_view(crate::tab::ui::CloseableView::LogSearch, cx)
        })
    });
    cx.update_window(handle, |_, window, _| window.activate_window())
        .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        // A click into the diff focuses it (key context `Diff`).
        window.click((ids::TEXT, 3usize), cx);
        window.render_frame(cx);
        assert!(view.read(cx).focus.contains_focused(window, cx));
    })
    .expect("window");
    cx.run_until_parked();
    draw(cx, handle);
    assert_eq!(
        cx.update(|cx| ui.read(cx).views.as_slice().last().copied()),
        Some(crate::tab::ui::CloseableView::Diff),
        "focusing the diff makes it the view Esc closes first (09 §1.1)"
    );
    cx.update_window(handle, |_, window, cx| {
        window.press("pagedown", cx);
        window.render_frame(cx);
    })
    .expect("window");
    let after_page = cx.update(|cx| view.read(cx).list_state().logical_scroll_top().item_ix);
    assert!(after_page > 10, "page down moved {after_page} rows");
    cx.update_window(handle, |_, window, cx| {
        window.press("home", cx);
        window.render_frame(cx);
    })
    .expect("window");
    assert_eq!(
        cx.update(|cx| view.read(cx).list_state().logical_scroll_top().item_ix),
        0
    );
    cx.update_window(handle, |_, window, cx| {
        window.press("down", cx);
        window.render_frame(cx);
    })
    .expect("window");
    let top = cx.update(|cx| view.read(cx).list_state().logical_scroll_top());
    assert!(
        top.item_ix > 0 || top.offset_in_item > px(0.),
        "down scrolls one line"
    );
}

#[gpui_kit::test]
fn reload_on_status_event_keeps_scroll_if_unchanged(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", &numbered(300));
    fx.commit_all("init");
    let edit = |extra: &str| -> String {
        (1..=300)
            .map(|i| match i {
                2 => format!("changed 2{extra}\n"),
                i if i % 10 == 0 => format!("changed {i}\n"),
                _ => format!("line {i}\n"),
            })
            .collect()
    };
    fx.write("a.txt", &edit(""));
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "a.txt");
    draw(cx, handle);
    let before = doc(&tab, cx);
    let row = row_of_line(&before, 202);
    let view = view(&tab, cx);
    cx.update(|cx| view.update(cx, |v, cx| v.scroll_to_row(row, cx)));
    draw(cx, handle);
    cx.update(|cx| tab.update(cx, |t, cx| t.reload(crate::backend::RefreshSet::STATUS, cx)));
    cx.run_until_parked();
    let same = doc(&tab, cx);
    assert!(
        Arc::ptr_eq(&before, &same),
        "unchanged sides keep the document"
    );
    assert_eq!(
        cx.update(|cx| view.read(cx).list_state().logical_scroll_top().item_ix),
        row
    );

    // Changed content above the viewport: new document, same top line.
    fx.write("a.txt", &edit(" again\nextra line"));
    cx.update(|cx| {
        tab.update(cx, |t, cx| {
            t.reload(crate::backend::RefreshSet::STATUS, cx);
        })
    });
    cx.run_until_parked();
    // ReloadDiff (stale-diff presentation) reloads too.
    cx.update(|cx| {
        let events = tab.read(cx).ctx().events.clone();
        events.update(cx, |_, cx| cx.emit(RepoTabEvent::ReloadDiff));
    });
    cx.run_until_parked();
    let after = doc(&tab, cx);
    assert!(
        !Arc::ptr_eq(&before, &after),
        "changed sides rebuild the document"
    );
    let top = cx.update(|cx| view.read(cx).list_state().logical_scroll_top().item_ix);
    let anchor = after.anchor(top).expect("anchor");
    assert_eq!(
        (anchor.old, anchor.new),
        (Some(202), Some(203)),
        "the top line follows the content (one line inserted above)"
    );
}

#[gpui_kit::test]
fn commit_diff_resolves_renames(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("old_name.txt", &numbered(30));
    fx.commit_all("init");
    fx.git(&["mv", "old_name.txt", "new_name.txt"]);
    fx.commit_all("rename");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    cx.update(|cx| {
        let ui = tab.read(cx).ctx().ui.clone();
        ui.update(cx, |ui, cx| {
            ui.select(
                Selection::Commit {
                    id: head(&fx),
                    is_stash: false,
                    scroll_to: false,
                },
                cx,
            )
        });
    });
    select(cx, &tab, FileList::CommitFiles, "new_name.txt");
    draw(cx, handle);
    let d = doc(&tab, cx);
    assert_eq!(
        d.diff.kind,
        ChangeKind::Renamed,
        "T13: renames come from the commit's files"
    );
    assert_eq!(
        d.diff.previous_path.as_ref().map(|p| p.to_string()),
        Some("old_name.txt".into())
    );
    assert!(matches!(
        &d.diff.content,
        DiffContent::Text(_) | DiffContent::Empty
    ));
    let (dir, name) = super::header::header_path(&d.diff);
    assert_eq!(
        (dir.as_str(), name.as_str()),
        ("", "old_name.txt \u{2192} new_name.txt")
    );
}

#[gpui_kit::test]
fn image_scaled_to_fit(cx: &mut TestAppContext) {
    recorder::take();
    let fx = Fixture::new();
    fx.write("keep.txt", "k\n");
    fx.commit_all("init");
    std::fs::write(fx.path().join("big.png"), png(4000, 3000)).expect("write png");
    std::fs::write(fx.path().join("small.png"), png(10, 10)).expect("write png");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "big.png");
    draw(cx, handle);
    assert!(matches!(
        doc(&tab, cx).diff.content,
        DiffContent::Image { .. }
    ));
    cx.update_window(handle, |_, window, cx| {
        let pane = bounds(window, ids::DIFF_VIEW);
        let image = bounds(window, ids::IMAGE_NEW);
        let w = image.size.width.as_f32();
        let h = image.size.height.as_f32();
        assert!(w > 0. && w <= pane.size.width.as_f32() - 2. * 24. + 0.5);
        assert!(h <= pane.size.height.as_f32());
        assert!((w / h - 4. / 3.).abs() < 0.01, "aspect ratio kept: {w}x{h}");
        window.click(ids::IMAGE_NEW, cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        matches!(recorder::take().as_slice(), [Recorded::Opened(p)] if p.extension().is_some_and(|e| e == "png") && p.exists()),
        "click writes a temporary file and opens it"
    );
    select(cx, &tab, FileList::Unstaged, "small.png");
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let image = bounds(window, ids::IMAGE_NEW);
        assert!(
            (image.size.width.as_f32() - 10.).abs() < 0.5,
            "never scaled up"
        );
        assert!((image.size.height.as_f32() - 10.).abs() < 0.5);
    })
    .expect("window");
}

#[gpui_kit::test]
fn binary_and_submodule_views(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("keep.txt", "k\n");
    fx.commit_all("init");
    std::fs::write(fx.path().join("data.bin"), b"\x00\x01\x02binary\x00data").expect("write");
    let sub = fx.dir.path().join("sub");
    std::fs::create_dir_all(&sub).expect("sub dir");
    let sub_str = sub.to_string_lossy().replace('\\', "/");
    fx.git(&["init", "-q", "-b", "main", &sub_str]);
    fx.git(&["-C", &sub_str, "commit", "-q", "--allow-empty", "-m", "s"]);
    fx.git(&["submodule", "add", "-q", &sub_str, "sub"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "data.bin");
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::BINARY).is_some(), "binary icon");
        assert!(
            window.try_find(ids::TOGGLE_SPLIT).is_none(),
            "no toggles for non-text diffs"
        );
        assert!(window.try_find(ids::STAGE_FILE).is_some());
    })
    .expect("window");
    select(cx, &tab, FileList::Staged, "sub");
    draw(cx, handle);
    assert!(matches!(
        doc(&tab, cx).diff.content,
        DiffContent::Submodule { .. }
    ));
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::SUBMODULE).is_some());
        assert!(
            window.try_find(ids::OPEN_SUBMODULE).is_some(),
            "checked out at the index commit"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn mode_only_change_shows_no_changes(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "a\n");
    fx.commit_all("init");
    fx.write("empty.txt", "");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "empty.txt");
    draw(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(ids::NO_CHANGES).is_some(),
            "empty file: no content changes"
        );
    })
    .expect("window");
    // Close: deselects the file.
    cx.update_window(handle, |_, window, cx| window.click(ids::CLOSE, cx))
        .expect("window");
    cx.run_until_parked();
    let files = cx.update(|cx| tab.read(cx).ctx().ui.read(cx).files.paths.len());
    assert_eq!(files, 0, "close deselects the file");
}

#[gpui_kit::test]
#[ignore = "release-mode benchmark (21 §4.1): cargo test --release -p gitcthulhu --lib diff_50k -- --ignored"]
fn diff_50k_rows_build_under_150ms(cx: &mut TestAppContext) {
    let _ = cx;
    let fx = Fixture::new();
    fx.write("big.txt", &numbered(50_000));
    fx.commit_all("init");
    let text: String = (1..=50_000)
        .map(|i| {
            if i % 2 == 0 {
                format!("changed {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    fx.write("big.txt", &text);
    let diff = Arc::new(file_diff(&fx, unstaged("big.txt")));
    let start = Instant::now();
    let doc = build_doc(diff, DiffMode::Split, 1, &CancelToken::new());
    let elapsed = start.elapsed();
    assert!(doc.rows.len() > 50_000);
    assert!(elapsed.as_millis() < 150, "build took {elapsed:?}");
}

#[gpui_kit::test]
fn loading_header_and_progress_geometry(cx: &mut TestAppContext) {
    let fx = modified_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "src/lib.rs");
    let view = view(&tab, cx);
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            // As after 150 ms without a result for a new target.
            v.stale = true;
            v.loading = true;
            v.show_loading = true;
            cx.notify();
        })
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let pane = bounds(window, ids::DIFF_VIEW);
        let header = bounds(window, ids::LOADING_HEADER);
        let progress = bounds(window, ids::PROGRESS);
        assert_eq!(header.origin, pane.origin);
        assert!((header.size.height.as_f32() - 40.).abs() < 0.5);
        assert!((progress.size.height.as_f32() - 4.).abs() < 0.5);
        assert!((progress.top() - header.bottom()).abs() < px(0.5));
        assert!(window.try_find(ids::HEADER).is_none());
    })
    .expect("window");
}

#[gpui_kit::test]
fn read_only_view_shows_fixed_targets_without_header(cx: &mut TestAppContext) {
    let fx = modified_fixture();
    fx.commit_all("change");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let view = cx
        .update_window(handle, |_, window, cx| {
            cx.new(|cx| DiffView::read_only(ctx, window, cx))
        })
        .expect("window");
    let shown = view.clone();
    let (probe, _) = crate::test_support::geometry::open_probe(cx, 700., 500., move |_, _| {
        gpui_kit::IntoElement::into_any_element(shown.clone())
    });
    let target = |path: &str| DiffTarget::Commit {
        commit: head(&fx),
        against: crate::backend::Against::FirstParent,
        path: RelaPath::from(path),
        previous_path: None,
    };
    cx.update(|cx| view.update(cx, |v, cx| v.set_target(Some(target("src/lib.rs")), cx)));
    cx.run_until_parked();
    draw(cx, probe);
    cx.update_window(probe, |_, window, _| {
        assert!(
            window.try_find(ids::HEADER).is_none(),
            "file history shows no diff header"
        );
        assert!(window.try_find(ids::LIST).is_some());
        assert!(window.try_find((ids::HUNK_STAGE, 0usize)).is_none());
    })
    .expect("window");
    cx.update(|cx| view.update(cx, |v, cx| v.set_target(Some(target("missing.txt")), cx)));
    cx.run_until_parked();
    draw(cx, probe);
    cx.update_window(probe, |_, window, _| {
        assert!(
            window.try_find(ids::ERROR).is_some(),
            "load errors are shown, not swallowed"
        );
    })
    .expect("window");
    cx.update(|cx| view.update(cx, |v, cx| v.set_target(None, cx)));
    assert!(cx.update(|cx| view.read(cx).doc().is_none()));
}

/// `big.rs` with 700 committed lines, all of them changed in the worktree: more than the first pass highlights.
fn many_changes_fixture() -> Fixture {
    let fx = Fixture::new();
    let body = |tag: &str| -> String {
        (0..700)
            .map(|i| format!("fn f{i}() {{ let x = {i}; /* {tag} */ }}\n"))
            .collect()
    };
    fx.write("big.rs", &body("old"));
    fx.commit_all("init");
    fx.write("big.rs", &body("new"));
    fx
}

#[test]
fn highlighting_runs_in_two_passes_on_one_document() {
    use super::highlight::FIRST_PASS_CHANGED_LINES;
    let fx = many_changes_fixture();
    let diff = Arc::new(file_diff(&fx, unstaged("big.rs")));
    let doc = build_doc(diff, DiffMode::Unified, 1, &CancelToken::new());
    assert!(
        !doc.syntax.is_complete(),
        "the first pass stops at its limit"
    );
    let first = doc.syntax.tokens();
    let last = first.len() - 1;
    assert!(
        !first[1].is_empty() && first[last].is_empty(),
        "early lines are highlighted, late ones are plain after the first pass"
    );
    let copy = model::rebuild_mode(&doc, DiffMode::Split, 2);

    assert!(!doc.highlight_full(&{
        let c = CancelToken::new();
        c.cancel();
        c
    }));
    assert!(
        !doc.syntax.is_complete(),
        "a cancelled pass changes nothing"
    );
    assert!(doc.highlight_full(&CancelToken::new()));
    assert!(doc.syntax.is_complete());
    let full = doc.syntax.tokens();
    assert!(
        !full[last].is_empty(),
        "the second pass reaches the late lines"
    );
    assert_eq!(
        full[FIRST_PASS_CHANGED_LINES / 2],
        first[FIRST_PASS_CHANGED_LINES / 2],
        "lines of the first pass keep their tokens"
    );
    assert!(
        copy.syntax.is_complete() && !copy.syntax.tokens()[last].is_empty(),
        "a document copy for another mode sees the second pass"
    );
    assert!(
        !doc.highlight_full(&CancelToken::new()),
        "nothing is left to do"
    );
}

/// A renamed file's removed lines are highlighted with the OLD path's grammar (`old.rs` -> `new.txt`).
#[test]
fn renamed_file_old_side_uses_the_old_paths_syntax() {
    let fx = Fixture::new();
    fx.write(
        "old.rs",
        "fn keep() {}
fn old() {}
fn tail() {}
",
    );
    fx.commit_all("init");
    fx.git(&["mv", "old.rs", "new.txt"]);
    fx.write(
        "new.txt",
        "fn keep() {}
fn new() {}
fn tail() {}
",
    );
    fx.git(&["add", "new.txt"]);
    let diff = Arc::new(file_diff(
        &fx,
        DiffTarget::Staged {
            path: RelaPath::from("new.txt"),
            previous_path: Some(RelaPath::from("old.rs")),
        },
    ));
    assert_eq!(
        diff.previous_path.as_ref().map(|p| p.to_string()),
        Some("old.rs".into())
    );
    let doc = build_doc(diff, DiffMode::Unified, 1, &CancelToken::new());
    let text = doc.text().expect("a text diff");
    let tokens = doc.syntax.tokens();
    let of = |kind: LineKind| {
        text.lines
            .iter()
            .position(|l| l.kind == kind)
            .expect("a line of that kind")
    };
    assert!(
        !tokens[of(LineKind::Removed)].is_empty(),
        "the removed line has Rust tokens"
    );
    assert!(
        tokens[of(LineKind::Added)].is_empty(),
        "the added line of a .txt file stays plain"
    );
}

/// The second highlighting pass writes into the document's shared cell only: the text selection survives it.
#[gpui_kit::test]
fn second_highlight_pass_keeps_a_text_selection(cx: &mut TestAppContext) {
    let fx = many_changes_fixture();
    let diff = Arc::new(file_diff(&fx, unstaged("big.rs")));
    let doc = Arc::new(build_doc(diff, DiffMode::Unified, 1, &CancelToken::new()));
    assert!(!doc.syntax.is_complete(), "only the first pass has run");
    let mut selection = cx.update(super::DiffSelection::new);
    cx.update(|cx| {
        selection.reset(&doc, cx);
        selection.select_all(cx);
        assert!(
            selection.has_selection(cx),
            "selected before the second pass"
        );
    });
    let before = cx.update(|cx| selection.selected_text(cx));
    assert!(doc.highlight_full(&CancelToken::new()));
    cx.update(|cx| {
        assert!(
            selection.has_selection(cx),
            "the second pass keeps the selection"
        );
        assert_eq!(selection.selected_text(cx), before);
    });
}

#[gpui_kit::test]
fn second_highlight_pass_keeps_the_document_and_selection(cx: &mut TestAppContext) {
    let fx = many_changes_fixture();
    let (_handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    select(cx, &tab, FileList::Unstaged, "big.rs");
    let shown = doc(&tab, cx);
    assert!(shown.syntax.is_complete(), "the second pass has run");
    let last = shown.syntax.tokens().len() - 1;
    assert!(!shown.syntax.tokens()[last].is_empty());
    assert_eq!(
        shown.generation, 1,
        "the document was built and installed once"
    );
}
