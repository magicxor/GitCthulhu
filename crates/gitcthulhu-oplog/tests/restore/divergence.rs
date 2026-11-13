//! C: divergence between the recorded state and the repository at undo time (spec 17 §2.7, §3.2).

use gitcthulhu_oplog::{
    ConfirmReason, EntryId, InProgress, NetworkMarker, OperationKind, OperationResult, PushedRef,
    RefValueInfo, Refusal, UndoOutcome, UntrackedCapture,
    testing::{FailPoint, arm},
};

use crate::{
    assert_no_journal, confirmation, confirmed, done, fp, fsck, redo,
    support::{Fixture, b, desc, record_branch, record_commit, record_ok},
    undo, undo_with,
};

fn oid(hex: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(hex.as_bytes()).expect("valid object id")
}

fn read(fx: &Fixture, path: &str) -> String {
    std::fs::read_to_string(fx.path.join(path)).expect("the file can be read")
}

/// Record a discard of the modification `dirty` of `a.txt`.
fn record_discard(fx: &Fixture, oplog: &gitcthulhu_oplog::Oplog, dirty: &str) -> EntryId {
    fx.write("a.txt", dirty);
    let mut d = desc(OperationKind::Discard, "Discard 'a.txt'");
    d.untracked = UntrackedCapture::Paths(vec![b("a.txt")]);
    record_ok(fx, oplog, d, |fx| {
        fx.git(&["checkout", "-q", "--", "a.txt"]);
    })
}

#[test]
fn external_commit_asks_and_redo_brings_it_back() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "b");
    let parent = fx.rev("HEAD~1");
    fx.commit_file("c.txt", "c\n", "external");
    let external = fx.rev("HEAD");

    let ask = confirmation(undo(&fx, &oplog));
    match ask.reasons.as_slice() {
        [ConfirmReason::RefsMovedSince(moved)] => {
            assert_eq!(moved.len(), 1, "{moved:?}");
            assert_eq!(moved[0].name.as_bstr(), "refs/heads/main");
            assert_eq!(moved[0].actual, Some(RefValueInfo::Object(oid(&external))));
        }
        other => panic!("expected RefsMovedSince[main], got {other:?}"),
    }
    assert_eq!(fx.rev("HEAD"), external, "nothing was changed while asking");

    done(undo_with(&fx, &oplog, confirmed(&ask)));
    assert_eq!(
        fx.rev("main"),
        parent,
        "main is back before the recorded commit"
    );
    fsck(&fx);
    assert!(
        fx.git_output(&["cat-file", "-e", &external])
            .status
            .success(),
        "the external commit stays reachable"
    );
    fx.git(&["reflog", "expire", "--expire-unreachable=now", "--all"]);
    fx.git(&["gc", "-q", "--prune=now"]);
    assert!(
        fx.git_output(&["cat-file", "-e", &external])
            .status
            .success(),
        "the external commit survives gc: the undo entry keeps it"
    );

    done(redo(&fx, &oplog));
    assert_eq!(
        fx.rev("main"),
        external,
        "redo brings the external commit back"
    );
    fsck(&fx);
}

#[test]
fn a_token_does_not_cover_a_further_divergence() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "b");
    fx.commit_file("c.txt", "c\n", "external 1");
    let ask = confirmation(undo(&fx, &oplog));
    fx.commit_file("d.txt", "d\n", "external 2");
    let again = confirmation(undo_with(&fx, &oplog, confirmed(&ask)));
    assert_ne!(again.token, ask.token, "a different divergence asks again");
    assert_eq!(fx.git(&["log", "-1", "--format=%s"]).trim(), "external 2");
}

#[test]
fn unrelated_edits_are_carried_without_asking() {
    let fx = Fixture::new();
    fx.commit_file("unrelated.txt", "unrelated\n", "add unrelated");
    let oplog = fx.oplog();
    record_discard(&fx, &oplog, "a, dirty\n");
    fx.write("unrelated.txt", "unrelated, edited later\n");
    fx.write("new.txt", "brand new\n");

    done(undo(&fx, &oplog));
    assert_eq!(
        read(&fx, "a.txt"),
        "a, dirty\n",
        "the discarded edit is back"
    );
    assert_eq!(
        read(&fx, "unrelated.txt"),
        "unrelated, edited later\n",
        "the later edit is carried"
    );
    assert_eq!(
        read(&fx, "new.txt"),
        "brand new\n",
        "a new untracked file is kept"
    );
    fsck(&fx);
    assert_no_journal(&fx);
}

