//! Cross-region and fixture smoke tests of milestone 1 (owned by G12; spec 21 §8.3).
//!
//! Every test drives a real repository (the `git` executable, isolated configuration) through the regions' own
//! entry points and elements: clicks on rows and buttons, menu items, dialogs, the commit box. Assertions check the
//! repository with `git` and what the other regions show afterwards.

use core::prelude::v1::test;

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, ElementId, Entity, Modifiers, SharedString,
    TestAppContext, Window,
};

use crate::app_view::AppView;
use crate::backend::{
    CredentialPrompter, CredentialRequest, CredentialResponse, ObjectId, OpOutcome, PromptKind,
    RebaseAction, RefreshSet, RelaPath, Resolution, Sensitive, StartPoint,
};
use crate::diff::model::{DiffDoc, DiffRow};
use crate::settings::{self, DiffViewMode};
use crate::tab::ops::{OpKind, OpSpec};
use crate::tab::ui::FileList;
use crate::tab::{RepoTab, TabCtx};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, init_app, mount_fixture, mount_fixture_on, mount_fixture_with, open_app,
    toast_texts,
};
use crate::theme::{ActiveTokens as _, ThemeKind};

// ───────────────────────────── helpers ─────────────────────────────

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

fn with_window<R>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    let r = cx
        .update_window(handle, |_, window, cx| f(window, cx))
        .expect("window");
    cx.run_until_parked();
    r
}

/// Two frames (lists measure in the first), then the deferred work.
fn draw(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.simulate_next_frame(cx);
        window.render_frame(cx);
    });
    with_window(cx, handle, |window, cx| window.render_frame(cx));
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    });
    draw(cx, handle);
}

fn present(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
}

fn dialog_open(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.has_active_dialog(cx)
    })
}

fn menu_item(id: &str) -> SharedString {
    format!("menu-item-{id}").into()
}

fn ctx_of(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> TabCtx {
    cx.update(|cx| tab.read(cx).ctx().clone())
}

fn busy(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| tab.read(cx).busy().is_some())
}

fn status_paths(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> (Vec<String>, Vec<String>) {
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        let status = model.status.value.as_ref().expect("status loaded");
        let paths = |list: &[crate::backend::StatusEntry]| {
            list.iter().map(|e| e.path.to_string()).collect::<Vec<_>>()
        };
        (paths(&status.staged), paths(&status.unstaged))
    })
}

/// The commit summaries of the loaded log, newest first (the uncommitted row skipped).
fn log_summaries(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        let log = model.log.value.as_ref().expect("log loaded");
        log.rows
            .iter()
            .filter_map(|row| row.commit.as_ref().map(|c| c.summary.to_string()))
            .collect()
    })
}

fn local_branches(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<(String, bool)> {
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        let refs = model.refs.value.as_ref().expect("refs loaded");
        refs.local
            .iter()
            .map(|b| (b.name.clone(), b.is_head))
            .collect()
    })
}

fn stash_count(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        model.refs.value.as_ref().map_or(0, |r| r.stashes.len())
    })
}

fn oid(fx: &Fixture, rev: &str) -> ObjectId {
    ObjectId::from_hex(fx.git(&["rev-parse", rev]).trim().as_bytes()).expect("hex id")
}

fn errors_opened(app: &Entity<AppView>, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        app.read(cx)
            .errors_opened
            .iter()
            .map(|t| t.to_string())
            .collect()
    })
}

fn diff_doc(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Option<Arc<DiffDoc>> {
    cx.update(|cx| tab.read(cx).views().diff.read(cx).doc().cloned())
}

/// The diff row that shows the line with `lineno` on the given side.
fn row_of(doc: &DiffDoc, new_side: bool, lineno: u32) -> usize {
    let text = doc.text().expect("a text diff");
    doc.rows
        .iter()
        .position(|row| {
            let lines = match *row {
                DiffRow::Line { line } => vec![line],
                DiffRow::Pair { left, right, .. } => left.into_iter().chain(right).collect(),
                DiffRow::HunkHeader { .. } => vec![],
            };
            lines.iter().any(|l| {
                let line = &text.lines[*l as usize];
                if new_side {
                    line.new_lineno == Some(lineno) && line.old_lineno.is_none()
                } else {
                    line.old_lineno == Some(lineno) && line.new_lineno.is_none()
                }
            })
        })
        .expect("a row shows the line")
}

/// Waits (test time and wall time) until `done` holds: for work a real thread or process does.
fn wait_until(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
        cx.executor().advance_clock(Duration::from_millis(20));
    }
}

fn numbered(n: usize, changed: &[(usize, &str)]) -> String {
    (1..=n)
        .map(|i| match changed.iter().find(|(line, _)| *line == i) {
            Some((_, text)) => format!("{text}\n"),
            None => format!("line {i}\n"),
        })
        .collect()
}

