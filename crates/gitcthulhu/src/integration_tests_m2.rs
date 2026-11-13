//! Cross-feature tests of milestone 2 (owned by X7; spec 21 §9 X7, §8.4 M2 items).
//!
//! Every test drives real repositories (the `git` executable, isolated configuration) through the regions' own
//! entry points and elements: the toolbar's Undo / Redo buttons and the toasts' actions after the milestone-1
//! operations, the terminal panel of two tabs, the AI control of the commit box against a fake provider, the forge
//! badges and the Create-PR dialog against a fake forge (also offline), the update notice against a fake manifest,
//! the worktrees section from "add" to "remove", a milestone-1 settings file and every Settings page.

use core::prelude::v1::test;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, ElementId, Entity, TestAppContext, Window,
};

use crate::app_view::AppView;
use crate::backend::{DiscardMode, ObjectId, RelaPath, ResetMode, StartPoint};
use crate::tab::RepoTab;
use crate::test_support::seed::full;
use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture_with, toast_texts};
use crate::undo::ids as undo_ids;

// ───────────────────────────── helpers ─────────────────────────────

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
    cx.run_until_parked();
    draw(cx, handle);
}

#[cfg_attr(not(any(feature = "services", feature = "terminal")), allow(dead_code))]
fn present(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
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
    cx.run_until_parked();
    result
}

/// Waits (test time and wall time) until `done` holds: for work that a real thread, process or server does.
fn wait_until(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(40);
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

fn toasts(app: &Entity<AppView>, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| toast_texts(app, cx))
}

fn errors(app: &Entity<AppView>, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        app.read(cx)
            .errors_opened
            .iter()
            .map(|t| t.to_string())
            .collect()
    })
}

/// Clicks the action of the newest toast; `false` when no visible toast has one.
fn click_toast_action(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    let clicked = with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        let newest = (0usize..256)
            .rev()
            .find(|id| window.try_find(("toast-action", *id)).is_some());
        if let Some(id) = newest {
            window.click(("toast-action", id), cx);
        }
        newest.is_some()
    });
    cx.run_until_parked();
    cx.run_until_parked();
    draw(cx, handle);
    clicked
}

/// Lets every toast expire, so that the next operation's toast is the only one.
fn expire_toasts(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();
}

/// Enabled state of (Undo, Redo) in the toolbar.
fn undo_buttons(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> (bool, bool) {
    cx.update(|cx| {
        let controller = tab.read(cx).views().undo.clone();
        let (undo, redo) = controller.read(cx).tips(cx);
        (undo.enabled, redo.enabled)
    })
}

/// A click on the toolbar's Undo (main part of the split button).
fn toolbar_undo(cx: &mut TestAppContext, handle: AnyWindowHandle, tab: &Entity<RepoTab>) {
    draw(cx, handle);
    assert!(undo_buttons(tab, cx).0, "Undo is enabled before the click");
    click(
        cx,
        handle,
        (ElementId::from(undo_ids::TOOLBAR_UNDO), "main"),
    );
    cx.run_until_parked();
    draw(cx, handle);
}

/// A click on the toolbar's Redo.
fn toolbar_redo(cx: &mut TestAppContext, handle: AnyWindowHandle, tab: &Entity<RepoTab>) {
    draw(cx, handle);
    assert!(undo_buttons(tab, cx).1, "Redo is enabled before the click");
    click(cx, handle, undo_ids::TOOLBAR_REDO);
    cx.run_until_parked();
    draw(cx, handle);
}

fn head(fx: &Fixture) -> String {
    fx.git(&["rev-parse", "HEAD"]).trim().to_owned()
}

fn subject(fx: &Fixture) -> String {
    fx.git(&["log", "-1", "--format=%s"]).trim().to_owned()
}

fn current_branch(fx: &Fixture) -> String {
    fx.git(&["branch", "--show-current"]).trim().to_owned()
}

fn read(fx: &Fixture, path: &str) -> Option<String> {
    std::fs::read_to_string(fx.path().join(path)).ok()
}

fn oid(fx: &Fixture, rev: &str) -> ObjectId {
    ObjectId::from_hex(fx.git(&["rev-parse", rev]).trim().as_bytes()).expect("hex id")
}

fn stashes(fx: &Fixture) -> usize {
    fx.git(&["stash", "list"]).lines().count()
}

fn undone_toast(app: &Entity<AppView>, cx: &mut TestAppContext) -> bool {
    toasts(app, cx).iter().any(|t| t.starts_with("Undone: "))
}

// ───────────────────────────── undo / redo across milestone-1 operations ─────────────────────────────

/// Commit (through the commit box), checkout and branch delete; each undone and redone through the toolbar buttons
/// or the toasts' actions (17 §4.5, 21 §5.1).
#[gpui_kit::test]
fn undo_redo_commit_checkout_and_branch_delete(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.git(&["checkout", "-q", "-b", "topic"]);
    fx.write("topic.txt", "topic\n");
    fx.commit_all("topic work");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let init = head(&fx);
    let topic = oid(&fx, "topic");
    let (handle, app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
    draw(cx, handle);
    assert_eq!(
        undo_buttons(&tab, cx),
        (false, false),
        "a fresh repository has nothing to undo"
    );

    // Commit through the commit box.
    let commit_box = cx.update(|cx| tab.read(cx).views().status.read(cx).commit_box().clone());
    with_window(cx, handle, |window, cx| {
        commit_box.update(cx, |b, cx| b.set_message("Add b".to_owned(), window, cx))
    });
    draw(cx, handle);
    click(cx, handle, crate::status_pane::commit_box::ids::PRIMARY);
    assert_eq!(subject(&fx), "Add b", "the commit box committed");
    assert_eq!(undo_buttons(&tab, cx), (true, false));
    toolbar_undo(cx, handle, &tab);
    assert_eq!(head(&fx), init, "Undo moved HEAD back");
    assert_eq!(
        fx.git(&["diff", "--cached", "--name-only"]).trim(),
        "b.txt",
        "and the staged change is staged again"
    );
    assert!(undone_toast(&app, cx), "{:?}", toasts(&app, cx));
    assert_eq!(undo_buttons(&tab, cx), (false, true));
    toolbar_redo(cx, handle, &tab);
    assert_eq!(subject(&fx), "Add b", "Redo restored the commit");
    let committed = head(&fx);

    // Checkout: undone and redone through the toasts.
    expire_toasts(cx);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.checkout_branch(full("refs/heads/topic"), w, cx)
    });
    assert_eq!(current_branch(&fx), "topic");
    assert!(
        click_toast_action(cx, handle),
        "the checkout toast offers Undo"
    );
    assert_eq!(
        current_branch(&fx),
        "main",
        "the toast's Undo checked main out again"
    );
    assert_eq!(head(&fx), committed);
    assert!(click_toast_action(cx, handle), "the undo toast offers Redo");
    assert_eq!(current_branch(&fx), "topic", "the toast's Redo");
    toolbar_undo(cx, handle, &tab);
    assert_eq!(current_branch(&fx), "main");

    // Branch delete (an unmerged branch, forced): Undo brings it back at its tip.
    expire_toasts(cx);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.delete_branch(full("refs/heads/topic"), true, w, cx)
    });
    assert!(fx.git(&["branch", "--list", "topic"]).trim().is_empty());
    toolbar_undo(cx, handle, &tab);
    assert_eq!(
        oid(&fx, "refs/heads/topic"),
        topic,
        "Undo restored the branch"
    );
    toolbar_redo(cx, handle, &tab);
    assert!(
        fx.git(&["branch", "--list", "topic"]).trim().is_empty(),
        "Redo deleted it again"
    );
    assert!(errors(&app, cx).is_empty(), "{:?}", errors(&app, cx));
}

