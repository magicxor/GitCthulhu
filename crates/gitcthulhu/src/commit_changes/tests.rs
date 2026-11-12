use core::prelude::v1::test;

use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Entity, Modifiers, MouseButton, TestAppContext,
};

use super::{COPIED_FOR, CommitChangesView, PROGRESS_DELAY, ids, platform};
use crate::backend::{Against, BlameRev, ChangeKind, DiffTarget, ObjectId, RelaPath};
use crate::menus;
use crate::tab::ui::{CloseableView, FileList, Selection};
use crate::tab::{RepoTab, RepoUi};
use crate::test_support::geometry::{assert_height, assert_rect, assert_width, bounds};
use crate::test_support::input::click_with;
use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture};

/// A repository with two commits; the second deletes, modifies, adds (nested) and renames a file.
struct History {
    fixture: Fixture,
    first: ObjectId,
    second: ObjectId,
}

fn head(fixture: &Fixture) -> ObjectId {
    id_of(fixture, "HEAD")
}

fn id_of(fixture: &Fixture, rev: &str) -> ObjectId {
    ObjectId::from_hex(fixture.git(&["rev-parse", rev]).trim().as_bytes()).expect("a commit id")
}

fn history() -> History {
    let fixture = Fixture::new();
    fixture.write("README.md", "hello\n");
    fixture.write("src/lib.rs", "fn a() {}\n");
    fixture.write("src/old.rs", "pub fn moved() {}\npub fn moved_two() {}\n");
    fixture.write("docs/guide.md", "guide\n");
    fixture.commit_all("Initial import\n\nA body line that explains the import.");
    let first = head(&fixture);
    fixture.git(&["mv", "src/old.rs", "src/renamed.rs"]);
    fixture.write("src/lib.rs", "fn a() {}\nfn b() {}\n");
    fixture.git(&["rm", "-q", "docs/guide.md"]);
    fixture.write("src/new/deep.rs", "fn deep() {}\n");
    fixture.commit_all("Second change");
    let second = head(&fixture);
    History {
        fixture,
        first,
        second,
    }
}

fn row(ix: usize) -> (&'static str, usize) {
    (ids::ROW, ix)
}

fn dir(ix: usize) -> (&'static str, usize) {
    (ids::DIR_ROW, ix)
}

fn ui_of(tab: &Entity<RepoTab>, cx: &App) -> Entity<RepoUi> {
    tab.read(cx).ctx().ui.clone()
}

fn view_of(tab: &Entity<RepoTab>, cx: &App) -> Entity<CommitChangesView> {
    tab.read(cx).views().commit_changes.clone()
}

fn select(cx: &mut TestAppContext, tab: &Entity<RepoTab>, selection: Selection) {
    let ui = cx.update(|cx| ui_of(tab, cx));
    cx.update(|cx| ui.update(cx, |ui, cx| ui.select(selection, cx)));
    cx.run_until_parked();
}

fn select_commit(cx: &mut TestAppContext, tab: &Entity<RepoTab>, id: ObjectId) {
    select(
        cx,
        tab,
        Selection::Commit {
            id,
            is_stash: false,
            scroll_to: false,
        },
    );
}

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
}

fn paths_in_list(cx: &mut TestAppContext, handle: AnyWindowHandle, count: usize) -> Vec<bool> {
    cx.update_window(handle, |_, window, _| {
        (0..count)
            .map(|ix| window.try_find((ids::ROW, ix)).is_some())
            .collect()
    })
    .expect("window")
}

fn click_row(cx: &mut TestAppContext, handle: AnyWindowHandle, ix: usize) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click((ids::ROW, ix), cx);
    })
    .expect("window");
    cx.run_until_parked();
}

