use core::prelude::v1::test;

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{AnyWindowHandle, AppContext as _, Context, Entity, TestAppContext, Window};

use super::super::{OpKind, RestoreKind, execute_restore};
use crate::app_view::AppView;
use crate::backend::handle::RepoHandle;
use crate::backend::prompt::{Asker, UiRequest, UiRequestSender};
use crate::backend::{
    CommitOptions, DiscardMode, ObjectId, RefreshSet, RelaPath, ResetMode, UndoOutcome,
};
use crate::tab::RepoTab;
use crate::test_support::seed::full;
use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture_with, toast_texts};

/// A real repository in a tab (confirmations answered with `ask`).
struct Rig {
    fx: Fixture,
    handle: AnyWindowHandle,
    app: Entity<AppView>,
    tab: Entity<RepoTab>,
}

fn rig(cx: &mut TestAppContext, fx: Fixture, ask: bool) -> Rig {
    let (handle, app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, ask);
    Rig {
        fx,
        handle,
        app,
        tab,
    }
}

impl Rig {
    /// Runs `f` inside the tab and lets every background half finish.
    fn run<T>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>) -> T,
    ) -> T {
        let result = cx
            .update_window(self.handle, |_, window, cx| {
                self.tab.update(cx, |t, cx| f(t, window, cx))
            })
            .expect("window");
        cx.run_until_parked();
        // The reloads that follow need one more round.
        cx.run_until_parked();
        result
    }

    fn undo(&self, cx: &mut TestAppContext) {
        self.run(cx, |t, w, cx| assert!(t.undo(w, cx), "undo starts"));
    }

    fn redo(&self, cx: &mut TestAppContext) {
        self.run(cx, |t, w, cx| assert!(t.redo(w, cx), "redo starts"));
    }

    fn handle(&self, cx: &mut TestAppContext) -> Arc<RepoHandle> {
        cx.update(|cx| {
            self.tab
                .read(cx)
                .ctx()
                .backend
                .handle()
                .expect("a real backend")
                .clone()
        })
    }

    fn can_undo(&self, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            self.tab
                .read(cx)
                .ctx()
                .model
                .read(cx)
                .undo
                .as_ref()
                .is_some_and(|u| u.undo.is_some())
        })
    }

    fn can_redo(&self, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            self.tab
                .read(cx)
                .ctx()
                .model
                .read(cx)
                .undo
                .as_ref()
                .is_some_and(|u| u.redo.is_some())
        })
    }

    fn errors(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| {
            self.app
                .read(cx)
                .errors_opened
                .iter()
                .map(ToString::to_string)
                .collect()
        })
    }

    fn toasts(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| toast_texts(&self.app, cx))
    }

    fn head(&self) -> String {
        self.fx.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.fx.path().join(path)).ok()
    }

    /// Everything a restore must bring back, as text: refs, the index, the stash list and the working tree status.
    fn fingerprint(&self) -> String {
        format!(
            "{}\n--\n{}\n--\n{}\n--\n{}",
            self.fx
                .git(&["for-each-ref", "refs/heads", "refs/tags", "refs/stash"]),
            self.fx.git(&["status", "--porcelain=v1", "-uall"]),
            self.fx.git(&["stash", "list"]),
            self.fx.git(&["symbolic-ref", "-q", "HEAD"]),
        )
    }
}

fn oid_of(text: &str) -> ObjectId {
    ObjectId::from_hex(text.trim().as_bytes()).expect("an object id")
}

fn base() -> Fixture {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.write("tracked.txt", "t1\n");
    fx.commit_all("init");
    fx
}

fn opts(message: &str) -> CommitOptions {
    CommitOptions {
        message: message.into(),
        ..CommitOptions::default()
    }
}

// ── data safety: undo and redo restore exactly what the operation changed ──────────────────────────────

#[gpui_kit::test]
fn commit_undo_redo(cx: &mut TestAppContext) {
    let fx = base();
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let r = rig(cx, fx, true);
    let before = r.fingerprint();
    let head_before = r.head();
    r.run(cx, |t, w, cx| t.commit(opts("Add b"), w, cx));
    let head_after = r.head();
    assert_ne!(head_before, head_after, "the commit was made");
    let after = r.fingerprint();
    assert!(r.can_undo(cx), "the commit is on the undo stack");

    r.undo(cx);
    assert_eq!(r.head(), head_before, "undo moves HEAD back");
    assert_eq!(r.fingerprint(), before, "the staged b.txt is staged again");
    assert_eq!(r.read("b.txt").as_deref(), Some("bee\n"));
    assert!(r.can_redo(cx));
    assert!(
        r.toasts(cx).iter().any(|t| t.starts_with("Undone: ")),
        "{:?}",
        r.toasts(cx)
    );

    r.redo(cx);
    assert_eq!(r.head(), head_after, "redo brings the same commit back");
    assert_eq!(r.fingerprint(), after);
    assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
}

