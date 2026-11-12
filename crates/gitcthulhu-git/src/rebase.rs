//! Rebase, plain and interactive (WP-B7). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use gix::ObjectId;

use crate::{
    OpContext, OpOutcome, RebaseOptions, RebaseOutcome, RebasePlan, Repo, RepoExclusive, Result,
    TodoLine, internal::not_implemented,
};

/// `git rebase <upstream>`.
pub fn rebase(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    upstream: &str,
    opts: &RebaseOptions,
) -> Result<OpOutcome<RebaseOutcome>> {
    not_implemented("rebase::rebase")
}

/// The plan of an interactive rebase onto `base`; read-only.
pub fn plan_interactive(repo: &Repo, base: Option<ObjectId>) -> Result<RebasePlan> {
    not_implemented("rebase::plan_interactive")
}

/// Run `git rebase -i` with the edited `plan`.
pub fn start_interactive(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    plan: &RebasePlan,
    opts: &RebaseOptions,
) -> Result<OpOutcome<RebaseOutcome>> {
    not_implemented("rebase::start_interactive")
}

/// `git rebase --continue`.
pub fn continue_rebase(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    message: Option<&str>,
) -> Result<OpOutcome<RebaseOutcome>> {
    not_implemented("rebase::continue_rebase")
}

/// `git rebase --skip`.
pub fn skip_rebase(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<RebaseOutcome>> {
    not_implemented("rebase::skip_rebase")
}

/// `git rebase --abort`.
pub fn abort_rebase(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    not_implemented("rebase::abort_rebase")
}

/// Milestone 2: replace the remaining todo of a stopped interactive rebase (`git rebase --edit-todo` equivalent).
pub fn edit_todo(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    todo: &[TodoLine],
) -> Result<OpOutcome<()>> {
    not_implemented("rebase::edit_todo")
}
