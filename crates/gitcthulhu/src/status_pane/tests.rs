use core::prelude::v1::test;

use std::cell::Cell;
use std::process::Command;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Entity, InteractiveElement as _,
    IntoElement as _, Modifiers, MouseButton, StatefulInteractiveElement as _, Styled as _,
    TestAppContext, TestSupportExt as _, Window, div, px,
};

use super::commit_box::{self, BoxInputs, BoxMode, CommitBox, Primary, SLOT_OVERRIDE, layout};
use super::{StatusPaneView, actions, ids};
use crate::backend::{
    ChangeKind, GitError, InProgress, RebaseBackend, RebaseState, RebaseStopReason, RelaPath,
    Resolution,
};
use crate::menus::{self, MenuItem, MenuKind, MenuTarget};
use crate::metrics::{self, status as m};
use crate::tab::RepoTab;
use crate::tab::ui::FileList;
use crate::test_support::geometry::{
    assert_above, assert_height, assert_rect, assert_width, bounds,
};
use crate::test_support::input::click_with;
use crate::test_support::seed::{entry, oid, status};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, TestCall, calls, mount_fixture, mount_fixture_with,
    mount_seeded, script_op, toast_texts,
};

// ── helpers ──────────────────────────────────────────────────────────────────────────────────────

fn rid(prefix: &'static str, ix: usize) -> gpui_kit::ElementId {
    gpui_kit::ElementId::from((prefix, ix))
}

fn status_view(tab: &Entity<RepoTab>, cx: &App) -> Entity<StatusPaneView> {
    tab.read(cx).views().status.clone()
}

fn box_of(tab: &Entity<RepoTab>, cx: &App) -> Entity<CommitBox> {
    status_view(tab, cx).read(cx).commit_box().clone()
}

fn seed(staged: &[(&str, ChangeKind)], unstaged: &[(&str, ChangeKind)]) -> RepoSeed {
    RepoSeed::linear(3).with_changes(staged, unstaged)
}

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
}

fn type_message(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    text: &str,
) {
    let commit_box = cx.update(|cx| box_of(tab, cx));
    cx.update_window(handle, |_, window, cx| {
        commit_box.update(cx, |b, cx| b.focus(window, cx));
        window.render_frame(cx);
        window.input(text, cx);
    })
    .expect("window");
    cx.run_until_parked();
}

fn message(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> String {
    cx.update(|cx| box_of(tab, cx).read(cx).message(cx))
}

fn recorded(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> Vec<TestCall> {
    cx.update(|cx| calls(tab, cx))
}

fn labels(items: &[MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            MenuItem::Action { label, .. } => label.to_string(),
            MenuItem::Separator => "-".to_owned(),
        })
        .collect()
}

fn target_for(tab: &Entity<RepoTab>, cx: &App, path: &str) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::StatusFile,
        tab: tab.read(cx).ctx().clone(),
        branch: None,
        upstream: None,
        commit: None,
        path: Some(RelaPath::from(path)),
        is_current: false,
        checked_out_elsewhere: false,
    }
}

fn list_entry(
    tab: &Entity<RepoTab>,
    cx: &App,
    list: FileList,
    path: &str,
) -> crate::backend::StatusEntry {
    let model = tab.read(cx).ctx().model.read(cx);
    let status = model.status.value.as_ref().expect("status");
    let entries = if list == FileList::Staged {
        &status.staged
    } else {
        &status.unstaged
    };
    entries
        .iter()
        .find(|e| e.path == path.as_bytes())
        .cloned()
        .expect("the entry exists")
}

fn inputs(mode: BoxMode) -> BoxInputs {
    BoxInputs {
        mode,
        staged: 1,
        unstaged: 0,
        conflicted: 0,
        has_head: true,
        amend: false,
        message_blank: false,
    }
}

// ── layout (21 §8.2) ─────────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn lists_equal_heights_and_swap(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(
        cx,
        seed(
            &[
                ("a.txt", ChangeKind::Modified),
                ("b.txt", ChangeKind::Added),
            ],
            &[("c.txt", ChangeKind::Modified)],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let pane = bounds(window, ids::STATUS_PANE);
        let (px_, py, pw, ph) = (
            pane.origin.x.as_f32(),
            pane.origin.y.as_f32(),
            pane.size.width.as_f32(),
            pane.size.height.as_f32(),
        );
        let _ = px_;
        let staged = ids::STAGED;
        let unstaged = ids::UNSTAGED;
        assert_height(window, staged.header, metrics::HEADER_BAR_H);
        assert_height(window, unstaged.header, metrics::HEADER_BAR_H);
        let staged_top = bounds(window, staged.list).origin.y.as_f32();
        assert!(
            (staged_top - py).abs() <= 0.5,
            "staged list starts at the pane top"
        );
        let (sh, uh) = (
            bounds(window, staged.list).size.height.as_f32(),
            bounds(window, unstaged.list).size.height.as_f32(),
        );
        assert!((sh - uh).abs() <= 0.5, "equal heights: {sh} vs {uh}");
        assert_above(window, staged.list, unstaged.list, m::LIST_GAP);
        assert_above(window, unstaged.list, commit_box::COMMIT_BOX, m::LIST_GAP);
        assert_height(window, commit_box::COMMIT_BOX, m::COMMIT_BOX_H);
        let commit = bounds(window, commit_box::COMMIT_BOX);
        assert!(
            ((commit.origin.y + commit.size.height).as_f32() - (py + ph - metrics::PANE_EDGE_PAD))
                .abs()
                <= 0.5,
            "8 dp below the commit box"
        );
        assert_width(window, commit_box::COMMIT_BOX, pw - metrics::PANE_EDGE_PAD);
        assert_width(window, staged.list, pw - metrics::PANE_EDGE_PAD);
    })
    .expect("window");

    cx.update(|cx| crate::settings::update(cx, |s| s.swap_status_panes = true));
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert_above(window, ids::UNSTAGED.list, ids::STAGED.list, m::LIST_GAP);
    })
    .expect("window");
}

/// The composite bounds in literal dp (08 §4, 21 §8.2) at 1280×720, independent of the constants the pane renders
/// with: right pane x = 1280 − 330, y = tab strip 36 + toolbar 64, bottom = 720 − bottom bar 32 − divider 1;
/// 8 dp end/bottom padding; commit box 192; two equal lists with a 4 dp gap below each; header 34; rows 38; SAFE
/// box = message + 28 dp checkbox row + 2 dp gap + 36 dp Commit button; filter strip 46.
#[gpui_kit::test]
fn status_pane_bounds_in_literal_dp(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(
        cx,
        seed(
            &[("a.txt", ChangeKind::Modified)],
            &[("c.txt", ChangeKind::Modified)],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    // Pane: 950..1280 × 100..687; content 322 wide; lists share 687 − 8 − 192 − 100 = 387 (193.5 each incl. gap).
    cx.update_window(handle, |_, window, _| {
        assert_rect(window, ids::STATUS_PANE, 950., 100., 330., 587.);
        assert_rect(window, ids::STAGED.list, 950., 100., 322., 189.5);
        assert_rect(window, ids::STAGED.header, 950., 100., 322., 34.);
        assert_rect(window, ids::UNSTAGED.list, 950., 293.5, 322., 189.5);
        assert_rect(window, ids::UNSTAGED.header, 950., 293.5, 322., 34.);
        assert_rect(window, rid(ids::STAGED.row, 0), 950., 134., 322., 38.);
        assert_rect(window, rid(ids::UNSTAGED.row, 0), 950., 327.5, 322., 38.);
        assert_rect(window, commit_box::COMMIT_BOX, 950., 487., 322., 192.);
        assert_rect(window, commit_box::ids::MESSAGE, 950., 487., 322., 126.);
        assert_rect(window, commit_box::ids::CHECKBOX_ROW, 950., 613., 322., 28.);
        assert_rect(window, commit_box::ids::PRIMARY, 950., 643., 322., 36.);
        // The header button ends 16 dp before the list's right edge (1272).
        let action = bounds(window, ids::UNSTAGED.action);
        assert!(
            ((action.origin.x + action.size.width).as_f32() - 1256.).abs() <= 0.5,
            "header button right edge: {action:?}"
        );
    })
    .expect("window");
    // The filter strip: 46 dp right under the header, the rows move down by it.
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::UNSTAGED.search_toggle, cx)
    })
    .expect("window");
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert_rect(window, ids::UNSTAGED.search, 950., 327.5, 322., 46.);
        assert_rect(window, rid(ids::UNSTAGED.row, 0), 950., 373.5, 322., 38.);
    })
    .expect("window");
}

