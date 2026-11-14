use core::prelude::v1::test;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, Bounds, Context, Entity, Pixels, ScrollDelta, TestAppContext, Window, point,
    px, size,
};
use parking_lot::Mutex;

use super::chips::{self, ChipKind};
use super::graph::{RowGeom, Seg, emit_row, graph_mask};
use super::paging::{append_page, needs_page};
use super::search::{find_matches, haystack, step};
use super::*;
use crate::backend::present::OpFailure;
use crate::backend::{
    CancelToken, ChangeKind, Decorations, GitError, HeadState, InProgress, LocalBranch, MergeOut,
    NodeKind, ObjectKind, OpOutcome, RefList, RefreshSet, RemoteBranch, RemoteInfo,
    SequenceOutcome, TagInfo, Upstream,
};
use crate::menus::{MenuItem, MenuKind, MenuSlot, MenuTarget};
use crate::tab::RepoTab;
use crate::tab::model::{ChunkedRows, GraphCell, LogSnapshot};
use crate::tab::ops::OpKind;
use crate::test_support::geometry::{assert_height, assert_rect, assert_width, bounds};
use crate::test_support::seed::{full, oid};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, TestCall, calls, mount_fixture, mount_seeded, script_op,
    toast_texts,
};

// ── helpers ─────────────────────────────────────────────────────────────────────────────────────────

fn view_of(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Entity<LogView> {
    cx.update(|cx| tab.read(cx).views().log.clone())
}

fn frame<R>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut gpui_kit::App) -> R,
) -> R {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        f(window, cx)
    })
    .expect("window")
}

fn run_on<T>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    f: impl FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>) -> T,
) -> T {
    let result = cx
        .update_window(handle, |_, window, cx| {
            tab.update(cx, |t, cx| f(t, window, cx))
        })
        .expect("window");
    cx.run_until_parked();
    result
}

fn log_origin(window: &Window) -> Bounds<Pixels> {
    bounds(window, ids::LOG_VIEW)
}

fn ops(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<(OpKind, Option<String>)> {
    cx.update(|cx| calls(tab, cx))
        .into_iter()
        .filter_map(|c| match c {
            TestCall::Op { kind, title } => Some((kind, title)),
            _ => None,
        })
        .collect()
}

fn ui_of(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Entity<crate::tab::RepoUi> {
    cx.update(|cx| tab.read(cx).ctx().ui.clone())
}

fn model_of(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Entity<crate::tab::RepoModel> {
    cx.update(|cx| tab.read(cx).ctx().model.clone())
}

fn geom(top: f32) -> RowGeom {
    RowGeom {
        x0: px(0.),
        top: px(top),
        h: px(38.),
        pitch: px(30.),
        half: px(15.),
        offset: px(2.),
    }
}

fn segments(g: &GraphCell, show: bool, top: f32) -> Vec<(usize, Seg)> {
    let mut out = Vec::new();
    emit_row(g, show, &geom(top), &mut out);
    out
}

fn endpoints(segs: &[(usize, Seg)]) -> Vec<(f32, f32)> {
    segs.iter()
        .map(|(_, s)| match s {
            Seg::MoveTo(p) | Seg::LineTo(p) | Seg::ArcTo { to: p, .. } => {
                (p.x.as_f32(), p.y.as_f32())
            }
        })
        .collect()
}

/// A seed whose commits use lanes up to `max_lane` (graph wider than the column).
fn wide_seed(max_lane: u16) -> RepoSeed {
    let mut seed = RepoSeed::linear(4);
    for (i, row) in seed.log_rows.iter_mut().enumerate().skip(1) {
        row.graph.passing = (1..=max_lane).collect();
        if i == 1 {
            row.graph.node_lane = 0;
        }
    }
    seed
}

// ── layout (21 §8.2) ────────────────────────────────────────────────────────────────────────────────

/// 21 §8.2: header 34; "Message" at graphWidth + 24; rows 38; the first row is the uncommitted row when changes
/// exist; graph column 120; node box 30×30 at x = 30(lane+1) − 15; date right edge = width − 4 − 16 + 16.
#[gpui_kit::test]
fn header_and_rows_geometry(cx: &mut TestAppContext) {
    let seed = RepoSeed::linear(3).with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    let (handle, _app, _tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        let o = log_origin(window);
        let (x0, y0, w) = (
            o.origin.x.as_f32(),
            o.origin.y.as_f32(),
            o.size.width.as_f32(),
        );
        // Literal spec values (08 §2.1, §2.4, §0.3), not the constants the view renders with.
        assert_rect(window, ids::HEADER, x0, y0, w, 34.);
        assert_rect(window, ids::HEADER_GRAPH, x0, y0, 120., 34.);
        assert_rect(window, ids::HEADER_DIVIDER, x0 + 120., y0, 8., 34.);
        let message = bounds(window, ids::HEADER_MESSAGE);
        assert_eq!(
            message.origin.x.as_f32() + 16.,
            x0 + 120. + 24.,
            "the Message label starts at graphWidth + 24 (08 §2.4)"
        );
        let search = bounds(window, ids::SEARCH_BUTTON);
        assert!(
            (search.origin.x + search.size.width).as_f32() <= x0 + w - 8. + 0.5,
            "the search button keeps 8 dp from the right edge"
        );
        let top = y0 + 34.;
        assert_rect(window, (ids::LOG_ROW, 0usize), x0, top, w, 38.);
        assert_rect(window, (ids::LOG_ROW, 1usize), x0, top + 38., w, 38.);
        assert!(
            window.try_find(ids::UNCOMMITTED_LABEL).is_some(),
            "the first row is the uncommitted row"
        );
        // Node of layout row 2 (lane 0): 30×30 at x = 30 × 1 − 15, vertically centred.
        let row2_top = top + 2. * 38.;
        assert_rect(
            window,
            (ids::NODE, 2usize),
            x0 + 15.,
            row2_top + 4.,
            30.,
            30.,
        );
        // Row 2 has no chips: the message element starts after the 16 dp chip padding (text at + 8 more).
        let msg = bounds(window, (ids::MESSAGE, 2usize));
        assert_eq!(
            msg.origin.x.as_f32() + 8.,
            x0 + 120. + 24.,
            "message text at graphWidth + 24 (08 §2.7.2)"
        );
        let date = bounds(window, (ids::DATE, 2usize));
        assert!(
            ((date.origin.x + date.size.width).as_f32() - (x0 + w - 4.)).abs() <= 0.5,
            "the date box (16 dp padding inside) ends 4 dp before the row's right edge"
        );
        let divider = bounds(window, ids::LIST_DIVIDER);
        assert_eq!(divider.origin.x.as_f32(), x0 + 120.);
        assert_width(window, ids::LIST_DIVIDER, 8.);
        assert_height(window, ids::LIST_AREA, o.size.height.as_f32() - 34.);
    });
}

#[gpui_kit::test]
fn compact_rows_are_34(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update(|cx| {
        crate::settings::update(cx, |s| {
            s.lines_height = crate::settings::LinesHeight::Compact
        })
    });
    frame(cx, handle, |window, _| {
        assert_height(window, (ids::LOG_ROW, 1usize), 34.);
        assert_height(window, (ids::LOG_ROW, 2usize), 34.);
    });
}

/// 21 §8.2: the divider drag changes the width and clamps at 100; the clamped accumulator leaves no dead zone
/// (fix of 08 §9.3).
#[gpui_kit::test]
fn graph_width_clamp_and_drag(cx: &mut TestAppContext) {
    assert_eq!(graph_width_from(120., 0.), 120.);
    assert_eq!(graph_width_from(120., -50.), 100.);
    assert_eq!(clamp_padding_from(120., -50.), -20.);
    assert_eq!(graph_real_width(120., None), 120., "empty log: 2 + 2 lanes");
    assert_eq!(graph_real_width(120., Some(0)), 120.);
    assert_eq!(graph_real_width(120., Some(6)), 240.);
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    let centre = frame(cx, handle, |window, _| {
        bounds(window, ids::HEADER_DIVIDER).center()
    });
    frame(cx, handle, |window, cx| {
        window.drag(centre, point(centre.x - px(50.), centre.y), cx);
    });
    frame(cx, handle, |window, _| {
        assert_width(window, ids::HEADER_GRAPH, 100.);
    });
    assert_eq!(
        cx.update(|cx| ui.read(cx).graph_padding),
        -20.,
        "the accumulator is clamped"
    );
    let centre = frame(cx, handle, |window, _| {
        bounds(window, ids::LIST_DIVIDER).center()
    });
    frame(cx, handle, |window, cx| {
        window.drag(centre, point(centre.x + px(30.), centre.y), cx);
    });
    frame(cx, handle, |window, _| {
        assert_width(window, ids::HEADER_GRAPH, 130.);
        let o = log_origin(window);
        assert_eq!(
            bounds(window, ids::LIST_DIVIDER).origin.x.as_f32(),
            o.origin.x.as_f32() + 130.,
            "the list divider follows"
        );
    });
}

// ── uncommitted row (08 §2.8) ───────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn uncommitted_row_labels_per_state(cx: &mut TestAppContext) {
    assert_eq!(uncommitted_label(None), "Uncommitted changes");
    assert_eq!(
        uncommitted_label(Some(InProgress::Merge)),
        "Pending changes to merge"
    );
    for s in [InProgress::Rebase, InProgress::RebaseInteractive] {
        assert_eq!(uncommitted_label(Some(s)), "Pending changes to rebase");
    }
    for s in [InProgress::CherryPick, InProgress::CherryPickSequence] {
        assert_eq!(uncommitted_label(Some(s)), "Pending changes to cherry-pick");
    }
    for s in [InProgress::Revert, InProgress::RevertSequence] {
        assert_eq!(uncommitted_label(Some(s)), "Pending changes to revert");
    }
    let seed = RepoSeed::linear(2).with_changes(
        &[("s.txt", ChangeKind::Added), ("r.txt", ChangeKind::Renamed)],
        &[
            ("a.txt", ChangeKind::Modified),
            ("b.txt", ChangeKind::Untracked),
            ("c.txt", ChangeKind::Deleted),
        ],
    );
    assert_eq!(
        summary_counts(Some(&seed.status)),
        [1, 2, 1, 0],
        "modified, added (incl. untracked), deleted, conflicting; renames are not counted"
    );
    // A revert with a clean worktree shows the row (fix of 08 §9.10).
    let (handle, _app, _tab) = mount_seeded(
        cx,
        RepoSeed::linear(2).in_state(InProgress::Revert),
        DEFAULT_SIZE,
    );
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find((ids::LOG_ROW, 0usize)).is_some(),
            "the row is shown"
        );
        assert!(
            window.try_find((ids::SUMMARY, 0usize)).is_none(),
            "no summary entry without changes"
        );
    });
}

