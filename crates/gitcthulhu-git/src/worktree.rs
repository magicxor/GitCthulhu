//! Linked worktrees (WP-B5, D0.7). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use std::path::Path;

use crate::{
    OpContext, OpOutcome, Repo, RepoExclusive, Result, WorktreeAddOptions, WorktreeInfo,
    internal::not_implemented,
};

/// All worktrees: the main one (`id = None`) and the linked ones. gix: `repo.main_repo()?.worktrees()`.
pub fn list(repo: &Repo) -> Result<Vec<WorktreeInfo>> {
    not_implemented("worktree::list")
}

/// `git worktree add`.
pub fn add(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &WorktreeAddOptions,
) -> Result<OpOutcome<WorktreeInfo>> {
    not_implemented("worktree::add")
}

/// Returns the id (directory name under `<common>/worktrees/`) of the removed worktree. The GUI then calls
/// `gitcthulhu_oplog::Oplog::clear_linked_worktree(repo, id, perm)` (17 §4.1) so its undo chain does not live on.
pub fn remove(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    path: &Path,
    force: bool,
) -> Result<OpOutcome<String>> {
    not_implemented("worktree::remove")
}

/// Returns the ids of pruned worktrees (ids listed by gix before minus after); the GUI clears their oplog chains.
pub fn prune(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<Vec<String>>> {
    not_implemented("worktree::prune")
}

/// `git worktree lock`.
pub fn lock(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    path: &Path,
    reason: Option<&str>,
) -> Result<OpOutcome<()>> {
    not_implemented("worktree::lock")
}

/// `git worktree unlock`.
pub fn unlock(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    path: &Path,
) -> Result<OpOutcome<()>> {
    not_implemented("worktree::unlock")
}