/// Discard with the untracked capture on (Undo brings the deleted untracked file back) and off (asked first; Undo
/// restores the tracked change only, and the history says so).
#[gpui_kit::test]
fn undo_redo_discard_with_and_without_untracked_capture(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "changed\n");
    fx.write("new.txt", "fresh\n");
    let (handle, app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
    let paths = vec![RelaPath::from("a.txt"), RelaPath::from("new.txt")];
    run_on(cx, handle, &tab, |t, w, cx| {
        t.discard_paths(
            paths.clone(),
            DiscardMode::StagedAndUnstaged,
            vec![RelaPath::from("new.txt")],
            w,
            cx,
        )
    });
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("one\n"));
    assert_eq!(read(&fx, "new.txt"), None, "the untracked file was deleted");
    toolbar_undo(cx, handle, &tab);
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("changed\n"));
    assert_eq!(
        read(&fx, "new.txt").as_deref(),
        Some("fresh\n"),
        "the captured untracked file is back"
    );
    toolbar_redo(cx, handle, &tab);
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("one\n"));
    assert_eq!(read(&fx, "new.txt"), None);

    // Without the capture (Settings > Environment).
    cx.update(|cx| crate::settings::update(cx, |s| s.undo_capture_untracked = false));
    cx.run_until_parked();
    cx.run_until_parked();
    fx.write("a.txt", "again\n");
    fx.write("lost.txt", "gone\n");
    run_on(cx, handle, &tab, |t, _, cx| {
        t.reload(crate::backend::RefreshSet::STATUS, cx)
    });
    run_on(cx, handle, &tab, |t, w, cx| {
        t.discard_paths(
            vec![RelaPath::from("a.txt"), RelaPath::from("lost.txt")],
            DiscardMode::StagedAndUnstaged,
            vec![RelaPath::from("lost.txt")],
            w,
            cx,
        )
    });
    assert_eq!(
        read(&fx, "lost.txt"),
        None,
        "confirmed (the asker answers yes)"
    );
    let entries = cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .backend
            .handle()
            .expect("real")
            .oplog_list(crate::backend::PageRequest {
                before: None,
                limit: 10,
                include_bookkeeping: false,
            })
            .expect("history")
            .entries
    });
    assert!(
        crate::undo::texts::badges(&entries[0]).contains(&"Some files not captured"),
        "the history row says the untracked file was not captured: {:?}",
        crate::undo::texts::badges(&entries[0])
    );
    toolbar_undo(cx, handle, &tab);
    assert_eq!(
        read(&fx, "a.txt").as_deref(),
        Some("again\n"),
        "the tracked change is back"
    );
    assert_eq!(
        read(&fx, "lost.txt"),
        None,
        "the uncaptured file stays deleted"
    );
    assert!(errors(&app, cx).is_empty(), "{:?}", errors(&app, cx));
}

/// Stash and pop (the toolbar's buttons), merge and a hard reset; each undone and redone with the toolbar.
#[gpui_kit::test]
fn undo_redo_stash_pop_merge_and_reset(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.write("f.txt", "feature\n");
    fx.commit_all("feature work");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("m.txt", "main\n");
    fx.commit_all("main work");
    fx.write("a.txt", "dirty\n");
    let main_tip = head(&fx);
    let (handle, app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
    draw(cx, handle);

    // Stash (toolbar).
    click(cx, handle, crate::toolbar::ids::STASH);
    assert_eq!(stashes(&fx), 1, "the toolbar's Stash stashed");
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("one\n"));
    toolbar_undo(cx, handle, &tab);
    assert_eq!(stashes(&fx), 0, "Undo dropped the stash");
    assert_eq!(
        read(&fx, "a.txt").as_deref(),
        Some("dirty\n"),
        "and restored the change"
    );
    toolbar_redo(cx, handle, &tab);
    assert_eq!(stashes(&fx), 1);
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("one\n"));

    // Pop (toolbar).
    click(cx, handle, crate::toolbar::ids::POP_STASH);
    assert_eq!(stashes(&fx), 0, "the toolbar's Pop popped");
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("dirty\n"));
    toolbar_undo(cx, handle, &tab);
    assert_eq!(stashes(&fx), 1, "Undo brought the stash back");
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("one\n"));
    toolbar_redo(cx, handle, &tab);
    assert_eq!(stashes(&fx), 0);
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("dirty\n"));

    // Merge (a real merge commit; the unrelated local change stays).
    run_on(cx, handle, &tab, |t, w, cx| {
        t.merge("feature".to_owned(), w, cx)
    });
    let merged = head(&fx);
    assert_eq!(
        fx.git(&["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count(),
        3,
        "a merge commit"
    );
    toolbar_undo(cx, handle, &tab);
    assert_eq!(head(&fx), main_tip, "Undo removed the merge");
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("dirty\n"));
    toolbar_redo(cx, handle, &tab);
    assert_eq!(head(&fx), merged, "Redo restored the same merge commit");

    // Hard reset: Undo brings the commit and the discarded local change back.
    let target = oid(&fx, "HEAD~1");
    run_on(cx, handle, &tab, |t, w, cx| {
        t.reset(target, ResetMode::Hard, w, cx)
    });
    assert_eq!(head(&fx), main_tip);
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("one\n"));
    toolbar_undo(cx, handle, &tab);
    assert_eq!(head(&fx), merged, "Undo of the reset");
    assert_eq!(
        read(&fx, "a.txt").as_deref(),
        Some("dirty\n"),
        "the change the hard reset threw away is back"
    );
    toolbar_redo(cx, handle, &tab);
    assert_eq!(head(&fx), main_tip);
    assert!(errors(&app, cx).is_empty(), "{:?}", errors(&app, cx));
}

