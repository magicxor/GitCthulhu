//! H: crash recovery (spec 17 §3.4, §3.6) with the fail points of the `test-hooks` feature.

use gitcthulhu_oplog::{
    OperationKind, OplogError, Recovery, UndoOutcome, UntrackedCapture,
    testing::{FailPoint, arm, arm_action},
};

use crate::{
    assert_no_journal, done, fp, fsck, redo,
    support::{Fixture, b, desc, record, record_ok},
    undo,
};

/// A recorded `reset --hard HEAD~1` that also discarded a dirty `a.txt`: its undo writes worktree, refs and index.
/// Returns the fingerprints before and after the reset.
fn recorded_reset() -> (Fixture, gitcthulhu_oplog::Oplog, String, String) {
    let fx = Fixture::new();
    fx.commit_file("b.txt", "b\n", "add b");
    fx.write("a.txt", "a, dirty\n");
    let oplog = fx.oplog();
    let before = fp(&fx);
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::ResetHard, "Reset to HEAD~1"),
        |fx| {
            fx.git(&["reset", "-q", "--hard", "HEAD~1"]);
        },
    );
    let after = fp(&fx);
    (fx, oplog, before, after)
}

fn try_undo(fx: &Fixture, oplog: &gitcthulhu_oplog::Oplog) -> Result<UndoOutcome, OplogError> {
    let mut guard = fx.guard();
    oplog.undo(&fx.repo(), Default::default(), guard.write_permission())
}

fn recover(fx: &Fixture, oplog: &gitcthulhu_oplog::Oplog, how: Recovery) {
    let mut guard = fx.guard();
    oplog
        .recover(&fx.repo(), how, guard.write_permission())
        .expect("recovery works");
}

fn journal_path(fx: &Fixture) -> std::path::PathBuf {
    fx.repo()
        .git_dir()
        .join("gitcthulhu")
        .join("oplog")
        .join("restore.json")
}

/// Undo with `point` armed: the undo is interrupted and everything refuses until recovery.
fn interrupt_undo(fx: &Fixture, oplog: &gitcthulhu_oplog::Oplog, point: FailPoint) {
    arm(fx.repo().git_dir(), point);
    match try_undo(fx, oplog) {
        Err(OplogError::InterruptedRestore) => {}
        other => panic!("expected the undo to be interrupted at {point:?}, got {other:?}"),
    }
    assert!(
        journal_path(fx).exists(),
        "the journal is left for recovery"
    );
    assert!(
        matches!(try_undo(fx, oplog), Err(OplogError::InterruptedRestore)),
        "undo refuses until recovery"
    );
    let mut guard = fx.guard();
    assert!(
        matches!(
            oplog.prepare(
                &fx.repo(),
                desc(OperationKind::Commit, "c"),
                guard.write_permission()
            ),
            Err(OplogError::InterruptedRestore)
        ),
        "prepare refuses until recovery"
    );
    drop(guard);
    let info = oplog
        .interrupted_restore(&fx.repo())
        .expect("reading works")
        .expect("the interrupted restore is reported");
    assert_eq!(info.kind, OperationKind::Undo);
    assert_eq!(info.title, "Reset to HEAD~1");
    assert!(
        oplog
            .state(&fx.repo())
            .expect("reading works")
            .interrupted_restore
    );
}

fn complete_after(point: FailPoint) {
    let (fx, oplog, before, after) = recorded_reset();
    interrupt_undo(&fx, &oplog, point);
    recover(&fx, &oplog, Recovery::Complete);
    assert_eq!(
        fp(&fx),
        before,
        "the finished undo reached the expected post-undo state"
    );
    assert_no_journal(&fx);
    fsck(&fx);
    done(redo(&fx, &oplog));
    assert_eq!(
        fp(&fx),
        after,
        "the completed undo was recorded and can be redone"
    );
    fsck(&fx);
}

fn roll_back_after(point: FailPoint) {
    let (fx, oplog, before, after) = recorded_reset();
    interrupt_undo(&fx, &oplog, point);
    recover(&fx, &oplog, Recovery::RollBack);
    assert_eq!(fp(&fx), after, "the roll back reached the pre-undo state");
    assert_no_journal(&fx);
    fsck(&fx);
    done(undo(&fx, &oplog));
    assert_eq!(fp(&fx), before, "undo works again after the roll back");
    fsck(&fx);
}