/// Writes an executable hook script (`#!/bin/sh`; Git for Windows runs it through its own shell).
fn write_hook(fx: &Fixture, name: &str, body: &str) {
    let path = fx.path().join(".git").join("hooks").join(name);
    std::fs::create_dir_all(path.parent().expect("hooks dir")).expect("hooks dir");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

/// A bare remote `remote.git` that `main` of the fixture tracks, and a second clone `other` that can publish.
struct Remote {
    bare: String,
    other: String,
}

fn with_remote(fx: &Fixture) -> Remote {
    let bare = fx
        .dir
        .path()
        .join("remote.git")
        .to_string_lossy()
        .into_owned();
    let other = fx.dir.path().join("other").to_string_lossy().into_owned();
    fx.git(&["init", "-q", "--bare", "-b", "main", &bare]);
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.git(&["remote", "add", "origin", &bare]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    fx.git(&["clone", "-q", &bare, &other]);
    Remote { bare, other }
}

impl Remote {
    /// A commit pushed from the other clone.
    fn publish(&self, fx: &Fixture, file: &str, text: &str) {
        std::fs::write(std::path::Path::new(&self.other).join(file), text).expect("write");
        fx.git(&["-C", &self.other, "add", "-A"]);
        fx.git(&[
            "-C",
            &self.other,
            "commit",
            "-q",
            "-m",
            &format!("theirs {file}"),
        ]);
        fx.git(&["-C", &self.other, "push", "-q", "origin", "main"]);
    }

    fn head(&self, fx: &Fixture) -> String {
        fx.git(&["--git-dir", &self.bare, "log", "-1", "--format=%s", "main"])
            .trim()
            .to_owned()
    }
}

// ───────────────────────────── open → regions ─────────────────────────────

/// Opening a repository through the app (not the harness) fills the side panel, the log and the status pane.
#[gpui_kit::test]
fn open_repository_populates_side_panel_log_and_status(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("first");
    fx.git(&["branch", "feature"]);
    fx.git(&["tag", "v1"]);
    fx.write("a.txt", "two\n");
    fx.commit_all("second");
    fx.write("a.txt", "dirty\n");
    fx.write("new.txt", "untracked\n");

    init_app(cx);
    let rt = fx.runtime();
    cx.update(|cx| {
        if let Some(state) = crate::app::state_mut(cx) {
            state.git = crate::backend::GitStatus::Ready(rt);
        }
    });
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    let path = fx.path().to_owned();
    with_window(cx, handle, |window, cx| {
        app.update(cx, |app, cx| app.open_repository(path, true, window, cx))
    });
    wait_until(cx, "the repository tab", |cx| {
        cx.update(|cx| app.read(cx).active_tab().is_some())
    });
    let tab = cx.update(|cx| app.read(cx).active_tab().cloned().expect("tab"));
    wait_until(cx, "the first loads", |cx| {
        cx.update(|cx| {
            let model = tab.read(cx).ctx().model.read(cx);
            model.status.value.is_some() && model.refs.value.is_some() && model.log.value.is_some()
        })
    });
    draw(cx, handle);

    assert_eq!(
        local_branches(&tab, cx),
        [("feature".to_owned(), false), ("main".to_owned(), true)],
        "both branches are loaded, main is HEAD"
    );
    assert_eq!(log_summaries(&tab, cx), ["second", "first"]);
    let (staged, unstaged) = status_paths(&tab, cx);
    assert!(staged.is_empty());
    assert_eq!(unstaged, ["a.txt", "new.txt"]);
    assert_eq!(
        cx.update(|cx| app.read(cx).tab_title(app.read(cx).active(), cx)),
        "repo",
        "the tab is named after the directory"
    );
    // What the regions render from it.
    for id in [
        crate::side_panel::ids::branch("refs/heads/main"),
        crate::side_panel::ids::branch("refs/heads/feature"),
    ] {
        assert!(present(cx, handle, id.clone()), "side panel row {id}");
    }
    assert!(
        present(
            cx,
            handle,
            (crate::log_view::ids::UNCOMMITTED_LABEL, 0usize)
        ) || present(cx, handle, crate::log_view::ids::UNCOMMITTED_LABEL),
        "the log starts with the uncommitted row"
    );
    assert!(
        present(cx, handle, (crate::log_view::ids::LOG_ROW, 2usize)),
        "uncommitted row + two commits"
    );
    assert!(present(cx, handle, crate::status_pane::ids::STATUS_PANE));
    assert!(present(
        cx,
        handle,
        crate::status_pane::commit_box::ids::PRIMARY
    ));
}

// ───────────────────────────── stage in the diff → commit → log ─────────────────────────────

/// Select the file in the status pane, stage a hunk and a line and discard a hunk in the diff, commit from the
/// commit box: the index, the worktree, the new commit and the log agree.
#[gpui_kit::test]
fn stage_hunk_and_line_in_diff_then_commit_updates_log(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("src/lib.rs", &numbered(60, &[]));
    fx.commit_all("init");
    fx.write(
        "src/lib.rs",
        &numbered(
            60,
            &[
                (5, "line five changed"),
                (25, "line 25 changed"),
                (50, "line fifty changed"),
            ],
        ),
    );
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ui = ctx_of(&tab, cx).ui;
    let select_unstaged = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            ui.update(cx, |ui, cx| {
                ui.click_file(
                    FileList::Unstaged,
                    RelaPath::from("src/lib.rs"),
                    Modifiers::default(),
                    &[],
                    cx,
                )
            })
        });
        cx.run_until_parked();
        draw(cx, handle);
    };
    select_unstaged(cx);
    let doc = diff_doc(&tab, cx).expect("the diff of the selected file");
    assert_eq!(doc.text().expect("text").hunks.len(), 3, "three changes");

    // Stage the first hunk with its header button.
    click(cx, handle, (crate::diff::ids::HUNK_STAGE, 0usize));
    assert!(
        fx.git(&["diff", "--cached"]).contains("+line five changed"),
        "the first hunk is staged"
    );
    let (staged, unstaged) = status_paths(&tab, cx);
    assert_eq!(
        (staged.as_slice(), unstaged.as_slice()),
        (
            &["src/lib.rs".to_owned()][..],
            &["src/lib.rs".to_owned()][..]
        )
    );

    // The diff follows the index: two hunks left. Stage only the added line of the second change by its chip.
    select_unstaged(cx);
    let doc = diff_doc(&tab, cx).expect("the diff is still shown");
    assert_eq!(
        doc.text().expect("text").hunks.len(),
        2,
        "the staged hunk left the unstaged diff"
    );
    let added = row_of(&doc, true, 25);
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.hover((crate::diff::ids::TEXT, added), cx);
        window.render_frame(cx);
        window.click((crate::diff::ids::CHIP, added), cx);
    });
    draw(cx, handle);
    let cached = fx.git(&["diff", "--cached"]);
    assert!(
        cached.contains("+line 25 changed") && !cached.contains("-line 25\n"),
        "only the added line was staged: {cached}"
    );

    // Discard the last hunk from the unstaged diff (a tracked file: no question).
    select_unstaged(cx);
    let doc = diff_doc(&tab, cx).expect("diff");
    // Hunk buttons are `(id, row)`: the header row of the last hunk.
    let last = doc
        .rows
        .iter()
        .rposition(|r| matches!(r, DiffRow::HunkHeader { .. }))
        .expect("a hunk header");
    click(cx, handle, (crate::diff::ids::HUNK_DISCARD, last));
    let worktree = std::fs::read_to_string(fx.path().join("src/lib.rs")).expect("file");
    assert!(
        worktree.contains("line 50\n") && !worktree.contains("fifty"),
        "the discarded hunk is back to HEAD in the worktree"
    );

    // Commit from the commit box.
    let commit_box = cx.update(|cx| tab.read(cx).views().status.read(cx).commit_box().clone());
    with_window(cx, handle, |window, cx| {
        commit_box.update(cx, |b, cx| {
            b.set_message("Stage parts of lib.rs".into(), window, cx)
        })
    });
    click(cx, handle, crate::status_pane::commit_box::ids::PRIMARY);
    wait_until(cx, "the commit", |cx| !busy(&tab, cx));
    assert_eq!(
        fx.git(&["log", "-1", "--format=%s"]).trim(),
        "Stage parts of lib.rs"
    );
    let committed = fx.git(&["show", "HEAD:src/lib.rs"]);
    assert!(committed.contains("line five changed") && committed.contains("line 25 changed"));
    assert!(
        committed.contains("line 25\n"),
        "the unselected removal was not committed"
    );
    assert_eq!(
        log_summaries(&tab, cx).first().map(String::as_str),
        Some("Stage parts of lib.rs"),
        "the log shows the new commit on top"
    );
    let (staged, unstaged) = status_paths(&tab, cx);
    assert!(staged.is_empty(), "nothing staged after the commit");
    assert_eq!(
        unstaged,
        ["src/lib.rs"],
        "the unstaged removal of line 25 remains"
    );
    let message = cx.update(|cx| commit_box.read(cx).message(cx));
    assert!(message.is_empty(), "the commit box is cleared: {message:?}");
}