/// 21 §4.6 / 08 §4.6: the label and the first glyph of the message share the 16 dp start inset (the kit pads the
/// editor by another 10 dp, which the field's inset makes up for): pane x 950 + 16.
#[gpui_kit::test]
fn message_text_starts_at_the_label_inset(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(&[], &[("c.txt", ChangeKind::Modified)]),
        DEFAULT_SIZE,
    );
    type_message(cx, handle, &tab, "Subject");
    render(cx, handle);
    let commit_box = cx.update(|cx| box_of(&tab, cx));
    let (text, _caret) = cx.update(|cx| commit_box.read(cx).text_geometry(cx));
    let text = text.expect("the message text was laid out");
    assert!(
        (text.origin.x.as_f32() - (950. + 16.)).abs() <= 0.5,
        "the message text starts at {:?}, expected x 966",
        text.origin.x
    );
}

/// Merge layout in literal dp: one 8 dp + 36 dp row of two equal buttons with 4 + 4 dp between them.
#[gpui_kit::test]
fn merge_button_row_in_literal_dp(cx: &mut TestAppContext) {
    let (handle, _app, _tab) =
        mount_seeded(cx, seed(&[], &[]).in_state(InProgress::Merge), DEFAULT_SIZE);
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert_rect(window, commit_box::ids::MESSAGE, 950., 487., 322., 148.);
        assert_rect(window, commit_box::ids::BUTTON_ROW, 950., 635., 322., 44.);
        assert_rect(window, commit_box::ids::ABORT, 950., 643., 157., 36.);
        assert_rect(window, commit_box::ids::PRIMARY, 1115., 643., 157., 36.);
    })
    .expect("window");
}

/// A real Escape key press in the focused filter field hides the strip and drops the filter (08 §4.3, T7).
#[gpui_kit::test]
fn escape_key_in_the_filter_closes_it(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[],
            &[
                ("alpha.txt", ChangeKind::Modified),
                ("beta.txt", ChangeKind::Modified),
            ],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::UNSTAGED.search_toggle, cx)
    })
    .expect("window");
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| window.input("alp", cx))
        .expect("window");
    render(cx, handle);
    let filter = |cx: &mut TestAppContext| {
        cx.update(|cx| tab.read(cx).ctx().ui.read(cx).unstaged_filter.clone())
    };
    assert_eq!(filter(cx).as_deref(), Some("alp"), "typing filters");
    cx.update_window(handle, |_, window, cx| window.press("escape", cx))
        .expect("window");
    render(cx, handle);
    assert_eq!(filter(cx), None, "Escape drops the filter");
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(ids::UNSTAGED.search).is_none(),
            "and hides the strip"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn commit_box_rows_have_the_spec_heights(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(
        cx,
        seed(&[("a.txt", ChangeKind::Modified)], &[]),
        DEFAULT_SIZE,
    );
    type_message(cx, handle, &_tab, "x");
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert_height(window, commit_box::ids::PRIMARY, m::BUTTON_H);
        assert_height(window, commit_box::ids::CHECKBOX_ROW, m::CHECKBOX_ROW_H);
        assert_height(
            window,
            commit_box::ids::BUTTON_ROW,
            m::BUTTON_H + m::COMMIT_BUTTON_GAP_TOP,
        );
        let row = bounds(window, commit_box::ids::CHECKBOX_ROW);
        let amend = bounds(window, commit_box::ids::AMEND);
        assert!(
            amend.size.width < row.size.width - px(100.),
            "the Amend hit area is content-wide ({:?} in {:?})",
            amend.size,
            row.size
        );
        assert!((amend.size.height.as_f32() - m::CHECKBOX_ROW_H).abs() <= 0.5);
        let primary = bounds(window, commit_box::ids::PRIMARY);
        let commit = bounds(window, commit_box::COMMIT_BOX);
        assert!(
            (primary.size.width - commit.size.width).as_f32().abs() <= 0.5,
            "the Commit button fills the box"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn state_button_layouts_merge_rebase_cherry_revert(cx: &mut TestAppContext) {
    for state in [
        InProgress::Merge,
        InProgress::CherryPick,
        InProgress::Revert,
        InProgress::Rebase,
    ] {
        let mut s = seed(&[], &[]).in_state(state);
        s.state.rebase = (state == InProgress::Rebase).then(|| rebase_state(None));
        let (handle, _app, tab) = mount_seeded(cx, s, DEFAULT_SIZE);
        let _ = tab;
        render(cx, handle);
        cx.update_window(handle, |_, window, _| {
            assert_height(window, commit_box::ids::ABORT, m::BUTTON_H);
            assert_height(window, commit_box::ids::PRIMARY, m::BUTTON_H);
            assert_left_gap(window, 2. * m::STATE_BUTTON_PAD);
            assert!(
                window.try_find(commit_box::ids::AMEND).is_none(),
                "no Amend checkbox in {state:?}"
            );
            let row = bounds(window, commit_box::ids::BUTTON_ROW);
            assert!((row.size.height.as_f32() - (m::BUTTON_H + m::STATE_ROW_PAD_TOP)).abs() <= 0.5);
        })
        .expect("window");
    }
}

fn assert_left_gap(window: &Window, gap: f32) {
    crate::test_support::geometry::assert_left_of(
        window,
        commit_box::ids::ABORT,
        commit_box::ids::PRIMARY,
        gap,
    );
}

fn rebase_state(amend: Option<crate::backend::ObjectId>) -> RebaseState {
    RebaseState {
        backend: RebaseBackend::Merge,
        interactive: true,
        head_name: None,
        onto: None,
        orig_head: None,
        step: Some((1, 2)),
        done: Vec::new(),
        todo: Vec::new(),
        stopped_at: None,
        amend,
        stop_reason: RebaseStopReason::Edit,
        message: None,
        started_by_us: true,
    }
}

#[test]
fn commit_enabled_rules() {
    let safe = |staged, amend, blank, has_head| BoxInputs {
        mode: BoxMode::Safe,
        staged,
        unstaged: 0,
        conflicted: 0,
        has_head,
        amend,
        message_blank: blank,
    };
    assert!(
        layout(&safe(1, false, false, true)).enabled,
        "message and staged file"
    );
    assert!(
        !layout(&safe(0, false, false, true)).enabled,
        "nothing staged"
    );
    assert!(
        !layout(&safe(1, false, true, true)).enabled,
        "blank message"
    );
    let amend = layout(&safe(0, true, false, true));
    assert_eq!(
        (amend.primary, amend.label, amend.enabled),
        (Primary::Amend, "Amend", true)
    );
    assert!(
        !layout(&safe(0, true, true, true)).enabled,
        "amend needs a message"
    );
    assert!(
        !layout(&safe(0, true, false, false)).enabled,
        "amend needs a HEAD commit"
    );
    assert!(
        !layout(&safe(0, false, false, false)).amend_checkbox,
        "no checkbox without commits"
    );
}

#[test]
fn state_buttons_follow_the_spec_table() {
    let merge = layout(&BoxInputs {
        unstaged: 1,
        ..inputs(BoxMode::Merging)
    });
    assert_eq!(
        (merge.label, merge.abort, merge.checkbox_row),
        ("Merge", true, false)
    );
    assert!(!merge.enabled, "Merge needs an empty unstaged list");
    assert!(layout(&inputs(BoxMode::Merging)).enabled);

    let pick = layout(&inputs(BoxMode::CherryPicking { sequence: false }));
    assert_eq!(
        (pick.label, pick.primary, pick.enabled),
        ("Commit", Primary::PickCommit, true)
    );

    let revert = layout(&BoxInputs {
        conflicted: 1,
        ..inputs(BoxMode::Reverting { sequence: false })
    });
    assert_eq!((revert.label, revert.enabled), ("Continue", false));
    assert!(
        layout(&BoxInputs {
            unstaged: 3,
            ..inputs(BoxMode::Reverting { sequence: false })
        })
        .enabled,
        "Continue only needs a message and no conflicts"
    );
    assert!(
        !layout(&BoxInputs {
            message_blank: true,
            ..inputs(BoxMode::Reverting { sequence: false })
        })
        .enabled
    );

    // Rebase: Continue when there is something to continue with, Skip otherwise.
    let rebasing = |amendable, staged, unstaged, amend, blank| BoxInputs {
        mode: BoxMode::Rebasing { amendable },
        staged,
        unstaged,
        conflicted: 0,
        has_head: true,
        amend,
        message_blank: blank,
    };
    let idle = layout(&rebasing(false, 0, 0, false, true));
    assert_eq!(
        (idle.primary, idle.label, idle.enabled, idle.read_only),
        (Primary::Skip, "Skip", true, true)
    );
    let with_files = layout(&rebasing(false, 1, 0, false, true));
    assert_eq!((with_files.label, with_files.enabled), ("Continue", true));
    assert!(
        !layout(&rebasing(false, 0, 1, false, true)).enabled,
        "Continue needs an empty unstaged list"
    );
    let amending = layout(&rebasing(true, 0, 0, true, false));
    assert_eq!(
        (amending.label, amending.checkbox_row, amending.read_only),
        ("Continue", true, false)
    );
    assert_eq!(
        layout(&rebasing(true, 0, 0, true, true)).label,
        "Skip",
        "amend without a message skips"
    );
    assert_eq!(layout(&rebasing(true, 0, 0, false, false)).label, "Skip");
}

#[test]
fn header_button_text_switches_at_2_selected() {
    use super::changes_list::header_button_label as label;
    assert_eq!(label(FileList::Staged, 0), "Unstage all");
    assert_eq!(label(FileList::Staged, 1), "Unstage all");
    assert_eq!(label(FileList::Staged, 2), "Unstage selected");
    assert_eq!(label(FileList::Unstaged, 0), "Stage all");
    assert_eq!(label(FileList::Unstaged, 1), "Stage all");
    assert_eq!(label(FileList::Unstaged, 5), "Stage selected");
}

// ── lists: rows, selection, actions ──────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn rows_are_virtualised(cx: &mut TestAppContext) {
    let names: Vec<String> = (0..20_000).map(|i| format!("dir/file{i:05}.txt")).collect();
    let unstaged: Vec<(&str, ChangeKind)> = names
        .iter()
        .map(|n| (n.as_str(), ChangeKind::Modified))
        .collect();
    let (handle, _app, tab) = mount_seeded(cx, seed(&[], &unstaged), DEFAULT_SIZE);
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(rid(ids::UNSTAGED.row, 0)).is_some(),
            "the first row exists"
        );
        assert!(
            window.try_find(rid(ids::UNSTAGED.row, 10_000)).is_none(),
            "rows outside the viewport are not built"
        );
        let first = bounds(window, rid(ids::UNSTAGED.row, 0));
        assert_eq!(first.size.height.as_f32(), 38.);
    })
    .expect("window");
    let count = cx.update(|cx| {
        let view = status_view(&tab, cx);
        let list = view.read(cx).unstaged().clone();
        list.read(cx).data().rows.len()
    });
    assert_eq!(count, 20_000);
}

