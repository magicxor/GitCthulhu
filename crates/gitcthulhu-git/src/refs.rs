//! Reading refs: branches, remotes, tags, stashes, decorations (WP-B5). Stub: bodies return `NotImplemented`
//! until the package merges.
#![allow(unused_variables)]

use gix::ObjectId;

use crate::{
    AheadBehind, CancelToken, Decorations, RefList, RefListOptions, Repo, Result, StashInfo,
    internal::not_implemented,
};

impl RefListOptions {
    /// Remotes, tags and stashes.
    pub const ALL: RefListOptions = RefListOptions {
        remotes: true,
        tags: true,
        stashes: true,
    };
}

/// All refs of the repository.
pub fn list(repo: &Repo, opts: RefListOptions) -> Result<RefList> {
    not_implemented("refs::list")
}

/// Ahead/behind per (local, upstream tracking) pair; walks at most `cap` commits per side ("999+" in the UI).
pub fn ahead_behind(
    repo: &Repo,
    pairs: &[(ObjectId, ObjectId)],
    cap: u32,
    cancel: &CancelToken,
) -> Result<Vec<AheadBehind>> {
    not_implemented("refs::ahead_behind")
}

/// The stash list, `stash@{0}` first.
pub fn stash_list(repo: &Repo) -> Result<Vec<StashInfo>> {
    not_implemented("refs::stash_list")
}

/// rev-parse like `git rev-parse --verify <spec>^{commit}`.
pub fn resolve_commit(repo: &Repo, spec: &str) -> Result<ObjectId> {
    not_implemented("refs::resolve_commit")
}

/// Is `branch` fully merged into `into` (HEAD if None)? For the "delete unmerged branch?" prompt.
pub fn is_merged(repo: &Repo, branch: ObjectId, into: Option<ObjectId>) -> Result<bool> {
    not_implemented("refs::is_merged")
}

/// Labels per commit; pure, also filled into `RefList::decorations` by `list`.
///
/// Stub: no decorations.
pub fn decorations(list: &RefList) -> Decorations {
    Decorations::default()
}