/// 08 §5.1 / §8.2: header 34, message area 120, author footer 72 with the 40 dp avatar at x 16; rows 38 high;
/// the pane keeps 8 dp at its right and bottom edges.
#[gpui_kit::test]
fn layout_footer_and_message(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Literal spec values (08 §0.3, §0.5, §5.1; 07 right pane 330 wide, 8 dp right/bottom padding).
        let pane = bounds(window, ids::COMMIT_CHANGES);
        let (x, y) = (pane.origin.x.as_f32(), pane.origin.y.as_f32());
        let (w, hgt) = (pane.size.width.as_f32(), pane.size.height.as_f32());
        // Right pane: x = 1280 - 330, below the 36 dp tab strip and the 64 dp toolbar.
        assert_eq!((x, y, w), (950., 100., 330.));
        let inner_w = w - 8.;
        let bottom = y + hgt - 8.;
        assert_rect(window, ids::HEADER, x, y, inner_w, 34.);
        assert_height(window, ids::MESSAGE, 120.);
        assert_width(window, ids::MESSAGE, inner_w);
        assert_rect(window, ids::FOOTER, x, bottom - 72., inner_w, 72.);
        assert_rect(window, ids::MESSAGE, x, bottom - 72. - 120., inner_w, 120.);
        assert_rect(window, ids::AVATAR, x + 16., bottom - 72. + 16., 40., 40.);
        // The files column (background colour) ends 4 dp above the message box: the gap shows the surface.
        assert_rect(
            window,
            ids::FILES,
            x,
            y,
            inner_w,
            hgt - 8. - 72. - 120. - 4.,
        );
        // The first row sits right under the 34 dp header, 38 dp high (spaced).
        assert_rect(window, row(0), x, y + 34., inner_w, 38.);
        assert!(
            window.try_find(ids::PROGRESS).is_none(),
            "no progress bar when nothing is loading"
        );
    })
    .expect("window");
}

/// 08 §5.1: the short hash is 7 characters, click copies the FULL hash and "Copied!" shows for two seconds.
#[gpui_kit::test]
fn hash_copy_shows_copied_for_2s(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        assert!(window.try_find(ids::COPIED).is_none());
        window.click(ids::HASH, cx);
    })
    .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    cx.update(|cx| {
        assert_eq!(
            platform::recorded(cx).clipboard,
            vec![h.second.to_string()],
            "the full hash was copied"
        );
    });
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::COPIED).is_some(), "Copied! shows");
    })
    .expect("window");
    cx.executor()
        .advance_clock(COPIED_FOR - Duration::from_millis(1));
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(ids::COPIED).is_some(),
            "still there at 1999 ms"
        );
    })
    .expect("window");
    cx.executor().advance_clock(Duration::from_millis(2));
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::COPIED).is_none(), "gone after 2 s");
    })
    .expect("window");
}

/// 08 §5.2: the previous commit's list stays while the next one loads.
#[gpui_kit::test]
fn previous_list_kept_while_loading(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.first);
    let view = cx.update(|cx| view_of(&tab, cx));
    cx.update(|cx| {
        view.update(cx, |v, _| v.set_test_load_delay(Duration::from_secs(1)));
    });
    select_commit(cx, &tab, h.second);
    render(cx, handle);
    cx.update(|cx| {
        let v = view.read(cx);
        assert!(v.is_loading());
        assert_eq!(v.shown_commit(), Some(h.first), "still the old content");
    });
    let present = paths_in_list(cx, handle, 5);
    assert_eq!(
        present,
        vec![true, true, true, true, false],
        "the first commit's 4 files are listed"
    );
    // The stale list is inert: a click on it selects nothing.
    click_row(cx, handle, 0);
    let ui = cx.update(|cx| ui_of(&tab, cx));
    cx.update(|cx| assert!(ui.read(cx).files.paths.is_empty()));
    cx.executor().advance_clock(Duration::from_millis(1100));
    cx.run_until_parked();
    render(cx, handle);
    cx.update(|cx| {
        let v = view.read(cx);
        assert!(!v.is_loading());
        assert_eq!(v.shown_commit(), Some(h.second));
    });
    assert_eq!(
        paths_in_list(cx, handle, 5),
        vec![true, true, true, true, false]
    );
}

/// 08 §5.2: the progress bar appears only when loading takes longer than 150 ms.
#[gpui_kit::test]
fn progress_after_150ms(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.first);
    let view = cx.update(|cx| view_of(&tab, cx));
    cx.update(|cx| {
        view.update(cx, |v, _| v.set_test_load_delay(Duration::from_secs(1)));
    });
    select_commit(cx, &tab, h.second);
    cx.executor()
        .advance_clock(PROGRESS_DELAY - Duration::from_millis(1));
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::PROGRESS).is_none(), "no bar at 149 ms");
    })
    .expect("window");
    cx.executor().advance_clock(Duration::from_millis(2));
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert_height(window, ids::PROGRESS, 4.);
    })
    .expect("window");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::PROGRESS).is_none(), "gone once loaded");
    })
    .expect("window");
}

/// 08 §5.2: a quick load never shows the bar.
#[gpui_kit::test]
fn fast_load_shows_no_progress(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::PROGRESS).is_none());
    })
    .expect("window");
}