#[test]
fn conflicting_edit_asks_and_redo_returns_it_byte_exact() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_discard(&fx, &oplog, "a, dirty\n");
    fx.write("a.txt", "edited after the discard\r\nwith CRLF\r\n");
    fx.write("new.txt", "brand new\n");
    let before_undo = fp(&fx);

    let ask = confirmation(undo(&fx, &oplog));
    assert_eq!(
        ask.reasons,
        vec![ConfirmReason::WorktreeChangesConflict(vec![b("a.txt")])]
    );
    assert_eq!(fp(&fx), before_undo, "nothing was changed while asking");
    assert_no_journal(&fx);

    done(undo_with(&fx, &oplog, confirmed(&ask)));
    assert_eq!(
        read(&fx, "a.txt"),
        "a, dirty\n",
        "the confirmed undo replaces the file"
    );
    assert_eq!(
        read(&fx, "new.txt"),
        "brand new\n",
        "a forced undo keeps new untracked files"
    );
    fsck(&fx);

    done(redo(&fx, &oplog));
    assert_eq!(
        std::fs::read(fx.path.join("a.txt")).expect("readable"),
        b"edited after the discard\r\nwith CRLF\r\n",
        "redo brings the edit back byte-exact"
    );
    assert_eq!(fp(&fx), before_undo);
    fsck(&fx);
}

#[test]
fn staging_after_a_commit_is_carried() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    fx.write("a.txt", "a, staged later\n");
    fx.git(&["add", "a.txt"]);

    done(undo(&fx, &oplog));
    assert_eq!(
        fx.git(&["diff", "--cached", "--name-only"]),
        "a.txt\n",
        "the later staging is carried; b.txt did not exist before the commit"
    );
    assert!(!fx.path.join("b.txt").exists());
    assert_eq!(read(&fx, "a.txt"), "a, staged later\n");
    fsck(&fx);
}

#[test]
fn conflicting_staging_asks() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "a.txt", "a2\n", "change a");
    fx.write("a.txt", "a3\n");
    fx.git(&["add", "a.txt"]);
    let ask = confirmation(undo(&fx, &oplog));
    assert_eq!(
        ask.reasons,
        vec![ConfirmReason::StagedChangesConflict(vec![b("a.txt")])]
    );
    // The worktree has the same conflicting edit; confirming the staging conflict reveals it.
    let ask_more = confirmation(undo_with(&fx, &oplog, confirmed(&ask)));
    assert_eq!(
        ask_more.reasons,
        vec![
            ConfirmReason::StagedChangesConflict(vec![b("a.txt")]),
            ConfirmReason::WorktreeChangesConflict(vec![b("a.txt")]),
        ]
    );
    done(undo_with(&fx, &oplog, confirmed(&ask_more)));
    assert_eq!(read(&fx, "a.txt"), "a\n", "the target's content");
    assert_eq!(
        fx.git(&["status", "--porcelain"]),
        "",
        "index and worktree are the target's"
    );
    fsck(&fx);
}

#[test]
fn external_stash_asks_about_refs_stash() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    fx.write("a.txt", "a, stashed outside\n");
    fx.git(&["stash", "push", "-q"]);
    let ask = confirmation(undo(&fx, &oplog));
    match ask.reasons.as_slice() {
        [ConfirmReason::RefsMovedSince(moved)] => {
            let names: Vec<_> = moved.iter().map(|d| d.name.as_bstr().to_string()).collect();
            assert_eq!(names, vec!["refs/stash"]);
            assert_eq!(moved[0].expected, None, "there was no stash");
        }
        other => panic!("expected RefsMovedSince[refs/stash], got {other:?}"),
    }
    done(undo_with(&fx, &oplog, confirmed(&ask)));
    assert!(
        !fx.git_output(&["rev-parse", "--verify", "-q", "refs/stash"])
            .status
            .success(),
        "the undo restored the stash state of its target (no stash)"
    );
    done(redo(&fx, &oplog));
    assert_eq!(
        fx.git(&["stash", "list", "--format=%gs"]).lines().count(),
        1,
        "redo brings the external stash back"
    );
    fsck(&fx);
}

#[test]
fn pushed_commit_asks_on_undo_but_not_on_redo() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    let commit = record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    let marker = {
        let mut guard = fx.guard();
        oplog
            .record_marker(
                &fx.repo(),
                NetworkMarker {
                    kind: OperationKind::Push,
                    title: "Push main to origin/main".into(),
                    remote: "origin".into(),
                    updates: vec![PushedRef {
                        local: Some("refs/heads/main".try_into().expect("valid")),
                        remote_ref: "refs/heads/main".try_into().expect("valid"),
                        new_target: Some(oid(&fx.rev("HEAD"))),
                        forced: false,
                    }],
                },
                guard.write_permission(),
            )
            .expect("recording the marker works")
    };
    let ask = confirmation(undo(&fx, &oplog));
    match ask.reasons.as_slice() {
        [ConfirmReason::PushedSince(pushed)] => {
            assert_eq!(pushed.len(), 1);
            assert_eq!(pushed[0].marker, marker);
            assert_eq!(pushed[0].remote, "origin");
            assert_eq!(pushed[0].local.as_bstr(), "refs/heads/main");
        }
        other => panic!("expected PushedSince, got {other:?}"),
    }
    let report = done(undo_with(&fx, &oplog, confirmed(&ask)));
    assert_eq!(report.reversed, commit);
    done(redo(&fx, &oplog));
    fsck(&fx);
}

