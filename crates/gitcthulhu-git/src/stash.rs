//! Stashes (WP-B6). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use gix::ObjectId;

use crate::{
    OpContext, OpOutcome, RelaPath, Repo, RepoExclusive, Result, StashApplyOptions,
    StashApplyResult, StashPushOptions, internal::not_implemented,
};

/// `git stash push`; `None` when there was nothing to stash.
pub fn stash_push(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &StashPushOptions,
) -> Result<OpOutcome<Option<ObjectId>>> {
    not_implemented("stash::stash_push")
}

/// `git stash apply`.
pub fn stash_apply(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    stash: ObjectId,
    opts: StashApplyOptions,
) -> Result<OpOutcome<StashApplyResult>> {
    not_implemented("stash::stash_apply")
}

/// `git stash pop`.
pub fn stash_pop(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    stash: ObjectId,
    opts: StashApplyOptions,
) -> Result<OpOutcome<StashApplyResult>> {
    not_implemented("stash::stash_pop")
}

/// `git stash drop`.
pub fn stash_drop(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    stash: ObjectId,
) -> Result<OpOutcome<()>> {
    not_implemented("stash::stash_drop")
}

/// `git stash clear`.
pub fn stash_clear(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    not_implemented("stash::stash_clear")
}

/// Read: paths stored in the stash's untracked-files commit (parent 3), empty if none. The GUI passes them as
/// `UntrackedCapture::Paths` when recording `StashApply`/`StashPop` (17 §1.3), so undo removes them again.
pub fn untracked_paths(repo: &Repo, stash: ObjectId) -> Result<Vec<RelaPath>> {
    not_implemented("stash::untracked_paths")
}