#[test]
fn complete_after_worktree() {
    complete_after(FailPoint::AfterWorktree);
}

#[test]
fn complete_after_refs() {
    complete_after(FailPoint::AfterRefs);
}

#[test]
fn complete_after_index() {
    complete_after(FailPoint::AfterIndex);
}

#[test]
fn roll_back_after_worktree() {
    roll_back_after(FailPoint::AfterWorktree);
}

#[test]
fn roll_back_after_refs() {
    roll_back_after(FailPoint::AfterRefs);
}

#[test]
fn roll_back_after_index() {
    roll_back_after(FailPoint::AfterIndex);
}

/// An undo of a merge that stopped on conflicts makes the index conflict-free before the checkout and keeps a
/// backup; rolling back puts the conflicted index back.
#[test]
fn roll_back_restores_a_conflicted_index() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("a.txt", "feature\n", "feature change");
    fx.git(&["checkout", "-q", "main"]);
    fx.commit_file("a.txt", "main\n", "main change");
    let oplog = fx.oplog();
    let before = fp(&fx);
    record(
        &fx,
        &oplog,
        desc(OperationKind::Merge, "Merge 'feature'"),
        |fx| {
            assert!(!fx.git_output(&["merge", "feature"]).status.success());
            Err("conflict".into())
        },
    )
    .expect("recording works")
    .expect("recorded");
    let conflicted = fp(&fx);
    arm(fx.repo().git_dir(), FailPoint::AfterWorktree);
    assert!(matches!(
        try_undo(&fx, &oplog),
        Err(OplogError::InterruptedRestore)
    ));
    assert!(
        fx.repo()
            .git_dir()
            .join("gitcthulhu/oplog/index.backup")
            .exists(),
        "the conflicted index was backed up"
    );
    recover(&fx, &oplog, Recovery::RollBack);
    assert_eq!(
        fp(&fx),
        conflicted,
        "the conflicted state is back, stages included"
    );
    assert_no_journal(&fx);
    done(undo(&fx, &oplog));
    assert_eq!(fp(&fx), before);
    assert!(fx.repo().state().is_none());
    fsck(&fx);
}

#[test]
fn ref_guard_failure_rolls_back_the_worktree() {
    let (fx, oplog, _before, after) = recorded_reset();
    let main = fx.rev("main");
    let path = fx.path.clone();
    let moved_to = fx.rev("HEAD@{1}");
    assert_ne!(moved_to, main, "the action moves main somewhere else");
    arm_action(fx.repo().git_dir(), FailPoint::BeforeRefs, move || {
        // A terminal `git commit` lands between preflight and the ref transaction.
        crate::support::git_in(&path, &["update-ref", "refs/heads/main", &moved_to]);
    });
    match try_undo(&fx, &oplog) {
        Err(OplogError::ConcurrentModification) => {}
        other => panic!("expected ConcurrentModification, got {other:?}"),
    }
    assert_no_journal(&fx);
    fx.git(&["update-ref", "refs/heads/main", &main]);
    assert_eq!(fp(&fx), after, "worktree and index were rolled back");
    fsck(&fx);
}

#[test]
fn recover_without_journal_does_nothing() {
    let (fx, oplog, _before, after) = recorded_reset();
    recover(&fx, &oplog, Recovery::Complete);
    recover(&fx, &oplog, Recovery::RollBack);
    assert_eq!(fp(&fx), after);
}

#[test]
fn unreadable_journal_can_be_rolled_back() {
    let (fx, oplog, before, _after) = recorded_reset();
    let journal = journal_path(&fx);
    std::fs::create_dir_all(journal.parent().expect("has a parent")).expect("creatable");
    std::fs::write(&journal, b"{ not json").expect("writable");
    assert!(matches!(
        try_undo(&fx, &oplog),
        Err(OplogError::InterruptedRestore)
    ));
    assert!(
        oplog
            .interrupted_restore(&fx.repo())
            .expect("reading works")
            .is_some(),
        "an unreadable journal is still reported"
    );
    let mut guard = fx.guard();
    assert!(
        oplog
            .recover(&fx.repo(), Recovery::Complete, guard.write_permission())
            .is_err(),
        "an unreadable journal cannot be completed"
    );
    drop(guard);
    recover(&fx, &oplog, Recovery::RollBack);
    assert_no_journal(&fx);
    done(undo(&fx, &oplog));
    assert_eq!(fp(&fx), before);
}