#[test]
fn merge_started_in_a_terminal_refuses_undo() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("a.txt", "feature\n", "feature change");
    fx.git(&["checkout", "-q", "main"]);
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "a.txt", "main\n", "main change");
    assert!(
        !fx.git_output(&["merge", "feature"]).status.success(),
        "conflict"
    );
    let before = fp(&fx);
    match undo(&fx, &oplog) {
        UndoOutcome::Refused(Refusal::InProgressMismatch { expected, actual }) => {
            assert_eq!(expected, None);
            assert_eq!(actual, Some(InProgress::Merge));
        }
        other => panic!("expected InProgressMismatch, got {other:?}"),
    }
    assert_eq!(fp(&fx), before, "nothing changed");
}

#[test]
fn branch_checked_out_in_another_worktree_refuses_undo() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_branch(&fx, &oplog, "topic");
    let wt = fx.tmp().join("wt");
    fx.git(&["worktree", "add", "-q", &wt.display().to_string(), "topic"]);
    match undo(&fx, &oplog) {
        UndoOutcome::Refused(Refusal::BranchCheckedOutElsewhere { branch, worktrees }) => {
            assert_eq!(branch.as_bstr(), "refs/heads/topic");
            assert_eq!(worktrees.len(), 1, "{worktrees:?}");
            let canonical = |p: &std::path::Path| gix::path::realpath(p).expect("exists");
            assert_eq!(canonical(&worktrees[0]), canonical(&wt));
        }
        other => panic!("expected BranchCheckedOutElsewhere, got {other:?}"),
    }
    assert!(
        fx.git_output(&["rev-parse", "--verify", "-q", "refs/heads/topic"])
            .status
            .success(),
        "the branch is still there"
    );
}

#[test]
fn failed_after_capture_asks_after_state_unknown() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    let before = fp(&fx);
    {
        let mut guard = fx.guard();
        let pending = oplog
            .prepare(
                &fx.repo(),
                desc(OperationKind::Commit, "Commit \"b\""),
                guard.write_permission(),
            )
            .expect("prepare works")
            .expect("recording is enabled");
        fx.commit_file("b.txt", "b\n", "b");
        arm(fx.repo().git_dir(), FailPoint::Capture);
        oplog
            .finish(
                &fx.repo(),
                pending,
                OperationResult::Succeeded,
                guard.write_permission(),
            )
            .expect("finish works")
            .expect("recorded without after-state");
    }
    let ask = confirmation(undo(&fx, &oplog));
    assert_eq!(ask.reasons, vec![ConfirmReason::AfterStateUnknown]);
    done(undo_with(&fx, &oplog, confirmed(&ask)));
    assert_eq!(fp(&fx), before, "forced undo restores the before-state");
    fsck(&fx);
}