#[gpui_kit::test]
fn uncommitted_summary_entries_render_in_order(cx: &mut TestAppContext) {
    let seed = RepoSeed::linear(2).with_changes(
        &[],
        &[
            ("a.txt", ChangeKind::Modified),
            ("b.txt", ChangeKind::Untracked),
            ("c.txt", ChangeKind::Conflicted),
        ],
    );
    let (handle, _app, _tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        let modified = bounds(window, (ids::SUMMARY, 0usize));
        let added = bounds(window, (ids::SUMMARY, 1usize));
        let conflicting = bounds(window, (ids::SUMMARY, 3usize));
        assert!(
            window.try_find((ids::SUMMARY, 2usize)).is_none(),
            "no deleted entry"
        );
        assert!(modified.origin.x < added.origin.x && added.origin.x < conflicting.origin.x);
    });
}

/// 20 §3.5: hidden row => HEAD's `line_up` is dropped when it exists only for the uncommitted row and the link lane
/// is not drawn; the list starts with layout row 1.
#[gpui_kit::test]
fn hidden_uncommitted_row_skips_link_lane_and_line_up(cx: &mut TestAppContext) {
    let mut head = GraphCell::node(0, NodeKind::Commit);
    head.line_up = true;
    head.line_down = true;
    head.line_up_from_uncommitted_only = true;
    let shown = segments(&head, true, 0.);
    let hidden = segments(&head, false, 0.);
    assert!(
        shown.contains(&(0, Seg::LineTo(point(px(30.), px(19. - 15.))))),
        "line_up ends at the node box while shown"
    );
    assert!(
        !hidden
            .iter()
            .any(|(_, s)| *s == Seg::MoveTo(point(px(30.), px(0.)))),
        "no line_up while hidden"
    );
    let mut above = GraphCell::node(1, NodeKind::Commit);
    above.passing.push(0);
    above.uncommitted_link_lane = Some(0);
    assert!(
        segments(&above, true, 0.)
            .iter()
            .any(|(c, s)| *c == 0 && *s == Seg::LineTo(point(px(30.), px(38.)))),
        "the link lane is drawn while shown"
    );
    assert!(
        segments(&above, false, 0.).iter().all(|(c, _)| *c != 0),
        "the link lane is not drawn while hidden"
    );
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        let o = log_origin(window);
        assert!(
            window.try_find((ids::LOG_ROW, 0usize)).is_none(),
            "row 0 is hidden"
        );
        assert_eq!(
            bounds(window, (ids::LOG_ROW, 1usize)).origin.y.as_f32(),
            o.origin.y.as_f32() + metrics::HEADER_BAR_H,
            "layout row 1 is the first visible row"
        );
    });
}

// ── graph painting (08 §2.9) ───────────────────────────────────────────────────────────────────────

#[test]
fn graph_shapes_paint_segments() {
    let rows = RepoSeed::branchy().log_rows;
    // Merge row: lane 0 with a merge-out to lane 1 (curve from the node edge down to the bottom of lane 1).
    let merge = segments(&rows[1].graph, true, 0.);
    let to_lane1: Vec<Seg> = merge
        .iter()
        .filter(|(c, _)| *c == 1)
        .map(|(_, s)| *s)
        .collect();
    assert_eq!(
        to_lane1,
        vec![
            Seg::MoveTo(point(px(45.), px(19.))),
            Seg::LineTo(point(px(60. - 7.5), px(19.))),
            Seg::ArcTo {
                radii: point(px(7.5), px(7.5)),
                sweep: true,
                to: point(px(60.), px(19. + 7.5)),
            },
            Seg::LineTo(point(px(60.), px(38.))),
        ],
        "merge-out: horizontal, quarter ellipse down, vertical (colour of the other lane)"
    );
    // Fork row: lanes 1 and 2 end in the node with curves from the top.
    let fork = segments(&rows[5].graph, true, 0.);
    let to_lane2: Vec<Seg> = fork
        .iter()
        .filter(|(c, _)| *c == 2)
        .map(|(_, s)| *s)
        .collect();
    assert_eq!(
        to_lane2,
        vec![
            Seg::MoveTo(point(px(45.), px(19.))),
            Seg::LineTo(point(px(90. - 7.5), px(19.))),
            Seg::ArcTo {
                radii: point(px(7.5), px(7.5)),
                sweep: false,
                to: point(px(90.), px(19. - 7.5)),
            },
            Seg::LineTo(point(px(90.), px(0.))),
        ],
        "fork-in: horizontal, quarter ellipse up, vertical to the top"
    );
    // Stash row: passing lanes 0 and 1 full height, line down on lane 2.
    let stash = segments(&rows[4].graph, true, 100.);
    for (lane, x) in [(0, 30.), (1, 60.)] {
        assert!(stash.contains(&(lane, Seg::MoveTo(point(px(x), px(100.))))));
        assert!(stash.contains(&(lane, Seg::LineTo(point(px(x), px(138.))))));
    }
    assert!(stash.contains(&(2, Seg::MoveTo(point(px(90.), px(119. + 15.))))));
    // Both fork-ins and merge-outs: the horizontals are 2 dp apart.
    let mut both = GraphCell::node(1, NodeKind::Merge);
    both.fork_ins.push(2);
    both.merge_outs.push(MergeOut {
        lane: 0,
        joins_existing: false,
    });
    let segs = segments(&both, true, 0.);
    assert!(
        segs.contains(&(2, Seg::MoveTo(point(px(75.), px(17.))))),
        "fork at cy − 2"
    );
    assert!(
        segs.contains(&(0, Seg::MoveTo(point(px(45.), px(21.))))),
        "merge at cy + 2"
    );
    assert!(
        segs.iter().any(|(c, s)| *c == 0
            && matches!(s, Seg::ArcTo { radii, sweep: false, .. } if *radii == point(px(7.5), px(6.5)))),
        "ry = (15 − d) / 2; a merge to the left bends counter-clockwise"
    );
}

/// Every segment stops at the 30 dp node box (the decoration paints over the node elements).
#[test]
fn node_clearance() {
    let mut cells: Vec<GraphCell> = RepoSeed::branchy()
        .log_rows
        .into_iter()
        .map(|r| r.graph)
        .collect();
    let mut both = GraphCell::node(1, NodeKind::Merge);
    both.line_up = true;
    both.line_down = true;
    both.fork_ins.extend([0, 3]);
    both.merge_outs.push(MergeOut {
        lane: 2,
        joins_existing: true,
    });
    cells.push(both);
    for g in &cells {
        let xn = 30. * (f32::from(g.node_lane) + 1.);
        for (x, y) in endpoints(&segments(g, true, 0.)) {
            let inside = (x - xn).abs() < 15. - 0.01 && (y - 19.).abs() < 15. - 0.01;
            assert!(
                !inside,
                "({x}, {y}) is inside the node box of lane {}",
                g.node_lane
            );
        }
    }
}