#[gpui_kit::test]
fn checkout_undo_redo(cx: &mut TestAppContext) {
    let fx = base();
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.write("a.txt", "feature\n");
    fx.commit_all("on feature");
    fx.git(&["checkout", "-q", "main"]);
    let r = rig(cx, fx, true);
    let on_main = r.fingerprint();
    r.run(cx, |t, w, cx| {
        t.checkout_branch(full("refs/heads/feature"), w, cx)
    });
    assert_eq!(r.read("a.txt").as_deref(), Some("feature\n"));
    let on_feature = r.fingerprint();
    assert_ne!(on_main, on_feature);

    r.undo(cx);
    assert_eq!(r.fingerprint(), on_main);
    assert_eq!(r.read("a.txt").as_deref(), Some("one\n"));

    r.redo(cx);
    assert_eq!(r.fingerprint(), on_feature);
    assert_eq!(r.read("a.txt").as_deref(), Some("feature\n"));
}

#[gpui_kit::test]
fn branch_delete_undo_redo(cx: &mut TestAppContext) {
    let fx = base();
    fx.git(&["branch", "topic"]);
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("x.txt", "x\n");
    fx.commit_all("only on other");
    fx.git(&["checkout", "-q", "main"]);
    fx.git(&["branch", "-f", "topic", "other"]);
    let r = rig(cx, fx, true);
    let with_topic = r.fingerprint();
    let topic_tip = r.fx.git(&["rev-parse", "topic"]);
    r.run(cx, |t, w, cx| {
        t.delete_branch(full("refs/heads/topic"), true, w, cx)
    });
    assert!(
        r.fx.git(&["branch", "--list", "topic"]).trim().is_empty(),
        "the branch is gone"
    );

    r.undo(cx);
    assert_eq!(
        r.fx.git(&["rev-parse", "topic"]),
        topic_tip,
        "the branch is back at its old commit"
    );
    assert_eq!(r.fingerprint(), with_topic);

    r.redo(cx);
    assert!(r.fx.git(&["branch", "--list", "topic"]).trim().is_empty());
}

/// Discard of a modified tracked file and an untracked file; `capture` = "Let Undo restore deleted untracked
/// files".
fn discard_rig(cx: &mut TestAppContext, capture: bool, ask: bool) -> Rig {
    let fx = base();
    fx.write(
        "a.txt", "two
",
    );
    fx.write(
        "new.txt", "fresh
",
    );
    let r = rig(cx, fx, ask);
    if !capture {
        // `mount_fixture_with` resets the settings: switch the option off on the open tab (applied in the
        // background through `undo_settings`).
        cx.update(|cx| crate::settings::update(cx, |s| s.undo_capture_untracked = false));
        cx.run_until_parked();
        cx.run_until_parked();
    }
    r
}

fn discard_both(r: &Rig, cx: &mut TestAppContext) {
    r.run(cx, |t, w, cx| {
        t.discard_paths(
            vec![RelaPath::from("a.txt"), RelaPath::from("new.txt")],
            DiscardMode::Unstaged,
            vec![RelaPath::from("new.txt")],
            w,
            cx,
        )
    });
}

#[gpui_kit::test]
fn discard_with_untracked_capture_undo_redo(cx: &mut TestAppContext) {
    let r = discard_rig(cx, true, true);
    let dirty = r.fingerprint();
    discard_both(&r, cx);
    assert_eq!(r.read("a.txt").as_deref(), Some("one\n"));
    assert!(r.read("new.txt").is_none(), "the untracked file is deleted");
    let clean = r.fingerprint();

    r.undo(cx);
    assert_eq!(r.read("a.txt").as_deref(), Some("two\n"));
    assert_eq!(
        r.read("new.txt").as_deref(),
        Some("fresh\n"),
        "the deleted untracked file is restored"
    );
    assert_eq!(r.fingerprint(), dirty);

    r.redo(cx);
    assert_eq!(r.fingerprint(), clean);
    assert!(r.read("new.txt").is_none());
    assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
}