#[gpui_kit::test]
fn conflicts_prioritized(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[],
            &[
                ("a.txt", ChangeKind::Modified),
                ("a.txt", ChangeKind::Conflicted),
                ("b.txt", ChangeKind::Modified),
            ],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(rid(ids::UNSTAGED.row, 1)).is_some());
        assert!(
            window.try_find(rid(ids::UNSTAGED.row, 2)).is_none(),
            "the duplicate path is one row"
        );
    })
    .expect("window");
    let kinds = cx.update(|cx| {
        let list = status_view(&tab, cx).read(cx).unstaged().clone();
        list.read(cx)
            .data()
            .entries
            .iter()
            .map(|e| e.kind)
            .collect::<Vec<_>>()
    });
    assert_eq!(kinds, [ChangeKind::Conflicted, ChangeKind::Modified]);
}

#[gpui_kit::test]
fn click_ctrl_and_shift_select_in_one_list(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[("s.txt", ChangeKind::Added)],
            &[
                ("a.txt", ChangeKind::Modified),
                ("b.txt", ChangeKind::Modified),
                ("c.txt", ChangeKind::Modified),
                ("d.txt", ChangeKind::Modified),
            ],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    let row = |i: usize| rid(ids::UNSTAGED.row, i);
    cx.update_window(handle, |_, window, cx| {
        window.click(row(0), cx);
        click_with(window, cx, row(2), Modifiers::shift(), MouseButton::Left);
    })
    .expect("window");
    let selected = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let ui = tab.read(cx).ctx().ui.read(cx);
            (
                ui.files.list,
                ui.files
                    .paths
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>(),
            )
        })
    };
    assert_eq!(
        selected(cx),
        (
            Some(FileList::Unstaged),
            vec!["a.txt".into(), "b.txt".into(), "c.txt".into()]
        ),
        "shift adds the range"
    );
    cx.update_window(handle, |_, window, cx| {
        click_with(
            window,
            cx,
            row(1),
            Modifiers::secondary_key(),
            MouseButton::Left,
        );
    })
    .expect("window");
    assert_eq!(
        selected(cx).1,
        ["a.txt", "c.txt"],
        "ctrl toggles one file out"
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(rid(ids::STAGED.row, 0), cx);
    })
    .expect("window");
    assert_eq!(
        selected(cx),
        (Some(FileList::Staged), vec!["s.txt".into()]),
        "the selection lives in one list only"
    );
}

