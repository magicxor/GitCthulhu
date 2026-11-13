//! Recovery of an interrupted restore (spec 17 §3.6): finish it, or roll it back to the pre-restore state P.
//!
//! Every step is idempotent: refs that already have the wanted value are skipped, the worktree is merged with a
//! forced base, and the index is rebuilt from the snapshot.

use bstr::ByteSlice as _;

use super::{
    Finish, RestoreKind, index, index::NewIndex, journal, journal::Journal, refs,
    refuse_leftover_locks, stash, worktree, worktree::Checkout,
};
use crate::{
    Oplog, OplogError,
    capture::capture,
    state::{CapturedState, FileState},
    types::{Recovery, Scope, UntrackedCapture},
};

/// See [`Oplog::recover()`]. Without a journal there is nothing to do.
pub(super) fn recover(
    oplog: &Oplog,
    repo: &gix::Repository,
    how: Recovery,
) -> Result<(), OplogError> {
    match journal::read(oplog, repo)? {
        journal::Read::Absent => Ok(()),
        journal::Read::Valid(journal) => match how {
            Recovery::Complete => complete(oplog, repo, journal),
            Recovery::RollBack => roll_back(oplog, repo, &journal),
        },
        journal::Read::Unreadable(reason) => match how {
            Recovery::Complete => Err(OplogError::Other(anyhow::anyhow!(
                "{reason}; the interrupted undo can only be rolled back"
            ))),
            Recovery::RollBack => {
                // Nothing is known about the restore except the index backup; put that back and forget it.
                index::restore_backup(repo, &journal::index_backup_path(oplog, repo))?;
                journal::remove(oplog, repo)
            }
        },
    }
}

/// Read a snapshot tree named by the journal.
fn read_state(repo: &gix::Repository, hex: &str) -> Result<CapturedState, OplogError> {
    let tree = journal::oid(hex)?;
    CapturedState::read_tree(repo, tree)
        .map_err(|err| OplogError::Other(err.context("reading a snapshot of the restore journal")))
}

/// Make a conflicted index conflict-free for the checkout (step C2), keeping an existing backup, which holds the
/// pre-restore index.
fn make_index_conflict_free(
    oplog: &Oplog,
    repo: &gix::Repository,
    worktree: gix::ObjectId,
) -> Result<(), OplogError> {
    if index::has_conflicts(repo)? {
        index::make_conflict_free(
            repo,
            &journal::index_backup_path(oplog, repo),
            true,
            worktree,
        )?;
    }
    Ok(())
}

