//! Branches and checkout (WP-B6). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use gix::{ObjectId, refs::FullName};

use crate::{
    CheckoutOptions, OpContext, OpOutcome, Repo, RepoExclusive, Result, StartPoint,
    internal::not_implemented,
};

/// Switch to a local branch.
pub fn checkout_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
    opts: CheckoutOptions,
) -> Result<OpOutcome<()>> {
    not_implemented("branch::checkout_branch")
}

/// Remote-tracking ref -> local tracking branch. If a local branch already tracks it, check that one out.
pub fn checkout_remote_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    remote_ref: &FullName,
    local_name: Option<&str>,
    opts: CheckoutOptions,
) -> Result<OpOutcome<FullName>> {
    not_implemented("branch::checkout_remote_branch")
}

/// Detach HEAD at a commit.
pub fn checkout_detached(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    commit: ObjectId,
    opts: CheckoutOptions,
) -> Result<OpOutcome<()>> {
    not_implemented("branch::checkout_detached")
}

/// Create a branch, optionally checking it out.
pub fn create_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
    start: &StartPoint,
    checkout: bool,
) -> Result<OpOutcome<FullName>> {
    not_implemented("branch::create_branch")
}

/// Rename a branch.
pub fn rename_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
    new_name: &str,
) -> Result<OpOutcome<FullName>> {
    not_implemented("branch::rename_branch")
}

/// Delete a branch.
pub fn delete_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
    force: bool,
) -> Result<OpOutcome<()>> {
    not_implemented("branch::delete_branch")
}

/// Set (or with `None` unset) the upstream of a branch.
pub fn set_upstream(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
    upstream: Option<&FullName>,
) -> Result<OpOutcome<()>> {
    not_implemented("branch::set_upstream")
}

/// Pure check with `gix::validate::reference::name` + "refs/heads/" prefix; also rejects an existing branch.
pub fn validate_branch_name(repo: &Repo, name: &str) -> Result<FullName> {
    not_implemented("branch::validate_branch_name")
}
