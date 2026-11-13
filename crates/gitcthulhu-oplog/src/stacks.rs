//! Classification of new entries and the replay that derives the undo/redo stacks (spec 17 §4.3).

use std::collections::HashMap;

use crate::{
    meta::EntryMeta,
    types::{EntryId, EntryRole, EntryStatus, InProgress, OperationKind},
};

/// One item of the undo or redo stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StackItem {
    /// The entry whose before-state Undo (resp. Redo) restores, and which the new entry reverses.
    pub reverse: EntryId,
    /// The user-visible operation (the original operation for redo items and composites).
    pub label: EntryId,
    /// The entry whose after-state is the expected current state.
    pub expected: EntryId,
}

/// The result of [`replay()`].
#[derive(Clone, Debug, Default)]
pub(crate) struct Stacks {
    /// Bottom → top.
    pub undo: Vec<StackItem>,
    /// Bottom → top.
    pub redo: Vec<StackItem>,
    /// The display status of every replayed entry.
    pub status: HashMap<EntryId, EntryStatus>,
}

/// Replay `entries` (oldest → newest) into the undo/redo stacks.
///
/// `linked_workdir` is `Some(path)` for the chain of a linked worktree: an entry written by a worktree at another
/// path (a removed worktree whose id was reused) clears both stacks (§2.1). The main chain passes `None`.
pub(crate) fn replay<'a>(
    entries: impl IntoIterator<Item = &'a EntryMeta>,
    linked_workdir: Option<&str>,
) -> Stacks {
    let mut undo: Vec<StackItem> = Vec::new();
    let mut redo: Vec<StackItem> = Vec::new();
    let mut status = HashMap::new();

    fn clear_undo(undo: &mut Vec<StackItem>, status: &mut HashMap<EntryId, EntryStatus>) {
        for item in undo.drain(..) {
            status.insert(item.label, EntryStatus::NotUndoable);
        }
    }
    fn clear_redo(redo: &mut Vec<StackItem>, status: &mut HashMap<EntryId, EntryStatus>) {
        for item in redo.drain(..) {
            status.insert(item.label, EntryStatus::Superseded);
        }
    }
    let own = |id: EntryId| StackItem {
        reverse: id,
        label: id,
        expected: id,
    };

    for entry in entries {
        let id = entry.id();
        if linked_workdir.is_some_and(|workdir| !same_worktree_path(&entry.worktree_path, workdir))
        {
            status.insert(id, EntryStatus::NotUndoable);
            clear_undo(&mut undo, &mut status);
            clear_redo(&mut redo, &mut status);
            continue;
        }
        match &entry.role {
            EntryRole::Operation { undoable: true } => {
                status.insert(id, EntryStatus::Undoable { next: false });
                undo.push(own(id));
                clear_redo(&mut redo, &mut status);
            }
            EntryRole::Operation { undoable: false } | EntryRole::Barrier { .. } => {
                status.insert(id, EntryStatus::NotUndoable);
                clear_undo(&mut undo, &mut status);
                clear_redo(&mut redo, &mut status);
            }
            EntryRole::Continuation { origin, abort } => {
                match undo.last_mut() {
                    Some(top) if top.reverse == *origin => {
                        status.insert(id, EntryStatus::PartOf { origin: *origin });
                        if *abort {
                            if let Some(item) = undo.pop() {
                                status.insert(item.label, EntryStatus::NotUndoable);
                            }
                            clear_redo(&mut redo, &mut status);
                        } else {
                            top.expected = id;
                        }
                    }
                    _ => {
                        // Dangling origin (pruned, or another operation intervened): a barrier.
                        status.insert(id, EntryStatus::NotUndoable);
                        clear_undo(&mut undo, &mut status);
                        clear_redo(&mut redo, &mut status);
                    }
                }
            }
            EntryRole::Undo { reverses, redoable } => {
                status.insert(id, EntryStatus::Bookkeeping);
                if undo.last().is_some_and(|top| top.reverse == *reverses)
                    && let Some(item) = undo.pop()
                {
                    if *redoable {
                        status.insert(item.label, EntryStatus::Undone { next: false });
                        redo.push(StackItem {
                            reverse: id,
                            label: item.label,
                            expected: id,
                        });
                    } else {
                        status.insert(item.label, EntryStatus::Superseded);
                        clear_redo(&mut redo, &mut status);
                    }
                }
            }
            EntryRole::Redo { reverses } => {
                status.insert(id, EntryStatus::Bookkeeping);
                if redo.last().is_some_and(|top| top.reverse == *reverses)
                    && let Some(item) = redo.pop()
                {
                    status.insert(item.label, EntryStatus::Undoable { next: false });
                    undo.push(StackItem {
                        reverse: id,
                        label: item.label,
                        expected: id,
                    });
                }
            }
            EntryRole::RestoreTo { .. } => {
                status.insert(id, EntryStatus::Undoable { next: false });
                undo.push(own(id));
                clear_redo(&mut redo, &mut status);
            }
            EntryRole::Marker => {
                status.insert(id, EntryStatus::Marker);
            }
        }
    }

    let last = undo.len().checked_sub(1);
    for (idx, item) in undo.iter().enumerate() {
        status.insert(
            item.label,
            EntryStatus::Undoable {
                next: Some(idx) == last,
            },
        );
    }
    let last = redo.len().checked_sub(1);
    for (idx, item) in redo.iter().enumerate() {
        status.insert(
            item.label,
            EntryStatus::Undone {
                next: Some(idx) == last,
            },
        );
    }
    Stacks { undo, redo, status }
}