/// The clip mask is the viewport's graph column, also after scrolling (never the canvas bounds).
#[gpui_kit::test]
fn decoration_mask_is_viewport_after_scroll(cx: &mut TestAppContext) {
    let canvas = Bounds::new(point(px(10.), px(100. - 5000.)), size(px(500.), px(400.)));
    let mask = graph_mask(canvas, point(px(0.), px(-5000.)), px(120.));
    assert_eq!(
        mask,
        Bounds::new(point(px(10.), px(100.)), size(px(120.), px(400.)))
    );
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(400), DEFAULT_SIZE);
    let view = view_of(&tab, cx);
    frame(cx, handle, |_, cx| {
        view.read(cx)
            .scroll_handle()
            .scroll_to_item_strict(300, gpui_kit::ScrollStrategy::Top);
    });
    frame(cx, handle, |window, _| {
        let area = bounds(window, ids::LIST_AREA);
        let row = bounds(window, (ids::LOG_ROW, 301usize));
        assert_eq!(
            row.origin.y, area.origin.y,
            "row 301 (visible 300) is at the top"
        );
        let node = bounds(window, (ids::NODE, 301usize));
        assert!(
            area.contains(&node.center()),
            "its node is inside the viewport"
        );
    });
}

/// 08 §2.1 LOG_BOTTOM_PADDING: the 80 dp after the last row belong to the scrolled content, not to the list's
/// bounds. At the top of a long log rows are drawn down to the list's bottom edge (the padding must not hide the
/// bottom 80 dp), and after scrolling to the end the last row stops 80 dp (rounded up to whole 38 dp rows: 114)
/// above the bottom edge.
#[gpui_kit::test]
fn bottom_padding_is_scroll_content_not_list_padding(cx: &mut TestAppContext) {
    assert_eq!(bottom_padding_rows(500, 38.), 3, "80 dp = 3 rows of 38 dp");
    assert_eq!(bottom_padding_rows(500, 34.), 3, "80 dp = 3 rows of 34 dp");
    assert_eq!(bottom_padding_rows(0, 38.), 0, "no padding without rows");
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(200), DEFAULT_SIZE);
    let last_rendered = |window: &mut Window| {
        (0..201usize)
            .filter(|ix| window.try_find((ids::LOG_ROW, *ix)).is_some())
            .map(|ix| (ix, bounds(window, (ids::LOG_ROW, ix))))
            .next_back()
            .expect("rows are rendered")
    };
    frame(cx, handle, |window, _| {
        let area = bounds(window, ids::LIST_AREA);
        let (_, row) = last_rendered(window);
        assert!(
            (row.origin.y + row.size.height).as_f32() >= area.bottom().as_f32() - 0.5,
            "rows reach the bottom edge of the list at scroll 0 (no 80 dp dead zone)"
        );
    });
    frame(cx, handle, |window, cx| {
        window.scroll(
            ids::LIST_AREA,
            ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
            cx,
        );
    });
    frame(cx, handle, |_, _| {});
    frame(cx, handle, |window, _| {
        let area = bounds(window, ids::LIST_AREA);
        let (ix, row) = last_rendered(window);
        assert_eq!(ix, 200, "the last commit row is reachable");
        let gap = area.bottom().as_f32() - (row.origin.y + row.size.height).as_f32();
        assert!(
            (80. - 0.5..80. + 38.).contains(&gap),
            "at least 80 dp of blank padding after the last row, got {gap}"
        );
    });
}

// ── chips (08 §2.12) ────────────────────────────────────────────────────────────────────────────────

fn local(name: &str, target: u64, head: bool, upstream: Option<&str>) -> LocalBranch {
    LocalBranch {
        name: name.into(),
        full: full(&format!("refs/heads/{name}")),
        target: oid(target),
        is_head: head,
        upstream: upstream.map(|u| Upstream {
            remote: "origin".into(),
            merge: full(&format!("refs/heads/{name}")),
            tracking: Some(full(u)),
            gone: false,
        }),
        ahead_behind: None,
        checked_out_in: None,
    }
}

fn remote_branch(name: &str, target: u64) -> RemoteBranch {
    RemoteBranch {
        remote: "origin".into(),
        name: name.into(),
        full: full(&format!("refs/remotes/origin/{name}")),
        target: oid(target),
    }
}

fn tag(name: &str, target: u64, kind: ObjectKind) -> TagInfo {
    TagInfo {
        name: name.into(),
        full: full(&format!("refs/tags/{name}")),
        target: oid(target),
        peeled_kind: kind,
        tag_object: None,
        message_summary: None,
        tagger_time: None,
    }
}

fn refs_for_chips(head: HeadState) -> RefList {
    RefList {
        head,
        local: vec![
            local("feature", 1, false, None),
            local("main", 1, true, Some("refs/remotes/origin/main")),
            local("topic", 2, false, Some("refs/remotes/origin/topic")),
        ],
        remotes: vec![RemoteInfo {
            name: "origin".into(),
            fetch_url: None,
            push_url: None,
            head_branch: Some("main".into()),
            branches: vec![
                remote_branch("HEAD", 1),
                remote_branch("main", 1),
                remote_branch("other", 1),
                remote_branch("topic", 3),
            ],
        }],
        tags: vec![
            tag("v1", 1, ObjectKind::Commit),
            tag("tree-tag", 1, ObjectKind::Tree),
        ],
        stashes: Vec::new(),
        decorations: Decorations::default(),
    }
}

#[gpui_kit::test]
fn chips_order_and_grouping(cx: &mut TestAppContext) {
    let refs = refs_for_chips(HeadState::Branch {
        name: full("refs/heads/main"),
        target: oid(1),
    });
    let map = chips::build(&refs, true);
    let row: Vec<(ChipKind, String)> = map[&oid(1)]
        .iter()
        .map(|c| (c.kind.clone(), c.name.to_string()))
        .collect();
    assert_eq!(
        row,
        vec![
            (ChipKind::Tag, "v1".into()),
            (ChipKind::LocalBranch, "feature".into()),
            (ChipKind::LocalBranch, "main".into()),
            (ChipKind::RemoteBranch, "origin/other".into()),
        ],
        "tags, local branches, remotes; origin/main absorbed, origin/HEAD and tree tags hidden"
    );
    let main = &map[&oid(1)][2];
    assert!(main.is_current, "main is the current branch");
    assert_eq!(
        main.tracking.as_ref().map(|t| t.name.to_string()),
        Some("origin/main".into())
    );
    assert!(
        map[&oid(2)][0].tracking.is_none() && map[&oid(3)][0].kind == ChipKind::RemoteBranch,
        "an upstream on another commit is not grouped"
    );
    let ungrouped = chips::build(&refs, false);
    assert_eq!(
        ungrouped[&oid(1)].len(),
        5,
        "without grouping origin/main is its own chip"
    );
    let detached = chips::build(
        &refs_for_chips(HeadState::Detached { target: oid(3) }),
        true,
    );
    assert_eq!(detached[&oid(3)][0].kind, ChipKind::DetachedHead);
    assert_eq!(detached[&oid(3)][0].name.as_ref(), "HEAD");

    // In the view: chips start 16 + 4 dp into the message area; stash rows have none.
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        let o = log_origin(window);
        let chip = bounds(window, ids::chip(5, 0));
        assert_eq!(
            chip.origin.x.as_f32(),
            o.origin.x.as_f32() + 120. + 16. + 4.,
            "the v1.0 chip of 'Base': graph 120, chip row padding 16, chip margin 4"
        );
        assert_eq!(chip.size.height.as_f32(), 26.);
        assert!(
            window.try_find(ids::chip(4, 0)).is_none(),
            "the stash row shows no chips"
        );
        let msg = bounds(window, (ids::MESSAGE, 4usize));
        assert_eq!(
            msg.origin.x.as_f32(),
            o.origin.x.as_f32() + 120. + 16.,
            "stash rows keep the chip row's start padding (deviation: aligned with the other rows)"
        );
    });
}

#[gpui_kit::test]
fn chip_click_selects_row_double_click_checks_out(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    // Row 2 = 'Feature work' with the 'feature' branch chip.
    frame(cx, handle, |window, cx| window.click(ids::chip(2, 0), cx));
    assert_eq!(
        cx.update(|cx| ui.read(cx).selection.clone()),
        crate::tab::ui::Selection::Commit {
            id: oid(11),
            is_stash: false,
            scroll_to: false
        },
        "a click on a chip selects its row's commit"
    );
    frame(cx, handle, |window, cx| {
        window.double_click(ids::chip(2, 0), cx)
    });
    cx.run_until_parked();
    assert!(
        ops(&tab, cx)
            .iter()
            .any(|(k, _)| *k == OpKind::CheckoutBranch),
        "double-click checks the branch out: {:?}",
        ops(&tab, cx)
    );
}

// ── paging (08 §2.11) ───────────────────────────────────────────────────────────────────────────────

#[test]
fn paging_requests_at_threshold() {
    assert!(!needs_page(2001, 1801, false), "200 rows left: no request");
    assert!(needs_page(2001, 1802, false), "199 rows left: request");
    assert!(!needs_page(2001, 2001, true), "exhausted: never");
    let old = LogSnapshot {
        rows: ChunkedRows::new().appended(RepoSeed::linear(2).log_rows),
        max_lane: 3,
        exhausted: false,
        generation: 4,
    };
    let page = RepoSeed::linear(5).log_rows[1..].to_vec();
    let next = append_page(&old, 4, page, 1, true).expect("current generation");
    assert_eq!(next.rows.len(), 3 + 5);
    assert_eq!(next.max_lane, 3, "the graph never narrows");
    assert!(next.exhausted);
}

