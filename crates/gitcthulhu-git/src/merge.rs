//! Merge, cherry-pick, revert, reset and operation control (WP-B7). Stub: bodies return `NotImplemented` until the
//! package merges.
#![allow(unused_variables)]

use gix::ObjectId;

use crate::{
    ContinueOutcome, MergeOptions, MergeOutcome, OpContext, OpOutcome, PickOptions, Repo,
    RepoExclusive, ResetMode, Result, SequenceOutcome, internal::not_implemented,
};

/// `git merge`.
pub fn merge(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &MergeOptions,
) -> Result<OpOutcome<MergeOutcome>> {
    not_implemented("merge::merge")
}

/// `git cherry-pick`.
pub fn cherry_pick(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    commits: &[ObjectId],
    opts: PickOptions,
) -> Result<OpOutcome<SequenceOutcome>> {
    not_implemented("merge::cherry_pick")
}

/// `git revert`.
pub fn revert(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    commits: &[ObjectId],
    opts: PickOptions,
) -> Result<OpOutcome<SequenceOutcome>> {
    not_implemented("merge::revert")
}

/// `git reset`.
pub fn reset(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    target: ObjectId,
    mode: ResetMode,
) -> Result<OpOutcome<()>> {
    not_implemented("merge::reset")
}

/// Dispatch on the current state: merge / cherry-pick / revert / rebase / am. `message` (if Some) is written to
/// MERGE_MSG (merge, cherry-pick, revert) before continuing.
pub fn continue_operation(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    message: Option<&str>,
) -> Result<OpOutcome<ContinueOutcome>> {
    not_implemented("merge::continue_operation")
}

/// Abort the operation in progress.
pub fn abort_operation(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    not_implemented("merge::abort_operation")
}

/// Skip the current step of the operation in progress.
pub fn skip_operation(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<ContinueOutcome>> {
    not_implemented("merge::skip_operation")
}