// ───────────────────────────── terminal panel ─────────────────────────────

#[cfg(feature = "terminal")]
mod terminal {
    use super::*;
    use crate::metrics;
    use crate::terminal::{TerminalPanel, ids, panel_of, toggle};
    use crate::test_support::geometry::bounds;
    use crate::test_support::mount_fixture;
    use gpui_kit::{point, px};
    use std::path::Path;

    /// Adds a second tab over another fixture to the window of [`mount_fixture`] (same options: no watcher, no prompts).
    fn add_fixture_tab(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        app: &Entity<AppView>,
        fixture: &Fixture,
    ) -> Entity<RepoTab> {
        use crate::backend::RefreshSet;
        use crate::backend::handle::{self, TabBackend};
        use crate::tab::{PrompterMode, TabId, TabOptions};

        let rt = fixture.runtime();
        let undo = cx.update(|cx| crate::app::undo_settings(cx));
        let repo = handle::open_repo(&rt, fixture.path(), &undo).expect("the fixture opens");
        let tab = cx
            .update_window(handle, |_, window, cx| {
                let name = repo.display_name();
                let path = repo.workdir().unwrap_or_default();
                let tab = cx.new(|cx| {
                    RepoTab::new(
                        TabId::next(),
                        TabBackend::Real(std::sync::Arc::new(repo)),
                        name,
                        path,
                        TabOptions {
                            watch: false,
                            prompter: PrompterMode::Refuse,
                        },
                        window,
                        cx,
                    )
                });
                app.update(cx, |app, cx| {
                    app.add_repo_tab(tab.clone(), true, window, cx)
                });
                tab.update(cx, |tab, cx| tab.reload(RefreshSet::ALL, cx));
                tab
            })
            .expect("window");
        cx.run_until_parked();
        tab
    }

    fn panel(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> Entity<TerminalPanel> {
        let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
        cx.update(|cx| panel_of(&ctx, cx))
            .expect("a terminal panel")
    }

    fn toggle_tab(cx: &mut TestAppContext, handle: AnyWindowHandle, tab: &Entity<RepoTab>) {
        let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
        with_window(cx, handle, |window, cx| toggle(&ctx, window, cx));
        draw(cx, handle);
    }

    fn visible(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> bool {
        cx.update(|cx| tab.read(cx).ctx().ui.read(cx).terminal_visible)
    }

    fn wait_for_prompt(cx: &mut TestAppContext, panel: &Entity<TerminalPanel>) {
        wait_until(cx, "the shell prints its prompt", |cx| {
            cx.update(|cx| {
                panel.read(cx).active_terminal().is_some_and(|view| {
                    let view = view.read(cx);
                    view.has_printed() && !view.screen_text().trim().is_empty()
                })
            })
        });
    }

    fn select_tab(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        app: &Entity<AppView>,
        ix: usize,
    ) {
        with_window(cx, handle, |window, cx| {
            app.update(cx, |app, cx| app.select_tab(ix, window, cx))
        });
        draw(cx, handle);
    }

    /// Whether the process `pid` still runs.
    fn process_alive(pid: u32) -> bool {
        if cfg!(windows) {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .expect("tasklist runs");
            String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        } else {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .is_ok_and(|s| s.success())
        }
    }

    /// The command that writes the process id of a long-running child of the shell to `file` and keeps it running.
    fn child_writing_pid(file: &Path) -> String {
        let file = file.to_string_lossy();
        if cfg!(windows) {
            format!(
                "powershell -NoProfile -Command \"$PID | Out-File -Encoding ascii '{file}'; Start-Sleep 120\"\r"
            )
        } else {
            format!("sh -c 'echo $$ > \"{file}\"; exec sleep 120'\r")
        }
    }

    /// Two repository tabs: each has its own panel and visibility; the height dragged in one is persisted and used by
    /// the other; closing a tab ends its shell and the shell's children (21 §5.2, 18 §5.3).
    #[gpui_kit::test]
    fn terminal_panel_per_tab_resize_persist_and_child_shutdown(cx: &mut TestAppContext) {
        let first = Fixture::new();
        first.write("README.md", "first\n");
        first.commit_all("init");
        let second = Fixture::new();
        second.write("README.md", "second\n");
        second.commit_all("init");
        let (handle, app, tab_a) = mount_fixture(cx, &first, DEFAULT_SIZE);
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::settings::update(cx, |s| {
                s.terminal_shell = if cfg!(windows) {
                    crate::settings::TerminalShell::Cmd
                } else {
                    crate::settings::TerminalShell::Auto
                }
            })
        });
        let tab_b = add_fixture_tab(cx, handle, &app, &second);
        let (panel_a, panel_b) = (panel(cx, &tab_a), panel(cx, &tab_b));
        assert_ne!(
            panel_a.entity_id(),
            panel_b.entity_id(),
            "one panel per tab"
        );

        // Tab B (selected): show, drag higher; the height is persisted globally.
        toggle_tab(cx, handle, &tab_b);
        wait_for_prompt(cx, &panel_b);
        assert!(visible(cx, &tab_b) && !visible(cx, &tab_a));
        with_window(cx, handle, |window, cx| {
            window.render_frame(cx);
            let grab = bounds(window, ids::RESIZER).center();
            window.drag(grab, point(grab.x, grab.y - px(50.)), cx);
            window.render_frame(cx);
        });
        let dragged = cx.update(|cx| crate::settings::state::get(cx).terminal_height);
        assert_eq!(
            dragged,
            metrics::TERMINAL_DEFAULT_H + 50.,
            "the dragged height is persisted"
        );

        // Tab A: hidden until toggled, then at the persisted height; B keeps its own state.
        select_tab(cx, handle, &app, 0);
        with_window(cx, handle, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                bounds(window, ids::PANEL).size.height.as_f32(),
                0.,
                "tab A's panel is hidden"
            );
        });
        toggle_tab(cx, handle, &tab_a);
        wait_for_prompt(cx, &panel_a);
        with_window(cx, handle, |window, cx| {
            window.render_frame(cx);
            let panel = bounds(window, ids::PANEL);
            assert_eq!(
                panel.size.height.as_f32(),
                dragged + 16.,
                "the persisted height"
            );
            assert_eq!(
                panel.bottom().as_f32(),
                720. - metrics::BOTTOM_BAR_H - metrics::FRAME_DIVIDER_H,
                "the panel sits on the divider above the 32 dp bottom bar"
            );
            let toolbar = bounds(window, crate::toolbar::ids::CENTER_GROUP);
            assert!(
                toolbar.top().as_f32() >= 36.,
                "the tab strip keeps its 36 dp"
            );
        });
        toggle_tab(cx, handle, &tab_a);
        assert!(!visible(cx, &tab_a), "hidden again");
        assert!(visible(cx, &tab_b), "tab B's panel is still shown");
        assert_eq!(
            cx.update(|cx| panel_a.read(cx).terminals().count()),
            1,
            "a hidden panel keeps its shell"
        );

        // Close tab B with a long-running child of its shell: the shell and the child end.
        select_tab(cx, handle, &app, 1);
        let view = cx
            .update(|cx| panel_b.read(cx).active_terminal().cloned())
            .expect("tab B's terminal");
        let pid_file = second.dir.path().join("child.pid");
        let command = child_writing_pid(&pid_file);
        with_window(cx, handle, |window, cx| {
            view.update(cx, |v, cx| v.send_text(&command, window, cx))
        });
        wait_until(cx, "the child writes its process id", |_| {
            std::fs::read_to_string(&pid_file)
                .ok()
                .is_some_and(|t| t.trim().parse::<u32>().is_ok())
        });
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .expect("pid file")
            .trim()
            .parse()
            .expect("a process id");
        assert!(process_alive(pid), "the child runs while the tab is open");
        let id = cx.update(|cx| tab_b.read(cx).id());
        let weak = panel_b.downgrade();
        drop((panel_b, view, tab_b));
        with_window(cx, handle, |window, cx| {
            app.update(cx, |app, cx| app.close_tab(id, window, cx));
        });
        draw(cx, handle);
        if present(cx, handle, "dialog-primary") {
            click(cx, handle, "dialog-primary");
        }
        wait_until(cx, "the closed tab's terminal is released", |_| {
            weak.upgrade().is_none()
        });
        wait_until(cx, "the shell's child ends with the tab", |_| {
            !process_alive(pid)
        });
        assert_eq!(cx.update(|cx| app.read(cx).tabs().len()), 1);
    }
}

