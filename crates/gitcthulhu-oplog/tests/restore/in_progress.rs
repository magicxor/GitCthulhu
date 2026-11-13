//! D: undo across sequencer states (spec 17 §1.4 R1-R5, §3.3 C9).

use gitcthulhu_oplog::{
    InProgress, OperationKind, RestoreWarning, UndoOutcome, UntrackedCapture,
    testing::{FailPoint, arm},
};

use crate::{
    assert_no_journal, done, fp, fsck, redo,
    support::{Fixture, b, desc, record, record_commit, record_ok},
    undo,
};

/// `main` and `feature` both change `a.txt` from the initial commit; `main` is checked out.
fn conflicting_branches() -> Fixture {
    let fx = Fixture::new();
    fx.commit_file("unrelated.txt", "unrelated\n", "add unrelated");
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("a.txt", "feature\n", "feature change");
    fx.git(&["checkout", "-q", "main"]);
    fx.commit_file("a.txt", "main\n", "main change");
    fx
}

/// Record `args` (which must stop with a conflict) as `kind`, and return the fingerprint before it.
fn record_conflicting(
    fx: &Fixture,
    oplog: &gitcthulhu_oplog::Oplog,
    kind: OperationKind,
    args: &'static [&'static str],
) -> String {
    let before = fp(fx);
    record(fx, oplog, desc(kind, "conflicting operation"), |fx| {
        let out = fx.git_output(args);
        assert!(!out.status.success(), "git {args:?} stops on a conflict");
        Err("conflict".into())
    })
    .expect("recording works")
    .expect("the operation changed the repository");
    assert!(fx.repo().state().is_some(), "an operation is in progress");
    before
}

/// Undo returns to `before`, ends the operation, and cannot be redone (R5).
fn undo_ends_operation(
    fx: &Fixture,
    oplog: &gitcthulhu_oplog::Oplog,
    before: &str,
) -> Vec<RestoreWarning> {
    let report = done(undo(fx, oplog));
    assert_eq!(fp(fx), before, "the clean pre-operation state is back");
    assert!(fx.repo().state().is_none(), "the operation was ended");
    for file in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
    ] {
        assert!(!fx.repo().git_dir().join(file).exists(), "{file} is gone");
    }
    fsck(fx);
    assert_no_journal(fx);
    assert!(
        matches!(redo(fx, oplog), UndoOutcome::Nothing),
        "an undo that ended an operation cannot be redone"
    );
    report.warnings
}

