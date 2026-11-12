use core::prelude::v1::test;

use std::io::Write as _;
use std::process::{Command, Stdio};

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, ScrollStrategy, TestAppContext};

use super::{FileHistoryView, ids};
use crate::backend::RelaPath;
use crate::diff::model::DiffMode;
use crate::metrics;
use crate::metrics::history as m;
use crate::settings::DiffViewMode;
use crate::tab::RepoTab;
use crate::tab::ui::CloseableView;
use crate::test_support::geometry::{assert_height, assert_rect, assert_width, bounds};
use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture};

const FILE: &str = "src/lib.rs";

/// A file with 20 lines of which line 10 carries the revision number.
fn revision(n: usize) -> String {
    (0..20)
        .map(|i| {
            if i == 10 {
                format!("version {n}\n")
            } else {
                format!("constant line {i}\n")
            }
        })
        .collect()
}

/// `n` commits of `FILE` in one `git fast-import` run (a `git commit` per revision takes seconds on Windows).
fn import_history(f: &Fixture, n: usize) {
    let mut stream = String::new();
    for i in 0..n {
        let message = format!("revision {i}");
        let content = revision(i);
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter Test User <test@example.com> {} +0000\ndata {}\n{message}\n",
            1_700_000_000 + i * 60,
            message.len(),
        ));
        stream.push_str(&format!(
            "M 100644 inline {FILE}\ndata {}\n{content}\n",
            content.len()
        ));
    }
    let mut child = Command::new("git")
        .current_dir(f.path())
        .args(["fast-import", "--quiet"])
        .env("GIT_CONFIG_GLOBAL", f.dir.path().join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", f.dir.path().join("home"))
        .stdin(Stdio::piped())
        .spawn()
        .expect("git fast-import starts");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stream.as_bytes())
        .expect("the stream is written");
    assert!(child.wait().expect("fast-import ends").success());
    f.git(&["reset", "--hard", "-q"]);
}

fn open(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    path: &str,
) -> Entity<FileHistoryView> {
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    cx.update(|cx| super::open(&ctx, RelaPath::from(path), cx));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    cx.update(|cx| {
        tab.read(cx)
            .views
            .history
            .as_ref()
            .map(|(_, v)| v.clone())
            .expect("the history view exists")
    })
}

/// Spec 09 §2.2: the view replaces everything under the toolbar; header 40, list 300 wide, rows 72; no row is
/// selected and the diff area is blank until a commit is clicked.
#[gpui_kit::test]
fn history_replaces_frame_content(cx: &mut TestAppContext) {
    let f = Fixture::new();
    import_history(&f, 5);
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let view = open(cx, handle, &tab, FILE);
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.items().len(), 5);
        assert!(v.exhausted(), "5 commits fit in one page");
        assert!(v.diff_doc(cx).is_none(), "no initial selection");
        assert_eq!(&*v.items()[0].summary, "revision 4", "newest first");
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let top = metrics::TAB_STRIP_H + metrics::TOOLBAR_H;
        let height =
            DEFAULT_SIZE.height.as_f32() - top - metrics::FRAME_DIVIDER_H - metrics::BOTTOM_BAR_H;
        assert_rect(
            window,
            ids::FILE_HISTORY,
            0.,
            top,
            DEFAULT_SIZE.width.as_f32(),
            height,
        );
        assert!(
            window
                .try_find(crate::side_panel::ids::SIDE_PANEL)
                .is_none(),
            "the side panel is replaced"
        );
        assert!(window.try_find(crate::ids::TOOLBAR).is_some());
        // Literal spec values (09 §2.2): header 40, list 300 wide, rows 72; the date box (with its 16 dp end
        // padding) ends at the list's right edge.
        assert_height(window, ids::HISTORY_HEADER, 40.);
        assert_width(window, ids::HISTORY_LIST, 300.);
        assert_height(window, (ids::HISTORY_ROW, 0usize), 72.);
        let list = bounds(window, ids::HISTORY_LIST);
        let date = bounds(window, (ids::HISTORY_DATE, 0usize));
        assert!(
            (list.right() - date.right()).abs().as_f32() < 0.5,
            "date box ends at the list's right edge: {date:?} vs {list:?}"
        );
        assert_height(window, ids::HISTORY_HEADER, crate::metrics::blame::HEADER_H);
        assert_width(window, ids::HISTORY_LIST, m::LIST_W);
        assert_height(window, (ids::HISTORY_ROW, 0usize), m::ROW_H);
        assert_width(window, (ids::HISTORY_ROW, 0usize), m::LIST_W);
        let (r0, r1) = (
            bounds(window, (ids::HISTORY_ROW, 0usize)),
            bounds(window, (ids::HISTORY_ROW, 1usize)),
        );
        assert!((r1.top() - r0.bottom()).abs().as_f32() < 0.5, "no dividers");
        let diff = bounds(window, ids::HISTORY_DIFF);
        assert!(
            (diff.left().as_f32() - m::LIST_W).abs() < 0.5,
            "the diff area starts after the list"
        );
        assert!(window.try_find(crate::diff::ids::LIST).is_none());
    })
    .expect("window");
}

