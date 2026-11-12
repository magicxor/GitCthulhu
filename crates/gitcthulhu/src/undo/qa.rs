//! Scenario verbs of the undo UI (feature `qa`; see `qa::verbs`). They call the same public entry points as the
//! toolbar and the history popover. The two `undo demo` verbs make small, synthetic changes so that a scenario has
//! something to undo: the repository is always inside the QA sandbox (the runner's gate).

use gpui_kit::{App, Entity, Window};

use crate::app_view::AppViewGlobal;
use crate::backend::{CommitOptions, RelaPath, RestoreTarget, SnapshotSide, StartPoint};
use crate::qa::verbs::{Verb, register_verb};
use crate::tab::RepoTab;

/// Registers the verbs of the undo UI.
pub(super) fn register(cx: &mut App) {
    // A screenshot must not catch the popover while its first page is still being read.
    crate::qa::register_idle_probe(cx, |_, cx| {
        let tab = active_tab(cx).ok()?;
        let controller = tab.read(cx).views().undo.clone();
        let popover = controller.read(cx).history()?.clone();
        matches!(popover.read(cx).load_state(), super::history::Load::Loading)
            .then(|| "the undo history is loading".to_owned())
    });
    register_verb(
        cx,
        Verb::new(
            "undo",
            "undo",
            "Clicks Undo (runs RepoTab::undo on the selected tab)",
            |window, cx, _| step(window, cx, "undo", |t, w, cx| t.undo(w, cx)),
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "redo",
            "redo",
            "Clicks Redo (runs RepoTab::redo on the selected tab)",
            |window, cx, _| step(window, cx, "redo", |t, w, cx| t.redo(w, cx)),
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "undo history open",
            "undo history open",
            "Opens the undo history popover under the Undo chevron",
            |window, cx, _| {
                let tab = active_tab(cx)?;
                let controller = tab.read(cx).views().undo.clone();
                controller.update(cx, |c, cx| c.open_history(window, cx));
                Ok("history opened".into())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "undo history close",
            "undo history close",
            "Closes the undo history popover",
            |_, cx, _| {
                let tab = active_tab(cx)?;
                let controller = tab.read(cx).views().undo.clone();
                controller.update(cx, |c, cx| c.close_history(cx));
                Ok("history closed".into())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "undo history restore",
            "undo history restore <row from 0> before|after",
            "Restores the repository to the state before or after the operation of a history row (asks first; the \
             step does not wait for the confirmation: `wait 500`, `shot`, then `close dialog` declines it)",
            |window, cx, args| {
                let row: usize = args.number(0, "row")?;
                let side = match args.text(1, "before|after")?.as_str() {
                    "before" => SnapshotSide::Before,
                    "after" => SnapshotSide::After,
                    other => return Err(format!("expected before or after, not '{other}'")),
                };
                let tab = active_tab(cx)?;
                let controller = tab.read(cx).views().undo.clone();
                let entry = controller
                    .read(cx)
                    .history()
                    .and_then(|p| p.read(cx).entries().get(row).map(|e| (e.id, e.title.clone())))
                    .ok_or_else(|| format!("history row {row} does not exist (open the history first)"))?;
                controller.update(cx, |c, cx| c.close_history(cx));
                // The confirmation that follows is the scenario's own dialog.
                crate::qa::monitor::scenario_dialog(cx, true);
                tab.update(cx, |t, cx| {
                    t.restore_to(
                        RestoreTarget {
                            entry: entry.0,
                            side,
                        },
                        window,
                        cx,
                    )
                });
                Ok(format!("restoring to {side:?} '{}'", entry.1))
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "undo history clear",
            "undo history clear",
            "Clears the undo history (asks first; the step does not wait for the confirmation)",
            |window, cx, _| {
                let tab = active_tab(cx)?;
                let controller = tab.read(cx).views().undo.clone();
                controller.update(cx, |c, cx| c.close_history(cx));
                crate::qa::monitor::scenario_dialog(cx, true);
                tab.update(cx, |t, cx| t.clear_undo_history(window, cx));
                Ok("clearing".into())
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "undo demo branch",
            "undo demo branch <name>",
            "Creates a branch at HEAD (a recorded operation to undo)",
            |window, cx, args| {
                let name = args.text(0, "branch name")?;
                step(window, cx, "create branch", move |t, w, cx| {
                    t.create_branch(name, StartPoint::Head, false, w, cx)
                })
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "undo demo stage",
            "undo demo stage <file>",
            "Writes a synthetic file into the worktree and stages it (not recorded)",
            |window, cx, args| {
                let file = args.text(0, "file name")?;
                let tab = active_tab(cx)?;
                let dir = tab
                    .read(cx)
                    .ctx()
                    .backend
                    .handle()
                    .and_then(|h| h.workdir())
                    .ok_or_else(|| "the repository has no worktree".to_owned())?;
                std::fs::write(dir.join(&file), format!("synthetic content of {file}\n"))
                    .map_err(|e| format!("cannot write {file}: {e}"))?;
                tab.update(cx, |t, cx| {
                    t.stage_paths(vec![RelaPath::from(file.as_str())], window, cx)
                });
                Ok(format!("staged {file}"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "undo demo commit",
            "undo demo commit <message>",
            "Commits what is staged (a recorded operation to undo)",
            |window, cx, args| {
                let message = args.rest(0);
                if message.is_empty() {
                    return Err("a commit message is required".into());
                }
                step(window, cx, "commit", move |t, w, cx| {
                    t.commit(
                        CommitOptions {
                            message,
                            ..CommitOptions::default()
                        },
                        w,
                        cx,
                    )
                })
            },
        ),
    );
}

fn active_tab(cx: &App) -> Result<Entity<RepoTab>, String> {
    let view = cx
        .try_global::<AppViewGlobal>()
        .and_then(|g| g.0.upgrade())
        .ok_or_else(|| "the window content is gone".to_owned())?;
    view.read(cx)
        .active_tab()
        .cloned()
        .ok_or_else(|| "the selected tab is not a repository (open one first)".to_owned())
}

fn step(
    window: &mut Window,
    cx: &mut App,
    what: &str,
    f: impl FnOnce(&mut RepoTab, &mut Window, &mut gpui_kit::Context<RepoTab>) -> bool,
) -> Result<String, String> {
    let tab = active_tab(cx)?;
    if tab.update(cx, |t, cx| f(t, window, cx)) {
        Ok(format!("{what} started"))
    } else {
        Err(format!(
            "{what} did not start (another operation is running?)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

    use crate::qa::verbs::{Action, StepResult};
    use crate::qa::{Args, QaRun, args::QaArgs, dsl, gate::Sandbox, init, verbs};
    use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture_with};

    fn run_line(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        sandbox: &std::path::Path,
        text: &str,
    ) -> StepResult {
        let lines = dsl::parse(text).expect("a valid line");
        let sandbox = sandbox.to_path_buf();
        let result = cx
            .update_window(handle, |_, window, cx| {
                let (verb, consumed) = verbs::lookup(cx, &lines[0].tokens)
                    .unwrap_or_else(|| panic!("no verb for '{text}'"));
                let args = Args::new(lines[0].tokens[consumed..].to_vec(), sandbox);
                match &verb.action {
                    Action::Sync(handler) => handler(window, cx, &args),
                    Action::Runner(kind) => panic!("{kind:?} is a runner verb"),
                }
            })
            .expect("the test window is open");
        cx.run_until_parked();
        cx.run_until_parked();
        result
    }

    /// Every line of the shipped scenario names a known verb, and the undo verbs do what they say on a real
    /// repository.
    #[gpui_kit::test]
    fn the_scenario_runs_against_a_real_repository(cx: &mut TestAppContext) {
        let fx = Fixture::new();
        fx.write("a.txt", "one\n");
        fx.commit_all("init");
        let (handle, _app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, true);
        let dir = tempfile::tempdir().expect("sandbox dir");
        let run = QaRun {
            sandbox: Sandbox::for_tests(dir.path().to_path_buf()),
            args: QaArgs::default(),
        };
        cx.update(|cx| init(cx, &run));

        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/qa/scenarios/x2-undo.qa"
        ))
        .expect("the scenario exists");
        let lines = dsl::parse(&text).expect("the scenario parses");
        cx.update(|cx| {
            for line in &lines {
                assert!(
                    verbs::lookup(cx, &line.tokens).is_some(),
                    "line {}: no verb for '{}'",
                    line.number,
                    line.text
                );
            }
        });

        let sandbox = dir.path().to_path_buf();
        let count = |tab: &gpui_kit::Entity<crate::tab::RepoTab>, cx: &mut TestAppContext| {
            cx.update(|cx| {
                tab.read(cx)
                    .ctx()
                    .model
                    .read(cx)
                    .undo
                    .as_ref()
                    .map(|u| (u.undo_depth, u.redo_depth))
            })
        };
        run_line(cx, handle, &sandbox, "undo demo branch feature-x").expect("branch");
        run_line(cx, handle, &sandbox, "undo demo stage notes.txt").expect("stage");
        run_line(cx, handle, &sandbox, "undo demo commit Add notes").expect("commit");
        assert_eq!(count(&tab, cx), Some((2, 0)));
        run_line(cx, handle, &sandbox, "undo history open").expect("open");
        run_line(cx, handle, &sandbox, "undo history close").expect("close");
        run_line(cx, handle, &sandbox, "undo").expect("undo");
        assert_eq!(count(&tab, cx), Some((1, 1)));
        run_line(cx, handle, &sandbox, "redo").expect("redo");
        assert_eq!(count(&tab, cx), Some((2, 0)));
        assert!(fx.path().join("notes.txt").exists());
        assert!(
            run_line(cx, handle, &sandbox, "undo history restore 0 before").is_err(),
            "the history must be open first"
        );
    }
}