// ───────────────────────────── commit → changes → diff → blame → history ─────────────────────────────

#[gpui_kit::test]
fn commit_selection_flows_into_changes_diff_blame_and_history(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("src/lib.rs", &numbered(10, &[]));
    fx.write("README.md", "readme\n");
    fx.commit_all("first");
    fx.write("src/lib.rs", &numbered(10, &[(3, "line three")]));
    fx.commit_all("second");
    fx.write(
        "src/lib.rs",
        &numbered(10, &[(3, "line three"), (8, "line eight")]),
    );
    fx.commit_all("third");
    let second = oid(&fx, "HEAD~1");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ui = ctx_of(&tab, cx).ui;
    draw(cx, handle);

    // A click on the log row of "second" (row ids are layout rows: 0 = the hidden uncommitted row, 1 = HEAD)
    // selects it; the right pane shows its changes.
    click(cx, handle, (crate::log_view::ids::LOG_ROW, 2usize));
    assert_eq!(
        cx.update(|cx| ui.read(cx).selection.commit()),
        Some((second, false)),
        "the log row selected the commit"
    );
    wait_until(cx, "the commit's files", |cx| {
        present(cx, handle, (crate::commit_changes::ids::ROW, 0usize))
    });
    assert!(present(cx, handle, crate::commit_changes::ids::MESSAGE));
    assert!(
        !present(cx, handle, (crate::commit_changes::ids::ROW, 1usize)),
        "one file changed in that commit"
    );

    // The file row opens its diff in the centre.
    click(cx, handle, (crate::commit_changes::ids::ROW, 0usize));
    wait_until(cx, "the commit diff", |cx| diff_doc(&tab, cx).is_some());
    let doc = diff_doc(&tab, cx).expect("diff");
    let text = doc.text().expect("text");
    assert!(
        text.lines
            .iter()
            .any(|l| l.new_lineno == Some(3) && l.old_lineno.is_none()),
        "the diff is the commit's change of line 3"
    );
    assert!(present(cx, handle, crate::diff::ids::HEADER));
    assert!(
        !present(cx, handle, crate::diff::ids::STAGE_FILE),
        "a commit diff has no Stage button"
    );

    // Its menu opens blame at that commit.
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.right_click((crate::commit_changes::ids::ROW, 0usize), cx);
        window.render_frame(cx);
        window.click(menu_item(crate::menus::commit_files::ids::BLAME), cx);
    });
    wait_until(cx, "the blame", |cx| {
        present(cx, handle, (crate::blame::ids::BLAME_COMMIT, 0usize))
    });
    assert!(
        cx.update(|cx| ui.read(cx).blame.as_ref().map(|b| b.path.clone()))
            == Some(RelaPath::from("src/lib.rs"))
    );
    // A blame row selects its commit in the log.
    click(cx, handle, (crate::blame::ids::BLAME_COMMIT, 2usize));
    assert_eq!(
        cx.update(|cx| ui.read(cx).selection.commit()),
        Some((second, false)),
        "line 3 was last changed by the second commit"
    );
    // Esc closes the blame (view stack), then the file history opens from the same menu.
    with_window(cx, handle, |window, cx| window.press("escape", cx));
    draw(cx, handle);
    assert!(
        cx.update(|cx| ui.read(cx).blame.is_none()),
        "Esc closed the blame"
    );
    wait_until(cx, "the commit's files", |cx| {
        present(cx, handle, (crate::commit_changes::ids::ROW, 0usize))
    });
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.right_click((crate::commit_changes::ids::ROW, 0usize), cx);
        window.render_frame(cx);
        window.click(menu_item(crate::menus::commit_files::ids::HISTORY), cx);
    });
    wait_until(cx, "the file history", |cx| {
        present(cx, handle, (crate::file_history::ids::HISTORY_ROW, 2usize))
    });
    assert!(
        !present(cx, handle, (crate::file_history::ids::HISTORY_ROW, 3usize)),
        "three commits touched src/lib.rs"
    );
    assert!(
        !present(cx, handle, crate::side_panel::ids::SIDE_PANEL),
        "the history replaces the frame's panes"
    );
    click(cx, handle, crate::file_history::ids::HISTORY_CLOSE);
    assert!(cx.update(|cx| ui.read(cx).history.is_none()));
    assert!(present(cx, handle, crate::side_panel::ids::SIDE_PANEL));
}

// ───────────────────────────── branch → checkout → merge conflict → resolve → commit ─────────────────────────────

