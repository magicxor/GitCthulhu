//! Staging, unstaging, discarding, conflict resolution (WP-B3). Stub: bodies return `NotImplemented` until the
//! package merges.
#![allow(unused_variables)]

use crate::{
    DiscardMode, OpContext, OpOutcome, RelaPath, Repo, RepoExclusive, Resolution, Result,
    SelectionRequest, internal::not_implemented,
};

/// `git add` of whole files.
pub fn stage_paths(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
) -> Result<OpOutcome<()>> {
    not_implemented("stage::stage_paths")
}

/// `git add -A`.
pub fn stage_all(repo: &Repo, perm: &mut RepoExclusive, ctx: &OpContext) -> Result<OpOutcome<()>> {
    not_implemented("stage::stage_all")
}

/// Unstage whole files.
pub fn unstage_paths(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
) -> Result<OpOutcome<()>> {
    not_implemented("stage::unstage_paths")
}

/// Unstage everything.
pub fn unstage_all(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    not_implemented("stage::unstage_all")
}

/// Stage selected hunks or lines.
pub fn stage_selection(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    req: &SelectionRequest,
) -> Result<OpOutcome<()>> {
    not_implemented("stage::stage_selection")
}

/// Unstage selected hunks or lines.
pub fn unstage_selection(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    req: &SelectionRequest,
) -> Result<OpOutcome<()>> {
    not_implemented("stage::unstage_selection")
}

/// Throw away changes of whole files.
pub fn discard_paths(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    mode: DiscardMode,
) -> Result<OpOutcome<()>> {
    not_implemented("stage::discard_paths")
}

/// Throw away selected hunks or lines.
pub fn discard_selection(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    req: &SelectionRequest,
) -> Result<OpOutcome<()>> {
    not_implemented("stage::discard_selection")
}

/// Delete untracked files.
pub fn delete_untracked(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
) -> Result<OpOutcome<()>> {
    not_implemented("stage::delete_untracked")
}

/// Resolve conflicted paths.
pub fn resolve_conflict(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    how: Resolution,
) -> Result<OpOutcome<()>> {
    not_implemented("stage::resolve_conflict")
}
