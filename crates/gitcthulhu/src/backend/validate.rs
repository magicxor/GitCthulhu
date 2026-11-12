//! Validation and app-level backend calls of the dialogs (owned by G10 after G0b; spec 21 §2.4).

use std::sync::Arc;

use super::{CloneOptions, FullName, GitError, GitRuntime, HelperStatus, ObjectId, OpContext};
use super::{OpOutcome, Repo};

/// The full name a new branch called `name` would get, or `InvalidName` / `AlreadyExists`.
pub fn validate_branch_name(repo: &Repo, name: &str) -> Result<FullName, GitError> {
    gitcthulhu_git::branch::validate_branch_name(repo, name)
}

/// The full name a new tag called `name` would get, or `InvalidName` / `AlreadyExists`.
pub fn validate_tag_name(repo: &Repo, name: &str) -> Result<FullName, GitError> {
    gitcthulhu_git::tag::validate_tag_name(repo, name)
}

/// Resolves a revision (`main~2`, a hash prefix) to a commit.
pub fn resolve_commit(repo: &Repo, spec: &str) -> Result<ObjectId, GitError> {
    gitcthulhu_git::refs::resolve_commit(repo, spec)
}

/// Where the helper programs are and whether they exist (About, `AuthFailed` dialogs).
pub fn helper_status(rt: &GitRuntime) -> HelperStatus {
    rt.helper_status()
}

/// `network::clone` (app-level, with its own progress dialog; not a tab operation).
pub fn clone_repository(
    rt: &Arc<GitRuntime>,
    ctx: &OpContext,
    opts: &CloneOptions,
) -> Result<OpOutcome<Repo>, GitError> {
    gitcthulhu_git::network::clone(rt, ctx, opts)
}