#[gpui_kit::test]
fn branch_create_checkout_merge_conflict_resolve_commit(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "base\n");
    fx.commit_all("base");
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);

    // Create "feature" through the dialog (checkout on by default).
    with_window(cx, handle, |window, cx| {
        crate::dialogs::branch::open_new_branch(window, cx, ctx.clone(), StartPoint::Head)
    });
    draw(cx, handle);
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("single-field-input", cx);
        window.render_frame(cx);
        window.input("feature", cx);
    });
    with_window(cx, handle, |window, cx| window.press("enter", cx));
    wait_until(cx, "the new branch", |cx| {
        local_branches(&tab, cx).iter().any(|(n, _)| n == "feature")
    });
    draw(cx, handle);
    assert!(present(
        cx,
        handle,
        crate::side_panel::ids::branch("refs/heads/feature")
    ));
    assert!(!dialog_open(cx, handle), "the dialog closed");

    // Diverge: a commit on feature, one on main.
    fx.write("a.txt", "feature\n");
    fx.commit_all("feature change");
    // Back to main by double-clicking its side panel row.
    run_on(cx, handle, &tab, |t, _, cx| t.reload(RefreshSet::ALL, cx));
    draw(cx, handle);
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.double_click(crate::side_panel::ids::branch("refs/heads/main"), cx);
    });
    wait_until(cx, "the checkout", |cx| !busy(&tab, cx));
    assert_eq!(fx.git(&["branch", "--show-current"]).trim(), "main");
    assert!(
        local_branches(&tab, cx).contains(&("main".to_owned(), true)),
        "the side panel model follows HEAD"
    );
    fx.write("a.txt", "main\n");
    fx.commit_all("main change");
    run_on(cx, handle, &tab, |t, _, cx| t.reload(RefreshSet::ALL, cx));

    // Merge feature: conflicts stop it (Warning toast), the status shows the conflicted file.
    run_on(cx, handle, &tab, |t, w, cx| {
        t.merge("feature".into(), w, cx)
    });
    wait_until(cx, "the merge", |cx| !busy(&tab, cx));
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .iter()
            .any(|t| t.contains("conflict")),
        "a conflict toast: {:?}",
        cx.update(|cx| toast_texts(&app, cx))
    );
    let in_progress = cx.update(|cx| {
        ctx.model
            .read(cx)
            .state
            .value
            .as_ref()
            .and_then(|s| s.in_progress)
    });
    assert_eq!(in_progress, Some(crate::backend::InProgress::Merge));
    draw(cx, handle);
    assert!(
        present(cx, handle, (crate::log_view::ids::LOG_ROW, 0usize)),
        "the uncommitted row shows during the merge"
    );

    // Resolve with "theirs" and commit through the commit box (Merge button).
    run_on(cx, handle, &tab, |t, w, cx| {
        t.resolve_conflict(vec!["a.txt".into()], Resolution::Theirs, w, cx)
    });
    wait_until(cx, "the resolution", |cx| !busy(&tab, cx));
    assert_eq!(
        std::fs::read_to_string(fx.path().join("a.txt")).expect("file"),
        "feature\n"
    );
    draw(cx, handle);
    // The checkout selected the branch (commit changes on the right); the uncommitted row brings the commit box.
    click(cx, handle, (crate::log_view::ids::LOG_ROW, 0usize));
    click(cx, handle, crate::status_pane::commit_box::ids::PRIMARY);
    wait_until(cx, "the merge commit", |cx| !busy(&tab, cx));
    assert_eq!(
        fx.git(&["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count(),
        3,
        "HEAD is a merge commit"
    );
    let state = cx.update(|cx| ctx.model.read(cx).state.value.clone().expect("state"));
    assert_eq!(state.in_progress, None, "the merge finished");
    assert!(
        log_summaries(&tab, cx)
            .first()
            .is_some_and(|s| s.contains("Merge")),
        "the log shows the merge commit on top: {:?}",
        log_summaries(&tab, cx)
    );
}

// ───────────────────────────── dialogs against a real repository ─────────────────────────────

/// Types `text` into the field with element id `id` of the open dialog, replacing what it holds.
fn type_into(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str, text: &str) {
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
        window.render_frame(cx);
        window.press("ctrl-a", cx);
        window.input(text, cx);
    });
}

#[gpui_kit::test]
fn rename_branch_dialog_renames_the_branch(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "base\n");
    fx.commit_all("base");
    fx.git(&["branch", "topic"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    with_window(cx, handle, |window, cx| {
        crate::dialogs::branch::open_rename_branch(
            window,
            cx,
            ctx.clone(),
            crate::test_support::seed::full("refs/heads/topic"),
        )
    });
    draw(cx, handle);
    type_into(cx, handle, "single-field-input", "renamed");
    with_window(cx, handle, |window, cx| window.press("enter", cx));
    wait_until(cx, "the rename", |cx| {
        local_branches(&tab, cx).iter().any(|(n, _)| n == "renamed")
    });
    let branches = fx.git(&["branch", "--format=%(refname:short)"]);
    assert!(
        branches.lines().any(|b| b == "renamed") && !branches.lines().any(|b| b == "topic"),
        "git has the new name only: {branches}"
    );
    assert!(!dialog_open(cx, handle), "the dialog closed");
}

#[gpui_kit::test]
fn reset_dialog_moves_head_with_the_chosen_mode(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("first");
    let first = oid(&fx, "HEAD");
    fx.write("a.txt", "two\n");
    fx.commit_all("second");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    with_window(cx, handle, |window, cx| {
        crate::dialogs::reset::open(window, cx, ctx.clone(), first)
    });
    draw(cx, handle);
    assert!(present(
        cx,
        handle,
        crate::dialogs::reset::ids::RESET_DIALOG
    ));
    click(cx, handle, "dialog-primary");
    wait_until(cx, "the reset", |cx| {
        !busy(&tab, cx) && !dialog_open(cx, handle)
    });
    assert_eq!(
        fx.git(&["rev-parse", "HEAD"]).trim(),
        first.to_string(),
        "HEAD moved to the first commit"
    );
    assert_eq!(
        std::fs::read_to_string(fx.path().join("a.txt")).expect("a.txt"),
        "two\n",
        "the default mode (mixed) keeps the working tree"
    );
    assert!(
        fx.git(&["status", "--porcelain"]).contains("a.txt"),
        "the change is unstaged now"
    );
}

#[gpui_kit::test]
fn new_remote_dialog_adds_the_remote(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "base\n");
    fx.commit_all("base");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    with_window(cx, handle, |window, cx| {
        crate::dialogs::remote::open_new_remote(window, cx, ctx.clone())
    });
    draw(cx, handle);
    type_into(
        cx,
        handle,
        crate::dialogs::remote::ids::REMOTE_NAME,
        "mirror",
    );
    type_into(
        cx,
        handle,
        crate::dialogs::remote::ids::REMOTE_FETCH,
        "https://example.invalid/mirror.git",
    );
    click(cx, handle, "dialog-primary");
    wait_until(cx, "the new remote", |cx| {
        !busy(&tab, cx) && !dialog_open(cx, handle)
    });
    assert_eq!(
        fx.git(&["remote", "get-url", "mirror"]).trim(),
        "https://example.invalid/mirror.git"
    );
}

// ───────────────────────────── interactive rebase ─────────────────────────────