#[test]
fn stale_page_dropped_after_reload() {
    let old = LogSnapshot {
        rows: ChunkedRows::new().appended(RepoSeed::linear(2).log_rows),
        max_lane: 0,
        exhausted: false,
        generation: 5,
    };
    assert!(
        append_page(&old, 4, RepoSeed::linear(3).log_rows, 0, true).is_none(),
        "a page of generation 4 is dropped by a generation-5 snapshot"
    );
}

/// A fixture with `n` commits whose log snapshot is cut to the first `first` commits (not exhausted), with the
/// real pager of that snapshot.
fn fixture_with_cut_log(
    cx: &mut TestAppContext,
    n: usize,
    first: usize,
    generation: u64,
) -> (Fixture, AnyWindowHandle, Entity<RepoTab>) {
    let fx = Fixture::new();
    // One `git fast-import` instead of `n` processes (each `git commit` costs tens of ms on Windows).
    let mut stream = String::new();
    for i in 0..n {
        let message = format!("Commit {i}");
        stream.push_str(&format!(
            "commit refs/heads/main
committer Test User <test@example.com> {} +0000
data {}
{message}

",
            1_700_000_000 + i,
            message.len()
        ));
    }
    fx.git_stdin(&["fast-import", "--quiet"], &stream);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let model = model_of(&tab, cx);
    let repo = cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .backend
            .repo()
            .cloned()
            .expect("real repo")
    });
    let cancel = CancelToken::new();
    let mut pager = crate::backend::log::LogPager::new(&repo, &cancel).expect("pager");
    let (rows, max_lane, exhausted) = pager.next_rows(first, &cancel).expect("rows");
    assert!(!exhausted, "the cut snapshot is not exhausted");
    model.update(cx, |m, cx| {
        m.log_pager = Some(Arc::new(Mutex::new(pager)));
        m.set_log(
            Arc::new(LogSnapshot {
                rows: ChunkedRows::new().appended(rows),
                max_lane,
                exhausted,
                generation,
            }),
            cx,
        );
    });
    (fx, handle, tab)
}

#[gpui_kit::test]
fn fixture_paging_appends_with_the_real_pager(cx: &mut TestAppContext) {
    let (_fx, handle, tab) = fixture_with_cut_log(cx, 30, 5, 77);
    frame(cx, handle, |_, _| {});
    cx.run_until_parked();
    frame(cx, handle, |_, _| {});
    cx.run_until_parked();
    let model = model_of(&tab, cx);
    let (len, exhausted, generation) = cx.update(|cx| {
        let s = model.read(cx).log.value.clone().expect("log");
        (s.rows.len(), s.exhausted, s.generation)
    });
    assert_eq!(len, 1 + 30, "the page appended the remaining commits");
    assert!(exhausted, "the walk is exhausted");
    assert_eq!(generation, 77, "same generation");
}

#[gpui_kit::test]
fn fixture_stale_page_is_dropped(cx: &mut TestAppContext) {
    let (_fx, handle, tab) = fixture_with_cut_log(cx, 30, 5, 77);
    let model = model_of(&tab, cx);
    let view = view_of(&tab, cx);
    let replacement = RepoSeed::linear(3).snapshot();
    cx.update_window(handle, |_, _, cx| {
        view.update(cx, |v, cx| v.request_page(1, cx));
        assert!(view.read(cx).paging.is_running(), "a page request runs");
        model.update(cx, |m, cx| {
            m.set_log(
                Arc::new(LogSnapshot {
                    generation: 78,
                    ..replacement.clone()
                }),
                cx,
            )
        });
    })
    .expect("window");
    cx.run_until_parked();
    let len = cx.update(|cx| model.read(cx).log.value.as_ref().map(|s| s.rows.len()));
    assert_eq!(
        len,
        Some(replacement.rows.len()),
        "the stale page was dropped"
    );
}

// ── search (08 §2.3) ────────────────────────────────────────────────────────────────────────────────

#[test]
fn search_matches_full_message_from_cache() {
    let rows = ChunkedRows::new().appended(RepoSeed::linear(3).log_rows);
    let mut cache = HashMap::new();
    assert!(find_matches(&rows, "needle", &cache).is_empty());
    let c = rows.get(2).and_then(|r| r.commit.clone()).expect("commit");
    cache.insert(c.id, haystack(&c, "Summary\n\nThe body has a NEEDLE in it"));
    assert_eq!(
        find_matches(&rows, "Needle", &cache),
        vec![2],
        "full message, case-insensitive"
    );
    assert_eq!(
        find_matches(&rows, "commit 3", &cache),
        vec![1],
        "summary without a cache entry"
    );
    assert_eq!(
        find_matches(&rows, "TEST@example", &cache).len(),
        3,
        "author e-mail"
    );
    assert_eq!(
        find_matches(&rows, "0000000000", &cache).len(),
        3,
        "hash prefix"
    );
    assert!(
        find_matches(&rows, "   ", &cache).is_empty(),
        "blank query: no matches"
    );
    assert_eq!(step(1, 3, -1), 1, "no wrap at the start");
    assert_eq!(step(3, 3, 1), 3, "no wrap at the end");
    assert_eq!(step(0, 0, 1), 0);
}

#[gpui_kit::test]
fn search_navigation_scrolls_without_select(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(300), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        window.click(ids::SEARCH_BUTTON, cx)
    });
    cx.run_until_parked();
    assert!(
        cx.update(|cx| ui
            .read(cx)
            .views
            .contains(crate::tab::ui::CloseableView::LogSearch)),
        "the search registered its closeable view"
    );
    frame(cx, handle, |window, cx| {
        let o = log_origin(window);
        assert_rect(
            window,
            ids::SEARCH_BAR,
            o.origin.x.as_f32(),
            o.origin.y.as_f32(),
            o.size.width.as_f32(),
            56.,
        );
        assert_eq!(
            bounds(window, ids::HEADER).origin.y.as_f32(),
            o.origin.y.as_f32() + 56.,
            "the search bar is above the header"
        );
        assert!(
            window.try_find((ids::MATCH_BAR, 1usize)).is_none(),
            "empty query dims nothing"
        );
        window.input("Commit 1", cx);
    });
    settle_search(cx);
    let (index, total) = cx.update(|cx| {
        let s = ui.read(cx).log_search.clone().expect("searching");
        (s.index, s.matches.len())
    });
    assert_eq!(index, 1, "the first match is current");
    assert_eq!(
        total, 111,
        "'Commit 1', 'Commit 10'..'Commit 19', 'Commit 100'..'Commit 199'"
    );
    let view = view_of(&tab, cx);
    for _ in 0..30 {
        view.update(cx, |v, cx| v.navigate(1, cx));
    }
    frame(cx, handle, |_, _| {});
    let (index, target) = cx.update(|cx| {
        let s = ui.read(cx).log_search.clone().expect("searching");
        (s.index, s.matches[s.index - 1])
    });
    assert_eq!(index, 31);
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find((ids::LOG_ROW, target)).is_some(),
            "the match is scrolled into view"
        );
        assert!(
            window.try_find((ids::MATCH_BAR, target)).is_some(),
            "matches get the bar"
        );
    });
    assert_eq!(
        cx.update(|cx| ui.read(cx).selection.clone()),
        crate::tab::ui::Selection::UncommittedChanges,
        "navigation never selects"
    );
    frame(cx, handle, |window, cx| window.press("escape", cx));
    cx.run_until_parked();
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::SEARCH_BAR).is_none(),
            "Escape closes the search"
        );
    });
}

/// Enter / Shift+Enter in the field move one match (21 §2.12 `LogSearchNext` / `LogSearchPrev`), not two.
#[gpui_kit::test]
fn search_enter_steps_one_match(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(30), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        window.click(ids::SEARCH_BUTTON, cx)
    });
    cx.run_until_parked();
    frame(cx, handle, |window, cx| window.input("Commit 1", cx));
    settle_search(cx);
    let index =
        |cx: &mut TestAppContext| cx.update(|cx| ui.read(cx).log_search.as_ref().map(|s| s.index));
    assert_eq!(index(cx), Some(1));
    frame(cx, handle, |window, cx| window.press("enter", cx));
    cx.run_until_parked();
    assert_eq!(index(cx), Some(2), "Enter moves to the next match once");
    frame(cx, handle, |window, cx| window.press("enter", cx));
    cx.run_until_parked();
    assert_eq!(index(cx), Some(3));
    frame(cx, handle, |window, cx| window.press("shift-enter", cx));
    cx.run_until_parked();
    assert_eq!(
        index(cx),
        Some(2),
        "Shift+Enter moves to the previous match once"
    );
}