#[gpui_kit::test]
fn double_click_stages(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[("s.txt", ChangeKind::Added)],
            &[("a.txt", ChangeKind::Modified)],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.double_click(rid(ids::UNSTAGED.row, 0), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        matches!(
            recorded(cx, &tab).as_slice(),
            [TestCall::Op {
                kind: crate::tab::ops::OpKind::StageFile,
                ..
            }]
        ),
        "a double click stages the file: {:?}",
        recorded(cx, &tab)
    );
    cx.update_window(handle, |_, window, cx| {
        window.double_click(rid(ids::STAGED.row, 0), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        recorded(cx, &tab).iter().any(|c| matches!(
            c,
            TestCall::Op {
                kind: crate::tab::ops::OpKind::UnstageFile,
                ..
            }
        )),
        "and unstages in the staged list"
    );
}

#[gpui_kit::test]
fn hover_button_stages(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[],
            &[
                ("a.txt", ChangeKind::Modified),
                ("b.txt", ChangeKind::Modified),
            ],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.hover(rid(ids::UNSTAGED.row, 1), cx);
        window.render_frame(cx);
        window.click(rid(ids::UNSTAGED.row_action, 1), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        matches!(
            recorded(cx, &tab).as_slice(),
            [TestCall::Op {
                kind: crate::tab::ops::OpKind::StageFile,
                ..
            }]
        ),
        "the hover button stages: {:?}",
        recorded(cx, &tab)
    );
    let selected = cx.update(|cx| tab.read(cx).ctx().ui.read(cx).files.paths.len());
    assert_eq!(selected, 0, "the button does not also select the row");
}

#[gpui_kit::test]
fn header_buttons_stage_all_and_selected(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[("s.txt", ChangeKind::Added)],
            &[
                ("a.txt", ChangeKind::Modified),
                ("b.txt", ChangeKind::Modified),
            ],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::UNSTAGED.action, cx)
    })
    .expect("window");
    cx.run_until_parked();
    assert!(matches!(
        recorded(cx, &tab).as_slice(),
        [TestCall::Op {
            kind: crate::tab::ops::OpKind::StageAll,
            ..
        }]
    ));
    cx.update_window(handle, |_, window, cx| {
        window.click(rid(ids::UNSTAGED.row, 0), cx);
        click_with(
            window,
            cx,
            rid(ids::UNSTAGED.row, 1),
            Modifiers::secondary_key(),
            MouseButton::Left,
        );
        window.render_frame(cx);
        window.click(ids::UNSTAGED.action, cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        recorded(cx, &tab).iter().any(|c| matches!(
            c,
            TestCall::Op {
                kind: crate::tab::ops::OpKind::StageFiles,
                ..
            }
        )),
        "two selected: the selected files are staged: {:?}",
        recorded(cx, &tab)
    );
    cx.update_window(handle, |_, window, cx| window.click(ids::STAGED.action, cx))
        .expect("window");
    cx.run_until_parked();
    assert!(recorded(cx, &tab).iter().any(|c| matches!(
        c,
        TestCall::Op {
            kind: crate::tab::ops::OpKind::UnstageAll,
            ..
        }
    )),);
}

#[gpui_kit::test]
fn unstage_all_is_blocked_while_conflicts_exist(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[("s.txt", ChangeKind::Added)],
            &[("c.txt", ChangeKind::Conflicted)],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| window.click(ids::STAGED.action, cx))
        .expect("window");
    cx.run_until_parked();
    assert!(
        recorded(cx, &tab).is_empty(),
        "the backend refuses it, so the button does nothing"
    );
}

#[gpui_kit::test]
fn tree_mode_collapses_directories(cx: &mut TestAppContext) {
    cx.update(|_| ());
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[],
            &[
                ("src/a.rs", ChangeKind::Modified),
                ("src/sub/b.rs", ChangeKind::Added),
                ("z.txt", ChangeKind::Modified),
            ],
        ),
        DEFAULT_SIZE,
    );
    cx.update(|cx| crate::settings::update(cx, |s| s.changes_as_tree = true));
    render(cx, handle);
    let rows = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            status_view(&tab, cx)
                .read(cx)
                .unstaged()
                .read(cx)
                .data()
                .rows
                .len()
        })
    };
    assert_eq!(rows(cx), 5, "src/, a.rs, sub/, b.rs, z.txt");
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window.try_find(rid(ids::UNSTAGED.dir_row, 0)).is_some(),
            "a directory row"
        );
        window.click(rid(ids::UNSTAGED.dir_row, 0), cx);
    })
    .expect("window");
    render(cx, handle);
    assert_eq!(rows(cx), 2, "collapsing src/ hides everything below it");
    // The toggle switches the shared setting back to a list.
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::UNSTAGED.tree_toggle, cx)
    })
    .expect("window");
    render(cx, handle);
    assert_eq!(rows(cx), 3, "list mode: one row per file");
}

#[gpui_kit::test]
fn search_filters_rows_and_escape_closes(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[],
            &[
                ("alpha.txt", ChangeKind::Modified),
                ("beta.txt", ChangeKind::Modified),
            ],
        ),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window.try_find(ids::UNSTAGED.search).is_none(),
            "hidden by default"
        );
        window.click(ids::UNSTAGED.search_toggle, cx);
    })
    .expect("window");
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window.try_find(ids::UNSTAGED.search).is_some(),
            "the strip is shown"
        );
        assert_height(window, ids::UNSTAGED.search, m::SEARCH_STRIP_H);
        window.input("BET", cx);
    })
    .expect("window");
    render(cx, handle);
    let paths = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            status_view(&tab, cx)
                .read(cx)
                .unstaged()
                .read(cx)
                .data()
                .entries
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        paths(cx),
        ["beta.txt"],
        "case-insensitive substring of the path"
    );
    cx.update(|cx| {
        let ui = tab.read(cx).ctx().ui.clone();
        ui.update(cx, |ui, cx| {
            assert!(ui.close_last_view(cx), "Escape closes the search")
        });
    });
    render(cx, handle);
    assert_eq!(paths(cx).len(), 2, "closing shows everything again");
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::UNSTAGED.search).is_none());
    })
    .expect("window");
}

// ── menus ────────────────────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn context_menus_single_multi_dir(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(
        cx,
        seed(
            &[
                ("m.txt", ChangeKind::Modified),
                ("n.txt", ChangeKind::Added),
            ],
            &[
                ("u.txt", ChangeKind::Modified),
                ("new.txt", ChangeKind::Untracked),
                ("gone.txt", ChangeKind::Deleted),
                ("dir/x.txt", ChangeKind::Modified),
            ],
        ),
        DEFAULT_SIZE,
    );
    cx.update(|cx| {
        let single = |list, path: &str| {
            let target = target_for(&tab, cx, path);
            let entry = list_entry(&tab, cx, list, path);
            labels(&menus::files::single_file_menu(&target, list, &entry, cx))
        };
        assert_eq!(
            single(FileList::Unstaged, "u.txt"),
            [
                "Discard file changes",
                "Blame file",
                "File history",
                "Open file in folder",
                "-",
                "Copy path",
                "Copy relative path"
            ],
            "a modified unstaged file: no Delete (tracked)"
        );
        assert_eq!(
            single(FileList::Unstaged, "new.txt"),
            [
                "Delete file",
                "Open file in folder",
                "-",
                "Copy path",
                "Copy relative path"
            ],
            "an untracked file: no Discard, Blame or History, but Delete"
        );
        assert_eq!(
            single(FileList::Unstaged, "gone.txt"),
            [
                "Discard file changes",
                "Open file in folder",
                "-",
                "Copy path",
                "Copy relative path"
            ],
            "a deleted file: no Blame, History or Delete"
        );
        assert_eq!(
            single(FileList::Staged, "n.txt"),
            [
                "Open file in folder",
                "-",
                "Copy path",
                "Copy relative path"
            ],
            "a staged added file has no Discard"
        );
        assert_eq!(single(FileList::Staged, "m.txt")[0], "Discard file changes");

        let target = target_for(&tab, cx, "m.txt");
        let staged = vec![
            list_entry(&tab, cx, FileList::Staged, "m.txt"),
            list_entry(&tab, cx, FileList::Staged, "n.txt"),
        ];
        assert_eq!(
            labels(&menus::files::multi_file_menu(
                &target,
                FileList::Staged,
                staged.clone(),
                cx
            )),
            [
                "Unstage 2 files",
                "Discard 2 files",
                "-",
                "Copy paths",
                "Copy relative paths"
            ]
        );
        assert_eq!(
            labels(&menus::files::multi_file_menu(
                &target,
                FileList::Unstaged,
                staged,
                cx
            ))[0],
            "Stage 2 files"
        );

        let below = vec![list_entry(&tab, cx, FileList::Unstaged, "dir/x.txt")];
        let dir_target = target_for(&tab, cx, "dir");
        assert_eq!(
            labels(&menus::files::directory_menu(
                &dir_target,
                FileList::Unstaged,
                RelaPath::from("dir"),
                below.clone(),
                cx
            )),
            ["Stage changes in the directory", "-", "Discard changes"]
        );
        assert_eq!(
            labels(&menus::files::directory_menu(
                &dir_target,
                FileList::Staged,
                RelaPath::from("dir"),
                below,
                cx
            )),
            ["Unstage changes in the directory"]
        );
        // The slot-less entry point infers the list from the selection (unstaged by default).
        assert_eq!(
            labels(&menus::files::status_file(
                &target_for(&tab, cx, "u.txt"),
                cx
            ))[0],
            "Discard file changes"
        );
    });
}

