//! A8 (integration part), A9, A13: classification of real operations and the capture-failure policy.

use gitcthulhu_oplog::{
    EntryId, EntryRole, EntryStatus, InProgress, OperationKind, OplogError,
    testing::{FailPoint, arm},
};

use crate::support::{Fixture, all_entries, desc, record, record_commit, record_ok, refs_only};

/// `main` and `feature` both change `a.txt` from the initial commit; `main` is checked out.
fn conflicting_branches() -> Fixture {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("a.txt", "feature\n", "feature change");
    fx.git(&["checkout", "-q", "main"]);
    fx.commit_file("a.txt", "main\n", "main change");
    fx
}

fn role_of(fx: &Fixture, oplog: &gitcthulhu_oplog::Oplog, id: EntryId) -> EntryRole {
    oplog
        .entry(&fx.repo(), id)
        .expect("reading works")
        .expect("the entry exists")
        .role
}

#[test]
fn merge_with_conflicts_then_resolve_and_commit_is_one_composite() {
    let fx = conflicting_branches();
    let oplog = fx.oplog();
    let merge = record(
        &fx,
        &oplog,
        desc(OperationKind::Merge, "Merge 'feature' into main"),
        |fx| {
            let out = fx.git_output(&["merge", "feature"]);
            assert!(!out.status.success(), "the merge stops on a conflict");
            Err("conflict".into())
        },
    )
    .expect("recording works")
    .expect("the merge changed the repository");
    let state = oplog.state(&fx.repo()).expect("reading works");
    let top = state.undo.expect("the merge is undoable (R1)");
    assert_eq!(top.operation, merge);
    assert_eq!(
        top.ends_in_progress,
        Some(InProgress::Merge),
        "undoing it would also end the merge"
    );

    let resolve = record_ok(
        &fx,
        &oplog,
        desc(OperationKind::ResolveConflicts, "Resolve a.txt"),
        |fx| {
            fx.write("a.txt", "resolved\n");
            fx.git(&["add", "a.txt"]);
        },
    );
    let commit = record_ok(
        &fx,
        &oplog,
        desc(OperationKind::Commit, "Commit merge"),
        |fx| {
            fx.git(&["commit", "-q", "--no-edit"]);
        },
    );
    let continuation = EntryRole::Continuation {
        origin: merge,
        abort: false,
    };
    assert_eq!(
        role_of(&fx, &oplog, merge),
        EntryRole::Operation { undoable: true }
    );
    assert_eq!(
        role_of(&fx, &oplog, resolve),
        continuation,
        "R2: resolving continues the merge"
    );
    assert_eq!(
        role_of(&fx, &oplog, commit),
        continuation,
        "R2: committing finishes the merge"
    );

    let state = oplog.state(&fx.repo()).expect("reading works");
    assert_eq!(state.undo_depth, 1, "the whole merge is one undo step");
    let top = state.undo.expect("the composite is undoable");
    assert_eq!((top.reverses, top.operation), (merge, merge));
    assert_eq!(
        top.title, "Merge 'feature' into main",
        "labelled as the merge"
    );
    assert_eq!(
        top.ends_in_progress, None,
        "the composite ended with the merge concluded"
    );
    let statuses: Vec<_> = all_entries(&fx, &oplog).iter().map(|e| e.status).collect();
    assert_eq!(
        statuses,
        vec![
            EntryStatus::Undoable { next: true },
            EntryStatus::PartOf { origin: merge },
            EntryStatus::PartOf { origin: merge },
        ]
    );
}

