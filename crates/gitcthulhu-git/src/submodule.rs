//! Submodules (WP-B5). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use crate::{
    CancelToken, OpContext, OpOutcome, RelaPath, Repo, RepoExclusive, Result, SubmoduleInfo,
    internal::not_implemented,
};

/// The submodules of the repository; `with_dirty` also checks whether their worktrees are dirty.
pub fn list(repo: &Repo, with_dirty: bool, cancel: &CancelToken) -> Result<Vec<SubmoduleInfo>> {
    not_implemented("submodule::list")
}

/// `git submodule add`.
pub fn add(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    url: &str,
    path: &RelaPath,
    name: Option<&str>,
    branch: Option<&str>,
) -> Result<OpOutcome<()>> {
    not_implemented("submodule::add")
}

/// `git submodule update --init`.
pub fn init_update(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    recursive: bool,
) -> Result<OpOutcome<()>> {
    not_implemented("submodule::init_update")
}

/// `git submodule update`.
pub fn update(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    recursive: bool,
) -> Result<OpOutcome<()>> {
    not_implemented("submodule::update")
}

/// `git submodule sync`.
pub fn sync(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    recursive: bool,
) -> Result<OpOutcome<()>> {
    not_implemented("submodule::sync")
}

/// `git submodule deinit`.
pub fn deinit(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    force: bool,
) -> Result<OpOutcome<()>> {
    not_implemented("submodule::deinit")
}

/// Remove a submodule completely.
pub fn remove(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    path: &RelaPath,
) -> Result<OpOutcome<()>> {
    not_implemented("submodule::remove")
}