/// The error of a recovery checkout that untracked or changed files block.
fn in_the_way(paths: &[bstr::BString]) -> OplogError {
    OplogError::Other(anyhow::anyhow!(
        "files changed since the interrupted restore are in the way: {}",
        paths
            .iter()
            .map(|p| format!("'{p}'"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Re-run step C3 of an interrupted restore from `pre` to `target`.
///
/// First with the journal's `base`, the carry base the restore used: the worktree may hold the checkout's result
/// already, and merging `base -> target` into it again keeps the edits the checkout carried (a forced base would
/// revert them, as `pre` holds them and `target` doesn't). If that is in the way (the user had confirmed a
/// conflict, and the checkout didn't run yet), with the forced base of `pre -> target`, as the restore did.
fn complete_worktree(
    oplog: &Oplog,
    repo: &gix::Repository,
    journal: &Journal,
    pre: &CapturedState,
    target: &CapturedState,
) -> Result<(), OplogError> {
    let (Some(pre_files), Some(target_files)) = (&pre.files, &target.files) else {
        return Ok(());
    };
    let limit = oplog.options().untracked_file_limit;
    let forced = worktree::force_base(repo, pre, target)?;
    let mut bases = Vec::with_capacity(2);
    if let Some(base) = journal.base.as_deref() {
        bases.push(journal::oid(base)?);
    }
    if !bases.contains(&forced) {
        bases.push(forced);
    }
    let mut blocked = Vec::new();
    for base in bases {
        if base == target_files.worktree {
            return Ok(());
        }
        make_index_conflict_free(oplog, repo, pre_files.worktree)?;
        match worktree::checkout_guarded(repo, target_files.worktree, base, limit)? {
            Checkout::Done => return Ok(()),
            Checkout::InTheWay(paths) => blocked = paths,
        }
    }
    Err(in_the_way(&blocked))
}

/// `Recovery::Complete`: re-run C3 (forced base) through C11 against the journal's target.
fn complete(oplog: &Oplog, repo: &gix::Repository, mut journal: Journal) -> Result<(), OplogError> {
    let kind = RestoreKind::from_journal(journal.kind, journal.side).ok_or_else(|| {
        OplogError::Other(anyhow::anyhow!(
            "the restore journal names an unknown restore kind '{}'",
            journal.kind.as_str()
        ))
    })?;
    let target = read_state(repo, &journal.target_tree)?;
    let pre = read_state(repo, &journal.pre_restore_tree)?;
    refuse_leftover_locks(repo, target.files.is_some())?;
    complete_worktree(oplog, repo, &journal, &pre, &target)?;

    let cur = capture(
        repo,
        Scope::RefsOnly,
        &UntrackedCapture::None,
        &oplog.limits(),
    )?;
    let message = format!("gitcthulhu: {} {}", kind.verb(), journal.title);
    refs::apply(repo, &cur, &target, &message)?;

    let new_index = match (&journal.new_index, &target.files) {
        (Some(tree), _) => Some(NewIndex {
            tree: journal::oid(tree)?,
            conflicts: journal
                .new_index_conflicts
                .as_deref()
                .map(journal::oid)
                .transpose()?,
        }),
        (None, Some(files)) => Some(NewIndex::of(files)),
        (None, None) => None,
    };
    let title = journal.title.clone();
    let untracked = journal.untracked.clone();
    let finish = Finish {
        oplog,
        repo,
        kind,
        reversing: crate::types::EntryId(journal.reversing),
        title: &title,
        target: &target,
        pre: &pre,
        cur: &cur,
        new_index,
        untracked: &untracked,
        quit: journal.quit,
        started_at_ms: journal.started_at_ms,
    };
    finish.run(&mut journal)?;
    Ok(())
}

/// `Recovery::RollBack`: put back P (worktree, refs guarded by their current values, stash, branch config, index).
/// No entry is appended.
///
/// The worktree is merged with `target -> pre`, the inverse of step C3: what the restore wrote goes back to P,
/// whether the checkout ran or not, and files changed since keep their changes or block. A file P doesn't have
/// that changed since the restore wrote it (or that appeared since) is kept, never deleted.
fn roll_back(oplog: &Oplog, repo: &gix::Repository, journal: &Journal) -> Result<(), OplogError> {
    let pre = read_state(repo, &journal.pre_restore_tree)?;
    let target = read_state(repo, &journal.target_tree)?;
    refuse_leftover_locks(repo, pre.files.is_some())?;
    let backup = journal::index_backup_path(oplog, repo);
    // The backup of step C2 holds P's index; one made below would hold the restore's.
    let had_backup = backup.is_file();
    if let (Some(pre_files), Some(target_files)) = (&pre.files, &target.files) {
        roll_back_worktree(oplog, repo, pre_files, target_files)?;
        if !(had_backup && index::restore_backup(repo, &backup)?) {
            journal::remove_file(&backup)?;
            index::write_restored(repo, NewIndex::of(pre_files), &pre_files.index_flags)?;
        }
    }
    let cur = capture(
        repo,
        Scope::RefsOnly,
        &UntrackedCapture::None,
        &oplog.limits(),
    )?;
    let kind =
        RestoreKind::from_journal(journal.kind, journal.side).map_or("restore", RestoreKind::verb);
    let message = format!("gitcthulhu: roll back {kind} {}", journal.title);
    refs::apply(repo, &cur, &pre, &message)?;
    stash::restore(repo, &cur, &pre)?;
    refs::restore_branch_config(repo, &cur, &pre)?;
    journal::remove_file(&backup)?;
    journal::remove(oplog, repo)
}

/// The worktree part of [`roll_back()`]: merge `target -> pre` into the worktree. Paths in the way that `pre`
/// doesn't have are files the restore wrote and that changed since, or files that appeared since: they are kept by
/// taking them out of the base, so the merge sees them as added on disk.
fn roll_back_worktree(
    oplog: &Oplog,
    repo: &gix::Repository,
    pre: &FileState,
    target: &FileState,
) -> Result<(), OplogError> {
    if pre.worktree == target.worktree {
        return Ok(());
    }
    let limit = oplog.options().untracked_file_limit;
    make_index_conflict_free(oplog, repo, target.worktree)?;
    let paths = match worktree::checkout_guarded(repo, pre.worktree, target.worktree, limit)? {
        Checkout::Done => return Ok(()),
        Checkout::InTheWay(paths) => paths,
    };
    let pre_tree = repo.find_tree(pre.worktree).map_err(anyhow::Error::from)?;
    let mut editor = repo
        .edit_tree(target.worktree)
        .map_err(anyhow::Error::from)?;
    for path in &paths {
        let in_pre = pre_tree
            .lookup_entry(path.split_str("/"))
            .map_err(anyhow::Error::from)?
            .is_some();
        if in_pre {
            return Err(in_the_way(&paths));
        }
        editor.remove(path).map_err(anyhow::Error::from)?;
    }
    let base = editor.write().map_err(anyhow::Error::from)?.detach();
    match worktree::checkout_guarded(repo, pre.worktree, base, limit)? {
        Checkout::Done => Ok(()),
        Checkout::InTheWay(paths) => Err(in_the_way(&paths)),
    }
}