#[gpui_kit::test]
fn discard_without_untracked_capture_asks_first_and_restores_the_rest(cx: &mut TestAppContext) {
    // Declined: nothing is deleted.
    let r = discard_rig(cx, false, false);
    let dirty = r.fingerprint();
    discard_both(&r, cx);
    assert_eq!(
        r.fingerprint(),
        dirty,
        "declining the question changes nothing"
    );
    assert_eq!(r.read("new.txt").as_deref(), Some("fresh\n"));
    assert!(!r.can_undo(cx));
    drop(r);

    // Accepted: the file is gone for good, the tracked change is still restored by undo.
    let r = discard_rig(cx, false, true);
    discard_both(&r, cx);
    assert!(r.read("new.txt").is_none());
    assert_eq!(r.read("a.txt").as_deref(), Some("one\n"));
    r.undo(cx);
    assert_eq!(
        r.read("a.txt").as_deref(),
        Some("two\n"),
        "the tracked change comes back"
    );
    assert!(
        r.read("new.txt").is_none(),
        "the file that was never captured stays deleted"
    );
    assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
}

/// No snapshot holds an untracked nested repository: deleting or discarding one always asks first, even with the
/// untracked capture on, and declined it stays with its `.git`.
#[gpui_kit::test]
fn deleting_a_nested_repository_asks_first(cx: &mut TestAppContext) {
    for ask in [false, true] {
        let fx = base();
        fx.git(&["init", "-q", "nested"]);
        fx.write("nested/n.txt", "n\n");
        fx.write("junk/inner/.git", "gitdir: ../../elsewhere\n");
        fx.write("junk/inner/file.txt", "f\n");
        let r = rig(cx, fx, ask);
        r.run(cx, |t, w, cx| {
            t.delete_untracked(vec![RelaPath::from("nested")], w, cx)
        });
        r.run(cx, |t, w, cx| {
            t.discard_paths(
                vec![RelaPath::from("junk/")],
                DiscardMode::Unstaged,
                vec![RelaPath::from("junk/")],
                w,
                cx,
            )
        });
        let path = r.fx.path().to_owned();
        assert_eq!(
            path.join("nested/.git").exists(),
            !ask,
            "the nested repository is deleted only when the question is accepted (ask = {ask})"
        );
        assert_eq!(
            path.join("junk/inner/.git").exists(),
            !ask,
            "the directory holding a repository is discarded only when accepted (ask = {ask})"
        );
        assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
        drop(r);
    }
}

