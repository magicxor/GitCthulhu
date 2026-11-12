//! Working tree status (WP-B1). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use crate::{
    CancelToken, FileStamp, RenameMode, Repo, RepoExclusive, Result, Status, StatusOptions,
    SubmoduleStatusMode, UntrackedMode, internal::not_implemented,
};

impl Default for StatusOptions {
    fn default() -> Self {
        StatusOptions {
            untracked: UntrackedMode::Files,
            renames: RenameMode::IndexAndWorktree { limit: 1000 },
            submodules: SubmoduleStatusMode::HeadOnly,
            pathspec: Vec::new(),
        }
    }
}

/// Staged and unstaged changes, computed in-process with gix. Never writes the index.
pub fn status(repo: &Repo, opts: &StatusOptions, cancel: &CancelToken) -> Result<Status> {
    not_implemented("status::status")
}

/// `git update-index -q --refresh --ignore-submodules` (CommandKind::Mutation, no hooks, no progress). Git takes
/// `index.lock` *before* reading the index, so staging done concurrently by a terminal or IDE is never lost. Returns
/// the new `index` FileStamp (the GUI passes it to `RepoWatcher::ignore_index_stamp`, §7.3). Lock held by another
/// git => Ok(None), silently.
pub fn refresh_index(
    repo: &Repo,
    perm: &mut RepoExclusive,
    cancel: &CancelToken,
) -> Result<Option<FileStamp>> {
    not_implemented("status::refresh_index")
}