/// 08 §5.2: single selection only; Ctrl and Shift do not extend it; the click opens the diff target.
#[gpui_kit::test]
fn single_selection_only(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    click_row(cx, handle, 0);
    let ui = cx.update(|cx| ui_of(&tab, cx));
    cx.update(|cx| {
        let files = &ui.read(cx).files;
        assert_eq!(files.list, Some(FileList::CommitFiles));
        assert_eq!(files.paths.len(), 1);
        assert_eq!(files.paths.first(), Some(&RelaPath::from("docs/guide.md")));
    });
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
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        click_with(window, cx, row(3), Modifiers::shift(), MouseButton::Left);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        let files = &ui.read(cx).files;
        assert_eq!(
            files.paths.len(),
            1,
            "Ctrl and Shift never extend the selection"
        );
        assert_eq!(files.paths.first(), Some(&RelaPath::from("src/renamed.rs")));
        assert!(
            ui.read(cx).views.contains(CloseableView::Diff),
            "exactly one file: the diff opens"
        );
    });
    // Double-click does nothing special.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.double_click(row(2), cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(ui.read(cx).files.paths.len(), 1));
}

/// T13: renames come from the commit details: the diff target of a renamed file carries the previous path.
#[gpui_kit::test]
fn rename_diff_target_has_previous_path(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    click_row(cx, handle, 3);
    cx.update(|cx| {
        let ctx = tab.read(cx).ctx().clone();
        let target = ctx
            .ui
            .read(cx)
            .diff_target(ctx.model.read(cx))
            .expect("a diff target");
        assert_eq!(
            target,
            DiffTarget::Commit {
                commit: h.second,
                against: Against::FirstParent,
                path: RelaPath::from("src/renamed.rs"),
                previous_path: Some(RelaPath::from("src/old.rs")),
            }
        );
    });
}

/// Stashes list their untracked files too; those diff against the stash's untracked-files commit.
#[gpui_kit::test]
fn stash_lists_untracked_files_with_their_own_target(cx: &mut TestAppContext) {
    let h = history();
    h.fixture.write("README.md", "changed\n");
    h.fixture.write("scratch.txt", "untracked\n");
    h.fixture.git(&["stash", "push", "-u", "-q"]);
    let stash = id_of(&h.fixture, "stash@{0}");
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select(
        cx,
        &tab,
        Selection::Stash {
            id: stash,
            index: 0,
        },
    );
    render(cx, handle);
    // README.md (tracked, modified) and scratch.txt (untracked).
    assert_eq!(paths_in_list(cx, handle, 3), vec![true, true, false]);
    click_row(cx, handle, 1);
    cx.update(|cx| {
        let ctx = tab.read(cx).ctx().clone();
        let target = ctx
            .ui
            .read(cx)
            .diff_target(ctx.model.read(cx))
            .expect("a diff target");
        match target {
            DiffTarget::Commit {
                commit,
                against,
                path,
                ..
            } => {
                assert_ne!(commit, stash, "the untracked-files commit");
                assert_eq!(against, Against::Empty);
                assert_eq!(path, RelaPath::from("scratch.txt"));
            }
            other => panic!("unexpected target {other:?}"),
        }
    });
}

/// 09 §4.8: right-click opens the three-item menu; Blame and File history open their views through `RepoUi`.
#[gpui_kit::test]
fn blame_history_menu_opens_views(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    let ui = cx.update(|cx| ui_of(&tab, cx));
    // src/lib.rs is row 1 (modified).
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.right_click(row(1), cx);
        window.render_frame(cx);
        for id in [
            menus::commit_files::ids::BLAME,
            menus::commit_files::ids::HISTORY,
            menus::commit_files::ids::OPEN_IN_FOLDER,
        ] {
            assert!(
                window.try_find(format!("menu-item-{id}")).is_some(),
                "{id} is in the menu"
            );
        }
        window.click(format!("menu-item-{}", menus::commit_files::ids::BLAME), cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        let ui = ui.read(cx);
        let blame = ui.blame.as_ref().expect("blame opened");
        assert_eq!(blame.path, RelaPath::from("src/lib.rs"));
        assert_eq!(blame.rev, BlameRev::Commit(h.second));
        assert!(!blame.minimized);
        assert!(ui.views.contains(CloseableView::Blame));
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.right_click(row(1), cx);
        window.render_frame(cx);
        window.click(
            format!("menu-item-{}", menus::commit_files::ids::HISTORY),
            cx,
        );
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        let ui = ui.read(cx);
        assert_eq!(ui.history, Some(RelaPath::from("src/lib.rs")));
        assert!(ui.views.contains(CloseableView::FileHistory));
    });
}