#[gpui_kit::test]
fn conflicted_files_offer_ours_and_theirs(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(
        cx,
        seed(&[], &[("c.txt", ChangeKind::Conflicted)]),
        DEFAULT_SIZE,
    );
    cx.update(|cx| {
        let target = target_for(&tab, cx, "c.txt");
        let entry = list_entry(&tab, cx, FileList::Unstaged, "c.txt");
        let items = labels(&menus::files::single_file_menu(
            &target,
            FileList::Unstaged,
            &entry,
            cx,
        ));
        assert_eq!(
            &items[..3],
            ["Resolve using ours", "Resolve using theirs", "-"]
        );
    });
}

thread_local! {
    static COPIED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    static REVEALED: std::cell::RefCell<Vec<std::path::PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn install_recording_platform(cx: &mut TestAppContext) {
    COPIED.with(|c| c.borrow_mut().clear());
    REVEALED.with(|c| c.borrow_mut().clear());
    cx.update(|cx| {
        cx.set_global(super::platform::FilesPlatform {
            copy: |_, text| COPIED.with(|c| c.borrow_mut().push(text)),
            reveal: |_, path| REVEALED.with(|c| c.borrow_mut().push(path)),
        });
    });
}

#[gpui_kit::test]
fn menu_items_copy_and_reveal_through_the_platform(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("dir/a.txt", "one\n");
    fx.commit_all("init");
    fx.write("dir/a.txt", "two\n");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    install_recording_platform(cx);
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.right_click(rid(ids::UNSTAGED.row, 0), cx);
        window.render_frame(cx);
        window.click("menu-item-status-copy-path", cx);
        window.render_frame(cx);
        window.right_click(rid(ids::UNSTAGED.row, 0), cx);
        window.render_frame(cx);
        window.click("menu-item-status-copy-relative", cx);
        window.render_frame(cx);
        window.right_click(rid(ids::UNSTAGED.row, 0), cx);
        window.render_frame(cx);
        window.click("menu-item-status-reveal", cx);
    })
    .expect("window");
    let copied = COPIED.with(|c| c.borrow().clone());
    assert_eq!(copied.len(), 2);
    let absolute = std::path::PathBuf::from(&copied[0]);
    assert!(absolute.is_absolute(), "absolute path: {absolute:?}");
    assert!(
        absolute.ends_with(std::path::Path::new("dir").join("a.txt")),
        "{absolute:?}"
    );
    assert_eq!(copied[1], "dir/a.txt", "the relative path is Git's");
    assert_eq!(REVEALED.with(|c| c.borrow().len()), 1);
    let _ = tab;
}

#[gpui_kit::test]
fn blame_and_history_items_open_the_views(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(&[], &[("u.txt", ChangeKind::Modified)]),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.right_click(rid(ids::UNSTAGED.row, 0), cx);
        window.render_frame(cx);
        window.click("menu-item-status-blame", cx);
    })
    .expect("window");
    let blame = cx.update(|cx| {
        let ui = tab.read(cx).ctx().ui.read(cx);
        (
            ui.blame.clone(),
            ui.views.contains(crate::tab::ui::CloseableView::Blame),
        )
    });
    assert_eq!(blame.0.expect("blame target").path, "u.txt");
    assert!(blame.1, "Escape closes it");
}

// ── actions ──────────────────────────────────────────────────────────────────────────────────────

#[test]
fn discard_passes_untracked_subset() {
    let entries = [
        entry("tracked.txt", ChangeKind::Modified),
        entry("new.txt", ChangeKind::Untracked),
        {
            let mut renamed = entry("after.txt", ChangeKind::Renamed);
            renamed.previous_path = Some(RelaPath::from("before.txt"));
            renamed
        },
    ];
    let (paths, mode, untracked) = actions::discard_args(FileList::Unstaged, &entries);
    assert_eq!(paths.len(), 4, "renames pass both paths");
    assert_eq!(mode, crate::backend::DiscardMode::Unstaged);
    assert_eq!(
        untracked,
        [RelaPath::from("new.txt")],
        "only the untracked files"
    );
    let (_, mode, _) = actions::discard_args(FileList::Staged, &entries);
    assert_eq!(mode, crate::backend::DiscardMode::StagedAndUnstaged);
}

#[test]
fn rename_operations_pass_both_paths() {
    let mut renamed = entry("after.txt", ChangeKind::Renamed);
    renamed.previous_path = Some(RelaPath::from("before.txt"));
    let paths = actions::paths_of([&renamed, &renamed]);
    assert_eq!(
        paths,
        [RelaPath::from("after.txt"), RelaPath::from("before.txt")]
    );
    assert_eq!(actions::relative_text(&paths), "after.txt\nbefore.txt");
}

fn run_in_tab(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App),
) {
    cx.update_window(handle, |_, window, cx| f(window, cx))
        .expect("window");
    cx.run_until_parked();
}

fn git_ignoring_failure(fx: &Fixture, args: &[&str]) {
    let _ = Command::new("git")
        .current_dir(fx.path())
        .args(args)
        .env("GIT_CONFIG_GLOBAL", fx.dir.path().join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", fx.dir.path().join("home"))
        .env("USERPROFILE", fx.dir.path().join("home"))
        .output();
}

fn status_of(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> (Vec<String>, Vec<String>) {
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        let s = model.status.value.as_ref().expect("status");
        (
            s.staged.iter().map(|e| e.path.to_string()).collect(),
            s.unstaged.iter().map(|e| e.path.to_string()).collect(),
        )
    })
}

#[gpui_kit::test]
fn real_double_click_stages_and_unstages(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    fx.write("new.txt", "fresh\n");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    assert_eq!(status_of(cx, &tab).1, ["a.txt", "new.txt"]);
    run_in_tab(cx, handle, |window, cx| {
        window.double_click(rid(ids::UNSTAGED.row, 0), cx)
    });
    render(cx, handle);
    assert_eq!(
        status_of(cx, &tab),
        (vec!["a.txt".into()], vec!["new.txt".into()])
    );
    run_in_tab(cx, handle, |window, cx| {
        window.double_click(rid(ids::STAGED.row, 0), cx)
    });
    render(cx, handle);
    assert_eq!(
        status_of(cx, &tab).0,
        Vec::<String>::new(),
        "unstaged again"
    );
}

