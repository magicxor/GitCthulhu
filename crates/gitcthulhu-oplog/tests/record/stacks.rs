//! A8 (table-driven part): the replay of roles into undo/redo stacks and display statuses.

use gitcthulhu_oplog::{
    EntryId, EntryRole, EntryStatus, InProgress, OperationKind, Side,
    testing::{Replayed, classify_entry, replay_roles},
};

const WT: &str = "C:/repo";

fn op(seq: u64) -> (u64, EntryRole, &'static str) {
    (seq, EntryRole::Operation { undoable: true }, WT)
}
fn barrier_op(seq: u64) -> (u64, EntryRole, &'static str) {
    (seq, EntryRole::Operation { undoable: false }, WT)
}
fn undo(seq: u64, reverses: u64, redoable: bool) -> (u64, EntryRole, &'static str) {
    (
        seq,
        EntryRole::Undo {
            reverses: EntryId(reverses),
            redoable,
        },
        WT,
    )
}
fn redo(seq: u64, reverses: u64) -> (u64, EntryRole, &'static str) {
    (
        seq,
        EntryRole::Redo {
            reverses: EntryId(reverses),
        },
        WT,
    )
}
fn cont(seq: u64, origin: u64, abort: bool) -> (u64, EntryRole, &'static str) {
    (
        seq,
        EntryRole::Continuation {
            origin: EntryId(origin),
            abort,
        },
        WT,
    )
}
fn marker(seq: u64) -> (u64, EntryRole, &'static str) {
    (seq, EntryRole::Marker, WT)
}

fn replay(entries: &[(u64, EntryRole, &str)]) -> Replayed {
    replay_roles(entries, None)
}

#[test]
fn operations_push_onto_the_undo_stack() {
    let r = replay(&[op(1), op(2), marker(3)]);
    assert_eq!(r.undo, vec![(1, 1, 1), (2, 2, 2)]);
    assert_eq!(r.redo, vec![]);
    assert_eq!(
        r.status,
        vec![
            (1, EntryStatus::Undoable { next: false }),
            (2, EntryStatus::Undoable { next: true }),
            (3, EntryStatus::Marker),
        ],
        "markers change nothing"
    );
}

#[test]
fn undo_and_redo_move_items_between_the_stacks() {
    let r = replay(&[op(1), op(2), undo(3, 2, true)]);
    assert_eq!(r.undo, vec![(1, 1, 1)]);
    assert_eq!(
        r.redo,
        vec![(3, 2, 3)],
        "redo restores the undo's before-state, labelled as the operation"
    );
    assert_eq!(r.status[1], (2, EntryStatus::Undone { next: true }));
    assert_eq!(r.status[2], (3, EntryStatus::Bookkeeping));

    let r = replay(&[op(1), op(2), undo(3, 2, true), redo(4, 3)]);
    assert_eq!(
        r.undo,
        vec![(1, 1, 1), (4, 2, 4)],
        "the redo entry is undone next, labelled as op 2"
    );
    assert_eq!(r.redo, vec![]);
    assert_eq!(r.status[1], (2, EntryStatus::Undoable { next: true }));

    let r = replay(&[op(1), op(2), undo(3, 2, true), redo(4, 3), undo(5, 4, true)]);
    assert_eq!(r.undo, vec![(1, 1, 1)]);
    assert_eq!(r.redo, vec![(5, 2, 5)]);
}

#[test]
fn a_new_operation_supersedes_the_redo_stack() {
    let r = replay(&[op(1), undo(2, 1, true), op(3)]);
    assert_eq!(r.undo, vec![(3, 3, 3)]);
    assert_eq!(r.redo, vec![]);
    assert_eq!(r.status[0], (1, EntryStatus::Superseded));
}

#[test]
fn non_redoable_undo_clears_the_redo_stack() {
    let r = replay(&[op(1), op(2), undo(3, 2, true), op(4), undo(5, 4, false)]);
    assert_eq!(r.undo, vec![(1, 1, 1)]);
    assert_eq!(
        r.redo,
        vec![],
        "an undo that ended an in-progress operation cannot be redone"
    );
    assert_eq!(r.status[3], (4, EntryStatus::Superseded));
}

#[test]
fn barriers_clear_both_stacks() {
    let r = replay(&[
        op(1),
        op(2),
        undo(3, 2, true),
        (
            4,
            EntryRole::Barrier {
                reason: "paused".into(),
            },
            WT,
        ),
    ]);
    assert_eq!(r.undo, vec![]);
    assert_eq!(r.redo, vec![]);
    assert_eq!(r.status[0], (1, EntryStatus::NotUndoable));
    assert_eq!(r.status[1], (2, EntryStatus::Superseded));
    assert_eq!(r.status[3], (4, EntryStatus::NotUndoable));

    let r = replay(&[op(1), barrier_op(2), op(3)]);
    assert_eq!(
        r.undo,
        vec![(3, 3, 3)],
        "an in-progress barrier also clears the stacks"
    );
    assert_eq!(r.status[0], (1, EntryStatus::NotUndoable));
}