#[test]
fn rebase_stop_then_continue_is_one_composite() {
    let fx = conflicting_branches();
    fx.git(&["checkout", "-q", "feature"]);
    let oplog = fx.oplog();
    let rebase = record(
        &fx,
        &oplog,
        desc(OperationKind::Rebase, "Rebase feature onto main"),
        |fx| {
            let out = fx.git_output(&["rebase", "main"]);
            assert!(!out.status.success(), "the rebase stops on a conflict");
            Err("conflict".into())
        },
    )
    .expect("recording works")
    .expect("the rebase changed the repository");
    assert!(
        matches!(
            oplog
                .entry(&fx.repo(), rebase)
                .expect("reading works")
                .expect("exists")
                .after_in_progress,
            Some(InProgress::Rebase | InProgress::RebaseInteractive)
        ),
        "the rebase stopped"
    );
    let resolve = record_ok(
        &fx,
        &oplog,
        desc(OperationKind::ResolveConflicts, "Resolve"),
        |fx| {
            fx.write("a.txt", "resolved\n");
            fx.git(&["add", "a.txt"]);
        },
    );
    let resume = record_ok(
        &fx,
        &oplog,
        desc(OperationKind::ContinueOperation, "Continue rebase"),
        |fx| {
            fx.git_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")]);
        },
    );
    assert!(fx.repo().state().is_none(), "the rebase is finished");
    let continuation = EntryRole::Continuation {
        origin: rebase,
        abort: false,
    };
    assert_eq!(role_of(&fx, &oplog, resolve), continuation);
    assert_eq!(role_of(&fx, &oplog, resume), continuation);
    let state = oplog.state(&fx.repo()).expect("reading works");
    assert_eq!(state.undo_depth, 1, "the whole rebase is one undo step");
    assert_eq!(state.undo.expect("undoable").operation, rebase);
}

#[test]
fn resolving_a_merge_started_in_a_terminal_is_a_barrier() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("a.txt", "feature\n", "feature change");
    fx.git(&["checkout", "-q", "main"]);
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "a.txt", "main\n", "main change");
    assert!(oplog.can_undo(&fx.repo()).expect("reading works"));

    let out = fx.git_output(&["merge", "feature"]);
    assert!(
        !out.status.success(),
        "the terminal merge stops on a conflict"
    );
    let resolve = record_ok(
        &fx,
        &oplog,
        desc(OperationKind::ResolveConflicts, "Resolve"),
        |fx| {
            fx.write("a.txt", "resolved\n");
            fx.git(&["add", "a.txt"]);
        },
    );
    assert_eq!(
        role_of(&fx, &oplog, resolve),
        EntryRole::Operation { undoable: false },
        "R4: inside a sequencer state the app did not start"
    );
    let state = oplog.state(&fx.repo()).expect("reading works");
    assert!(
        state.undo.is_none() && state.redo.is_none(),
        "a barrier clears both stacks"
    );
}

#[test]
fn refs_only_operation_that_moves_head_is_not_undoable() {
    let fx = Fixture::new();
    fx.git(&["branch", "other"]);
    fx.commit_file("b.txt", "b\n", "add b");
    let oplog = fx.oplog();
    let id = record_ok(
        &fx,
        &oplog,
        refs_only(OperationKind::Checkout, "Check out 'other'"),
        |fx| {
            fx.git(&["checkout", "-q", "other"]);
        },
    );
    assert_eq!(
        role_of(&fx, &oplog, id),
        EntryRole::Operation { undoable: false },
        "RefsOnly must not move HEAD^{{commit}}"
    );
    assert!(!oplog.can_undo(&fx.repo()).expect("reading works"));
}

#[test]
fn renaming_the_current_branch_is_a_valid_refs_only_operation() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    let id = record_ok(
        &fx,
        &oplog,
        refs_only(OperationKind::RenameBranch, "Rename 'main' to 'trunk'"),
        |fx| {
            fx.git(&["branch", "-m", "main", "trunk"]);
        },
    );
    assert_eq!(
        role_of(&fx, &oplog, id),
        EntryRole::Operation { undoable: true },
        "HEAD's symbolic target changes, its commit doesn't"
    );
}

#[test]
fn capture_failure_keeps_the_stacks() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "one");
    let two = record_commit(&fx, &oplog, "c.txt", "c\n", "two");

    arm(fx.repo().git_dir(), FailPoint::Capture);
    {
        let mut guard = fx.guard();
        let err = oplog
            .prepare(
                &fx.repo(),
                desc(OperationKind::Commit, "Commit \"three\""),
                guard.write_permission(),
            )
            .err()
            .expect("the armed capture fails");
        assert!(matches!(err, OplogError::Capture(_)), "{err:?}");
        // The GUI runs the operation unrecorded.
        fx.commit_file("d.txt", "d\n", "three");
    }
    let state = oplog.state(&fx.repo()).expect("reading works");
    assert_eq!(state.undo_depth, 2, "nothing was cleared");
    assert_eq!(
        state.undo.expect("undo is still offered").operation,
        two,
        "undo still offers commit two"
    );
}