/// A tracked file edited after the operation is carried through the undo. Finishing an undo that was interrupted
/// after its checkout keeps the carried edit instead of reverting it.
fn complete_keeps_a_carried_edit(point: FailPoint) {
    let fx = Fixture::new();
    fx.commit_file("c.txt", "c\n", "add c");
    fx.commit_file("b.txt", "b\n", "add b");
    let undone = fx.rev("HEAD~1");
    let oplog = fx.oplog();
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::ResetHard, "Reset to HEAD~1"),
        |fx| {
            fx.git(&["reset", "-q", "--hard", "HEAD~1"]);
        },
    );
    fx.write("c.txt", "c, edited after the reset\n");
    fx.write("a.txt", "a, edited after the reset\n");
    arm(fx.repo().git_dir(), point);
    assert!(matches!(
        try_undo(&fx, &oplog),
        Err(OplogError::InterruptedRestore)
    ));
    recover(&fx, &oplog, Recovery::Complete);
    for (path, content) in [
        ("c.txt", "c, edited after the reset\n"),
        ("a.txt", "a, edited after the reset\n"),
    ] {
        assert_eq!(
            std::fs::read_to_string(fx.path.join(path)).expect("readable"),
            content,
            "finishing the undo keeps the edit of {path} it carried"
        );
    }
    assert_eq!(
        std::fs::read_to_string(fx.path.join("b.txt")).expect("readable"),
        "b\n",
        "the undone reset's file is back"
    );
    assert_ne!(
        fx.rev("main"),
        undone,
        "main is back at the commit with b.txt"
    );
    assert_eq!(
        fx.git(&["status", "--porcelain"]),
        " M a.txt\n M c.txt\n",
        "only the carried edits are left as changes"
    );
    assert_no_journal(&fx);
    fsck(&fx);
}

#[test]
fn complete_after_worktree_keeps_a_carried_edit() {
    complete_keeps_a_carried_edit(FailPoint::AfterWorktree);
}

#[test]
fn complete_after_index_keeps_a_carried_edit() {
    complete_keeps_a_carried_edit(FailPoint::AfterIndex);
}

/// A recorded deletion of the untracked `u.txt`; its undo brings `u.txt` back.
fn recorded_delete_untracked() -> (Fixture, gitcthulhu_oplog::Oplog, String) {
    let fx = Fixture::new();
    fx.write("u.txt", "untracked\n");
    let oplog = fx.oplog();
    let mut d = desc(OperationKind::DeleteUntracked, "Delete 'u.txt'");
    d.untracked = UntrackedCapture::Paths(vec![b("u.txt")]);
    record_ok(&fx, &oplog, d, |fx| {
        std::fs::remove_file(fx.path.join("u.txt")).expect("removable");
    });
    let after = fp(&fx);
    (fx, oplog, after)
}

#[test]
fn roll_back_removes_an_untracked_file_the_undo_brought_back() {
    let (fx, oplog, after) = recorded_delete_untracked();
    arm(fx.repo().git_dir(), FailPoint::AfterRefs);
    assert!(matches!(
        try_undo(&fx, &oplog),
        Err(OplogError::InterruptedRestore)
    ));
    assert!(fx.path.join("u.txt").exists(), "the checkout ran");
    recover(&fx, &oplog, Recovery::RollBack);
    assert_eq!(fp(&fx), after, "the roll back reached the pre-undo state");
    assert_no_journal(&fx);
    fsck(&fx);
}

#[test]
fn roll_back_keeps_an_untracked_file_edited_after_the_interruption() {
    let (fx, oplog, _after) = recorded_delete_untracked();
    arm(fx.repo().git_dir(), FailPoint::AfterRefs);
    assert!(matches!(
        try_undo(&fx, &oplog),
        Err(OplogError::InterruptedRestore)
    ));
    fx.write("u.txt", "untracked, edited after the interruption\n");
    recover(&fx, &oplog, Recovery::RollBack);
    assert_eq!(
        std::fs::read_to_string(fx.path.join("u.txt")).expect("readable"),
        "untracked, edited after the interruption\n",
        "an edit the restore never saw is not deleted"
    );
    assert_no_journal(&fx);
    fsck(&fx);
}