/// Open the editor from HEAD~3, reword one step, squash one and drop one, complete: Git runs the plan through the
/// editor helper and the editor closes on success.
#[gpui_kit::test]
fn interactive_rebase_editor_round_trip(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("base.txt", "base\n");
    fx.commit_all("base");
    for (file, msg) in [
        ("a.txt", "add a"),
        ("b.txt", "add b"),
        ("c.txt", "add c"),
        ("d.txt", "add d"),
    ] {
        fx.write(file, &format!("{file}\n"));
        fx.commit_all(msg);
    }
    let Some(rt) = fx.runtime_with_helpers() else {
        eprintln!("skipped: the helper binaries are built by a full `cargo test -p gitcthulhu`");
        return;
    };
    let base = oid(&fx, "HEAD~4");
    let (handle, _app, tab) = mount_fixture_on(cx, &fx, rt, DEFAULT_SIZE, false);
    let ctx = ctx_of(&tab, cx);
    cx.update(|cx| crate::rebase_interactive::open(&ctx, Some(base), cx));
    wait_until(cx, "the plan", |cx| {
        cx.update(|cx| ctx.ui.read(cx).rebase_plan.is_some())
    });
    draw(cx, handle);
    assert!(present(cx, handle, crate::rebase_interactive::ids::REBASE));
    assert!(
        !present(cx, handle, crate::side_panel::ids::SIDE_PANEL),
        "the side panel hides while the plan is edited"
    );
    // Oldest first: add a, add b, add c, add d.
    cx.update(|cx| {
        ctx.ui.update(cx, |ui, cx| {
            let plan = ui.rebase_plan.as_mut().expect("plan");
            let summaries: Vec<_> = plan.steps.iter().map(|s| s.summary.clone()).collect();
            assert_eq!(summaries, ["add a", "add b", "add c", "add d"]);
            plan.steps[0].action = RebaseAction::Reword;
            plan.steps[0].new_message = Some("add a (reworded)".into());
            // Squash takes no message of its own in the editor (only Reword has a field): the group keeps the
            // reworded message of its leading step.
            plan.steps[1].action = RebaseAction::Squash;
            plan.steps[2].action = RebaseAction::Drop;
            cx.notify();
        })
    });
    draw(cx, handle);
    click(cx, handle, crate::rebase_interactive::ids::COMPLETE);
    wait_until(cx, "the rebase", |cx| !busy(&tab, cx));
    assert_eq!(
        fx.git(&["log", "--format=%s", "HEAD~2..HEAD"])
            .lines()
            .collect::<Vec<_>>(),
        ["add d", "add a (reworded)"],
        "squashed, dropped and reworded as planned"
    );
    assert_eq!(
        fx.git(&["ls-tree", "--name-only", "HEAD~1"])
            .lines()
            .collect::<Vec<_>>(),
        ["a.txt", "b.txt", "base.txt"],
        "b was squashed into a"
    );
    assert!(
        !fx.path().join("c.txt").exists(),
        "the dropped commit's file is gone"
    );
    assert!(
        cx.update(|cx| ctx.ui.read(cx).rebase_plan.is_none()),
        "the editor closes after a successful rebase"
    );
    draw(cx, handle);
    assert!(present(cx, handle, crate::side_panel::ids::SIDE_PANEL));
    assert_eq!(
        log_summaries(&tab, cx),
        ["add d", "add a (reworded)", "base"],
        "the log shows the rewritten history"
    );
}

// ───────────────────────────── stash / pop ─────────────────────────────

/// Also a regression test: popping from the row's menu removes the row under the resting pointer, which moved a
/// section header with a hover-only "+" under it and crashed the paint (side panel header, 21 §6 #49).
#[gpui_kit::test]
fn stash_and_pop_through_toolbar_and_side_panel(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "changed\n");
    fx.write("new.txt", "untracked\n");
    // Undo history captures untracked files: no question is needed.
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    draw(cx, handle);
    click(cx, handle, crate::toolbar::ids::STASH);
    wait_until(cx, "the stash", |cx| !busy(&tab, cx));
    assert_eq!(fx.git(&["stash", "list"]).lines().count(), 1, "one stash");
    assert!(
        !fx.path().join("new.txt").exists(),
        "untracked files are stashed too"
    );
    let (staged, unstaged) = status_paths(&tab, cx);
    assert!(staged.is_empty() && unstaged.is_empty(), "a clean tree");
    assert_eq!(
        stash_count(&tab, cx),
        1,
        "the side panel model shows the stash"
    );
    draw(cx, handle);
    let stash_id = fx.git(&["rev-parse", "stash@{0}"]).trim().to_owned();
    assert!(present(
        cx,
        handle,
        crate::side_panel::ids::stash(&stash_id)
    ));

    // Pop from the stash row's menu.
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.right_click(crate::side_panel::ids::stash(&stash_id), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.click(menu_item("stash-pop"), cx);
    });
    wait_until(cx, "the pop", |cx| !busy(&tab, cx));
    assert!(
        fx.git(&["stash", "list"]).trim().is_empty(),
        "the stash was dropped"
    );
    assert_eq!(
        std::fs::read_to_string(fx.path().join("a.txt")).expect("file"),
        "changed\n"
    );
    assert!(
        fx.path().join("new.txt").exists(),
        "the untracked file is back"
    );
    assert_eq!(stash_count(&tab, cx), 0);
}

// ───────────────────────────── network ─────────────────────────────

#[gpui_kit::test]
fn fetch_pull_push_against_local_bare_remote(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    let remote = with_remote(&fx);
    remote.publish(&fx, "theirs.txt", "theirs\n");
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);

    run_on(cx, handle, &tab, |t, w, cx| t.fetch_all(w, cx));
    wait_until(cx, "the fetch", |cx| !busy(&tab, cx));
    assert_eq!(
        fx.git(&["log", "-1", "--format=%s", "origin/main"]).trim(),
        "theirs theirs.txt",
        "fetch updated the remote-tracking branch"
    );

    // Pull from the toolbar: a fast-forward.
    draw(cx, handle);
    click(cx, handle, crate::toolbar::ids::PULL);
    wait_until(cx, "the pull", |cx| !busy(&tab, cx));
    assert!(
        fx.path().join("theirs.txt").exists(),
        "pull fast-forwarded main"
    );
    assert_eq!(
        log_summaries(&tab, cx).first().map(String::as_str),
        Some("theirs theirs.txt")
    );

    // A local commit pushed from the toolbar.
    fx.write("mine.txt", "mine\n");
    fx.commit_all("mine");
    run_on(cx, handle, &tab, |t, _, cx| t.reload(RefreshSet::ALL, cx));
    click(cx, handle, crate::toolbar::ids::PUSH);
    wait_until(cx, "the push", |cx| !busy(&tab, cx));
    assert_eq!(remote.head(&fx), "mine", "the remote has the commit");
    assert!(
        errors_opened(&app, cx).is_empty(),
        "no error dialog: {:?}",
        errors_opened(&app, cx)
    );
}