/// Blame of a file the commit deleted opens at the parent (the file does not exist in the commit itself).
#[gpui_kit::test]
fn blame_of_deleted_file_uses_the_parent(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    let ui = cx.update(|cx| ui_of(&tab, cx));
    // docs/guide.md is row 0 (deleted).
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.right_click(row(0), cx);
        window.render_frame(cx);
        window.click(format!("menu-item-{}", menus::commit_files::ids::BLAME), cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        let blame = ui.read(cx).blame.clone().expect("blame opened");
        assert_eq!(blame.rev, BlameRev::Commit(h.first));
    });
}

/// "Open file in folder" reveals the file; a deleted file falls back to its nearest existing folder.
#[gpui_kit::test]
fn open_in_folder_reveals_file_or_nearest_folder(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    let workdir = h.fixture.path().to_path_buf();
    for (row, _) in [(1usize, "src/lib.rs"), (0, "docs/guide.md")] {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.right_click((ids::ROW, row), cx);
            window.render_frame(cx);
            window.click(
                format!("menu-item-{}", menus::commit_files::ids::OPEN_IN_FOLDER),
                cx,
            );
        })
        .expect("window");
        cx.run_until_parked();
    }
    cx.update(|cx| {
        let recorded = platform::recorded(cx);
        assert_eq!(recorded.revealed, vec![workdir.join("src").join("lib.rs")]);
        // docs/guide.md was removed in the commit, and so was its folder in the worktree.
        assert_eq!(recorded.opened, vec![workdir.clone()]);
    });
}

/// 08 §4.3 / §5.2: the magnifier shows the filter, typing filters (case-insensitive path substring), Esc hides it
/// and clears the filter.
#[gpui_kit::test]
fn filter_narrows_rows_and_escape_clears_it(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    let ui = cx.update(|cx| ui_of(&tab, cx));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(ids::SEARCH).is_none());
        window.click(ids::TOGGLE_SEARCH, cx);
        window.render_frame(cx);
        assert!(window.try_find(ids::SEARCH).is_some(), "the field is shown");
        window.input("RENAMED", cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    cx.update(|cx| {
        let ui = ui.read(cx);
        assert_eq!(ui.commit_files_filter.as_deref(), Some("RENAMED"));
        assert!(ui.views.contains(CloseableView::CommitChangesSearch));
    });
    assert_eq!(
        paths_in_list(cx, handle, 3),
        vec![true, false, false],
        "only src/renamed.rs matches"
    );
    // Escape in the focused field reaches the frame: hides the field, clears the filter, restores the rows.
    cx.update_window(handle, |_, window, cx| window.press("escape", cx))
        .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::SEARCH).is_none(), "field hidden");
    })
    .expect("window");
    cx.update(|cx| assert_eq!(ui.read(cx).commit_files_filter, None));
    assert_eq!(
        paths_in_list(cx, handle, 5),
        vec![true, true, true, true, false]
    );
}

/// The search field's close button hides the field and clears the filter; the magnifier toggles it again.
#[gpui_kit::test]
fn search_close_button_and_toggle(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    let ui = cx.update(|cx| ui_of(&tab, cx));
    let click = |cx: &mut TestAppContext, id: &'static str| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
        .expect("window");
        cx.run_until_parked();
        render(cx, handle);
    };
    let search_shown = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, _| {
            window.try_find(ids::SEARCH).is_some()
        })
        .expect("window")
    };
    click(cx, ids::TOGGLE_SEARCH);
    assert!(search_shown(cx));
    click(cx, "commit-changes-search-close");
    assert!(!search_shown(cx), "the close button hides the field");
    click(cx, ids::TOGGLE_SEARCH);
    assert!(search_shown(cx));
    click(cx, ids::TOGGLE_SEARCH);
    assert!(!search_shown(cx), "the magnifier toggles");
    cx.update(|cx| {
        let ui = ui.read(cx);
        assert_eq!(ui.commit_files_filter, None);
        assert!(!ui.views.contains(CloseableView::CommitChangesSearch));
    });
}