/// With undo history off the preview fails closed (every path "can't be restored") and probes nothing: the nested
/// repository is still named as such, and only an accepted question that named it lets the backend delete it.
#[gpui_kit::test]
fn nested_repository_is_named_with_undo_history_off(cx: &mut TestAppContext) {
    use super::super::Prepared;
    use super::super::files::{NestedAgreed, confirm_untracked_loss};

    let fx = base();
    fx.git(&["init", "-q", "junk/nested"]);
    fx.write("junk/nested/n.txt", "n\n");
    fx.write("junk/plain.txt", "p\n");
    fx.write("solo/.git", "gitdir: ../elsewhere\n");
    let r = rig(cx, fx, true);
    cx.update(|cx| crate::settings::update(cx, |s| s.record_undo_history = false));
    cx.run_until_parked();
    cx.run_until_parked();
    let handle = r.handle(cx);

    for answer in [false, true] {
        let (tx, mut rx) = UiRequestSender::channel();
        let asker = Asker::Ui { tx, tab: None };
        let agreed = NestedAgreed::default();
        let worker = {
            let (handle, agreed) = (handle.clone(), agreed.clone());
            std::thread::spawn(move || {
                let paths = [RelaPath::from("junk/"), RelaPath::from("solo")];
                let prepared =
                    confirm_untracked_loss(&handle, &asker, "Delete", &paths, Some(&agreed));
                matches!(prepared, Ok(Prepared::Continue { .. }))
            })
        };
        let request = loop {
            match rx.try_recv() {
                Ok(request) => break request,
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let UiRequest::Confirm { request, reply, .. } = request else {
            panic!("a confirmation was expected");
        };
        assert!(request.destructive);
        assert!(
            request.message.contains("including 2 Git repositories"),
            "the repositories are named: {}",
            request.message
        );
        assert_eq!(
            request.details,
            [
                "junk/nested (contains a Git repository)",
                "solo (contains a Git repository)",
                "junk/ (capturing untracked files is off)",
            ],
            "each repository once, before the other paths"
        );
        reply.send(answer).expect("the worker waits");
        let continued = worker.join().expect("the worker does not panic");
        assert_eq!(continued, answer, "declined, the operation is cancelled");
        assert_eq!(agreed.get(), answer, "only an accepted question agrees");
    }
}

#[test]
fn nested_repositories_lead_the_unrestorable_question() {
    use crate::backend::{SkipReason, SkippedFile};
    let skipped = [
        SkippedFile {
            path: "big.bin".into(),
            size: 9,
            reason: SkipReason::TooLarge,
        },
        SkippedFile {
            path: "nested".into(),
            size: 0,
            reason: SkipReason::NestedRepository,
        },
    ];
    let request = crate::backend::present::confirm_unrestorable("Delete 2 files", &skipped);
    assert!(request.destructive, "losing a repository is destructive");
    assert_eq!(
        request.details,
        ["nested (contains a Git repository)", "big.bin (too large)"],
        "the repository is listed first"
    );
    assert!(
        request.message.contains("including a Git repository"),
        "the message names the repository: {}",
        request.message
    );
}

#[gpui_kit::test]
fn stash_undo_redo(cx: &mut TestAppContext) {
    let fx = base();
    fx.write("a.txt", "dirty\n");
    let r = rig(cx, fx, true);
    let dirty = r.fingerprint();
    r.run(cx, |t, w, cx| t.stash(None, false, w, cx));
    assert!(!r.fx.git(&["stash", "list"]).trim().is_empty(), "stashed");
    assert_eq!(r.read("a.txt").as_deref(), Some("one\n"));
    let stashed = r.fingerprint();

    r.undo(cx);
    assert_eq!(r.read("a.txt").as_deref(), Some("dirty\n"));
    assert!(
        r.fx.git(&["stash", "list"]).trim().is_empty(),
        "the stash entry is removed again"
    );
    assert_eq!(r.fingerprint(), dirty);

    r.redo(cx);
    assert_eq!(r.fingerprint(), stashed);
}

#[gpui_kit::test]
fn merge_undo_redo(cx: &mut TestAppContext) {
    let fx = base();
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.write("f.txt", "feature file\n");
    fx.commit_all("feature work");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("m.txt", "main file\n");
    fx.commit_all("main work");
    let r = rig(cx, fx, true);
    let before = r.fingerprint();
    let head_before = r.head();
    r.run(cx, |t, w, cx| t.merge("feature".to_owned(), w, cx));
    let merged = r.head();
    assert_ne!(merged, head_before, "a merge commit was made");
    assert_eq!(r.read("f.txt").as_deref(), Some("feature file\n"));
    let after = r.fingerprint();

    r.undo(cx);
    assert_eq!(r.head(), head_before);
    assert!(r.read("f.txt").is_none(), "the merged file is gone again");
    assert_eq!(r.fingerprint(), before);

    r.redo(cx);
    assert_eq!(r.head(), merged, "redo brings the same merge commit back");
    assert_eq!(r.fingerprint(), after);
    assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
}

#[gpui_kit::test]
fn reset_hard_undo_redo_keeps_uncommitted_changes(cx: &mut TestAppContext) {
    let fx = base();
    let first = oid_of(&fx.git(&["rev-parse", "HEAD"]));
    fx.write("a.txt", "second\n");
    fx.commit_all("second");
    fx.write("a.txt", "uncommitted\n");
    fx.write("tracked.txt", "t2 uncommitted\n");
    let r = rig(cx, fx, true);
    let before = r.fingerprint();
    let head_before = r.head();
    r.run(cx, |t, w, cx| t.reset(first, ResetMode::Hard, w, cx));
    assert_eq!(r.head(), first.to_string());
    assert_eq!(r.read("a.txt").as_deref(), Some("one\n"));
    assert_eq!(r.read("tracked.txt").as_deref(), Some("t1\n"));
    let after = r.fingerprint();

    r.undo(cx);
    assert_eq!(r.head(), head_before, "the commit is back");
    assert_eq!(
        r.read("a.txt").as_deref(),
        Some("uncommitted\n"),
        "the discarded tracked changes are back"
    );
    assert_eq!(r.read("tracked.txt").as_deref(), Some("t2 uncommitted\n"));
    assert_eq!(r.fingerprint(), before);

    r.redo(cx);
    assert_eq!(r.fingerprint(), after);
}

// ── divergence: confirmations ──────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn undo_after_an_outside_commit_asks_and_declining_changes_nothing(cx: &mut TestAppContext) {
    let fx = base();
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let r = rig(cx, fx, false);
    r.run(cx, |t, w, cx| t.commit(opts("Add b"), w, cx));
    // Someone else moves the branch.
    r.fx.git(&["commit", "-q", "--allow-empty", "-m", "outside"]);
    let moved = r.fingerprint();
    r.undo(cx);
    assert_eq!(r.fingerprint(), moved, "declined: nothing changed");
    assert!(r.errors(cx).is_empty());
    assert!(r.can_undo(cx), "the entry stays on the stack");
}

#[gpui_kit::test]
fn undo_after_an_outside_commit_proceeds_when_confirmed(cx: &mut TestAppContext) {
    let fx = base();
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let r = rig(cx, fx, true);
    let head_before = r.head();
    r.run(cx, |t, w, cx| t.commit(opts("Add b"), w, cx));
    r.fx.git(&["commit", "-q", "--allow-empty", "-m", "outside"]);
    r.undo(cx);
    assert_eq!(r.head(), head_before, "undo returned to the recorded state");
    assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
}

// ── refusals: error dialogs with the exact reason, nothing changes ─────────────────────────────────────

/// What `Oplog::undo` / `redo` refuses with right now (confirmations are accepted on the way), as the dialog's
/// text.
fn refusal_now(r: &Rig, cx: &mut TestAppContext, redo: bool) -> String {
    let handle = r.handle(cx);
    let kind = if redo {
        RestoreKind::Redo
    } else {
        RestoreKind::Undo
    };
    let mut options = crate::backend::RestoreOptions::default();
    loop {
        let mut guard = handle
            .try_lock(Duration::from_secs(2))
            .expect("the guard is free");
        match handle.oplog_restore(&kind, options.clone(), guard.write_permission()) {
            Ok(UndoOutcome::Refused(why)) => {
                return crate::backend::present::refusal_text(&why, "Undo");
            }
            Ok(UndoOutcome::NeedsConfirmation(c)) => options.confirmation = Some(c.token),
            Ok(_) => panic!("not refused"),
            Err(_) => panic!("oplog error"),
        }
    }
}

#[gpui_kit::test]
fn refused_while_an_operation_is_in_progress_shows_the_reason(cx: &mut TestAppContext) {
    let fx = base();
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("a.txt", "theirs\n");
    fx.commit_all("theirs");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let r = rig(cx, fx, true);
    r.run(cx, |t, w, cx| t.commit(opts("Add b"), w, cx));
    r.fx.write("a.txt", "ours\n");
    r.fx.commit_all("ours");
    // A conflicting merge outside the app leaves a merge in progress.
    let _ = std::process::Command::new("git")
        .current_dir(r.fx.path())
        .args(["merge", "other"])
        .env("GIT_CONFIG_GLOBAL", r.fx.dir.path().join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output();
    let state = r.fingerprint();
    let reason = refusal_now(&r, cx, false);
    assert!(
        reason == "A merge is in progress. Finish or abort it first."
            || reason
                .starts_with("The repository has a merge in progress but the recorded state has"),
        "{reason}"
    );
    r.undo(cx);
    assert_eq!(r.errors(cx), ["Can't undo"], "the refusal is a dialog");
    assert_eq!(r.fingerprint(), state, "nothing changed");
}

/// `main` tracks g.txt, `feature` does not; the app checks out `feature` (g.txt leaves the working tree) and the user
/// then creates a new, untracked g.txt that no snapshot holds.
fn untracked_in_the_way_rig(cx: &mut TestAppContext, ask: bool) -> Rig {
    let fx = base();
    fx.write(
        "g.txt",
        "tracked on main
",
    );
    fx.commit_all("g on main");
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.git(&["rm", "-q", "g.txt"]);
    fx.commit_all("drop g");
    fx.git(&["checkout", "-q", "main"]);
    let r = rig(cx, fx, ask);
    r.run(cx, |t, w, cx| {
        t.checkout_branch(full("refs/heads/feature"), w, cx)
    });
    assert!(r.read("g.txt").is_none(), "g.txt left with the checkout");
    r.fx.write(
        "g.txt",
        "precious
",
    );
    r
}

#[gpui_kit::test]
fn undo_over_an_untracked_file_asks_first_and_declining_keeps_the_file(cx: &mut TestAppContext) {
    let r = untracked_in_the_way_rig(cx, false);
    let state = r.fingerprint();
    // The question lists the file (17 §4.5, WorktreeChangesConflict).
    let handle = r.handle(cx);
    let reasons = {
        let mut guard = handle.try_lock(Duration::from_secs(2)).expect("guard");
        match handle.oplog_restore(
            &RestoreKind::Undo,
            Default::default(),
            guard.write_permission(),
        ) {
            Ok(UndoOutcome::NeedsConfirmation(c)) => c.reasons,
            Ok(UndoOutcome::Refused(why)) => {
                panic!(
                    "refused: {}",
                    crate::backend::present::refusal_text(&why, "Undo")
                )
            }
            _ => panic!("a confirmation was expected"),
        }
    };
    let question = crate::backend::present::confirm_restore(&RestoreKind::Undo, &reasons);
    assert_eq!(question.title, "Undo anyway?");
    assert!(
        question
            .message
            .contains("These files changed after the operation: g.txt."),
        "{}",
        question.message
    );
    assert!(question.destructive);

    r.undo(cx);
    assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
    assert_eq!(
        r.read("g.txt").as_deref(),
        Some(
            "precious
"
        ),
        "declined: untouched"
    );
    assert_eq!(r.fingerprint(), state);
}

#[gpui_kit::test]
fn undo_over_an_untracked_file_when_confirmed_keeps_it_for_redo(cx: &mut TestAppContext) {
    let r = untracked_in_the_way_rig(cx, true);
    r.undo(cx);
    assert_eq!(
        r.read("g.txt").as_deref(),
        Some(
            "tracked on main
"
        ),
        "the confirmed undo restores main's g.txt"
    );
    r.redo(cx);
    assert_eq!(
        r.read("g.txt").as_deref(),
        Some(
            "precious
"
        ),
        "redo brings the user's untracked file back"
    );
    assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
}

#[gpui_kit::test]
fn restore_to_captures_the_untracked_file_so_that_undo_brings_it_back(cx: &mut TestAppContext) {
    let r = untracked_in_the_way_rig(cx, true);
    let handle = r.handle(cx);
    let entry = handle
        .oplog_list(crate::backend::PageRequest {
            before: None,
            limit: 10,
            include_bookkeeping: false,
        })
        .expect("history")
        .entries
        .into_iter()
        .find(|e| e.kind == crate::backend::OperationKind::Checkout)
        .expect("the checkout is recorded");
    let state = r.fingerprint();
    r.run(cx, |t, w, cx| {
        t.restore_to(
            crate::backend::RestoreTarget {
                entry: entry.id,
                side: crate::backend::SnapshotSide::Before,
            },
            w,
            cx,
        )
    });
    assert!(r.errors(cx).is_empty(), "{:?}", r.errors(cx));
    assert_eq!(
        r.read("g.txt").as_deref(),
        Some(
            "tracked on main
"
        )
    );
    assert!(
        r.toasts(cx).iter().any(|t| t == "Repository restored"),
        "{:?}",
        r.toasts(cx)
    );
    // The restore is itself undoable and puts the user's file back.
    r.undo(cx);
    assert_eq!(
        r.read("g.txt").as_deref(),
        Some(
            "precious
"
        )
    );
    assert_eq!(r.fingerprint(), state);
}

#[gpui_kit::test]
fn undo_refuses_when_the_untracked_file_cannot_be_captured(cx: &mut TestAppContext) {
    let r = untracked_in_the_way_rig(cx, true);
    // With "Let Undo restore deleted untracked files" off the user's g.txt could not be put back by redo, so the
    // restore must not replace it at all.
    cx.update(|cx| crate::settings::update(cx, |s| s.undo_capture_untracked = false));
    cx.run_until_parked();
    cx.run_until_parked();
    let state = r.fingerprint();
    r.undo(cx);
    assert_eq!(r.errors(cx), ["Can't undo"], "toasts: {:?}", r.toasts(cx));
    assert_eq!(
        r.read("g.txt").as_deref(),
        Some(
            "precious
"
        ),
        "untouched"
    );
    assert_eq!(r.fingerprint(), state, "nothing changed");
    assert_eq!(
        refusal_now(&r, cx, false),
        "Undo needs to write g.txt, but an untracked file is in the way. Move it away and try again."
    );
}

#[gpui_kit::test]
fn refused_when_the_branch_is_checked_out_in_another_worktree(cx: &mut TestAppContext) {
    let fx = base();
    let r = rig(cx, fx, true);
    let tip = oid_of(&r.head());
    r.run(cx, |t, w, cx| {
        t.create_branch(
            "topic".to_owned(),
            crate::backend::StartPoint::Commit(tip),
            false,
            w,
            cx,
        )
    });
    assert!(!r.fx.git(&["branch", "--list", "topic"]).trim().is_empty());
    let wt = r.fx.dir.path().join("wt");
    r.fx.git(&[
        "worktree",
        "add",
        "-q",
        wt.to_str().expect("utf-8"),
        "topic",
    ]);
    r.undo(cx);
    assert_eq!(r.errors(cx), ["Can't undo"]);
    assert!(
        !r.fx.git(&["branch", "--list", "topic"]).trim().is_empty(),
        "the branch was not deleted"
    );
    let reason = refusal_now(&r, cx, false);
    assert!(
        reason.starts_with("This would move 'topic', which is checked out in another worktree ("),
        "{reason}"
    );
}

// ── the confirmation loop releases the guard ───────────────────────────────────────────────────────────

#[gpui_kit::test]
fn confirmation_loop_drops_guard_between_dialogs(cx: &mut TestAppContext) {
    let fx = base();
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let r = rig(cx, fx, true);
    let head_before = r.head();
    r.run(cx, |t, w, cx| t.commit(opts("Add b"), w, cx));
    r.fx.git(&["commit", "-q", "--allow-empty", "-m", "outside"]);
    let handle = r.handle(cx);

    let (tx, mut rx) = UiRequestSender::channel();
    let asker = Asker::Ui { tx, tab: None };
    let worker = {
        let handle = handle.clone();
        std::thread::spawn(move || execute_restore(&handle, RestoreKind::Undo, &asker))
    };
    // The worker asks; while the dialog is "open" this thread takes the repository guard.
    let request = loop {
        match rx.try_recv() {
            Ok(request) => break request,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let UiRequest::Confirm { request, reply, .. } = request else {
        panic!("a confirmation was expected");
    };
    assert_eq!(request.title, "Undo anyway?");
    {
        let guard = handle.try_lock(Duration::from_millis(500));
        assert!(
            guard.is_some(),
            "the guard is free while the dialog is open (another thread could take it)"
        );
    }
    reply.send(true).expect("the worker waits");
    let done = worker.join().expect("the worker does not panic");
    assert!(
        matches!(
            done,
            Ok(super::super::RestoreDone::Outcome(UndoOutcome::Done(_)))
        ),
        "the confirmed undo ran"
    );
    assert_eq!(r.head(), head_before);
}

// ── clearing the history ───────────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn clear_history_confirms(cx: &mut TestAppContext) {
    // Declined: the history stays.
    let fx = base();
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let r = rig(cx, fx, false);
    r.run(cx, |t, w, cx| t.commit(opts("Add b"), w, cx));
    assert!(r.can_undo(cx));
    r.run(cx, |t, w, cx| t.clear_undo_history(w, cx));
    assert!(r.can_undo(cx), "declined: the history is kept");
    assert!(!r.toasts(cx).iter().any(|t| t == "Undo history cleared"));
    drop(r);

    // Accepted: the history is gone, the repository is not touched.
    let fx = base();
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let r = rig(cx, fx, true);
    r.run(cx, |t, w, cx| t.commit(opts("Add b"), w, cx));
    let head = r.head();
    r.run(cx, |t, w, cx| t.clear_undo_history(w, cx));
    assert!(!r.can_undo(cx), "no history left");
    assert!(!r.can_redo(cx));
    assert_eq!(r.head(), head, "clearing the history changes nothing else");
    assert!(
        r.toasts(cx).iter().any(|t| t == "Undo history cleared"),
        "{:?}",
        r.toasts(cx)
    );
    let _ = (OpKind::ClearUndoHistory, RefreshSet::UNDO);
}

#[test]
fn clear_request_is_destructive() {
    let request = super::clear_history_request();
    assert!(request.destructive);
    assert_eq!(request.title, "Clear the undo history?");
}