/// A failing pre-push hook stops the push; the error dialog names the hook and shows its output, and "Retry
/// without hooks" pushes.
#[gpui_kit::test]
fn failing_pre_push_hook_shown_in_error_dialog(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    let remote = with_remote(&fx);
    fx.write("mine.txt", "mine\n");
    fx.commit_all("mine");
    write_hook(&fx, "pre-push", "echo 'tests must pass first' >&2\nexit 1");
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_current(false, false, w, cx)
    });
    wait_until(cx, "the push", |cx| !busy(&tab, cx));
    draw(cx, handle);
    assert_eq!(remote.head(&fx), "init", "the hook stopped the push");
    assert!(dialog_open(cx, handle), "an error dialog is open");
    assert!(present(
        cx,
        handle,
        crate::dialogs::error::ids::ERROR_HOOK_OUTPUT
    ));
    let shown = cx.update(|cx| app.read(cx).error_texts.join("\n"));
    assert!(
        shown.contains("tests must pass first"),
        "the hook's output is shown: {shown:?}"
    );
    assert!(
        cx.update(|cx| app.read(cx).error_texts.join("\n"))
            .contains("pre-push"),
        "the dialog names the hook: {:?}",
        errors_opened(&app, cx)
    );
    click(cx, handle, crate::dialogs::error::ids::ERROR_RETRY);
    wait_until(cx, "the retry", |cx| !busy(&tab, cx));
    assert_eq!(remote.head(&fx), "mine", "the retry skipped the hook");
}

/// A prompter that answers from the test thread without the UI (the operation's background half runs on the test
/// thread in GPUI tests, so a prompt answered by a dialog would wait for itself).
struct Answers(parking_lot::Mutex<Vec<CredentialRequest>>);

impl CredentialPrompter for Answers {
    fn prompt(&self, request: &CredentialRequest) -> CredentialResponse {
        self.0.lock().push(request.clone());
        let answer = match &request.kind {
            PromptKind::HttpsUsername { .. } => "alice",
            PromptKind::HttpsPassword { .. } => "s3cret",
            _ => return CredentialResponse::Cancel,
        };
        CredentialResponse::Answer(Sensitive::new(answer.to_owned()))
    }
}

/// Credentials asked during a push reach the tab's prompter through the askpass helper (a pre-push hook runs
/// `git credential fill`, which asks through Git's askpass like an HTTPS remote would), and the window's prompter
/// shows the credential dialog for the asking tab and hands the typed answer back.
#[gpui_kit::test]
fn credential_prompt_via_askpass_and_dialog(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    let remote = with_remote(&fx);
    fx.write("mine.txt", "mine\n");
    fx.commit_all("mine");
    write_hook(
        &fx,
        "pre-push",
        "printf 'protocol=https\\nhost=gitcthulhu.invalid\\n\\n' | git -c credential.helper= credential fill > \"$(git rev-parse --git-dir)/cred.txt\"",
    );
    let Some(rt) = fx.runtime_with_helpers() else {
        eprintln!("skipped: the helper binaries are built by a full `cargo test -p gitcthulhu`");
        return;
    };
    let (handle, _app, tab) = mount_fixture_on(cx, &fx, rt, DEFAULT_SIZE, false);

    // 1. Askpass → the tab's prompter, during a real push.
    let answers = Arc::new(Answers(parking_lot::Mutex::default()));
    cx.update(|cx| tab.update(cx, |t, _| t.prompter = answers.clone()));
    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_current(false, false, w, cx)
    });
    wait_until(cx, "the push", |cx| !busy(&tab, cx));
    assert_eq!(remote.head(&fx), "mine", "the push went through");
    let asked: Vec<_> = answers.0.lock().iter().map(|r| r.kind.clone()).collect();
    assert!(
        matches!(
            asked.as_slice(),
            [
                PromptKind::HttpsUsername { .. },
                PromptKind::HttpsPassword { .. }
            ]
        ),
        "username and password were asked through askpass: {asked:?}"
    );
    let cred = std::fs::read_to_string(fx.path().join(".git").join("cred.txt")).expect("cred.txt");
    assert!(
        cred.contains("username=alice") && cred.contains("password=s3cret"),
        "git received the answers: {cred}"
    );

    // 2. The window's prompter → the credential dialog → the answer (a thread waits like Git's helper does).
    let tx = cx.update(|cx| crate::app::ui_requests(cx).expect("the request pump"));
    let tab_id = cx.update(|cx| tab.read(cx).id());
    let request = CredentialRequest {
        kind: PromptKind::HttpsPassword {
            url: "https://gitcthulhu.invalid".into(),
            username: Some("alice".into()),
        },
        raw_prompt: "Password for 'https://alice@gitcthulhu.invalid': ".into(),
        operation: "Push".into(),
        repo: None,
        attempt: 1,
        cancel: crate::backend::CancelToken::new(),
    };
    // What `GuiPrompter::prompt` sends (it then blocks on the reply, which a GPUI test thread cannot do).
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    assert!(tx.send(crate::backend::prompt::UiRequest::Credential {
        tab: Some(tab_id),
        request,
        reply,
    }));
    wait_until(cx, "the credential dialog", |cx| {
        present(
            cx,
            handle,
            crate::dialogs::credentials::ids::CREDENTIAL_DIALOG,
        )
    });
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(crate::dialogs::credentials::ids::CREDENTIAL_FIELD, cx);
        window.render_frame(cx);
        window.input("typed-secret", cx);
    });
    with_window(cx, handle, |window, cx| window.press("enter", cx));
    match answer.try_recv().expect("the dialog answered") {
        CredentialResponse::Answer(secret) => assert_eq!(secret.expose(), "typed-secret"),
        CredentialResponse::Cancel => panic!("the dialog answered Cancel"),
    }
    assert!(!dialog_open(cx, handle), "the dialog closed");
}

// ───────────────────────────── hooks around commit and checkout (§8.3) ─────────────────────────────

