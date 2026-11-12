//! Committing and the commit message draft (WP-B6). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use crate::{
    CommitOptions, CommitOutcome, OpContext, OpOutcome, Repo, RepoExclusive, Result,
    internal::not_implemented,
};

/// `git commit`, hooks included.
pub fn commit(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &CommitOptions,
) -> Result<OpOutcome<CommitOutcome>> {
    not_implemented("commit::commit")
}

/// Persist (or with `None` delete) the message of the commit being composed.
pub fn save_draft_message(repo: &Repo, message: Option<&str>) -> Result<()> {
    not_implemented("commit::save_draft_message")
}

/// The persisted draft message, if any.
pub fn load_draft_message(repo: &Repo) -> Result<Option<String>> {
    not_implemented("commit::load_draft_message")
}