/// Enter right after typing (before the debounce pause ends) steps through the typed query's matches, and the
/// pending scan does not later reset the index to the first match.
#[gpui_kit::test]
fn search_enter_before_the_pause_uses_the_typed_query(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(30), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        window.click(ids::SEARCH_BUTTON, cx)
    });
    cx.run_until_parked();
    frame(cx, handle, |window, cx| window.input("Commit 1", cx));
    cx.run_until_parked();
    let state = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = ui.read(cx).log_search.clone().expect("searching");
            (s.index, s.matches.len(), s.query.to_string())
        })
    };
    assert_eq!(state(cx).1, 0, "the scan is still pending");
    frame(cx, handle, |window, cx| window.press("enter", cx));
    cx.run_until_parked();
    let (index, total, query) = state(cx);
    assert!(total > 1, "several matches of 'Commit 1'");
    assert_eq!(query, "Commit 1");
    assert_eq!(
        index, 2,
        "Enter scanned the typed query and moved one match"
    );
    settle_search(cx);
    assert_eq!(state(cx).0, 2, "no late scan resets the index");
}

/// Lets the debounced scan of the typed query run.
fn settle_search(cx: &mut TestAppContext) {
    cx.run_until_parked();
    cx.executor()
        .advance_clock(search::QUERY_DEBOUNCE + Duration::from_millis(1));
    cx.run_until_parked();
}

/// The typed query is scanned once the user pauses (50 ms), never per keystroke: right after typing the matches
/// are not computed yet, a second keystroke restarts the pause, and clearing the field is immediate.
#[gpui_kit::test]
fn search_scan_is_debounced(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(30), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        window.click(ids::SEARCH_BUTTON, cx)
    });
    cx.run_until_parked();
    let matches = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            ui.read(cx)
                .log_search
                .clone()
                .expect("searching")
                .matches
                .len()
        })
    };
    frame(cx, handle, |window, cx| window.input("Commit 1", cx));
    cx.run_until_parked();
    assert_eq!(matches(cx), 0, "no scan before the pause");
    cx.executor().advance_clock(Duration::from_millis(30));
    frame(cx, handle, |window, cx| window.input("1", cx));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(30));
    cx.run_until_parked();
    assert_eq!(matches(cx), 0, "the second keystroke restarted the pause");
    cx.executor().advance_clock(Duration::from_millis(30));
    cx.run_until_parked();
    assert_eq!(matches(cx), 1, "'Commit 11' matches after the pause");
    frame(cx, handle, |window, cx| {
        for _ in 0..9 {
            window.press("backspace", cx);
        }
    });
    cx.run_until_parked();
    assert_eq!(matches(cx), 0, "clearing the query needs no pause");
}

#[gpui_kit::test]
fn fixture_search_matches_full_message(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.git(&["commit", "-q", "--allow-empty", "-m", "First"]);
    fx.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "Second\n\nBody with a needle",
    ]);
    fx.git(&["commit", "-q", "--allow-empty", "-m", "Third"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        window.click(ids::SEARCH_BUTTON, cx)
    });
    cx.run_until_parked();
    frame(cx, handle, |window, cx| window.input("NEEDLE", cx));
    settle_search(cx);
    let matches = cx.update(|cx| ui.read(cx).log_search.clone().expect("searching").matches);
    assert_eq!(*matches, vec![2], "'Second' matches through its body");
}

// ── scrolling (08 §2.10) ────────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn scroll_pill_and_scroll_to_top(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(200), DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::SCROLL_TOP).is_none(),
            "no pill at the top"
        );
    });
    frame(cx, handle, |window, cx| {
        window.scroll(
            ids::LIST_AREA,
            ScrollDelta::Pixels(point(px(0.), px(-400.))),
            cx,
        );
    });
    let view = view_of(&tab, cx);
    frame(cx, handle, |_, _| {});
    assert!(cx.update(|cx| view.read(cx).first_visible()) > 0);
    frame(cx, handle, |window, cx| {
        let area = bounds(window, ids::LIST_AREA);
        let pill = bounds(window, ids::SCROLL_TOP);
        assert_eq!(pill.origin.y.as_f32(), area.origin.y.as_f32() + 16.);
        assert!(
            (pill.center().x - area.center().x).abs() <= px(0.5),
            "the pill is centred"
        );
        window.click(ids::SCROLL_TOP, cx);
    });
    frame(cx, handle, |_, _| {});
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::SCROLL_TOP).is_none(),
            "back at the top"
        );
    });
    assert_eq!(cx.update(|cx| view.read(cx).first_visible()), 0);
}

#[gpui_kit::test]
fn scroll_to_commit_only_when_not_visible(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(200), DEFAULT_SIZE);
    let view = view_of(&tab, cx);
    let ui = ui_of(&tab, cx);
    frame(cx, handle, |_, _| {});
    // Visible already: no scroll.
    ui.update(cx, |ui, cx| {
        ui.select(
            crate::tab::ui::Selection::Commit {
                id: oid(3),
                is_stash: false,
                scroll_to: true,
            },
            cx,
        )
    });
    frame(cx, handle, |_, _| {});
    assert_eq!(cx.update(|cx| view.read(cx).first_visible()), 0);
    // Far away: scrolled to the top of the viewport (uncommitted row hidden: visible = layout − 1).
    ui.update(cx, |ui, cx| {
        ui.select(
            crate::tab::ui::Selection::Commit {
                id: oid(150),
                is_stash: false,
                scroll_to: true,
            },
            cx,
        )
    });
    frame(cx, handle, |_, _| {});
    frame(cx, handle, |window, _| {
        let area = bounds(window, ids::LIST_AREA);
        assert_eq!(
            bounds(window, (ids::LOG_ROW, 150usize)).origin.y,
            area.origin.y
        );
    });
}

/// The horizontal graph scroll moves nodes by pixels (fix of 08 §9.2), × 10 per wheel line, clamped.
#[gpui_kit::test]
fn hscroll_in_pixels(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, wide_seed(8), DEFAULT_SIZE);
    let view = view_of(&tab, cx);
    let before = frame(cx, handle, |window, _| {
        bounds(window, (ids::NODE, 2usize)).origin.x
    });
    frame(cx, handle, |window, cx| {
        window.scroll(ids::LIST_AREA, ScrollDelta::Lines(point(-1., 0.)), cx);
    });
    let after = frame(cx, handle, |window, _| {
        bounds(window, (ids::NODE, 2usize)).origin.x
    });
    assert_eq!(cx.update(|cx| view.read(cx).hscroll()), px(10.));
    assert_eq!(before - after, px(10.), "the node moved 10 px left");
    frame(cx, handle, |window, cx| {
        window.scroll(ids::LIST_AREA, ScrollDelta::Lines(point(-100., 0.)), cx);
    });
    frame(cx, handle, |_, _| {});
    assert_eq!(
        cx.update(|cx| view.read(cx).hscroll()),
        px((8. + 2.) * 30. - 120.),
        "clamped to graphRealWidth − graphWidth"
    );
    frame(cx, handle, |window, _| {
        assert_eq!(
            bounds(window, (ids::LOG_ROW, 2usize)).origin.y,
            bounds(window, ids::LIST_AREA).origin.y + px(38.),
            "a horizontal wheel does not scroll the list vertically"
        );
    });
}

#[gpui_kit::test]
fn uncommitted_row_appearing_keeps_scrolled_rows(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(200), DEFAULT_SIZE);
    let model = model_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        window.scroll(
            ids::LIST_AREA,
            ScrollDelta::Pixels(point(px(0.), px(-380.))),
            cx,
        );
    });
    frame(cx, handle, |_, _| {});
    let top_before = frame(cx, handle, |window, _| {
        bounds(window, (ids::LOG_ROW, 11usize)).origin.y
    });
    let dirty = RepoSeed::linear(200).with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    model.update(cx, |m, cx| m.set_status(Arc::new(dirty.status), cx));
    frame(cx, handle, |_, _| {});
    let top_after = frame(cx, handle, |window, _| {
        bounds(window, (ids::LOG_ROW, 11usize)).origin.y
    });
    assert_eq!(
        top_before, top_after,
        "the rows in view stay where they were"
    );
}

// ── menus (08 §2.13) ────────────────────────────────────────────────────────────────────────────────

fn menu_ids(items: &[MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            MenuItem::Action { id, .. } => id.to_string(),
            MenuItem::Separator => "-".into(),
        })
        .collect()
}

fn commit_target(
    tab: &Entity<RepoTab>,
    id: crate::backend::ObjectId,
    cx: &mut TestAppContext,
) -> MenuTarget {
    cx.update(|cx| MenuTarget {
        kind: MenuKind::Commit,
        tab: tab.read(cx).ctx().clone(),
        branch: None,
        upstream: None,
        commit: Some(id),
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    })
}