#[test]
fn continuations_fold_into_the_origin_and_abort_pops_it() {
    let r = replay(&[op(1), op(2), cont(3, 2, false), cont(4, 2, false)]);
    assert_eq!(
        r.undo,
        vec![(1, 1, 1), (2, 2, 4)],
        "undo restores 2's before, expecting 4's after"
    );
    assert_eq!(r.status[2], (3, EntryStatus::PartOf { origin: EntryId(2) }));
    assert_eq!(r.status[3], (4, EntryStatus::PartOf { origin: EntryId(2) }));

    let r = replay(&[op(1), op(2), cont(3, 2, false), cont(4, 2, true)]);
    assert_eq!(r.undo, vec![(1, 1, 1)], "abort pops the composite");
    assert_eq!(r.status[1], (2, EntryStatus::NotUndoable));
    assert_eq!(r.status[0], (1, EntryStatus::Undoable { next: true }));
}

#[test]
fn dangling_references_after_pruning() {
    // The origin of a continuation was pruned: a barrier.
    let r = replay(&[op(5), cont(6, 4, false)]);
    assert_eq!(r.undo, vec![]);
    assert_eq!(r.status[1], (6, EntryStatus::NotUndoable));

    // An undo/redo whose target was pruned is ignored.
    let r = replay(&[op(5), undo(6, 4, true), redo(7, 3)]);
    assert_eq!(r.undo, vec![(5, 5, 5)]);
    assert_eq!(r.redo, vec![]);
    assert_eq!(r.status[1], (6, EntryStatus::Bookkeeping));
}

#[test]
fn restore_to_is_undoable_and_clears_redo() {
    let r = replay(&[
        op(1),
        undo(2, 1, true),
        (
            3,
            EntryRole::RestoreTo {
                target: EntryId(1),
                side: Side::After,
            },
            WT,
        ),
    ]);
    assert_eq!(r.undo, vec![(3, 3, 3)]);
    assert_eq!(r.redo, vec![]);
}

#[test]
fn foreign_entries_of_a_linked_chain_clear_the_stacks() {
    let entries = [
        op(1),
        (2, EntryRole::Operation { undoable: true }, "C:/elsewhere"),
        op(3),
    ];
    let r = replay_roles(&entries, Some(WT));
    assert_eq!(
        r.undo,
        vec![(3, 3, 3)],
        "entry 2 was written by another worktree"
    );
    assert_eq!(r.status[0], (1, EntryStatus::NotUndoable));
    let r = replay_roles(&entries, None);
    assert_eq!(r.undo.len(), 3, "the main chain ignores worktree paths");
}

#[test]
fn worktree_paths_compare_case_insensitively_only_on_windows() {
    let entries = [
        op(1),
        (2, EntryRole::Operation { undoable: true }, "c:/REPO"),
    ];
    let r = replay_roles(&entries, Some(WT));
    if cfg!(windows) {
        assert_eq!(
            r.undo.len(),
            2,
            "on Windows C:/repo and c:/REPO are the same directory"
        );
    } else {
        assert_eq!(
            r.undo,
            vec![],
            "case-sensitive file systems treat them as different directories: entry 2 is foreign, so it \
             clears the stacks and is not undoable itself"
        );
    }
}

#[test]
fn classification_of_new_entries() {
    assert_eq!(
        classify_entry(OperationKind::Commit, false, None, None),
        EntryRole::Operation { undoable: true },
        "R1: no in-progress state before"
    );
    assert_eq!(
        classify_entry(
            OperationKind::Merge,
            false,
            None,
            Some((1, 1, 1, Some(InProgress::Merge)))
        ),
        EntryRole::Operation { undoable: true },
        "R1 even when the top ended in progress"
    );
    assert_eq!(
        classify_entry(OperationKind::Checkout, true, None, None),
        EntryRole::Operation { undoable: false },
        "a RefsOnly entry that moved HEAD is a barrier"
    );
    assert_eq!(
        classify_entry(
            OperationKind::ContinueOperation,
            false,
            Some(InProgress::RebaseInteractive),
            Some((4, 4, 6, Some(InProgress::Rebase)))
        ),
        EntryRole::Continuation {
            origin: EntryId(4),
            abort: false
        },
        "R2: same family"
    );
    assert_eq!(
        classify_entry(
            OperationKind::AbortOperation,
            false,
            Some(InProgress::Merge),
            Some((4, 4, 4, Some(InProgress::Merge)))
        ),
        EntryRole::Continuation {
            origin: EntryId(4),
            abort: true
        },
        "R3: abort"
    );
    assert_eq!(
        classify_entry(
            OperationKind::ResolveConflicts,
            false,
            Some(InProgress::Merge),
            Some((4, 4, 4, Some(InProgress::CherryPick)))
        ),
        EntryRole::Operation { undoable: false },
        "R4: another family"
    );
    assert_eq!(
        classify_entry(
            OperationKind::ResolveConflicts,
            false,
            Some(InProgress::Merge),
            None
        ),
        EntryRole::Operation { undoable: false },
        "R4: no origin"
    );
}
