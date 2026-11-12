//! Blame (WP-B9). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use crate::{
    BlameOptions, BlameResult, BlameRev, CancelToken, RelaPath, Repo, Result,
    internal::not_implemented,
};

/// Attribute every line of `path` at `rev` to a commit.
pub fn blame(
    repo: &Repo,
    path: &RelaPath,
    rev: BlameRev,
    opts: &BlameOptions,
    cancel: &CancelToken,
) -> Result<BlameResult> {
    not_implemented("blame::blame")
}