#[gpui_kit::test]
fn failing_pre_commit_hook_dialog_and_retry_without_hooks(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    fx.git(&["add", "a.txt"]);
    write_hook(&fx, "pre-commit", "echo 'lint failed' >&2\nexit 1");
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let commit_box = cx.update(|cx| tab.read(cx).views().status.read(cx).commit_box().clone());
    with_window(cx, handle, |window, cx| {
        commit_box.update(cx, |b, cx| b.set_message("Change a".into(), window, cx))
    });
    click(cx, handle, crate::status_pane::commit_box::ids::PRIMARY);
    wait_until(cx, "the commit", |cx| !busy(&tab, cx));
    draw(cx, handle);
    assert_eq!(
        fx.git(&["log", "-1", "--format=%s"]).trim(),
        "init",
        "the hook stopped it"
    );
    assert!(
        cx.update(|cx| app.read(cx).error_texts.join("\n"))
            .contains("pre-commit"),
        "{:?}",
        errors_opened(&app, cx)
    );
    assert!(present(
        cx,
        handle,
        crate::dialogs::error::ids::ERROR_HOOK_OUTPUT
    ));
    click(cx, handle, crate::dialogs::error::ids::ERROR_RETRY);
    wait_until(cx, "the retry", |cx| !busy(&tab, cx));
    assert_eq!(fx.git(&["log", "-1", "--format=%s"]).trim(), "Change a");
}

#[gpui_kit::test]
fn failing_post_checkout_hook_shows_warning_toast(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.git(&["branch", "other"]);
    write_hook(
        &fx,
        "post-checkout",
        "echo 'post-checkout broke' >&2\nexit 3",
    );
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.checkout_branch(
            crate::backend::FullName::try_from("refs/heads/other").expect("ref name"),
            w,
            cx,
        )
    });
    wait_until(cx, "the checkout", |cx| !busy(&tab, cx));
    assert_eq!(
        fx.git(&["branch", "--show-current"]).trim(),
        "other",
        "the checkout happened"
    );
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts
            .iter()
            .any(|t| t.contains("post-checkout") && t.contains("exit code 3")),
        "a warning toast names the hook: {toasts:?}"
    );
    assert!(
        errors_opened(&app, cx).is_empty(),
        "no error dialog for a post hook"
    );
}

// ───────────────────────────── foundation guarantees ─────────────────────────────

/// A panic in an operation's background half ends the operation with an error dialog; the tab is usable again.
#[gpui_kit::test]
fn panicking_operation_leaves_the_tab_usable(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.run_op(
            OpSpec::new(OpKind::StageFile, |_env| -> Result<OpOutcome<()>, _> {
                panic!("boom in the background")
            }),
            w,
            cx,
        )
    });
    wait_until(cx, "the panicked operation", |cx| !busy(&tab, cx));
    draw(cx, handle);
    assert_eq!(errors_opened(&app, cx).len(), 1, "one error dialog");
    assert!(dialog_open(cx, handle));
    click(cx, handle, crate::dialogs::error::ids::ERROR_OK);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.stage_paths(vec!["a.txt".into()], w, cx)
    });
    wait_until(cx, "the next operation", |cx| !busy(&tab, cx));
    assert_eq!(
        status_paths(&tab, cx).0,
        ["a.txt"],
        "the next operation ran"
    );
}

/// Without undo history nothing can restore deleted untracked files: discard, delete and `stash -u` ask first
/// and run nothing when declined (fail closed, T6).
#[gpui_kit::test]
fn untracked_loss_without_capture_asks_first(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    fx.write("keep.txt", "keep me\n");
    let (handle, _app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, false);
    let off = crate::backend::handle::UndoSettings {
        enabled: false,
        ..crate::backend::handle::UndoSettings::default()
    };
    run_on(cx, handle, &tab, |t, _, cx| {
        t.apply_undo_settings(false, off, cx)
    });
    wait_until(cx, "the settings", |cx| !busy(&tab, cx));
    run_on(cx, handle, &tab, |t, w, cx| t.stash(None, true, w, cx));
    wait_until(cx, "the stash", |cx| !busy(&tab, cx));
    assert!(
        fx.git(&["stash", "list"]).trim().is_empty(),
        "declined: nothing stashed"
    );
    run_on(cx, handle, &tab, |t, w, cx| {
        t.delete_untracked(vec!["keep.txt".into()], w, cx)
    });
    wait_until(cx, "the delete", |cx| !busy(&tab, cx));
    assert!(
        fx.path().join("keep.txt").exists(),
        "declined: the file stays"
    );
}

/// A snapshot that cannot be written stops operations that delete untracked files before they run.
#[gpui_kit::test]
fn failed_snapshot_fails_closed_for_untracked_deletes(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("keep.txt", "keep me\n");
    let (handle, app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
    fx.write("a.txt", "two\n");
    crate::backend::oplog::FAIL_PREPARE.with(|f| f.set(true));
    run_on(cx, handle, &tab, |t, w, cx| {
        t.delete_untracked(vec!["keep.txt".into()], w, cx)
    });
    wait_until(cx, "the delete", |cx| !busy(&tab, cx));
    assert!(
        fx.path().join("keep.txt").exists(),
        "nothing was deleted unrecorded"
    );
    assert_eq!(
        errors_opened(&app, cx).len(),
        1,
        "the refusal is an error dialog"
    );
    assert!(
        cx.update(|cx| app.read(cx).error_texts.join("\n"))
            .contains("was not run"),
        "{:?}",
        cx.update(|cx| app.read(cx).error_texts.clone())
    );
    // Operations that delete no untracked files still run, with the "can't be undone" warning.
    run_on(cx, handle, &tab, |t, w, cx| {
        t.stage_paths(vec!["a.txt".into()], w, cx)
    });
    wait_until(cx, "the stage", |cx| !busy(&tab, cx));
    assert_eq!(status_paths(&tab, cx).0, ["a.txt"]);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.commit(
            crate::backend::CommitOptions {
                message: "unrecorded".into(),
                ..Default::default()
            },
            w,
            cx,
        )
    });
    wait_until(cx, "the commit", |cx| !busy(&tab, cx));
    crate::backend::oplog::FAIL_PREPARE.with(|f| f.set(false));
    assert_eq!(fx.git(&["log", "-1", "--format=%s"]).trim(), "unrecorded");
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .iter()
            .any(|t| t.contains("can't be undone")),
        "{:?}",
        cx.update(|cx| toast_texts(&app, cx))
    );
}

// ───────────────────────────── tabs, settings, theme ─────────────────────────────