// ───────────────────────────── AI, forge, update (services) ─────────────────────────────

#[cfg(feature = "services")]
mod services {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::ai::fake::{Behavior, FakeProvider};
    use crate::ai::{self, hub};
    use crate::backend::Sensitive;
    use crate::backend::ai::{self as backend_ai, AiProvider, AiSettings};
    use crate::backend::forge::testing::{FakeForge, passing, running, services_in};
    use crate::backend::forge::{NewAccount, PrState, add_account};
    use crate::forge::{ActivityOverride, ForgeController, ForgeStatus, controller_of};
    use crate::test_support::geometry::bounds;
    use crate::test_support::mount_fixture;

    fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        draw(cx, handle);
    }

    fn set_ai(cx: &mut TestAppContext, ai: &AiSettings) {
        let value = backend_ai::settings_value(ai);
        cx.update(|cx| crate::settings::update(cx, |s| s.ai = value));
        cx.update(|cx| {
            let hub = hub::hub(cx).expect("the hub is installed");
            hub.update(cx, |hub, cx| hub.reconfigure(cx));
        });
        cx.run_until_parked();
    }

    fn lm_studio_at(endpoint: &str) -> AiSettings {
        let mut ai = AiSettings {
            provider: Some(AiProvider::LMStudio),
            ..AiSettings::default()
        };
        ai.lmstudio.endpoint = endpoint.to_owned();
        ai.lmstudio.model = "fake-model".into();
        ai
    }

    fn message(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> String {
        cx.update(|cx| {
            let commit_box = tab.read(cx).views().status.read(cx).commit_box().clone();
            commit_box.read(cx).message(cx)
        })
    }

    fn generating(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> bool {
        cx.update(|cx| tab.read(cx).views().ai.read(cx).is_generating())
    }

    /// Generate into the commit box against a fake provider: Stop, a full run, the one-time consent for a provider
    /// that is not on this machine, then a commit of the generated message and Undo of that commit.
    #[gpui_kit::test]
    fn ai_generate_stop_consent_commit_and_undo(cx: &mut TestAppContext) {
        let fake = FakeProvider::start(Behavior::Reply {
            pieces: vec![
                "feat: add ".into(),
                "the b file\n\n".into(),
                "Adds b.".into(),
            ],
            delay: Duration::ZERO,
        })
        .expect("the fake provider starts");
        let fx = Fixture::new();
        fx.write("a.txt", "one\n");
        fx.commit_all("init");
        fx.write("b.txt", "bee\n");
        fx.git(&["add", "b.txt"]);
        let init = head(&fx);
        let (handle, app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
        let services =
            crate::backend::services::start_services(&crate::settings::Settings::default())
                .expect("the services start in a temporary home");
        cx.update(|cx| {
            crate::app::state_mut(cx).expect("the app state").services = Some(services);
        });
        set_ai(cx, &lm_studio_at(&fake.endpoint()));
        assert!(matches!(
            cx.update(|cx| hub::readiness(cx)),
            hub::Readiness::Ready { .. }
        ));
        render(cx, handle);
        assert!(
            present(cx, handle, ai::ids::BUTTON),
            "the AI control is in the commit box"
        );

        // Generate, then Stop before anything arrives: the notes stay, nothing is generating.
        let commit_box = cx.update(|cx| tab.read(cx).views().status.read(cx).commit_box().clone());
        with_window(cx, handle, |window, cx| {
            commit_box.update(cx, |b, cx| b.set_message("notes".to_owned(), window, cx))
        });
        render(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(ai::ids::BUTTON, cx);
            window.render_frame(cx);
            assert!(
                tab.read(cx).views().ai.read(cx).is_generating(),
                "the run started"
            );
            assert!(
                commit_box.read(cx).is_read_only(),
                "the message is read-only while it streams"
            );
            // The same button is Stop now.
            window.click(ai::ids::BUTTON, cx);
        })
        .expect("window");
        cx.run_until_parked();
        render(cx, handle);
        assert!(!generating(cx, &tab), "stopped");
        assert_eq!(message(cx, &tab), "notes", "Stop keeps the text so far");
        assert!(!cx.update(|cx| commit_box.read(cx).is_read_only()));

        // A full run.
        click(cx, handle, ai::ids::BUTTON);
        wait_until(cx, "the generated message arrives", |cx| {
            !generating(cx, &tab) && message(cx, &tab).starts_with("feat: add the b file")
        });
        assert_eq!(message(cx, &tab), "feat: add the b file\n\nAdds b.");
        let prompt = fake.requests().last().expect("a request").user_prompt();
        assert!(
            prompt.contains("b.txt"),
            "the staged patch went to the provider"
        );

        // The same fake addressed by a name that does not read as this machine: asked first, once.
        let hosted = fake.endpoint().replace("127.0.0.1", "0x7f.0.0.1");
        set_ai(cx, &lm_studio_at(&hosted));
        assert!(
            backend_ai::hosted_target(&lm_studio_at(&hosted)).is_some(),
            "the endpoint counts as hosted"
        );
        with_window(cx, handle, |window, cx| {
            commit_box.update(cx, |b, cx| b.set_message(String::new(), window, cx))
        });
        let requests = fake.requests().len();
        click(cx, handle, ai::ids::BUTTON);
        assert!(present(cx, handle, "dialog-primary"), "the consent dialog");
        assert_eq!(
            fake.requests().len(),
            requests,
            "nothing is sent before the answer"
        );
        click(cx, handle, "dialog-primary");
        wait_until(cx, "the message from the confirmed provider", |cx| {
            !generating(cx, &tab) && message(cx, &tab).starts_with("feat: add the b file")
        });
        assert!(!cx.update(|cx| {
            crate::settings::get_or_default(cx)
                .ai_hosted_consent
                .is_empty()
        }));

        // Commit the generated message and undo the commit from the toolbar.
        click(cx, handle, crate::status_pane::commit_box::ids::PRIMARY);
        assert_eq!(subject(&fx), "feat: add the b file");
        toolbar_undo(cx, handle, &tab);
        assert_eq!(head(&fx), init, "the AI-written commit is undone");
        assert!(errors(&app, cx).is_empty(), "{:?}", errors(&app, cx));
    }

    struct ForgeWorld {
        handle: AnyWindowHandle,
        app: Entity<AppView>,
        tab: Entity<RepoTab>,
        controller: Entity<ForgeController>,
        fake: FakeForge,
        fixture: Fixture,
        _tmp: tempfile::TempDir,
    }

    fn settle(cx: &mut TestAppContext, controller: &Entity<ForgeController>) {
        for _ in 0..200 {
            cx.run_until_parked();
            if !cx.update(|cx| controller.read(cx).is_busy()) {
                break;
            }
            cx.executor().advance_clock(Duration::from_millis(10));
        }
        cx.run_until_parked();
    }

    /// A repository whose origin is the fake forge (pushes go to a local bare repository), `feature` and `topic` with
    /// pull requests, `newfeat` with a commit and no upstream; signed in.
    fn forge_world(cx: &mut TestAppContext) -> ForgeWorld {
        let fake = FakeForge::start();
        let fixture = Fixture::new();
        fixture.write("README.md", "hello\n");
        fixture.commit_all("Initial commit");
        let tip = head(&fixture);
        fixture.git(&["remote", "add", "origin", &fake.server.remote_url()]);
        let bare = fixture.dir.path().join("bare.git");
        let bare_text = bare.to_string_lossy().into_owned();
        fixture.git(&["init", "-q", "--bare", &bare_text]);
        fixture.git(&["remote", "set-url", "--push", "origin", &bare_text]);
        fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fixture.git(&["config", "branch.main.remote", "origin"]);
        fixture.git(&["config", "branch.main.merge", "refs/heads/main"]);
        for (branch, draft, checks) in [("feature", false, passing()), ("topic", true, running())] {
            fixture.git(&["branch", branch]);
            fixture.git(&[
                "update-ref",
                &format!("refs/remotes/origin/{branch}"),
                "HEAD",
            ]);
            fixture.git(&["config", &format!("branch.{branch}.remote"), "origin"]);
            fixture.git(&[
                "config",
                &format!("branch.{branch}.merge"),
                &format!("refs/heads/{branch}"),
            ]);
            fake.add(branch, &format!("PR for {branch}"), draft, &tip, checks);
        }
        fixture.git(&["checkout", "-q", "-b", "newfeat"]);
        fixture.write("feature.txt", "feature\n");
        fixture.commit_all("Add the feature\n\nIt does things.");
        fixture.git(&["checkout", "-q", "main"]);
        let (handle, app, tab) = mount_fixture_with(cx, &fixture, DEFAULT_SIZE, true);
        cx.update(|cx| cx.set_global(ActivityOverride(Rc::new(Cell::new(true)))));
        let tmp = tempfile::tempdir().expect("a temporary directory");
        let services = services_in(tmp.path(), None);
        cx.update(|cx| {
            crate::app::state_mut(cx).expect("the app state").services = Some(services.clone());
        });
        let id = cx.update(|cx| tab.read(cx).id());
        let controller = cx
            .update(|cx| controller_of(id, cx))
            .expect("the tab has a forge controller");
        add_account(
            &services,
            NewAccount::GitHubEnterprise {
                api_url: fake.server.api_url(),
                token: Sensitive::new("secret-token".into()),
            },
        )
        .expect("sign in to the fake forge");
        cx.update(|cx| {
            crate::forge::accounts::installed(cx)
                .expect("the accounts model")
                .update(cx, |model, cx| model.added(cx));
        });
        settle(cx, &controller);
        ForgeWorld {
            handle,
            app,
            tab,
            controller,
            fake,
            fixture,
            _tmp: tmp,
        }
    }

    fn badge_state(cx: &mut TestAppContext, w: &ForgeWorld, branch: &str) -> Option<PrState> {
        cx.update(|cx| {
            w.controller
                .read(cx)
                .badge(&full(&format!("refs/heads/{branch}")))
                .map(|b| b.state)
        })
    }

    /// Forge, undo and terminal in one session: badges from the fake forge in the side panel, an undone branch
    /// operation that the badges follow, the Create-PR flow (push first, then the review), the terminal panel open
    /// meanwhile, and offline: the cached badges stay.
    #[gpui_kit::test]
    fn forge_badges_create_pr_undo_and_offline_in_one_session(cx: &mut TestAppContext) {
        use crate::dialogs::pull_request::{self, ids as pr_ids};
        let w = forge_world(cx);
        assert_eq!(
            cx.update(|cx| w.controller.read(cx).status().clone()),
            ForgeStatus::Idle
        );
        assert_eq!(badge_state(cx, &w, "feature"), Some(PrState::Open));
        assert_eq!(badge_state(cx, &w, "topic"), Some(PrState::Draft));
        assert_eq!(badge_state(cx, &w, "newfeat"), None);
        draw(cx, w.handle);
        with_window(cx, w.handle, |window, cx| {
            window.render_frame(cx);
            let pill = bounds(
                window,
                crate::forge::badges::ids::pill("refs/heads/feature"),
            );
            assert_eq!(pill.size.height.as_f32(), 18., "the PR pill (21 §5.5)");
            let row = bounds(window, crate::forge::badges::ids::pill("refs/heads/topic"));
            assert!(
                (row.top() - pill.top()).as_f32().abs() >= 36. - 0.5,
                "one pill per 36 dp side-panel row"
            );
        });

        // The terminal panel open in the same tab does not disturb the forge or undo.
        #[cfg(feature = "terminal")]
        {
            cx.executor().allow_parking();
            cx.update(|cx| {
                crate::settings::update(cx, |s| {
                    s.terminal_shell = if cfg!(windows) {
                        crate::settings::TerminalShell::Cmd
                    } else {
                        crate::settings::TerminalShell::Auto
                    }
                })
            });
            let ctx = cx.update(|cx| w.tab.read(cx).ctx().clone());
            with_window(cx, w.handle, |window, cx| {
                crate::terminal::toggle(&ctx, window, cx)
            });
            let panel = cx
                .update(|cx| crate::terminal::panel_of(&ctx, cx))
                .expect("panel");
            wait_until(cx, "the terminal starts", |cx| {
                cx.update(|cx| {
                    panel
                        .read(cx)
                        .active_terminal()
                        .is_some_and(|v| v.read(cx).has_printed())
                })
            });
        }

        // Delete `feature` and undo: the badge goes and comes back with the branch.
        run_on(cx, w.handle, &w.tab, |t, win, cx| {
            t.delete_branch(full("refs/heads/feature"), true, win, cx)
        });
        settle(cx, &w.controller);
        assert_eq!(badge_state(cx, &w, "feature"), None, "no branch, no badge");
        toolbar_undo(cx, w.handle, &w.tab);
        settle(cx, &w.controller);
        assert!(
            !w.fixture
                .git(&["branch", "--list", "feature"])
                .trim()
                .is_empty(),
            "the branch is back"
        );
        assert_eq!(
            badge_state(cx, &w, "feature"),
            Some(PrState::Open),
            "and so is its badge"
        );

        // Create a pull request for `newfeat`: pushed first, then created on the fake forge.
        let ctx = cx.update(|cx| w.tab.read(cx).ctx().clone());
        with_window(cx, w.handle, |window, cx| {
            pull_request::open(window, cx, ctx.clone(), full("refs/heads/newfeat"));
        });
        settle(cx, &w.controller);
        draw(cx, w.handle);
        with_window(cx, w.handle, |window, cx| {
            window.render_frame(cx);
            assert_eq!(bounds(window, pr_ids::DIALOG).size.width.as_f32(), 720.);
            window.click(pr_ids::CREATE, cx);
        });
        for _ in 0..60 {
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(50));
        }
        settle(cx, &w.controller);
        let bare = w.fixture.dir.path().join("bare.git");
        assert!(
            !w.fixture
                .git(&[
                    "--git-dir",
                    &bare.to_string_lossy(),
                    "rev-parse",
                    "refs/heads/newfeat"
                ])
                .trim()
                .is_empty(),
            "the branch was pushed first"
        );
        assert_eq!(
            w.fixture.git(&["config", "branch.newfeat.merge"]).trim(),
            "refs/heads/newfeat",
            "the first push set the upstream, so the toolbar's Push works for the branch"
        );
        assert!(
            w.fake.prs().iter().any(|pr| pr.branch == "newfeat"),
            "the review was created"
        );
        assert!(
            toasts(&w.app, cx)
                .iter()
                .any(|t| t.starts_with("Pull request #") && t.ends_with("created")),
            "{:?}",
            toasts(&w.app, cx)
        );
        cx.update(|cx| w.controller.update(cx, |c, cx| c.refresh_now(cx)));
        settle(cx, &w.controller);
        assert_eq!(
            badge_state(cx, &w, "newfeat"),
            Some(PrState::Open),
            "the new review's badge"
        );

        // Offline: the badges stay from the cache.
        w.fake.server.set_offline(true);
        cx.update(|cx| w.controller.update(cx, |c, cx| c.refresh_now(cx)));
        settle(cx, &w.controller);
        assert_eq!(
            cx.update(|cx| w.controller.read(cx).status().clone()),
            ForgeStatus::Offline
        );
        assert_eq!(
            badge_state(cx, &w, "feature"),
            Some(PrState::Open),
            "cached while offline"
        );
        // Undo still works offline (it is local).
        expire_toasts(cx);
        run_on(cx, w.handle, &w.tab, |t, win, cx| {
            t.create_branch(
                "offline-branch".to_owned(),
                StartPoint::Head,
                false,
                win,
                cx,
            )
        });
        toolbar_undo(cx, w.handle, &w.tab);
        assert!(
            w.fixture
                .git(&["branch", "--list", "offline-branch"])
                .trim()
                .is_empty()
        );
        assert!(errors(&w.app, cx).is_empty(), "{:?}", errors(&w.app, cx));
    }

    /// The Create-PR dialog's Generate link goes through the AI hook: a provider off this machine is confirmed first
    /// (nothing is sent before), then the generated text fills the fields.
    #[gpui_kit::test]
    fn create_pr_generate_asks_consent_and_fills_the_fields(cx: &mut TestAppContext) {
        use crate::dialogs::pull_request::{self, ids as pr_ids};
        let fake = FakeProvider::start(Behavior::Reply {
            pieces: vec!["Better feature\n\n".into(), "It does more things.".into()],
            delay: Duration::ZERO,
        })
        .expect("the fake provider starts");
        let w = forge_world(cx);
        let hosted = fake.endpoint().replace("127.0.0.1", "0x7f.0.0.1");
        set_ai(cx, &lm_studio_at(&hosted));
        let ctx = cx.update(|cx| w.tab.read(cx).ctx().clone());
        with_window(cx, w.handle, |window, cx| {
            pull_request::open(window, cx, ctx.clone(), full("refs/heads/newfeat"));
        });
        settle(cx, &w.controller);
        draw(cx, w.handle);
        assert!(
            present(cx, w.handle, pr_ids::GENERATE),
            "AI is ready: the Generate link"
        );
        click(cx, w.handle, pr_ids::GENERATE);
        assert_eq!(
            fake.requests().len(),
            0,
            "nothing is sent before the privacy confirmation"
        );
        let confirm = with_window(cx, w.handle, |window, cx| {
            window.render_frame(cx);
            window
                .try_find("dialog-primary")
                .map(|_| window.has_active_dialog(cx))
        });
        assert_eq!(
            confirm,
            Some(true),
            "the confirmation is open over the dialog"
        );
        click(cx, w.handle, "dialog-primary");
        wait_until(cx, "the generated text arrives", |cx| {
            cx.update(|cx| {
                cx.try_global::<pull_request::OpenDialog>()
                    .and_then(|d| d.0.upgrade())
                    .is_some_and(|d| d.read(cx).snapshot(cx).body.contains("more things"))
            })
        });
        let shot = cx.update(|cx| {
            cx.global::<pull_request::OpenDialog>()
                .0
                .upgrade()
                .expect("the dialog is still open")
                .read(cx)
                .snapshot(cx)
        });
        assert!(shot.title.contains("Better feature"), "{shot:?}");
        assert_eq!(fake.requests().len(), 1);
    }

    /// The update notice from a fake manifest in a repository tab's bottom bar: right-aligned before the version link,
    /// the 32 dp bar unchanged; "Remind me tomorrow" hides it.
    #[gpui_kit::test]
    fn update_notice_with_a_fake_manifest_in_a_repository_tab(cx: &mut TestAppContext) {
        use crate::update::loopback::{Loopback, Response, manifest};

        let fx = Fixture::new();
        fx.write("a.txt", "one\n");
        fx.commit_all("init");
        let (handle, _app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
        let server = Loopback::start(|path| {
            if path.starts_with("/manifest") {
                Response::json(manifest("9.9.9", "https://example.com/releases/v9"))
            } else {
                Response::status(404)
            }
        })
        .expect("loopback server");
        let dir = tempfile::tempdir().expect("temp dir");
        let services = crate::backend::update::services_with_manifest(
            dir.path().join("data"),
            dir.path().join("cache"),
            Some(&server.url("/manifest.json")),
            "0.1.0",
        )
        .expect("services start");
        cx.update(|cx| {
            crate::app::state_mut(cx).expect("app state").services = Some(services);
        });
        wait_until(cx, "the startup check finds the update", |cx| {
            cx.update(|cx| crate::update::notice(cx)).is_some()
        });
        draw(cx, handle);
        with_window(cx, handle, |window, cx| {
            window.render_frame(cx);
            let notice = bounds(window, crate::update::ids::NOTICE);
            let version = bounds(window, crate::shell::ids::BOTTOM_VERSION);
            let bar = bounds(window, crate::ids::BOTTOM_BAR);
            assert_eq!(bar.size.height.as_f32(), 32., "the bottom bar keeps 32 dp");
            assert_eq!(bar.bottom().as_f32(), 720., "at the bottom of the window");
            assert!(
                (version.left() - notice.right()).as_f32() - 16. <= 0.5,
                "the notice ends 16 dp before the version link"
            );
            assert!(notice.top() >= bar.top() && notice.bottom() <= bar.bottom());
        });
        cx.update(|cx| crate::update::snooze(cx, 24));
        draw(cx, handle);
        assert!(
            !present(cx, handle, crate::update::ids::NOTICE),
            "snoozed: no notice"
        );
    }

    /// Every Settings page opens (the milestone-2 pages need the services) and shows its rows in the content column.
    #[gpui_kit::test]
    fn all_settings_pages_open(cx: &mut TestAppContext) {
        use crate::dialogs::settings::{self as dialog, SettingsPage, ids};

        crate::backend::forge::testing::install_memory_keyring();
        let fx = Fixture::new();
        fx.write("a.txt", "one\n");
        fx.commit_all("init");
        let (handle, _app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
        let dir = tempfile::tempdir().expect("temp dir");
        let services = crate::backend::update::services_with_manifest(
            dir.path().join("data"),
            dir.path().join("cache"),
            Some("http://127.0.0.1:9/manifest.json"),
            "0.1.0",
        )
        .expect("services start");
        cx.update(|cx| {
            crate::app::state_mut(cx).expect("app state").services = Some(services);
        });
        cx.run_until_parked();
        let pages = [
            SettingsPage::Appearance,
            SettingsPage::Layout,
            SettingsPage::DateTime,
            SettingsPage::Environment,
            SettingsPage::Branches,
            SettingsPage::RemoteActions,
            SettingsPage::Proxy,
            SettingsPage::Authentication,
            SettingsPage::Security,
            SettingsPage::Accounts,
            SettingsPage::Ai,
            SettingsPage::Terminal,
            SettingsPage::Updates,
            SettingsPage::Logs,
        ];
        for page in pages {
            assert!(
                cx.update(|cx| dialog::is_available(cx, page)),
                "{page:?} is available with the services running"
            );
        }
        with_window(cx, handle, |window, cx| dialog::open(window, cx, None));
        draw(cx, handle);
        for page in pages {
            click(cx, handle, ids::entry(page));
            let view = cx
                .update(|cx| dialog::open_view(cx))
                .expect("the settings dialog is open");
            assert_eq!(
                cx.update(|cx| view.read(cx).page()),
                page,
                "{page:?} selected"
            );
            with_window(cx, handle, |window, cx| {
                window.render_frame(cx);
                let content = bounds(window, ids::CONTENT);
                assert!(
                    content.size.width.as_f32() > 600. && content.size.height.as_f32() > 400.,
                    "{page:?}: the content column is laid out: {content:?}"
                );
                for other in pages {
                    assert!(
                        window.try_find(ids::entry(other)).is_some(),
                        "{page:?}: the category list shows {other:?}"
                    );
                }
            });
        }
        with_window(cx, handle, |window, cx| {
            window.render_frame(cx);
            assert!(
                window.has_active_dialog(cx),
                "still open after visiting every page"
            );
        });
    }
}

// ───────────────────────────── worktrees ─────────────────────────────

fn worktree_list(fx: &Fixture) -> Vec<String> {
    fx.git(&["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree ").map(str::to_owned))
        .collect()
}

/// Add a worktree with the dialog, open it in a new tab, record an operation there (its own undo chain), close the
/// tab, remove the worktree from the main tab: the worktree and its undo chain are gone (21 §5.7, 17 §6).
#[gpui_kit::test]
fn worktree_add_open_record_remove_clears_its_undo_log(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.git(&["branch", "existing"]);
    let (handle, app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    with_window(cx, handle, |window, cx| {
        crate::dialogs::worktree::open_add_worktree(window, cx, ctx.clone())
    });
    draw(cx, handle);
    click(cx, handle, crate::dialogs::worktree::ids::MODE_EXISTING);
    click(cx, handle, "dialog-primary");
    for _ in 0..3 {
        cx.run_until_parked();
    }
    draw(cx, handle);
    let list = worktree_list(&fx);
    assert_eq!(list.len(), 2, "the worktree was added: {list:?}");
    let wt = PathBuf::from(&list[1]);

    // Open in a new tab and record an operation there.
    with_window(cx, handle, |window, cx| {
        crate::worktrees::open_in_new_tab(window, cx, wt.clone())
    });
    for _ in 0..5 {
        cx.run_until_parked();
    }
    let wt_tab = cx
        .update(|cx| app.read(cx).tabs().get(1).and_then(|s| s.repo().cloned()))
        .expect("the worktree's tab");
    wait_until(cx, "the worktree tab loads", |cx| {
        cx.update(|cx| wt_tab.read(cx).ctx().model.read(cx).refs.value.is_some())
    });
    run_on(cx, handle, &wt_tab, |t, w, cx| {
        t.create_branch("in-worktree".to_owned(), StartPoint::Head, false, w, cx)
    });
    let refs = fx.git(&["for-each-ref", "--format=%(refname)", "refs/gitcthulhu/"]);
    let linked: Vec<&str> = refs.lines().filter(|r| r.contains("/linked/")).collect();
    assert_eq!(
        linked.len(),
        1,
        "the worktree has its own undo chain: {refs}"
    );
    assert!(undo_buttons(&wt_tab, cx).0, "and Undo in its tab");
    assert!(
        !undo_buttons(&tab, cx).0,
        "the main tab's undo history is separate"
    );

    // Close the worktree's tab, remove the worktree from the main tab.
    let wt_id = cx.update(|cx| wt_tab.read(cx).id());
    drop(wt_tab);
    with_window(cx, handle, |window, cx| {
        app.update(cx, |app, cx| app.close_tab(wt_id, window, cx))
    });
    draw(cx, handle);
    with_window(cx, handle, |window, cx| {
        app.update(cx, |app, cx| app.select_tab(0, window, cx))
    });
    run_on(cx, handle, &tab, |t, w, cx| {
        t.remove_worktree(wt.clone(), false, w, cx)
    });
    assert!(!wt.exists(), "the directory is removed");
    assert_eq!(worktree_list(&fx).len(), 1);
    let refs = fx.git(&["for-each-ref", "--format=%(refname)", "refs/gitcthulhu/"]);
    assert!(
        !refs.contains("/linked/"),
        "the removed worktree's undo chain is gone: {refs}"
    );
    let listed = cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .model
            .read(cx)
            .worktrees
            .value
            .as_ref()
            .map_or(0, |l| l.len())
    });
    assert_eq!(listed, 1, "the side panel's list follows");
    assert!(errors(&app, cx).is_empty(), "{:?}", errors(&app, cx));
}

// ───────────────────────────── settings migration ─────────────────────────────

/// A `settings.json` / `state.json` written by milestone 1 (no D0 values ever set, one key a later version dropped)
/// loads: every value it had stays, every milestone-2 key takes its default, and the M2 features read those
/// defaults (AI not configured, update checks on, accounts offered for HTTPS prompts, automatic shell).
#[gpui_kit::test]
fn milestone_1_settings_file_loads_with_milestone_2_defaults(cx: &mut TestAppContext) {
    use crate::settings::state::UiState;
    use crate::settings::{
        AiCommitStyle, DiffViewMode, LinesHeight, Settings, TerminalShell, UpdateChannel,
    };
    use crate::theme::ThemeKind;

    let dir = tempfile::tempdir().expect("temp dir");
    let settings_path = dir.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{
  "theme": "Light",
  "lines_height": "Compact",
  "scale": 1.25,
  "swap_status_panes": true,
  "diff_view": "Split",
  "git_path": null,
  "record_undo_history": true,
  "undo_keep_entries": 200,
  "undo_keep_days": 7,
  "undo_capture_untracked": false,
  "push_with_lease": false,
  "custom_terminal_path": "C:\\Tools\\wt.exe"
}"#,
    )
    .expect("write");
    let state_path = dir.path().join("state.json");
    std::fs::write(
        &state_path,
        r#"{ "tabs_open": [], "selected_tab": 0, "first_pane_width": 260.0, "third_pane_width": 300.0 }"#,
    )
    .expect("write");

    let loaded = Settings::load_from(&settings_path);
    assert!(
        matches!(loaded, crate::settings::io::Loaded::Ok(_)),
        "a milestone-1 file is not corrupt (an unknown key is ignored)"
    );
    let s = loaded.into_value();
    // Kept.
    assert_eq!(s.theme, ThemeKind::Light);
    assert_eq!(s.lines_height, LinesHeight::Compact);
    assert_eq!(s.scale, 1.25);
    assert!(s.swap_status_panes);
    assert_eq!(s.diff_view, DiffViewMode::Split);
    assert_eq!((s.undo_keep_entries, s.undo_keep_days), (200, 7));
    assert!(!s.undo_capture_untracked && !s.push_with_lease);
    // Milestone-2 defaults.
    let d = Settings::default();
    assert_eq!(s.terminal_shell, TerminalShell::Auto);
    assert_eq!(s.terminal_font_size, d.terminal_font_size);
    assert!(s.ai.is_null(), "no AI provider");
    assert_eq!(s.ai_commit_style, AiCommitStyle::Conventional);
    assert!(s.ai_hosted_consent.is_empty(), "consent is never assumed");
    assert!(s.forge_https_answers);
    assert!(s.update_check);
    assert_eq!(s.update_channel, UpdateChannel::Release);
    let state = UiState::load_from(&state_path).into_value();
    assert_eq!(state.first_pane_width, 260.);
    assert_eq!(
        state.terminal_height,
        crate::metrics::TERMINAL_DEFAULT_H,
        "the terminal height starts at its default"
    );

    // The app with these settings: the M2 features see the defaults.
    crate::test_support::init_app(cx);
    cx.update(|cx| crate::settings::init(cx, s.clone()));
    #[cfg(feature = "services")]
    {
        let services = crate::backend::services::start_services(&s).expect("the services start");
        cx.update(|cx| {
            crate::app::state_mut(cx).expect("app state").services = Some(services);
        });
        cx.run_until_parked();
        assert!(
            !matches!(
                cx.update(|cx| crate::ai::readiness(cx)),
                crate::ai::Readiness::Ready { .. }
            ),
            "AI is not set up"
        );
    }
    let (handle, _app) = crate::test_support::open_app(cx, DEFAULT_SIZE);
    draw(cx, handle);
    with_window(cx, handle, |window, cx| {
        crate::dialogs::settings::open(
            window,
            cx,
            Some(crate::dialogs::settings::SettingsPage::Terminal),
        )
    });
    draw(cx, handle);
    assert!(
        with_window(cx, handle, |window, cx| window.has_active_dialog(cx)),
        "the Terminal page opens with the migrated settings"
    );
    cx.update(|cx| {
        let now = crate::settings::get(cx);
        assert_eq!(now.theme, ThemeKind::Light);
        assert_eq!(now.undo_keep_entries, 200);
        assert!(
            !crate::app::undo_settings(cx).capture_untracked,
            "the undo settings reach the oplog options"
        );
    });
}