#[test]
fn merge_with_conflicts() {
    let fx = conflicting_branches();
    let oplog = fx.oplog();
    let before = record_conflicting(&fx, &oplog, OperationKind::Merge, &["merge", "feature"]);
    let state = oplog.state(&fx.repo()).expect("reading works");
    assert_eq!(
        state.undo.expect("undoable").ends_in_progress,
        Some(InProgress::Merge)
    );
    let warnings = undo_ends_operation(&fx, &oplog, &before);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn rebase_stopped_on_a_conflict() {
    let fx = conflicting_branches();
    fx.git(&["checkout", "-q", "feature"]);
    let oplog = fx.oplog();
    let before = record_conflicting(&fx, &oplog, OperationKind::Rebase, &["rebase", "main"]);
    let warnings = undo_ends_operation(&fx, &oplog, &before);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        fx.git(&["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/feature"
    );
}

#[test]
fn cherry_pick_with_conflicts() {
    let fx = conflicting_branches();
    let oplog = fx.oplog();
    let before = record_conflicting(
        &fx,
        &oplog,
        OperationKind::CherryPick,
        &["cherry-pick", "feature"],
    );
    let warnings = undo_ends_operation(&fx, &oplog, &before);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn revert_with_conflicts() {
    let fx = Fixture::new();
    fx.commit_file("a.txt", "two\n", "two");
    fx.commit_file("a.txt", "three\n", "three");
    let oplog = fx.oplog();
    let before = record_conflicting(
        &fx,
        &oplog,
        OperationKind::Revert,
        &["revert", "--no-edit", "HEAD~1"],
    );
    let warnings = undo_ends_operation(&fx, &oplog, &before);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn rebase_with_autostash_saves_the_autostash() {
    let fx = conflicting_branches();
    fx.git(&["checkout", "-q", "feature"]);
    fx.write("unrelated.txt", "dirty\n");
    let oplog = fx.oplog();
    record_conflicting(
        &fx,
        &oplog,
        OperationKind::Rebase,
        &["rebase", "--autostash", "main"],
    );
    let report = done(undo(&fx, &oplog));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| matches!(w, RestoreWarning::AutostashSaved { kind } if kind.family() == InProgress::Rebase.family())),
        "{:?}",
        report.warnings
    );
    assert!(fx.repo().state().is_none());
    assert_eq!(
        fx.git(&["stash", "list"]).lines().count(),
        1,
        "the stash list contains the autostash"
    );
    assert_eq!(
        std::fs::read_to_string(fx.path.join("unrelated.txt")).expect("readable"),
        "dirty\n",
        "the worktree is the pre-rebase state"
    );
    fsck(&fx);
}

fn merge_with_autostash(fail_quit: bool) {
    let fx = conflicting_branches();
    fx.write("unrelated.txt", "dirty\n");
    let oplog = fx.oplog();
    record_conflicting(
        &fx,
        &oplog,
        OperationKind::Merge,
        &["merge", "--autostash", "feature"],
    );
    let git_dir = fx.repo().git_dir().to_owned();
    assert!(
        git_dir.join("MERGE_AUTOSTASH").exists(),
        "the merge has an autostash"
    );
    if fail_quit {
        arm(&git_dir, FailPoint::QuitFails);
    }
    let report = done(undo(&fx, &oplog));
    assert!(
        report.warnings.iter().any(|w| matches!(
            w,
            RestoreWarning::AutostashSaved {
                kind: InProgress::Merge
            }
        )),
        "{:?}",
        report.warnings
    );
    assert_eq!(
        report.warnings.iter().any(|w| matches!(
            w,
            RestoreWarning::QuitFailed {
                kind: InProgress::Merge,
                ..
            }
        )),
        fail_quit,
        "{:?}",
        report.warnings
    );
    assert!(
        !git_dir.join("MERGE_AUTOSTASH").exists(),
        "no MERGE_AUTOSTASH remains"
    );
    assert!(fx.repo().state().is_none(), "the merge was ended");
    let stashes = fx.git(&["stash", "list", "--format=%gs"]);
    assert_eq!(
        stashes.lines().count(),
        1,
        "the stash list contains the autostash: {stashes}"
    );
    assert_eq!(
        fx.git(&["stash", "show", "--name-only", "stash@{0}"]),
        "unrelated.txt\n",
        "it is the autostash"
    );
    fsck(&fx);
}

#[test]
fn merge_with_autostash_saves_the_autostash() {
    merge_with_autostash(false);
}

#[test]
fn merge_with_autostash_when_quit_fails_stores_it_before_cleaning_up() {
    merge_with_autostash(true);
}

#[test]
fn merge_resolve_commit_undoes_to_before_the_merge() {
    let fx = conflicting_branches();
    let oplog = fx.oplog();
    let before = record_conflicting(&fx, &oplog, OperationKind::Merge, &["merge", "feature"]);
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::ResolveConflicts, "Resolve a.txt"),
        |fx| {
            fx.write("a.txt", "resolved\n");
            fx.git(&["add", "a.txt"]);
        },
    );
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::Commit, "Commit merge"),
        |fx| {
            fx.git(&["commit", "-q", "--no-edit"]);
        },
    );
    let after = fp(&fx);
    done(undo(&fx, &oplog));
    assert_eq!(fp(&fx), before, "the whole merge is undone in one step");
    fsck(&fx);
    done(redo(&fx, &oplog));
    assert_eq!(fp(&fx), after, "the concluded merge can be redone");
    fsck(&fx);
}

#[test]
fn abort_pops_the_merge_from_the_undo_stack() {
    let fx = conflicting_branches();
    let oplog = fx.oplog();
    let commit = record_commit(&fx, &oplog, "c.txt", "c\n", "add c");
    record_conflicting(&fx, &oplog, OperationKind::Merge, &["merge", "feature"]);
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::AbortOperation, "Abort merge"),
        |fx| {
            fx.git(&["merge", "--abort"]);
        },
    );
    let state = oplog.state(&fx.repo()).expect("reading works");
    assert_eq!(
        state.undo.expect("the commit is undoable").operation,
        commit
    );
    assert!(state.redo.is_none(), "an abort is not redoable");
    done(undo(&fx, &oplog));
    assert!(
        !fx.path.join("c.txt").exists(),
        "the commit before the merge was undone"
    );
    fsck(&fx);
}

#[test]
fn stash_pop_with_conflicts() {
    let fx = Fixture::new();
    fx.write("a.txt", "stashed\n");
    fx.git(&["stash", "push", "-q"]);
    fx.commit_file("a.txt", "committed\n", "conflicting commit");
    let oplog = fx.oplog();
    let before = fp(&fx);
    let mut d = desc(OperationKind::StashPop, "Pop stash@{0}");
    d.untracked = UntrackedCapture::Paths(vec![b("does-not-exist")]);
    record(&fx, &oplog, d, |fx| {
        let out = fx.git_output(&["stash", "pop"]);
        assert!(!out.status.success(), "the pop conflicts");
        Err("conflict".into())
    })
    .expect("recording works")
    .expect("the pop changed the repository");
    assert!(
        fx.git(&["ls-files", "-u"]).contains("a.txt"),
        "the index is conflicted"
    );
    done(undo(&fx, &oplog));
    assert_eq!(fp(&fx), before, "stash list and pre-pop files are back");
    fsck(&fx);
    assert_no_journal(&fx);
}