/// 21 §8.3: pages of 100 commits; the next page is requested near the end of the loaded rows and the list stops
/// when the walk is exhausted.
#[gpui_kit::test]
fn history_paging(cx: &mut TestAppContext) {
    let f = Fixture::new();
    import_history(&f, 230);
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let view = open(cx, handle, &tab, FILE);
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.items().len(), m::PAGE_SIZE, "first page");
        assert!(!v.exhausted());
    });
    let scroll = cx.update(|cx| view.read(cx).list_scroll.clone());
    for (expected, exhausted) in [(200, false), (230, true)] {
        cx.update_window(handle, |_, window, cx| {
            let last = view.read(cx).items().len() - 1;
            scroll.scroll_to_item(last, ScrollStrategy::Top);
            window.render_frame(cx);
        })
        .expect("window");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("window");
        cx.update(|cx| {
            let v = view.read(cx);
            assert_eq!(v.items().len(), expected);
            assert_eq!(v.exhausted(), exhausted);
        });
    }
    cx.update(|cx| {
        let v = view.read(cx);
        let summaries: Vec<&str> = v.items().iter().map(|i| &*i.summary).collect();
        assert_eq!(summaries.first(), Some(&"revision 229"));
        assert_eq!(summaries.last(), Some(&"revision 0"));
        let mut unique = summaries.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 230, "no commit is listed twice");
    });
    // Rows far from the viewport are not built.
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find((ids::HISTORY_ROW, 5usize)).is_none());
    })
    .expect("window");
}