#[gpui_kit::test]
fn commit_menu_conditions(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    let head = commit_target(&tab, oid(1), cx);
    assert_eq!(
        menu_ids(&cx.update(|cx| crate::menus::log::commit(&head, cx))),
        [
            "log-amend",
            "-",
            "log-checkout",
            "log-create-branch",
            "log-create-tag",
            "-",
            "log-revert",
            "log-cherry-pick",
            "log-reset"
        ],
        "HEAD with a clean worktree: amend, no interactive rebase"
    );
    let older = commit_target(&tab, oid(2), cx);
    assert_eq!(
        menu_ids(&cx.update(|cx| crate::menus::log::commit(&older, cx))),
        [
            "log-checkout",
            "log-create-branch",
            "log-create-tag",
            "-",
            "log-rebase-interactive",
            "log-revert",
            "log-cherry-pick",
            "log-reset"
        ]
    );
    let model = model_of(&tab, cx);
    let dirty = RepoSeed::linear(3).with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    model.update(cx, |m, cx| m.set_status(Arc::new(dirty.status), cx));
    let items = cx.update(|cx| crate::menus::log::commit(&head, cx));
    assert!(
        !menu_ids(&items).contains(&"log-amend".to_owned()),
        "no amend entry with uncommitted changes"
    );
    // The amend entry selects the uncommitted changes and ticks Amend.
    let (handle, _app2, tab2) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    let head2 = commit_target(&tab2, oid(1), cx);
    let items = cx.update(|cx| crate::menus::log::commit(&head2, cx));
    let ui = ui_of(&tab2, cx);
    ui.update(cx, |ui, cx| {
        ui.select(
            crate::tab::ui::Selection::Commit {
                id: oid(1),
                is_stash: false,
                scroll_to: false,
            },
            cx,
        )
    });
    frame(cx, handle, |window, cx| {
        crate::menus::open_context_menu(window, cx, point(px(300.), px(300.)), items);
    });
    frame(cx, handle, |window, cx| {
        window.click("menu-item-log-amend", cx)
    });
    let (selection, amend) = cx.update(|cx| (ui.read(cx).selection.clone(), ui.read(cx).amend));
    assert_eq!(selection, crate::tab::ui::Selection::UncommittedChanges);
    assert!(amend, "Amend is ticked");
}

fn extender_item(_: &MenuTarget, items: &mut Vec<MenuItem>, _: &gpui_kit::App) {
    items.push(MenuItem::action("x-ext", "Extension", |_, _| {}));
}

#[gpui_kit::test]
fn commit_menu_end_slot_extended(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update(|cx| {
        crate::menus::register_extender(cx, MenuKind::Commit, MenuSlot::End, extender_item)
    });
    let target = commit_target(&tab, oid(2), cx);
    let ids = menu_ids(&cx.update(|cx| crate::menus::log::commit(&target, cx)));
    assert_eq!(
        &ids[ids.len() - 2..],
        ["log-reset", "x-ext"],
        "extenders come at the end, and the menu does not end in a separator"
    );
}

#[gpui_kit::test]
fn row_menu_opens_commit_and_stash_menus(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| {
        window.right_click((ids::MESSAGE, 3usize), cx)
    });
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find("menu-item-log-checkout").is_some(),
            "the commit menu opened"
        );
    });
}

#[gpui_kit::test]
fn clicking_rows_selects(cx: &mut TestAppContext) {
    let seed = RepoSeed::branchy().with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        window.click((ids::MESSAGE, 3usize), cx)
    });
    assert_eq!(
        cx.update(|cx| ui.read(cx).selection.clone()),
        crate::tab::ui::Selection::Commit {
            id: oid(12),
            is_stash: false,
            scroll_to: false
        }
    );
    frame(cx, handle, |window, cx| {
        window.click((ids::MESSAGE, 4usize), cx)
    });
    assert_eq!(
        cx.update(|cx| ui.read(cx).selection.clone()),
        crate::tab::ui::Selection::Stash {
            id: oid(13),
            index: 0
        },
        "a stash row selects the stash"
    );
    frame(cx, handle, |window, cx| {
        window.click(ids::UNCOMMITTED_LABEL, cx)
    });
    assert_eq!(
        cx.update(|cx| ui.read(cx).selection.clone()),
        crate::tab::ui::Selection::UncommittedChanges
    );
}

fn press(cx: &mut TestAppContext, handle: AnyWindowHandle, key: &str) {
    frame(cx, handle, |window, cx| window.press(key, cx));
    cx.run_until_parked();
}

fn commit_selection(id: ObjectId) -> crate::tab::ui::Selection {
    crate::tab::ui::Selection::Commit {
        id,
        is_stash: false,
        scroll_to: false,
    }
}

/// Up / Down move the selection to the neighbouring row after a click on a row (stash rows select the stash, the
/// uncommitted row the uncommitted changes); at either end of the list the selection stays put.
#[gpui_kit::test]
fn arrow_keys_move_the_commit_selection(cx: &mut TestAppContext) {
    let seed = RepoSeed::branchy().with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    let selection = |cx: &mut TestAppContext| cx.update(|cx| ui.read(cx).selection.clone());
    frame(cx, handle, |window, cx| {
        window.click((ids::MESSAGE, 3usize), cx)
    });
    assert_eq!(selection(cx), commit_selection(oid(12)));
    press(cx, handle, "down");
    assert_eq!(
        selection(cx),
        crate::tab::ui::Selection::Stash {
            id: oid(13),
            index: 0
        },
        "Down onto a stash row selects the stash"
    );
    press(cx, handle, "up");
    press(cx, handle, "up");
    assert_eq!(selection(cx), commit_selection(oid(11)));
    press(cx, handle, "up");
    press(cx, handle, "up");
    assert_eq!(selection(cx), crate::tab::ui::Selection::UncommittedChanges);
    press(cx, handle, "up");
    assert_eq!(
        selection(cx),
        crate::tab::ui::Selection::UncommittedChanges,
        "Up on the first row keeps it"
    );
    press(cx, handle, "down");
    assert_eq!(selection(cx), commit_selection(oid(10)));
    for _ in 0..6 {
        press(cx, handle, "down");
    }
    assert_eq!(
        selection(cx),
        commit_selection(oid(15)),
        "Down on the last row keeps it"
    );
}

/// Without the uncommitted row Up stops at the first commit; a click on a chip also gives the list the focus.
#[gpui_kit::test]
fn arrow_keys_after_a_chip_click_stop_at_the_first_commit(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    let selection = |cx: &mut TestAppContext| cx.update(|cx| ui.read(cx).selection.clone());
    frame(cx, handle, |window, cx| window.click(ids::chip(2, 0), cx));
    assert_eq!(selection(cx), commit_selection(oid(11)));
    press(cx, handle, "down");
    assert_eq!(selection(cx), commit_selection(oid(12)));
    press(cx, handle, "up");
    press(cx, handle, "up");
    press(cx, handle, "up");
    assert_eq!(
        selection(cx),
        commit_selection(oid(10)),
        "the hidden uncommitted row is not selectable"
    );
}

/// The commit of layout row `ix` of the current log snapshot.
fn commit_at(tab: &Entity<RepoTab>, ix: usize, cx: &mut TestAppContext) -> ObjectId {
    let model = model_of(tab, cx);
    cx.update(|cx| {
        let log = model.read(cx).log.value.clone().expect("log");
        log.rows
            .get(ix)
            .and_then(|r| r.commit.as_ref())
            .expect("a commit row")
            .id
    })
}

/// Whether layout row `ix` lies entirely inside the list's viewport.
fn row_visible(cx: &mut TestAppContext, handle: AnyWindowHandle, ix: usize) -> bool {
    frame(cx, handle, |window, _| {
        let area = bounds(window, ids::LIST_AREA);
        window.try_find((ids::LOG_ROW, ix)).is_some_and(|_| {
            let row = bounds(window, (ids::LOG_ROW, ix));
            row.top() >= area.top() && row.bottom() <= area.bottom()
        })
    })
}

/// Arrows past the viewport's edge scroll the selected row into view, in both directions.
#[gpui_kit::test]
fn arrow_keys_keep_the_selection_visible(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(200), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    let view = view_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        window.click((ids::MESSAGE, 1usize), cx)
    });
    assert!(
        !row_visible(cx, handle, 40),
        "row 40 starts below the viewport"
    );
    for _ in 0..39 {
        press(cx, handle, "down");
    }
    assert_eq!(
        cx.update(|cx| ui.read(cx).selection.clone()),
        commit_selection(commit_at(&tab, 40, cx))
    );
    assert!(
        row_visible(cx, handle, 40),
        "the selected row scrolled into view"
    );
    assert!(cx.update(|cx| view.read(cx).first_visible()) > 0);
    for _ in 0..39 {
        press(cx, handle, "up");
    }
    assert!(
        row_visible(cx, handle, 1),
        "back up: the first row is visible"
    );
    assert_eq!(cx.update(|cx| view.read(cx).first_visible()), 0);
}

/// Walking down with the arrows loads the next page once fewer than `PAGE_THRESHOLD` loaded rows remain below the
/// viewport (the cut snapshot is long enough not to page right away).
#[gpui_kit::test]
fn arrow_keys_load_the_next_page(cx: &mut TestAppContext) {
    let (_fx, handle, tab) = fixture_with_cut_log(cx, 260, 230, 77);
    let model = model_of(&tab, cx);
    let loaded = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let log = model.read(cx).log.value.clone().expect("log");
            (log.rows.len(), log.exhausted)
        })
    };
    frame(cx, handle, |_, _| {});
    cx.run_until_parked();
    assert_eq!(loaded(cx), (1 + 230, false), "no page at the top");
    frame(cx, handle, |window, cx| {
        window.click((ids::MESSAGE, 1usize), cx)
    });
    for _ in 0..40 {
        press(cx, handle, "down");
    }
    frame(cx, handle, |_, _| {});
    cx.run_until_parked();
    assert_eq!(loaded(cx), (1 + 260, true), "the arrows paged in the rest");
    assert!(row_visible(cx, handle, 41));
}

