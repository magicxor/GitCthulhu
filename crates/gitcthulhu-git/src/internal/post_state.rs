//! Repository state right after a failed command (WP-B0b).
//!
//! Stub: `read` is implemented by WP-B0b.

use crate::{GitError, PostState, Repo, Result};

/// `gix::Repository::state()`, one index scan for entries with stage != 0, HEAD's short branch name and the id in
/// `CHERRY_PICK_HEAD` / `REVERT_HEAD`.
pub(crate) fn read(repo: &Repo) -> Result<PostState> {
    let _ = repo;
    Err(GitError::NotImplemented {
        op: "internal::post_state::read",
    })
}
