//! Read wrappers with the GUI's defaults, used by the category loaders of `tab/loader.rs` (spec 21 §2.4, §2.6).
//! All blocking; call them inside `backend::bg` / `ReadSlot`.

use std::time::Duration;

use super::handle::RepoHandle;
use super::{
    AheadBehind, CancelToken, ConfigScope, FileStamp, GitError, IdentityInfo, ObjectId, RefList,
    RefListOptions, RefreshSet, RelaPath, Repo, RepoState, Status, StatusOptions, SubmoduleInfo,
    WorktreeInfo,
};

/// Ahead/behind walks stop after this many commits per side ("999+" style display).
pub const AHEAD_BEHIND_CAP: u32 = 10_000;

/// `status::status` with `StatusOptions::default()`.
pub fn status(repo: &Repo, cancel: &CancelToken) -> Result<Status, GitError> {
    gitcthulhu_git::status::status(repo, &StatusOptions::default(), cancel)
}

/// `state::repo_state`.
pub fn repo_state(repo: &Repo) -> Result<RepoState, GitError> {
    gitcthulhu_git::state::repo_state(repo)
}

/// `commit::load_draft_message`.
pub fn load_draft(repo: &Repo) -> Result<Option<String>, GitError> {
    gitcthulhu_git::commit::load_draft_message(repo)
}

/// `refs::list` with every namespace (remotes, tags, stashes).
pub fn refs(repo: &Repo) -> Result<RefList, GitError> {
    gitcthulhu_git::refs::list(
        repo,
        RefListOptions {
            remotes: true,
            tags: true,
            stashes: true,
        },
    )
}

/// `refs::ahead_behind` for (local, tracking) pairs, capped at [`AHEAD_BEHIND_CAP`].
pub fn ahead_behind(
    repo: &Repo,
    pairs: &[(ObjectId, ObjectId)],
    cancel: &CancelToken,
) -> Result<Vec<AheadBehind>, GitError> {
    gitcthulhu_git::refs::ahead_behind(repo, pairs, AHEAD_BEHIND_CAP, cancel)
}

/// `submodule::list` without the dirty check.
pub fn submodules(repo: &Repo, cancel: &CancelToken) -> Result<Vec<SubmoduleInfo>, GitError> {
    gitcthulhu_git::submodule::list(repo, false, cancel)
}

/// `worktree::list`.
pub fn worktrees(repo: &Repo) -> Result<Vec<WorktreeInfo>, GitError> {
    gitcthulhu_git::worktree::list(repo)
}

/// `config::identity` (global only when `repo` is `None`).
pub fn identity(repo: Option<&Repo>) -> Result<IdentityInfo, GitError> {
    gitcthulhu_git::config::identity(repo)
}

/// `Repo::reload` (re-read the configuration from disk).
pub fn reload_config(repo: &Repo) -> Result<(), GitError> {
    repo.reload()
}

/// `stash::untracked_paths`: the untracked files a stash entry would restore.
pub fn stash_untracked_paths(repo: &Repo, stash: ObjectId) -> Result<Vec<RelaPath>, GitError> {
    gitcthulhu_git::stash::untracked_paths(repo, stash)
}

/// `config::set` (no guard needed: a config write is atomic). Returns the refresh set (`CONFIG`).
pub fn config_set(
    repo: &Repo,
    scope: ConfigScope,
    key: &str,
    value: Option<&str>,
) -> Result<RefreshSet, GitError> {
    gitcthulhu_git::config::set(Some(repo), scope, key, value)
}

/// 20 §2.5: refresh the index stat data only when the guard is free right now (`try_lock(ZERO)`); `Ok(None)`
/// without running git when it is held or when another git holds `index.lock`.
pub fn refresh_index(
    handle: &RepoHandle,
    cancel: &CancelToken,
) -> Result<Option<FileStamp>, GitError> {
    let Some(mut guard) = handle.try_lock(Duration::ZERO) else {
        return Ok(None);
    };
    gitcthulhu_git::status::refresh_index(&handle.repo, guard.write_permission(), cancel)
}