/// The arrows do nothing while the list does not have the focus.
#[gpui_kit::test]
fn arrow_keys_need_the_focus(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    let ui = ui_of(&tab, cx);
    let before = cx.update(|cx| ui.read(cx).selection.clone());
    press(cx, handle, "down");
    assert_eq!(cx.update(|cx| ui.read(cx).selection.clone()), before);
}

// ── history operations (21 §2.9, §2.11) ─────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn history_ops_record_titles_and_refresh(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.merge("refs/heads/feature".into(), w, cx)
    });
    run_on(cx, handle, &tab, |t, w, cx| {
        t.rebase("origin/main".into(), w, cx)
    });
    run_on(cx, handle, &tab, |t, w, cx| t.revert(oid(11), None, w, cx));
    run_on(cx, handle, &tab, |t, w, cx| {
        t.reset(oid(12), crate::backend::ResetMode::Keep, w, cx)
    });
    run_on(cx, handle, &tab, |t, w, cx| {
        t.reset(oid(12), crate::backend::ResetMode::Soft, w, cx)
    });
    assert_eq!(
        ops(&tab, cx),
        [
            (OpKind::Merge, Some("Merge 'feature' into main".into())),
            (
                OpKind::Rebase,
                Some("Rebase main onto 'origin/main'".into())
            ),
            (OpKind::RevertCommit, Some("Revert 0000000".into())),
            (
                OpKind::ResetToCommit,
                Some("Reset (keep) main to 0000000".into())
            ),
            (
                OpKind::ResetToCommit,
                Some("Reset main to 0000000 (soft)".into())
            ),
        ]
    );
}

#[gpui_kit::test]
fn empty_pick_offers_skip_or_commit(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    let empty = || {
        Err::<OpOutcome<SequenceOutcome>, _>(OpFailure::Git(Arc::new(GitError::EmptyPick {
            commit: None,
            stderr: String::new(),
        })))
    };
    cx.update(|cx| script_op(&tab, cx, empty(), Duration::ZERO));
    run_on(cx, handle, &tab, |t, w, cx| {
        t.cherry_pick(oid(2), None, w, cx)
    });
    frame(cx, handle, |window, cx| {
        for k in 0..3usize {
            assert!(
                window.try_find(("choice-option", k)).is_some(),
                "option {k}"
            );
        }
        window.click(("choice-option", 1usize), cx);
    });
    cx.run_until_parked();
    assert!(
        ops(&tab, cx).contains(&(OpKind::Commit, Some("Commit \"Commit 2\"".into()))),
        "Commit empty commits with the pick's message: {:?}",
        ops(&tab, cx)
    );
    cx.update(|cx| script_op(&tab, cx, empty(), Duration::ZERO));
    run_on(cx, handle, &tab, |t, w, cx| {
        t.cherry_pick(oid(2), None, w, cx)
    });
    frame(cx, handle, |window, cx| {
        window.click(("choice-option", 0usize), cx)
    });
    cx.run_until_parked();
    assert!(
        ops(&tab, cx).iter().any(|(k, _)| *k == OpKind::SkipRebase),
        "Skip runs skip_operation: {:?}",
        ops(&tab, cx)
    );
}

#[gpui_kit::test]
fn merge_commit_pick_asks_mainline(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    let started = run_on(cx, handle, &tab, |t, w, cx| {
        t.cherry_pick(oid(10), None, w, cx)
    });
    assert!(!started, "nothing runs before the parent is chosen");
    assert!(ops(&tab, cx).is_empty());
    frame(cx, handle, |window, cx| {
        assert!(
            window.try_find(("choice-option", 2usize)).is_none(),
            "two parents"
        );
        window.click(("choice-option", 1usize), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        ops(&tab, cx),
        [(OpKind::CherryPickCommit, Some("Cherry-pick 0000000".into()))]
    );
    // The backend's fallback (a commit the log does not know) asks too.
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Err::<OpOutcome<SequenceOutcome>, _>(OpFailure::Git(Arc::new(GitError::Unsupported {
                what: "revert of a merge commit needs a mainline parent",
            }))),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| t.revert(oid(999), None, w, cx));
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(("choice-option", 1usize)).is_some(),
            "the parent choice opened"
        );
    });
}

#[gpui_kit::test]
fn merge_autostash_conflict_warns(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    let mut outcome = OpOutcome {
        value: crate::backend::MergeOutcome::UpToDate,
        refresh: RefreshSet::OPERATION_STATE,
        log: Default::default(),
        warnings: Vec::new(),
    };
    cx.update(|cx| script_op(&tab, cx, Ok(outcome.clone()), Duration::ZERO));
    run_on(cx, handle, &tab, |t, w, cx| {
        t.merge("feature".into(), w, cx)
    });
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .contains(&"Already up to date".to_owned())
    );
    outcome.value = crate::backend::MergeOutcome::AutostashConflict { new_head: oid(1) };
    outcome
        .warnings
        .push(crate::backend::OpWarning::AutostashConflict {
            output: "conflict".into(),
        });
    cx.update(|cx| script_op(&tab, cx, Ok(outcome), Duration::ZERO));
    run_on(cx, handle, &tab, |t, w, cx| {
        t.merge("feature".into(), w, cx)
    });
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .iter()
            .any(|t| t.contains("conflicted when re-applied")),
        "{:?}",
        cx.update(|cx| toast_texts(&app, cx))
    );
}

// ── fixture tests (real git) ────────────────────────────────────────────────────────────────────────

fn head_subject(fx: &Fixture) -> String {
    fx.git(&["log", "-1", "--format=%s"]).trim().to_owned()
}

fn rev(fx: &Fixture, spec: &str) -> crate::backend::ObjectId {
    crate::backend::ObjectId::from_hex(fx.git(&["rev-parse", spec]).trim().as_bytes())
        .expect("an object id")
}

#[gpui_kit::test]
fn fixture_log_shows_rows_and_uncommitted_row(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("Initial commit");
    fx.write("a.txt", "two\n");
    fx.commit_all("Second commit");
    fx.write("a.txt", "dirty\n");
    let (handle, _app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::UNCOMMITTED_LABEL).is_some(),
            "dirty: uncommitted row"
        );
        assert!(
            window.try_find((ids::MESSAGE, 1usize)).is_some(),
            "HEAD row"
        );
        assert!(
            window.try_find((ids::MESSAGE, 2usize)).is_some(),
            "root row"
        );
        assert!(
            window.try_find(ids::chip(1, 0)).is_some(),
            "the main chip on HEAD"
        );
    });
}

#[gpui_kit::test]
fn fixture_cherry_pick_revert_merge_rebase_reset(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "base\n");
    fx.commit_all("Base");
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.write("f.txt", "feature\n");
    fx.commit_all("Feature work");
    fx.git(&["checkout", "-q", "main"]);
    fx.git(&["checkout", "-q", "-b", "topic"]);
    fx.write("t.txt", "topic\n");
    fx.commit_all("Topic work");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("m.txt", "main\n");
    fx.commit_all("Main work");
    let feature = rev(&fx, "feature");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);

    run_on(cx, handle, &tab, |t, w, cx| {
        t.cherry_pick(feature, None, w, cx)
    });
    assert_eq!(head_subject(&fx), "Feature work", "cherry-picked onto main");
    let picked = rev(&fx, "HEAD");
    run_on(cx, handle, &tab, |t, w, cx| t.revert(picked, None, w, cx));
    assert!(
        head_subject(&fx).starts_with("Revert"),
        "reverted: {}",
        head_subject(&fx)
    );
    run_on(cx, handle, &tab, |t, w, cx| {
        t.merge("feature".into(), w, cx)
    });
    assert_eq!(
        fx.git(&["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count(),
        3,
        "a merge commit with two parents"
    );
    let main_work = rev(&fx, "HEAD~1");
    run_on(cx, handle, &tab, |t, w, cx| {
        t.reset(main_work, crate::backend::ResetMode::Hard, w, cx)
    });
    assert_eq!(rev(&fx, "HEAD"), main_work, "hard reset moved main");
    fx.git(&["checkout", "-q", "topic"]);
    run_on(cx, handle, &tab, |t, _, cx| t.reload(RefreshSet::ALL, cx));
    run_on(cx, handle, &tab, |t, w, cx| t.rebase("main".into(), w, cx));
    assert_eq!(
        rev(&fx, "topic~1"),
        rev(&fx, "main"),
        "topic rebased onto main"
    );
    assert_eq!(head_subject(&fx), "Topic work");
    let busy = cx.update(|cx| tab.read(cx).busy().is_some());
    assert!(!busy, "no operation left running");
}

/// A failing post-merge hook: the runner's Warning toast replaces the success toast (no second, Positive one).
#[gpui_kit::test]
fn fixture_merge_post_hook_warning_replaces_success_toast(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "base\n");
    fx.commit_all("Base");
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.write("f.txt", "feature\n");
    fx.commit_all("Feature work");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("m.txt", "main\n");
    fx.commit_all("Main work");
    fx.write(
        ".git/hooks/post-merge",
        "#!/bin/sh\necho post-merge failed >&2\nexit 3\n",
    );
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.merge("feature".into(), w, cx)
    });
    assert_eq!(
        fx.git(&["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count(),
        3,
        "merged"
    );
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts.iter().any(|t| t.contains("post-merge")),
        "the post-hook warning toast: {toasts:?}"
    );
    assert_eq!(toasts.len(), 1, "only the warning toast: {toasts:?}");
}