#[test]
fn undo_after_external_branch_switch_does_not_mix_trees() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("f.txt", "feature file\n", "feature");
    fx.git(&["checkout", "-q", "main"]);
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "x.txt", "x\n", "X");
    let parent = fx.rev("HEAD~1");
    // In the terminal: switch to feature and edit a file present on both branches.
    fx.git(&["switch", "-q", "feature"]);
    fx.write("a.txt", "a, edited on feature\n");

    let ask = confirmation(undo(&fx, &oplog));
    match ask.reasons.as_slice() {
        [ConfirmReason::RefsMovedSince(moved)] => {
            let names: Vec<_> = moved.iter().map(|d| d.name.as_bstr().to_string()).collect();
            assert_eq!(names, vec!["HEAD"]);
        }
        other => panic!("expected RefsMovedSince[HEAD], got {other:?}"),
    }
    done(undo_with(&fx, &oplog, confirmed(&ask)));
    assert_eq!(fx.git(&["symbolic-ref", "HEAD"]).trim(), "refs/heads/main");
    assert_eq!(fx.rev("main"), parent, "main is at X's parent");
    assert_eq!(
        fx.git(&["ls-files"]),
        "a.txt\n",
        "the index is X's parent tree, nothing of feature"
    );
    assert_eq!(
        fx.git(&["diff", "--cached", "--name-only"]),
        "",
        "nothing staged"
    );
    assert!(!fx.path.join("f.txt").exists(), "feature's file is gone");
    assert!(!fx.path.join("x.txt").exists(), "X's file is gone");
    assert_eq!(
        read(&fx, "a.txt"),
        "a, edited on feature\n",
        "only the uncommitted edit is carried"
    );
    fsck(&fx);

    done(redo(&fx, &oplog));
    assert_eq!(
        fx.git(&["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/feature"
    );
    assert!(fx.path.join("f.txt").exists());
    fsck(&fx);
}

#[test]
fn stash_apply_with_untracked_file_round_trips() {
    let fx = Fixture::new();
    fx.write("v.txt", "untracked\r\nbytes\0\n");
    fx.git(&["stash", "push", "-q", "-u"]);
    let oplog = fx.oplog();
    let mut d = desc(OperationKind::StashApply, "Apply stash@{0}");
    d.untracked = UntrackedCapture::Paths(vec![b("v.txt")]);
    record_ok(&fx, &oplog, d, |fx| {
        fx.git(&["stash", "apply", "-q"]);
    });
    done(undo(&fx, &oplog));
    assert!(
        !fx.path.join("v.txt").exists(),
        "undo removes the file the stash brought back"
    );
    done(redo(&fx, &oplog));
    assert_eq!(
        std::fs::read(fx.path.join("v.txt")).expect("readable"),
        b"untracked\r\nbytes\0\n",
        "redo brings it back byte-exact"
    );
    fsck(&fx);
}

#[test]
fn nothing_to_undo_or_redo() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    assert!(matches!(undo(&fx, &oplog), UndoOutcome::Nothing));
    assert!(matches!(redo(&fx, &oplog), UndoOutcome::Nothing));
}

#[test]
fn restore_to_asks_lists_ref_changes_and_is_undoable() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    let start = fp(&fx);
    let first = record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    record_commit(&fx, &oplog, "c.txt", "c\n", "add c");
    let end = fp(&fx);
    let initial = fx.rev("HEAD~2");
    let target = gitcthulhu_oplog::RestoreTarget {
        entry: first,
        side: gitcthulhu_oplog::Side::Before,
    };
    let ask = confirmation(crate::restore_to_with(
        &fx,
        &oplog,
        target,
        Default::default(),
    ));
    match ask.reasons.as_slice() {
        [ConfirmReason::ArbitraryRestore { ref_changes }] => {
            assert_eq!(ref_changes.len(), 1, "{ref_changes:?}");
            assert_eq!(ref_changes[0].name.as_bstr(), "refs/heads/main");
            assert_eq!(
                ref_changes[0].expected,
                Some(RefValueInfo::Object(oid(&initial))),
                "`expected` is the value the restore writes"
            );
        }
        other => panic!("expected ArbitraryRestore, got {other:?}"),
    }
    let report = done(crate::restore_to_with(&fx, &oplog, target, confirmed(&ask)));
    assert_eq!(report.reversed, first);
    assert_eq!(report.title, "Commit \"add b\"");
    assert_eq!(fp(&fx), start, "the state before the first commit");
    fsck(&fx);
    let state = oplog.state(&fx.repo()).expect("reading works");
    assert_eq!(
        state.undo.expect("the restore is undoable").operation,
        report.entry
    );

    done(undo(&fx, &oplog));
    assert_eq!(
        fp(&fx),
        end,
        "undoing the restore returns to where it started"
    );
    done(redo(&fx, &oplog));
    assert_eq!(fp(&fx), start);
    fsck(&fx);
}

#[test]
fn restore_to_refuses_entries_without_snapshots() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    let barrier = {
        let mut guard = fx.guard();
        oplog
            .record_barrier(
                &fx.repo(),
                OperationKind::Barrier,
                "Undo history was paused".into(),
                "paused".into(),
                guard.write_permission(),
            )
            .expect("recording works")
    };
    let side = gitcthulhu_oplog::Side::Before;
    match crate::restore_to_with(
        &fx,
        &oplog,
        gitcthulhu_oplog::RestoreTarget {
            entry: barrier,
            side,
        },
        Default::default(),
    ) {
        UndoOutcome::Refused(Refusal::EntryHasNoSnapshot(id)) => assert_eq!(id, barrier),
        other => panic!("expected EntryHasNoSnapshot, got {other:?}"),
    }
    let missing = EntryId(barrier.0 + 100);
    match crate::restore_to_with(
        &fx,
        &oplog,
        gitcthulhu_oplog::RestoreTarget {
            entry: missing,
            side,
        },
        Default::default(),
    ) {
        UndoOutcome::Refused(Refusal::EntryNotFound(id)) => assert_eq!(id, missing),
        other => panic!("expected EntryNotFound, got {other:?}"),
    }
}