/// `true` if the `EntryMeta::worktree_path` values `a` and `b` name the same directory. Windows file systems are
/// case-insensitive and the same worktree may be opened as `C:\Repo` and `c:\repo`; that must not make a linked
/// chain look foreign.
fn same_worktree_path(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// The role of a new entry recorded by `finish()` (§4.3).
///
/// - `refs_only_head_moved`: the entry is `Scope::RefsOnly` but `HEAD^{commit}` differs between before and after.
/// - `before_in_progress`: the in-progress state of the before-snapshot.
/// - `top`: the top of the undo stack before the entry is appended, with the in-progress state of its expected
///   (after) state.
pub(crate) fn classify(
    kind: OperationKind,
    refs_only_head_moved: bool,
    before_in_progress: Option<InProgress>,
    top: Option<(StackItem, Option<InProgress>)>,
) -> EntryRole {
    if refs_only_head_moved {
        return EntryRole::Operation { undoable: false };
    }
    let Some(before) = before_in_progress else {
        return EntryRole::Operation { undoable: true };
    };
    match top {
        Some((top, Some(expected))) if expected.family() == before.family() => {
            EntryRole::Continuation {
                origin: top.reverse,
                abort: kind == OperationKind::AbortOperation,
            }
        }
        _ => EntryRole::Operation { undoable: false },
    }
}

/// Test-only access to [`replay()`] (feature `test-hooks`).
#[cfg(feature = "test-hooks")]
pub mod testing {
    use super::*;
    use crate::types::{EntryResult, Scope, UntrackedCapture};

    /// The stacks and statuses derived from a sequence of roles.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Replayed {
        /// Undo stack bottom → top as `(reverse, label, expected)`.
        pub undo: Vec<(u64, u64, u64)>,
        /// Redo stack bottom → top as `(reverse, label, expected)`.
        pub redo: Vec<(u64, u64, u64)>,
        /// Status of every entry, in input order.
        pub status: Vec<(u64, EntryStatus)>,
    }

    /// Replay entries given as `(seq, role, worktree_path)` (oldest first), as the main chain (`linked_workdir =
    /// None`) or as a linked worktree's chain.
    pub fn replay_roles(
        entries: &[(u64, EntryRole, &str)],
        linked_workdir: Option<&str>,
    ) -> Replayed {
        let metas: Vec<EntryMeta> = entries
            .iter()
            .map(|(seq, role, worktree_path)| EntryMeta {
                version: crate::meta::FORMAT_VERSION,
                seq: *seq,
                role: role.clone(),
                kind: OperationKind::Commit,
                title: format!("entry {seq}"),
                detail: None,
                scope: Scope::Full,
                started_at_ms: 0,
                finished_at_ms: 0,
                result: EntryResult::Succeeded,
                refs_changed: Vec::new(),
                before_in_progress: None,
                after_in_progress: None,
                after_captured: true,
                network: None,
                untracked: UntrackedCapture::None,
                worktree_path: (*worktree_path).to_owned(),
                app_version: String::new(),
            })
            .collect();
        let stacks = replay(&metas, linked_workdir);
        let triple = |i: &StackItem| (i.reverse.0, i.label.0, i.expected.0);
        Replayed {
            undo: stacks.undo.iter().map(triple).collect(),
            redo: stacks.redo.iter().map(triple).collect(),
            status: entries
                .iter()
                .map(|(seq, ..)| {
                    (
                        *seq,
                        stacks
                            .status
                            .get(&EntryId(*seq))
                            .copied()
                            .unwrap_or(EntryStatus::NotUndoable),
                    )
                })
                .collect(),
        }
    }

    /// The role `finish()` would give a new entry (see [`classify()`]); `top` is `(reverse, label, expected,
    /// expected_after_in_progress)`.
    pub fn classify_entry(
        kind: OperationKind,
        refs_only_head_moved: bool,
        before_in_progress: Option<InProgress>,
        top: Option<(u64, u64, u64, Option<InProgress>)>,
    ) -> EntryRole {
        classify(
            kind,
            refs_only_head_moved,
            before_in_progress,
            top.map(|(reverse, label, expected, in_progress)| {
                (
                    StackItem {
                        reverse: EntryId(reverse),
                        label: EntryId(label),
                        expected: EntryId(expected),
                    },
                    in_progress,
                )
            }),
        )
    }
}