/// Spec 09 §2.2: a click shows the commit's diff, without header or actions; unified/split follows the global
/// setting live.
#[gpui_kit::test]
fn history_diff_follows_setting(cx: &mut TestAppContext) {
    let f = Fixture::new();
    import_history(&f, 4);
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let view = open(cx, handle, &tab, FILE);

    cx.update_window(handle, |_, window, cx| {
        window.click((ids::HISTORY_ROW, 0usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    let unified_rows = cx.update(|cx| {
        let doc = view.read(cx).diff_doc(cx).expect("the diff is shown");
        assert_eq!(doc.mode, DiffMode::Unified);
        assert!(!doc.rows.is_empty());
        doc.rows.len()
    });
    cx.update_window(handle, |_, window, _| {
        // Row 0 is the hunk header; the diff is one hunk (3 + 1 removed + 1 added + 3 lines of context).
        assert!(window.try_find((crate::diff::ids::HUNK, 0usize)).is_some());
        assert!(
            window
                .try_find((crate::diff::ids::LINE, unified_rows - 1))
                .is_some()
        );
        assert!(
            window
                .try_find((crate::diff::ids::LINE, unified_rows))
                .is_none()
        );
        assert!(
            window.try_find(crate::diff::ids::HEADER).is_none(),
            "no diff header, no actions (09 §2.2)"
        );
        assert!(
            window
                .try_find((crate::diff::ids::HUNK_STAGE, 0usize))
                .is_none(),
            "a history diff is read-only"
        );
    })
    .expect("window");

    cx.update(|cx| crate::settings::update(cx, |s| s.diff_view = DiffViewMode::Split));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    let split_rows = cx.update(|cx| {
        let doc = view.read(cx).diff_doc(cx).expect("still shown");
        assert_eq!(doc.mode, DiffMode::Split, "the mode switched live");
        doc.rows.len()
    });
    assert_eq!(
        split_rows,
        unified_rows - 1,
        "the removed and added line share one row"
    );
    cx.update_window(handle, |_, window, _| {
        let row = bounds(window, (crate::diff::ids::LINE, 1usize));
        let area = bounds(window, ids::HISTORY_DIFF);
        assert!(
            (row.size.width - area.size.width).abs().as_f32() < 0.5,
            "rows fill the diff area"
        );
    })
    .expect("window");

    // A click on another commit replaces the diff.
    cx.update(|cx| crate::settings::update(cx, |s| s.diff_view = DiffViewMode::Unified));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.click((ids::HISTORY_ROW, 3usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        let doc = view.read(cx).diff_doc(cx).expect("a diff");
        assert_eq!(
            doc.diff.kind,
            crate::backend::ChangeKind::Added,
            "the first revision adds the file"
        );
        assert_eq!(doc.mode, DiffMode::Unified);
    });
}

/// Spec 09 §2.2: X closes the history, and so does Esc; the split comes back.
#[gpui_kit::test]
fn history_closes_with_x_and_escape(cx: &mut TestAppContext) {
    let f = Fixture::new();
    import_history(&f, 2);
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let ui = cx.update(|cx| tab.read(cx).ctx().ui.clone());

    open(cx, handle, &tab, FILE);
    cx.update(|cx| assert!(ui.read(cx).views.contains(CloseableView::FileHistory)));
    cx.update_window(handle, |_, window, cx| window.click(ids::HISTORY_CLOSE, cx))
        .expect("window");
    cx.update(|cx| {
        assert!(ui.read(cx).history.is_none());
        assert!(!ui.read(cx).views.contains(CloseableView::FileHistory));
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(ids::FILE_HISTORY).is_none());
        assert!(
            window
                .try_find(crate::side_panel::ids::SIDE_PANEL)
                .is_some()
        );
    })
    .expect("window");

    open(cx, handle, &tab, FILE);
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| t.focus(window, cx));
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .expect("window");
    cx.update(|cx| assert!(ui.read(cx).history.is_none()));
}

/// Renames are followed and deleted files list their deletion; both have loadable diffs.
#[gpui_kit::test]
fn history_follows_renames_and_lists_deletions(cx: &mut TestAppContext) {
    let f = Fixture::new();
    f.write("old.txt", &revision(0));
    f.commit_all("add old");
    f.git(&["mv", "old.txt", "new.txt"]);
    f.commit_all("rename");
    f.write("new.txt", &revision(1));
    f.commit_all("edit");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let view = open(cx, handle, &tab, "new.txt");
    cx.update(|cx| {
        let summaries: Vec<String> = view
            .read(cx)
            .items()
            .iter()
            .map(|i| i.summary.to_string())
            .collect();
        assert_eq!(summaries, ["edit", "rename", "add old"]);
    });
    // The addition under the old name: its diff is read at the path the file had in that commit.
    cx.update_window(handle, |_, window, cx| {
        window.click((ids::HISTORY_ROW, 2usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        let doc = view.read(cx).diff_doc(cx).expect("a diff");
        assert_eq!(doc.diff.path, RelaPath::from("old.txt"));
    });

    // A deleted file: the walk starts at HEAD, so the deletion is the newest entry.
    f.git(&["rm", "-q", "new.txt"]);
    f.commit_all("remove");
    ctx_reload(cx, &tab);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    cx.update(|cx| super::close(&ctx, cx));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    let view = open(cx, handle, &tab, "new.txt");
    cx.update_window(handle, |_, window, cx| {
        window.click((ids::HISTORY_ROW, 0usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(&*v.items()[0].summary, "remove");
        let doc = v.diff_doc(cx).expect("the deletion's diff");
        assert_eq!(doc.diff.kind, crate::backend::ChangeKind::Deleted);
    });
}

fn ctx_reload(cx: &mut TestAppContext, tab: &Entity<RepoTab>) {
    cx.update(|cx| {
        tab.update(cx, |t, cx| t.reload(crate::backend::RefreshSet::ALL, cx));
    });
    cx.run_until_parked();
}

/// The history of a path that never existed is empty and finished (no error).
#[gpui_kit::test]
fn history_of_an_unknown_file_is_empty(cx: &mut TestAppContext) {
    let f = Fixture::new();
    import_history(&f, 1);
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let view = open(cx, handle, &tab, "no/such/file.rs");
    cx.update(|cx| {
        let v = view.read(cx);
        assert!(v.items().is_empty());
        assert!(v.exhausted());
    });
}

/// A unified/split switch keeps the diff's row index (09 §1.1: the list state survives a toggle); another commit
/// starts at the top.
#[gpui_kit::test]
fn mode_switch_keeps_the_diff_row(cx: &mut TestAppContext) {
    let f = Fixture::new();
    let body = |tag: &str| -> String { (0..300).map(|i| format!("{tag} line {i}\n")).collect() };
    f.write("big.txt", &body("old"));
    f.commit_all("first");
    f.write("big.txt", &body("new"));
    f.commit_all("second");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let view = open(cx, handle, &tab, "big.txt");
    cx.update_window(handle, |_, window, cx| {
        window.click((ids::HISTORY_ROW, 0usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    let list = cx.update(|cx| view.read(cx).diff_view().read(cx).list_state().clone());
    cx.update_window(handle, |_, window, cx| {
        list.scroll_to(gpui_kit::ListOffset {
            item_ix: 120,
            offset_in_item: gpui_kit::px(0.),
        });
        window.render_frame(cx);
    })
    .expect("window");
    cx.update(|cx| crate::settings::update(cx, |s| s.diff_view = DiffViewMode::Split));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).diff_doc(cx).expect("shown").mode,
            DiffMode::Split
        );
    });
    assert_eq!(
        list.logical_scroll_top().item_ix,
        120,
        "the row index is kept"
    );

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click((ids::HISTORY_ROW, 1usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        list.logical_scroll_top().item_ix,
        0,
        "another commit starts at the top"
    );
}

/// The history diff is the shared read-only diff view: its text can be selected and copied, and a long line is cut
/// with the same markers as the main diff (the old private renderer did neither).
#[gpui_kit::test]
fn history_diff_supports_selection_and_cut_lines(cx: &mut TestAppContext) {
    let f = Fixture::new();
    let long = format!("start {} end", "x".repeat(3000));
    f.write("wide.txt", "short\n");
    f.commit_all("first");
    f.write("wide.txt", &format!("short\n{long}\n"));
    f.commit_all("second");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let view = open(cx, handle, &tab, "wide.txt");
    cx.update_window(handle, |_, window, cx| {
        window.click((ids::HISTORY_ROW, 0usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    let diff = cx.update(|cx| view.read(cx).diff_view().clone());
    cx.update(|cx| {
        diff.update(cx, |view, cx| {
            let selection = view.selection();
            assert!(!selection.has_selection(cx), "nothing is selected yet");
            selection.select_all(cx);
            assert!(selection.has_selection(cx), "select all works");
            let text = selection.selected_text(cx).expect("selected text");
            assert!(
                text.contains(&long),
                "copying a cut line yields the full source line"
            );
        });
    });
}
