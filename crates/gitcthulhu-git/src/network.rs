//! Clone, fetch, pull and push (WP-B8). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use std::sync::Arc;

use crate::{
    CloneOptions, FetchOptions, FetchReport, ForceMode, GitRuntime, MergeOutcomeOrRebase,
    OpContext, OpOutcome, PullOptions, PushOptions, PushReport, Repo, RepoExclusive, Result,
    TagPush, internal::not_implemented,
};

/// `git clone`.
pub fn clone(
    rt: &Arc<GitRuntime>,
    ctx: &OpContext,
    opts: &CloneOptions,
) -> Result<OpOutcome<Repo>> {
    not_implemented("network::clone")
}

/// No `perm`: writes only refs/remotes/*, FETCH_HEAD (and refs named by explicit `refspecs`) under Git's own locks
/// (§1.5).
pub fn fetch(repo: &Repo, ctx: &OpContext, opts: &FetchOptions) -> Result<OpOutcome<FetchReport>> {
    not_implemented("network::fetch")
}

/// `git pull`.
pub fn pull(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &PullOptions,
) -> Result<OpOutcome<MergeOutcomeOrRebase>> {
    not_implemented("network::pull")
}

/// The push family takes no `perm` (§1.5). Rejections: Err(NonFastForward | LeaseRejected | RemoteRejected) carrying
/// the full `PushReport` (refs that did update included).
pub fn push(repo: &Repo, ctx: &OpContext, opts: &PushOptions) -> Result<OpOutcome<PushReport>> {
    not_implemented("network::push")
}

/// Gitnuro "Push" button: current branch to its upstream (or `<default remote>/<same name>` with -u when none).
pub fn push_current_branch(
    repo: &Repo,
    ctx: &OpContext,
    force: ForceMode,
    tags: TagPush,
    skip_hooks: bool,
) -> Result<OpOutcome<PushReport>> {
    not_implemented("network::push_current_branch")
}

/// Delete a branch on the remote.
pub fn delete_remote_branch(
    repo: &Repo,
    ctx: &OpContext,
    remote: &str,
    branch: &str,
) -> Result<OpOutcome<PushReport>> {
    not_implemented("network::delete_remote_branch")
}

/// Push one tag.
pub fn push_tag(
    repo: &Repo,
    ctx: &OpContext,
    remote: &str,
    tag: &str,
) -> Result<OpOutcome<PushReport>> {
    not_implemented("network::push_tag")
}

/// Delete a tag on the remote.
pub fn delete_remote_tag(
    repo: &Repo,
    ctx: &OpContext,
    remote: &str,
    tag: &str,
) -> Result<OpOutcome<PushReport>> {
    not_implemented("network::delete_remote_tag")
}