#[test]
fn undo_after_an_unrecorded_operation_asks_about_moved_refs() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "one");
    record_commit(&fx, &oplog, "c.txt", "c\n", "two");
    fx.commit_file("d.txt", "d\n", "three (unrecorded)");
    let mut guard = fx.guard();
    let outcome = oplog
        .undo(&fx.repo(), Default::default(), guard.write_permission())
        .expect("undo works");
    let gitcthulhu_oplog::UndoOutcome::NeedsConfirmation(confirmation) = outcome else {
        panic!("expected a confirmation, got {outcome:?}");
    };
    assert!(
        matches!(
            confirmation.reasons.as_slice(),
            [gitcthulhu_oplog::ConfirmReason::RefsMovedSince(d)] if d.len() == 1 && d[0].name.as_bstr() == "refs/heads/main"
        ),
        "{confirmation:?}"
    );
}

#[test]
fn failed_after_capture_still_records_the_entry() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
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
    let id = oplog
        .finish(
            &fx.repo(),
            pending,
            gitcthulhu_oplog::OperationResult::Succeeded,
            guard.write_permission(),
        )
        .expect("finish works")
        .expect("the entry is recorded without its after-state");
    drop(guard);
    let entry = oplog
        .entry(&fx.repo(), id)
        .expect("reading works")
        .expect("exists");
    assert!(!entry.after_captured, "the after-state is unknown");
    assert_eq!(
        entry.status,
        EntryStatus::Undoable { next: true },
        "still undoable"
    );
    let tree = fx.git(&["ls-tree", "--name-only", "refs/gitcthulhu/oplog/main"]);
    assert_eq!(tree, "before\nmeta.json\n", "no after/ tree");
    let parents = fx.git(&["rev-list", "--parents", "-n1", "refs/gitcthulhu/oplog/main"]);
    assert!(
        parents.contains(&fx.rev("HEAD")),
        "the new commit is kept, compared with the refs as they are now"
    );
}

#[test]
fn disabled_recording_returns_none_and_markers_do_not_change_the_stacks() {
    let fx = Fixture::new();
    let disabled = fx.oplog_with(gitcthulhu_oplog::OplogOptions {
        enabled: false,
        ..Default::default()
    });
    let mut guard = fx.guard();
    assert!(
        disabled
            .prepare(
                &fx.repo(),
                desc(OperationKind::Commit, "c"),
                guard.write_permission()
            )
            .expect("prepare works")
            .is_none(),
        "recording is disabled"
    );
    drop(guard);

    let oplog = fx.oplog();
    let commit = record_commit(&fx, &oplog, "b.txt", "b\n", "b");
    let mut guard = fx.guard();
    let head = gix::ObjectId::from_hex(fx.rev("HEAD").as_bytes()).expect("valid id");
    let marker = oplog
        .record_marker(
            &fx.repo(),
            gitcthulhu_oplog::NetworkMarker {
                kind: OperationKind::Push,
                title: "Push main to origin/main".into(),
                remote: "origin".into(),
                updates: vec![gitcthulhu_oplog::PushedRef {
                    local: Some("refs/heads/main".try_into().expect("valid")),
                    remote_ref: "refs/heads/main".try_into().expect("valid"),
                    new_target: Some(head),
                    forced: false,
                }],
            },
            guard.write_permission(),
        )
        .expect("recording a marker works");
    drop(guard);
    let entry = oplog
        .entry(&fx.repo(), marker)
        .expect("reading works")
        .expect("exists");
    assert_eq!(entry.status, EntryStatus::Marker);
    assert_eq!(
        entry.network.expect("the marker round-trips").updates[0].new_target,
        Some(head)
    );
    assert_eq!(
        oplog
            .state(&fx.repo())
            .expect("reading works")
            .undo
            .expect("undoable")
            .operation,
        commit,
        "a marker does not change the stacks"
    );

    let mut guard = fx.guard();
    oplog
        .record_barrier(
            &fx.repo(),
            OperationKind::Barrier,
            "Undo history was paused".into(),
            "paused".into(),
            guard.write_permission(),
        )
        .expect("recording a barrier works");
    drop(guard);
    assert!(
        !oplog.can_undo(&fx.repo()).expect("reading works"),
        "a barrier clears the stacks"
    );
}