#[gpui_kit::test]
fn real_discard_and_delete_restore_through_undo(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    fx.write("new.txt", "fresh\n");
    let (handle, _app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
    render(cx, handle);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let entries = cx.update(|cx| {
        vec![
            list_entry(&tab, cx, FileList::Unstaged, "a.txt"),
            list_entry(&tab, cx, FileList::Unstaged, "new.txt"),
        ]
    });
    run_in_tab(cx, handle, |window, cx| {
        actions::discard(&ctx, FileList::Unstaged, entries, window, cx)
    });
    assert_eq!(
        std::fs::read_to_string(fx.path().join("a.txt")).expect("a.txt"),
        "one\n"
    );
    assert!(
        !fx.path().join("new.txt").exists(),
        "the untracked file is deleted"
    );
    assert_eq!(status_of(cx, &tab), (vec![], vec![]));
}

#[gpui_kit::test]
fn real_delete_untracked_through_the_menu(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("new.txt", "fresh\n");
    let (handle, _app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
    render(cx, handle);
    run_in_tab(cx, handle, |window, cx| {
        window.right_click(rid(ids::UNSTAGED.row, 0), cx);
        window.render_frame(cx);
        window.click("menu-item-status-delete", cx);
    });
    assert!(!fx.path().join("new.txt").exists());
    assert_eq!(status_of(cx, &tab).1, Vec::<String>::new());
}

#[gpui_kit::test]
fn staging_conflict_markers_asks_first(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("c.txt", "base\n");
    fx.commit_all("base");
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("c.txt", "theirs\n");
    fx.commit_all("theirs");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("c.txt", "ours\n");
    fx.commit_all("ours");
    git_ignoring_failure(&fx, &["merge", "other"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    assert_eq!(status_of(cx, &tab).1, ["c.txt"], "the conflict is unstaged");
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let entry = cx.update(|cx| list_entry(&tab, cx, FileList::Unstaged, "c.txt"));
    assert_eq!(entry.kind, ChangeKind::Conflicted);
    run_in_tab(cx, handle, |window, cx| {
        actions::stage(&ctx, vec![entry.clone()], window, cx)
    });
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window.try_find("dialog-primary").is_some(),
            "markers remain: asks first"
        );
        window.click("dialog-primary", cx);
    })
    .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    assert_eq!(
        status_of(cx, &tab).1,
        Vec::<String>::new(),
        "marked resolved after the answer"
    );
    assert_eq!(status_of(cx, &tab).0, ["c.txt"]);
}

#[gpui_kit::test]
fn resolving_with_ours_takes_our_side(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("c.txt", "base\n");
    fx.commit_all("base");
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("c.txt", "theirs\n");
    fx.commit_all("theirs");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("c.txt", "ours\n");
    fx.commit_all("ours");
    git_ignoring_failure(&fx, &["merge", "other"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    run_in_tab(cx, handle, |window, cx| {
        actions::resolve(
            &ctx,
            vec![RelaPath::from("c.txt")],
            Resolution::Ours,
            window,
            cx,
        )
    });
    assert_eq!(
        std::fs::read_to_string(fx.path().join("c.txt")).expect("c.txt"),
        "ours\n"
    );
}

// ── commit box ───────────────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn commit_box_message_api(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    let commit_box = cx.update(|cx| box_of(&tab, cx));
    assert_eq!(message(cx, &tab), "");
    cx.update_window(handle, |_, window, cx| {
        commit_box.update(cx, |b, cx| {
            b.set_message("generated text".into(), window, cx)
        });
    })
    .expect("window");
    assert_eq!(message(cx, &tab), "generated text");
    cx.update(|cx| {
        assert!(!commit_box.read(cx).is_read_only());
        commit_box.update(cx, |b, cx| b.set_read_only(true, cx));
        assert!(
            commit_box.read(cx).is_read_only(),
            "AI streaming makes it read-only"
        );
        commit_box.update(cx, |b, cx| b.set_read_only(false, cx));
        assert!(!commit_box.read(cx).is_read_only());
        assert_eq!(commit_box.read(cx).ctx().id, tab.read(cx).ctx().id);
    });
    // The enabled state follows the message.
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(commit_box::ids::PRIMARY, cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        matches!(
            recorded(cx, &tab).as_slice(),
            [TestCall::Op { kind: crate::tab::ops::OpKind::Commit, title: Some(t) }] if t == "Commit \"generated text\""
        ),
        "{:?}",
        recorded(cx, &tab)
    );
}

#[gpui_kit::test]
fn disabled_commit_button_does_nothing(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(commit_box::ids::PRIMARY, cx)
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        recorded(cx, &tab).is_empty(),
        "a blank message cannot be committed"
    );
}

#[gpui_kit::test]
fn ctrl_enter_commits(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    type_message(cx, handle, &tab, "msg");
    assert_eq!(message(cx, &tab), "msg", "typing reaches the field");
    cx.update_window(handle, |_, window, cx| window.press("ctrl-enter", cx))
        .expect("window");
    cx.run_until_parked();
    assert!(
        matches!(
            recorded(cx, &tab).as_slice(),
            [TestCall::Op { kind: crate::tab::ops::OpKind::Commit, title: Some(t) }] if t == "Commit \"msg\""
        ),
        "{:?}",
        recorded(cx, &tab)
    );
}

#[gpui_kit::test]
fn ctrl_enter_inserts_no_newline(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    type_message(cx, handle, &tab, "msg");
    cx.update(|cx| {
        script_op::<crate::backend::OpOutcome<crate::backend::CommitOutcome>>(
            &tab,
            cx,
            Err(crate::backend::present::OpFailure::Cancelled),
            Duration::ZERO,
        )
    });
    cx.update_window(handle, |_, window, cx| window.press("ctrl-enter", cx))
        .expect("window");
    cx.run_until_parked();
    assert_eq!(
        message(cx, &tab),
        "msg",
        "a failed commit leaves the text as typed, without a newline"
    );
}

#[gpui_kit::test]
fn ctrl_enter_does_nothing_while_the_field_is_read_only(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    type_message(cx, handle, &tab, "half a gener");
    // The AI controller streams into the field: a half-written message must not be committed by the chord.
    cx.update(|cx| box_of(&tab, cx).update(cx, |b, cx| b.set_read_only(true, cx)));
    cx.update_window(handle, |_, window, cx| window.press("ctrl-enter", cx))
        .expect("window");
    cx.run_until_parked();
    assert!(recorded(cx, &tab).is_empty(), "{:?}", recorded(cx, &tab));
}

#[gpui_kit::test]
fn ctrl_enter_does_nothing_when_commit_is_disabled(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(&[], &[("a.txt", ChangeKind::Modified)]),
        DEFAULT_SIZE,
    );
    type_message(cx, handle, &tab, "msg");
    cx.update_window(handle, |_, window, cx| window.press("ctrl-enter", cx))
        .expect("window");
    cx.run_until_parked();
    assert!(recorded(cx, &tab).is_empty(), "nothing is staged");
}

#[gpui_kit::test]
fn successful_commit_clears_the_box_and_amend(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    type_message(cx, handle, &tab, "msg");
    cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .ui
            .clone()
            .update(cx, |ui, _| ui.amend = true);
    });
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(crate::backend::OpOutcome {
                value: crate::backend::CommitOutcome {
                    id: oid(9),
                    summary: "msg".into(),
                },
                refresh: crate::backend::RefreshSet::STATUS,
                log: Default::default(),
                warnings: Vec::new(),
            }),
            Duration::ZERO,
        )
    });
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| window.press("ctrl-enter", cx))
        .expect("window");
    cx.run_until_parked();
    assert_eq!(message(cx, &tab), "", "the message is consumed");
    assert!(!cx.update(|cx| tab.read(cx).ctx().ui.read(cx).amend));
}

#[gpui_kit::test]
fn identity_missing_opens_dialog_before_commit(cx: &mut TestAppContext) {
    let mut s = seed(&[("a.txt", ChangeKind::Added)], &[]);
    s.identity.effective = None;
    let (handle, app, tab) = mount_seeded(cx, s, DEFAULT_SIZE);
    type_message(cx, handle, &tab, "msg");
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(commit_box::ids::PRIMARY, cx)
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        recorded(cx, &tab).is_empty(),
        "no commit before the identity is known"
    );
    let dialog = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("dialog-primary").is_some()
        })
        .expect("window");
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        dialog || toasts.iter().any(|t| t.contains("Commit author identity")),
        "the Commit-author dialog is asked for (dialog: {dialog}, toasts: {toasts:?})"
    );
}