/// 08 §4.4: tree mode has one row per directory level; clicking a directory collapses it; the mode is the global
/// `changes_as_tree` setting and the header icon switches.
#[gpui_kit::test]
fn tree_mode_collapses_directories(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    render(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::TOGGLE_TREE, cx);
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.update(|cx| assert!(crate::settings::get(cx).changes_as_tree));
    // docs/, guide.md, src/, lib.rs, new/, deep.rs, renamed.rs
    cx.update_window(handle, |_, window, _| {
        for ix in 0..7 {
            let dir = [0usize, 2, 4].contains(&ix);
            let id: gpui_kit::ElementId = if dir {
                (ids::DIR_ROW, ix).into()
            } else {
                (ids::ROW, ix).into()
            };
            assert!(window.try_find(id).is_some(), "row {ix}");
        }
        assert!(window.try_find(row(7)).is_none());
    })
    .expect("window");
    cx.update_window(handle, |_, window, cx| {
        window.click(dir(2), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        // src/ collapsed: docs/, guide.md, src/
        assert!(window.try_find(dir(2)).is_some());
        assert!(window.try_find(row(3)).is_none(), "descendants are hidden");
    })
    .expect("window");
}

/// A commit with thousands of files stays virtualised: only the visible rows exist; scrolling reaches the end.
#[gpui_kit::test]
fn three_thousand_files_are_virtualised(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    for i in 0..3000 {
        fixture.write(&format!("dir{}/file{i:04}.txt", i % 10), "x\n");
    }
    fixture.commit_all("Many files");
    let id = head(&fixture);
    let (handle, _app, tab) = mount_fixture(cx, &fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, id);
    render(cx, handle);
    let present = paths_in_list(cx, handle, 3000);
    let built = present.iter().filter(|p| **p).count();
    assert!(built > 5 && built < 60, "{built} rows are built, not 3000");
    assert!(present[0]);
    assert!(!present[2999]);
    // Scroll far down: the first row is gone, a later one exists.
    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            ids::LIST,
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(
                gpui_kit::px(0.),
                gpui_kit::px(-60_000.),
            )),
            cx,
        );
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
    let present = paths_in_list(cx, handle, 3000);
    assert!(!present[0], "the top rows left the viewport");
    assert!(
        present.iter().filter(|p| **p).count() < 60,
        "still only a viewport of rows"
    );
}

/// Leaving the commit-based selection releases the content and the published file list, and closes an open
/// filter (a hidden filter left on the view stack would swallow the next Escape).
#[gpui_kit::test]
fn leaving_the_commit_selection_releases_content(cx: &mut TestAppContext) {
    let h = history();
    let (handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    let (view, ui) = cx.update(|cx| (view_of(&tab, cx), ui_of(&tab, cx)));
    cx.update(|cx| {
        assert_eq!(view.read(cx).shown_commit(), Some(h.second));
        assert!(ui.read(cx).commit_files.is_some());
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(ids::TOGGLE_SEARCH, cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            ui.read(cx)
                .views
                .contains(CloseableView::CommitChangesSearch)
        )
    });
    select(cx, &tab, Selection::UncommittedChanges);
    cx.update(|cx| {
        assert_eq!(view.read(cx).shown_commit(), None);
        let ui = ui.read(cx);
        assert!(ui.commit_files.is_none());
        assert_eq!(ui.commit_files_filter, None);
        assert!(!ui.views.contains(CloseableView::CommitChangesSearch));
    });
}

/// A branch or tag selection shows its target commit, and a commit that cannot be read shows an error line
/// instead of a stale list.
#[gpui_kit::test]
fn unreadable_commit_shows_an_error(cx: &mut TestAppContext) {
    let h = history();
    let (handle, app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.first);
    let missing = ObjectId::from_hex(b"1234567890123456789012345678901234567890").expect("hex");
    select_commit(cx, &tab, missing);
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::ERROR).is_some());
        assert!(window.try_find(row(0)).is_none(), "no stale rows");
    })
    .expect("window");
    cx.update(|cx| {
        let toasts = crate::test_support::toast_texts(&app, cx);
        assert!(
            toasts
                .iter()
                .any(|t| t.starts_with("Could not read the commit")),
            "{toasts:?}"
        );
    });
}

/// The files published for the diff view follow the loaded commit.
#[gpui_kit::test]
fn file_kinds_follow_the_commit(cx: &mut TestAppContext) {
    let h = history();
    let (_handle, _app, tab) = mount_fixture(cx, &h.fixture, DEFAULT_SIZE);
    select_commit(cx, &tab, h.second);
    let ui = cx.update(|cx| ui_of(&tab, cx));
    cx.update(|cx| {
        let ui = ui.read(cx);
        let (owner, files) = ui.commit_files.as_ref().expect("published");
        assert_eq!(*owner, h.second);
        let kinds: Vec<(String, ChangeKind)> =
            files.iter().map(|f| (f.path.to_string(), f.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                ("docs/guide.md".to_owned(), ChangeKind::Deleted),
                ("src/lib.rs".to_owned(), ChangeKind::Modified),
                ("src/new/deep.rs".to_owned(), ChangeKind::Added),
                ("src/renamed.rs".to_owned(), ChangeKind::Renamed),
            ]
        );
    });
}
