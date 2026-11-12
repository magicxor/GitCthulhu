//! Remote management and forge inputs (WP-B5). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use crate::{
    ForgeRemotes, OpContext, OpOutcome, Repo, RepoExclusive, Result, internal::not_implemented,
};

/// `git remote add`.
pub fn add_remote(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
    fetch_url: &str,
    push_url: Option<&str>,
) -> Result<OpOutcome<()>> {
    not_implemented("remote::add_remote")
}

/// Change the URLs of a remote.
pub fn set_remote_urls(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
    fetch_url: &str,
    push_url: Option<&str>,
) -> Result<OpOutcome<()>> {
    not_implemented("remote::set_remote_urls")
}

/// `git remote rename`.
pub fn rename_remote(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    old: &str,
    new: &str,
) -> Result<OpOutcome<()>> {
    not_implemented("remote::rename_remote")
}

/// `git remote remove`.
pub fn remove_remote(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
) -> Result<OpOutcome<()>> {
    not_implemented("remote::remove_remote")
}

/// The remotes the forge facade works with (rule of 15 §6.1). Pure read.
pub fn forge_remotes(repo: &Repo) -> Result<ForgeRemotes> {
    not_implemented("remote::forge_remotes")
}

/// Short name of the remote's default branch: refs/remotes/<r>/HEAD target, else "main"/"master" if present.
pub fn default_branch(repo: &Repo, remote: &str) -> Result<Option<String>> {
    not_implemented("remote::default_branch")
}