#[gpui_kit::test]
fn unknown_identity_error_asks_for_the_identity(cx: &mut TestAppContext) {
    let (handle, app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    type_message(cx, handle, &tab, "msg");
    cx.update(|cx| {
        script_op::<crate::backend::OpOutcome<crate::backend::CommitOutcome>>(
            &tab,
            cx,
            Err(crate::backend::present::OpFailure::Git(
                std::sync::Arc::new(GitError::IdentityUnknown {
                    stderr: String::new(),
                }),
            )),
            Duration::ZERO,
        )
    });
    cx.update_window(handle, |_, window, cx| window.press("ctrl-enter", cx))
        .expect("window");
    cx.run_until_parked();
    let dialog = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("dialog-primary").is_some()
        })
        .expect("window");
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        dialog || toasts.iter().any(|t| t.contains("Commit author identity")),
        "an IdentityUnknown failure opens the dialog instead of the error dialog ({toasts:?})"
    );
    assert_eq!(message(cx, &tab), "msg", "the message stays for the retry");
}

fn test_slot(
    _: &crate::tab::TabCtx,
    _: gpui_kit::WeakEntity<CommitBox>,
    _: &mut Window,
    _: &mut App,
) -> Option<AnyElement> {
    Some(
        div()
            .id("test-ai-button")
            .w(px(60.))
            .h(px(24.))
            .on_click(|_, _, _| AI_CLICKS.with(|c| c.set(c.get() + 1)))
            .test_support()
            .into_any_element(),
    )
}

thread_local! {
    static AI_CLICKS: Cell<u32> = const { Cell::new(0) };
}

#[gpui_kit::test]
fn ai_button_click_does_not_toggle_amend(cx: &mut TestAppContext) {
    AI_CLICKS.with(|c| c.set(0));
    SLOT_OVERRIDE.with(|s| s.set(Some(test_slot)));
    let (handle, _app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    render(cx, handle);
    let amend = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).ctx().ui.read(cx).amend);
    cx.update_window(handle, |_, window, cx| {
        let row = bounds(window, commit_box::ids::CHECKBOX_ROW);
        let button = bounds(window, "test-ai-button");
        assert!(
            (button.origin.x + button.size.width - (row.origin.x + row.size.width))
                .as_f32()
                .abs()
                <= 0.5,
            "the slot is at the right end of the row"
        );
        window.click("test-ai-button", cx);
    })
    .expect("window");
    assert_eq!(
        AI_CLICKS.with(Cell::get),
        1,
        "the slot element got the click"
    );
    assert!(
        !amend(cx),
        "Amend stays off: the checkbox row is content-wide"
    );
    cx.update_window(handle, |_, window, cx| {
        window.click(commit_box::ids::AMEND, cx)
    })
    .expect("window");
    assert!(amend(cx), "the checkbox itself toggles");
    SLOT_OVERRIDE.with(|s| s.set(None));
}

#[gpui_kit::test]
fn amend_fills_head_message(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("first line\n\nbody text");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    assert_eq!(message(cx, &tab), "");
    run_in_tab(cx, handle, |window, cx| {
        window.click(commit_box::ids::AMEND, cx)
    });
    assert!(
        message(cx, &tab).starts_with("first line\n\nbody text"),
        "ticking Amend with an empty box fills it: {:?}",
        message(cx, &tab)
    );
    assert!(cx.update(|cx| tab.read(cx).ctx().ui.read(cx).amend));
}

#[gpui_kit::test]
fn real_commit_through_the_box(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    run_in_tab(cx, handle, |window, cx| {
        window.double_click(rid(ids::UNSTAGED.row, 0), cx)
    });
    render(cx, handle);
    type_message(cx, handle, &tab, "second commit");
    cx.update_window(handle, |_, window, cx| window.press("ctrl-enter", cx))
        .expect("window");
    cx.run_until_parked();
    cx.run_until_parked();
    assert_eq!(
        fx.git(&["log", "-1", "--format=%s"]).trim(),
        "second commit"
    );
    assert_eq!(message(cx, &tab), "", "the box is empty after the commit");
    assert_eq!(status_of(cx, &tab), (vec![], vec![]));
}

#[gpui_kit::test]
fn real_amend_through_the_box(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    run_in_tab(cx, handle, |window, cx| {
        window.click(commit_box::ids::AMEND, cx)
    });
    type_message(cx, handle, &tab, " plus");
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(commit_box::ids::PRIMARY, cx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.run_until_parked();
    assert_eq!(
        fx.git(&["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "amended, not added"
    );
    assert!(fx.git(&["log", "-1", "--format=%s"]).contains("init"));
}

#[gpui_kit::test]
fn draft_is_saved_after_one_second_and_loaded_into_a_blank_box(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    type_message(cx, handle, &tab, "work in progress");
    let draft = fx
        .path()
        .join(".git")
        .join("gitcthulhu")
        .join("commit-draft");
    cx.executor()
        .advance_clock(Duration::from_millis(m::DRAFT_SAVE_MS / 2));
    cx.run_until_parked();
    assert!(!draft.exists(), "not saved before the debounce passed");
    cx.executor()
        .advance_clock(Duration::from_millis(m::DRAFT_SAVE_MS));
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(&draft).expect("the draft file"),
        "work in progress"
    );
    // A second tab on the same repository starts with the draft.
    let (_handle2, _app2, tab2) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    cx.run_until_parked();
    assert_eq!(
        message(cx, &tab2),
        "work in progress",
        "a blank box adopts the draft"
    );
}

#[gpui_kit::test]
fn typing_is_not_overwritten_by_a_late_draft_load(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    type_message(cx, handle, &tab, "typed");
    cx.update(|cx| {
        tab.update(cx, |t, cx| {
            t.reload(crate::backend::RefreshSet::REPO_STATE, cx)
        });
    });
    cx.run_until_parked();
    assert_eq!(
        message(cx, &tab),
        "typed",
        "a reload keeps what the user typed"
    );
}

#[gpui_kit::test]
fn merge_state_shows_the_merge_message_and_buttons(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("c.txt", "base\n");
    fx.commit_all("base");
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("c.txt", "theirs\n");
    fx.commit_all("theirs");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("c.txt", "ours\n");
    fx.commit_all("ours");
    git_ignoring_failure(&fx, &["merge", "other"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    assert!(
        message(cx, &tab).starts_with("Merge branch 'other'"),
        "the box shows git's merge message: {:?}",
        message(cx, &tab)
    );
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(commit_box::ids::ABORT).is_some(),
            "Abort and Merge show"
        );
        assert!(window.try_find(commit_box::ids::AMEND).is_none());
    })
    .expect("window");
    // Abort resets the state and the box leaves the merge layout.
    run_in_tab(cx, handle, |window, cx| {
        window.click(commit_box::ids::ABORT, cx)
    });
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(commit_box::ids::ABORT).is_none(),
            "back to the SAFE layout"
        );
    })
    .expect("window");
    assert_eq!(message(cx, &tab), "", "the merge message is gone");
}

#[test]
fn box_mode_of_repository_state() {
    let base = RepoSeed::linear(1);
    assert_eq!(BoxMode::of(None), BoxMode::Safe);
    assert_eq!(BoxMode::of(Some(&base.state)), BoxMode::Safe);
    let mut s = base.state;
    s.in_progress = Some(InProgress::RevertSequence);
    assert_eq!(BoxMode::of(Some(&s)), BoxMode::Reverting { sequence: true });
    s.in_progress = Some(InProgress::RebaseInteractive);
    s.rebase = Some(rebase_state(Some(oid(3))));
    assert_eq!(BoxMode::of(Some(&s)), BoxMode::Rebasing { amendable: true });
    s.rebase = Some(rebase_state(None));
    assert_eq!(
        BoxMode::of(Some(&s)),
        BoxMode::Rebasing { amendable: false }
    );
}