#[gpui_kit::test]
fn tabs_open_close_reorder_with_persistence(cx: &mut TestAppContext) {
    let fxs: Vec<Fixture> = (0..3)
        .map(|i| {
            let fx = Fixture::new();
            fx.write("a.txt", &format!("{i}\n"));
            fx.commit_all("init");
            fx
        })
        .collect();
    init_app(cx);
    let rt = fxs[0].runtime();
    cx.update(|cx| {
        if let Some(state) = crate::app::state_mut(cx) {
            state.git = crate::backend::GitStatus::Ready(rt);
        }
    });
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    for fx in &fxs {
        let path = fx.path().to_owned();
        with_window(cx, handle, |window, cx| {
            app.update(cx, |app, cx| app.open_repository(path, true, window, cx))
        });
    }
    wait_until(cx, "three repository tabs", |cx| {
        cx.update(|cx| {
            app.read(cx)
                .tabs()
                .iter()
                .filter(|s| s.repo().is_some())
                .count()
                == 3
        })
    });
    let persisted = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = crate::settings::state::get(cx);
            (s.tabs_open.clone(), s.selected_tab)
        })
    };
    let paths: Vec<_> = fxs.iter().map(|f| f.path().to_owned()).collect();
    let (open, selected) = persisted(cx);
    assert_eq!(open.len(), 3);
    assert_eq!(selected, 2, "the last opened tab is selected");
    // Reorder: the selected third tab moves to the front and stays selected.
    cx.update(|cx| app.update(cx, |app, cx| app.move_tab(2, 0, cx)));
    cx.run_until_parked();
    let (open, selected) = persisted(cx);
    assert_eq!(selected, 0, "the moved tab keeps the selection");
    assert!(
        same_dir(&open[0], &paths[2]) && same_dir(&open[1], &paths[0]),
        "the order is persisted: {open:?}"
    );
    // Close the selected tab: the next one to its left (none) → the first remaining is selected.
    let (first_id, closed_tab) = cx.update(|cx| {
        let slot = &app.read(cx).tabs()[0];
        (slot.id, slot.repo().cloned().expect("repo"))
    });
    with_window(cx, handle, |window, cx| {
        app.update(cx, |app, cx| app.close_tab(first_id, window, cx))
    });
    let (open, _) = persisted(cx);
    assert_eq!(open.len(), 2, "the closed tab is no longer restored");
    assert!(
        cx.update(|cx| closed_tab.read(cx).ctx().backend.handle().is_some()),
        "the entity may linger, but it was closed"
    );
    assert_eq!(
        cx.update(|cx| app.read(cx).tab_title(app.read(cx).active(), cx)),
        "repo"
    );
    // A new window restores the persisted tabs in order.
    let (_handle2, app2) = open_app(cx, DEFAULT_SIZE);
    let rt = fxs[0].runtime();
    cx.update(|cx| {
        if let Some(state) = crate::app::state_mut(cx) {
            state.git = crate::backend::GitStatus::Ready(rt);
        }
    });
    wait_until(cx, "the restored tabs", |cx| {
        cx.update(|cx| {
            app2.read(cx)
                .tabs()
                .iter()
                .filter(|s| s.repo().is_some())
                .count()
                == 2
        })
    });
    let restored: Vec<_> = cx.update(|cx| {
        app2.read(cx)
            .tabs()
            .iter()
            .filter_map(|s| s.path(cx))
            .collect()
    });
    assert!(
        same_dir(&restored[0], &paths[0]) && same_dir(&restored[1], &paths[1]),
        "restored in the persisted order: {restored:?}"
    );
}

fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    canon(a) == canon(b)
}

/// Settings changed in the Settings dialog apply to the open tab at once: the theme re-colours the window, row
/// density and the diff mode follow.
#[gpui_kit::test]
fn settings_apply_live_and_theme_switch(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("src/lib.rs", &numbered(10, &[]));
    fx.commit_all("init");
    fx.write("src/lib.rs", &numbered(10, &[(4, "four")]));
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    draw(cx, handle);
    let row_h = |cx: &mut TestAppContext| {
        with_window(cx, handle, |window, cx| {
            window.render_frame(cx);
            crate::test_support::geometry::bounds(window, (crate::log_view::ids::LOG_ROW, 1usize))
                .size
                .height
                .as_f32()
        })
    };
    assert!((row_h(cx) - 38.).abs() < 0.5, "spaced rows are 38");

    // Theme through the Settings dialog.
    with_window(cx, handle, |window, cx| {
        crate::dialogs::settings::open(window, cx, None)
    });
    draw(cx, handle);
    click(cx, handle, crate::dialogs::settings::ids::select("theme"));
    click(
        cx,
        handle,
        crate::dialogs::settings::ids::select_option("theme", 0),
    );
    cx.update(|cx| {
        assert_eq!(settings::get(cx).theme, ThemeKind::Light);
        assert_eq!(
            cx.theme_g().kind,
            ThemeKind::Light,
            "the theme applied at once"
        );
        assert_eq!(
            cx.tokens().background,
            crate::theme::builtin::light().background
        );
    });
    with_window(cx, handle, |window, cx| window.press("escape", cx));
    assert!(!dialog_open(cx, handle), "Esc closed the settings");

    // Density and diff mode (as the dialog writes them) reach the open tab without a reload.
    cx.update(|cx| {
        settings::update(cx, |s| {
            s.lines_height = crate::settings::LinesHeight::Compact
        })
    });
    draw(cx, handle);
    assert!((row_h(cx) - 34.).abs() < 0.5, "compact rows are 34");
    let ui = ctx_of(&tab, cx).ui;
    cx.update(|cx| {
        ui.update(cx, |ui, cx| {
            ui.click_file(
                FileList::Unstaged,
                RelaPath::from("src/lib.rs"),
                Modifiers::default(),
                &[],
                cx,
            )
        })
    });
    draw(cx, handle);
    cx.update(|cx| settings::update(cx, |s| s.diff_view = DiffViewMode::Split));
    draw(cx, handle);
    assert_eq!(
        diff_doc(&tab, cx).expect("diff").mode,
        crate::diff::model::DiffMode::Split
    );
    let doc = diff_doc(&tab, cx).expect("diff");
    let pair = row_of(&doc, true, 4);
    assert!(present(cx, handle, (crate::diff::ids::SPLIT_DIVIDER, pair)));
    // Back to Dark through the dialog.
    with_window(cx, handle, |window, cx| {
        crate::dialogs::settings::open(window, cx, None)
    });
    draw(cx, handle);
    click(cx, handle, crate::dialogs::settings::ids::select("theme"));
    click(
        cx,
        handle,
        crate::dialogs::settings::ids::select_option("theme", 1),
    );
    cx.update(|cx| {
        assert_eq!(
            cx.tokens().background,
            crate::theme::builtin::dark().background
        )
    });
}