#[gpui_kit::test]
fn fixture_empty_cherry_pick_commit_empty(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "base\n");
    fx.commit_all("Base");
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.write("a.txt", "changed\n");
    fx.commit_all("Change a");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("a.txt", "changed\n");
    fx.commit_all("Same change on main");
    let feature = rev(&fx, "feature");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.cherry_pick(feature, None, w, cx)
    });
    frame(cx, handle, |window, cx| {
        assert!(
            window.try_find(("choice-option", 1usize)).is_some(),
            "the empty-pick choice opened"
        );
        window.click(("choice-option", 1usize), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        head_subject(&fx),
        "Change a",
        "an empty commit with the pick's message"
    );
    assert_eq!(fx.git(&["rev-list", "--count", "HEAD"]).trim(), "3");
}

#[gpui_kit::test]
fn fixture_rebase_interactive_opens_the_editor(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    for i in 0..3 {
        fx.git(&["commit", "-q", "--allow-empty", "-m", &format!("C{i}")]);
    }
    let base = rev(&fx, "HEAD~2");
    let (_handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    cx.update(|cx| start_rebase_interactive(&ctx, base, cx));
    cx.run_until_parked();
    let steps = cx.update(|cx| ctx.ui.read(cx).rebase_plan.as_ref().map(|p| p.steps.len()));
    assert_eq!(steps, Some(2), "the plan of the two commits above the base");
}

#[test]
fn date_tooltip_is_absolute_with_time() {
    let format = crate::widgets::date::DateFormat {
        relative: true,
        ..Default::default()
    };
    let now = crate::backend::GitTime {
        seconds: 1_800_000_000,
        offset_minutes: 0,
    };
    let text = rows::date_tooltip(now, now, 0, &format);
    assert!(
        !text.starts_with("Today") && text.contains(':'),
        "absolute date with the time (08 §2.7.2): {text}"
    );
    assert_eq!(
        chips::tracking_tooltip("main", "origin/main"),
        "Local branch main is up to date with remote branch origin/main"
    );
}

#[test]
fn short_rev_strips_namespaces() {
    use crate::tab::ops::history::short_rev;
    assert_eq!(short_rev("refs/heads/main"), "main");
    assert_eq!(short_rev("refs/remotes/origin/x"), "origin/x");
    assert_eq!(short_rev("v1.0"), "v1.0");
}

/// 21 §6 #38: the default graph width grows to show every lane when that fits in 40 % of the log pane, else it
/// stays 120 dp and the graph scrolls.
#[test]
fn default_graph_width_fits_the_lanes_within_40_percent() {
    // (max_lane + 2) x 30 dp: lane 3 needs 150, lane 5 needs 210.
    assert_eq!(default_graph_width(Some(3), Some(1000.)), 150.);
    assert_eq!(default_graph_width(Some(5), Some(1000.)), 210.);
    assert_eq!(
        default_graph_width(Some(5), Some(530.)),
        210.,
        "just under 40 % of the pane fits"
    );
    assert_eq!(
        default_graph_width(Some(5), Some(520.)),
        120.,
        "more than 40 % keeps the default and scrolls"
    );
    assert_eq!(
        default_graph_width(Some(0), Some(1000.)),
        120.,
        "never below 120"
    );
    assert_eq!(default_graph_width(None, Some(1000.)), 120., "an empty log");
    assert_eq!(
        default_graph_width(Some(5), None),
        120.,
        "pane not laid out yet"
    );
    // The drag minimum stays 100 dp whatever the default is.
    assert_eq!(
        graph_width_from(210., clamp_padding_from(210., -500.)),
        100.
    );
    assert_eq!(graph_width_from(210., 0.), 210.);
}

/// A repository with several lanes opens with a graph column wide enough for them.
#[gpui_kit::test]
fn wide_graphs_open_with_a_column_that_fits_their_lanes(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    // Five lanes (the fixture's own graph needs only two).
    let max_lane = 5;
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.clone();
        model.update(cx, |m, cx| {
            let wide = m.log.value.as_ref().map(|s| LogSnapshot {
                rows: s.rows.clone(),
                max_lane,
                exhausted: s.exhausted,
                generation: s.generation,
            });
            m.log.value = wide.map(std::sync::Arc::new);
            cx.notify();
        });
    });
    let wanted = (f32::from(max_lane) + 2.) * 30.;
    assert!(wanted > 120.);
    // Let the pane be measured, then repaint.
    frame(cx, handle, |_, _| ());
    frame(cx, handle, |_, _| ());
    frame(cx, handle, |window, _| {
        let pane = bounds(window, ids::LOG_VIEW).size.width.as_f32();
        assert!(
            wanted <= pane * 0.4,
            "the lanes fit in 40 % of the {pane} px pane"
        );
        assert_width(window, ids::HEADER_GRAPH, wanted);
    });
}

/// A drag starts from the width on screen: narrowing a fitted (wider than 120 dp) column and pressing its divider
/// again does not jump back towards the default.
#[gpui_kit::test]
fn a_second_drag_of_a_wide_graph_starts_from_its_current_width(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.clone();
        model.update(cx, |m, cx| {
            let wide = m.log.value.as_ref().map(|s| LogSnapshot {
                rows: s.rows.clone(),
                max_lane: 5,
                exhausted: s.exhausted,
                generation: s.generation,
            });
            m.log.value = wide.map(std::sync::Arc::new);
            cx.notify();
        });
    });
    frame(cx, handle, |_, _| ());
    frame(cx, handle, |window, _| {
        assert_width(window, ids::HEADER_GRAPH, 210.);
    });
    for (divider, dx, wanted) in [
        (ids::HEADER_DIVIDER, -100., 110.),
        (ids::HEADER_DIVIDER, 10., 120.),
        (ids::LIST_DIVIDER, 15., 135.),
    ] {
        let centre = frame(cx, handle, |window, _| bounds(window, divider).center());
        frame(cx, handle, |window, cx| {
            window.drag(centre, point(centre.x + px(dx), centre.y), cx);
        });
        frame(cx, handle, |window, _| {
            assert_width(window, ids::HEADER_GRAPH, wanted);
        });
    }
}

/// The corner radii (in dp) of an element's style: top-left, top-right, bottom-right, bottom-left.
fn corner_dp(mut el: impl gpui_kit::Styled) -> [Option<f32>; 4] {
    use gpui_kit::AbsoluteLength;
    let r = el.style().corner_radii.clone();
    [r.top_left, r.top_right, r.bottom_right, r.bottom_left].map(|c| {
        c.map(|c| match c {
            AbsoluteLength::Rems(rems) => rems.0 * crate::metrics::BASE_REM,
            AbsoluteLength::Pixels(_) => panic!("chip corners are dp (rems)"),
        })
    })
}

/// 08 §2.12: the 16 dp chip radius makes the icon box the rounded left cap and the last coloured segment (cloud, or
/// the PR segment) the rounded right cap. The chip's `overflow_hidden` clips to a rectangle only, so the segments
/// round their own backgrounds.
#[gpui_kit::test]
fn chip_caps_are_rounded_16(cx: &mut TestAppContext) {
    crate::test_support::init_app(cx);
    let tokens = cx.update(|cx| *crate::theme::ActiveTokens::tokens(cx));
    let colors = chips::ChipColors {
        lane: tokens.primary,
        remote: tokens.primary_variant,
        tokens,
    };
    let left = [Some(16.), None, None, Some(16.)];
    let right = [None, Some(16.), Some(16.), None];
    let eps = |a: [Option<f32>; 4], b: [Option<f32>; 4]| {
        a.iter().zip(&b).all(|(x, y)| match (x, y) {
            (Some(x), Some(y)) => (x - y).abs() < 1e-3,
            (None, None) => true,
            _ => false,
        })
    };
    let icon_box = chips::icon_box(crate::icons::AppIcon::Tag, colors.lane, tokens.background);
    assert!(eps(corner_dp(icon_box), left), "icon box: left cap");
    assert!(
        eps(corner_dp(chips::cloud_cell(true, colors)), right),
        "a cloud without a PR segment is the right cap"
    );
    assert!(
        eps(corner_dp(chips::cloud_cell(false, colors)), [None; 4]),
        "a cloud followed by the PR segment stays square"
    );
    assert!(eps(
        corner_dp(chips::round_chip_end(
            gpui_kit::div(),
            chips::ChipEnd::Right
        )),
        right
    ));
}