#[gpui_kit::test]
fn amendable_rebase_stop_defaults_amend_on_and_shows_the_checkbox(cx: &mut TestAppContext) {
    let mut s = seed(&[], &[]).in_state(InProgress::RebaseInteractive);
    s.state.rebase = Some(rebase_state(Some(oid(1))));
    let (handle, _app, tab) = mount_seeded(cx, s, DEFAULT_SIZE);
    render(cx, handle);
    assert!(
        cx.update(|cx| tab.read(cx).ctx().ui.read(cx).amend),
        "ticked by default at an edit stop"
    );
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(commit_box::ids::AMEND).is_some());
        assert!(window.try_find(commit_box::ids::ABORT).is_some());
    })
    .expect("window");
}

#[gpui_kit::test]
fn non_amendable_rebase_step_is_read_only(cx: &mut TestAppContext) {
    let mut s = seed(&[], &[]).in_state(InProgress::Rebase);
    s.state.rebase = Some(rebase_state(None));
    let (handle, _app, tab) = mount_seeded(cx, s, DEFAULT_SIZE);
    render(cx, handle);
    let commit_box = cx.update(|cx| box_of(&tab, cx));
    assert!(
        cx.update(|cx| commit_box.read(cx).is_read_only()),
        "no editing at this step"
    );
}

#[gpui_kit::test]
fn progress_bar_shows_while_the_status_loads(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed(&[], &[("a.txt", ChangeKind::Modified)]),
        DEFAULT_SIZE,
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::PROGRESS).is_none());
    })
    .expect("window");
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.clone();
        model.update(cx, |m, cx| {
            m.status.loading = true;
            cx.notify();
        });
    });
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert_height(window, ids::PROGRESS, m::PROGRESS_H);
    })
    .expect("window");
}

#[gpui_kit::test]
fn status_pane_keeps_its_frame_bounds(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, seed(&[], &[]), DEFAULT_SIZE);
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let pane = bounds(window, ids::STATUS_PANE);
        assert_eq!(pane.size.width.as_f32(), metrics::RIGHT_PANE_W);
        assert_rect(
            window,
            ids::STATUS_PANE,
            pane.origin.x.as_f32(),
            pane.origin.y.as_f32(),
            metrics::RIGHT_PANE_W,
            pane.size.height.as_f32(),
        );
    })
    .expect("window");
    let _ = status(vec![], vec![]);
}

#[gpui_kit::test]
fn rebase_edit_stop_amends_and_continues(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("first");
    fx.write("b.txt", "two\n");
    fx.commit_all("second");
    git_ignoring_failure(
        &fx,
        &[
            "-c",
            "sequence.editor=sed -i -e 1s/^pick/edit/",
            "rebase",
            "-i",
            "HEAD~1",
        ],
    );
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    assert!(
        cx.update(|cx| tab.read(cx).ctx().ui.read(cx).amend),
        "an edit stop starts with Amend ticked"
    );
    assert!(
        message(cx, &tab).starts_with("second"),
        "the message of the stopped commit: {:?}",
        message(cx, &tab)
    );
    // Edit the message, then Continue: amend first, then `rebase --continue`.
    type_message(cx, handle, &tab, " (edited)");
    render(cx, handle);
    run_in_tab(cx, handle, |window, cx| {
        window.click(commit_box::ids::PRIMARY, cx)
    });
    cx.run_until_parked();
    render(cx, handle);
    assert_eq!(fx.git(&["rev-list", "--count", "HEAD"]).trim(), "2");
    assert!(
        fx.git(&["log", "-1", "--format=%s"]).contains("(edited)"),
        "the amended message is in the rebased commit: {}",
        fx.git(&["log", "-1", "--format=%s"])
    );
    assert!(
        !fx.path().join(".git").join("rebase-merge").exists(),
        "the rebase is finished"
    );
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(commit_box::ids::ABORT).is_none(),
            "back to the SAFE layout"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn cherry_pick_commit_button_concludes_the_pick(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("c.txt", "base\n");
    fx.commit_all("base");
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("c.txt", "theirs\n");
    fx.commit_all("picked commit");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("c.txt", "ours\n");
    fx.commit_all("ours");
    git_ignoring_failure(&fx, &["cherry-pick", "other"]);
    fx.write("c.txt", "resolved\n");
    fx.git(&["add", "c.txt"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    assert!(
        message(cx, &tab).starts_with("picked commit"),
        "the prepared message: {:?}",
        message(cx, &tab)
    );
    run_in_tab(cx, handle, |window, cx| {
        window.click(commit_box::ids::PRIMARY, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        fx.git(&["log", "-1", "--format=%s"]).trim(),
        "picked commit"
    );
    assert!(
        !fx.path().join(".git").join("CHERRY_PICK_HEAD").exists(),
        "the pick is concluded"
    );
}

/// A rebase that stops on a conflict again after Continue: the read-only box shows the new HEAD's message, not
/// the empty text it was cleared to (the adopt key does not change between two conflict stops).
#[gpui_kit::test]
fn read_only_rebase_step_follows_head_across_stops(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("c.txt", "base\n");
    fx.commit_all("base");
    fx.git(&["checkout", "-q", "-b", "topic"]);
    fx.write("c.txt", "topic one\n");
    fx.commit_all("topic one");
    fx.write("c.txt", "topic two\n");
    fx.commit_all("topic two");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("c.txt", "main\n");
    fx.commit_all("main side");
    fx.git(&["checkout", "-q", "topic"]);
    git_ignoring_failure(&fx, &["rebase", "main"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    render(cx, handle);
    assert!(
        message(cx, &tab).starts_with("main side"),
        "the first stop shows HEAD's message: {:?}",
        message(cx, &tab)
    );
    fx.write("c.txt", "resolved one\n");
    fx.git(&["add", "c.txt"]);
    cx.update(|cx| tab.update(cx, |t, cx| t.reload(crate::backend::RefreshSet::STATUS, cx)));
    render(cx, handle);
    run_in_tab(cx, handle, |window, cx| {
        window.click(commit_box::ids::PRIMARY, cx)
    });
    cx.run_until_parked();
    render(cx, handle);
    assert!(
        fx.path().join(".git").join("rebase-merge").exists()
            || fx.path().join(".git").join("rebase-apply").exists(),
        "the rebase stopped again"
    );
    assert!(
        message(cx, &tab).starts_with("topic one"),
        "the second stop shows the new HEAD's message: {:?}",
        message(cx, &tab)
    );
    assert!(cx.update(|cx| box_of(&tab, cx).read(cx).is_read_only()));
}

/// Git's MERGE_MSG has `#<TAB>path` lines; the field shows the gap (the kit draws a tab with no width) and the
/// committed message still has the tabs.
#[gpui_kit::test]
fn tab_characters_are_shown_and_kept(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed(&[("a.txt", ChangeKind::Added)], &[]), DEFAULT_SIZE);
    let commit_box = cx.update(|cx| box_of(&tab, cx));
    let original = "Merge branch 'dev'\n\n# Conflicts:\n#\tdocs/guide.md\n";
    cx.update_window(handle, |_, window, cx| {
        commit_box.update(cx, |b, cx| b.set_message(original.to_owned(), window, cx));
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
    let shown = cx.update(|cx| commit_box.read(cx).shown_text(cx));
    assert!(
        shown.contains("#   docs/guide.md") && !shown.contains('\t'),
        "the field shows the gap: {shown:?}"
    );
    assert_eq!(message(cx, &tab), original, "the message keeps its tabs");
}
