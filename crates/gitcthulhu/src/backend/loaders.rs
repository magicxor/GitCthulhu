//! Read wrappers with the GUI's defaults, used by the category loaders of `tab/loader.rs` (spec 21 §2.4, §2.6).
//! All blocking; call them inside `backend::bg` / `ReadSlot`.

use std::time::Duration;

use super::handle::RepoHandle;
use super::{
    AheadBehind, CancelToken, ConfigScope, FileStamp, GitError, IdentityInfo, ObjectId, RefList,
    RefListOptions, RefreshSet, RelaPath, RenameMode, Repo, RepoState, Status, StatusOptions,
    SubmoduleInfo, WorktreeInfo,
};

/// Ahead/behind walks stop after this many commits per side ("999+" style display).
pub const AHEAD_BEHIND_CAP: u32 = 10_000;

/// `status::status` with `StatusOptions::default()` but staged renames only (`RenameMode::IndexOnly`, like
/// `git status`): tracking worktree renames repeats the index-worktree half and hashes every untracked file whenever
/// a tracked file is deleted, seconds per watcher tick in a large repository. A bare repository has no working tree
/// and so no changes: its status is empty rather than the `BareRepository` error, which would show an Error toast on
/// every reload.
pub fn status(repo: &Repo, cancel: &CancelToken) -> Result<Status, GitError> {
    if repo.workdir().is_none() {
        return Ok(Status {
            staged: Vec::new(),
            unstaged: Vec::new(),
            counts: Default::default(),
            fingerprint: gitcthulhu_git::StatusFingerprint {
                index: None,
                head: None,
            },
            index_needs_refresh: false,
        });
    }
    let opts = StatusOptions {
        renames: RenameMode::IndexOnly,
        ..StatusOptions::default()
    };
    gitcthulhu_git::status::status(repo, &opts, cancel)
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

/// Whether `id` is reachable from a local branch, a remote-tracking branch or a tag: the question before a
/// detached worktree is removed (Git drops its HEAD reflog with it, so an unreachable commit is lost for good).
pub fn commit_on_a_ref(repo: &Repo, id: ObjectId) -> Result<bool, GitError> {
    let list = gitcthulhu_git::refs::list(
        repo,
        RefListOptions {
            remotes: true,
            tags: true,
            stashes: false,
        },
    )?;
    let mut tips: Vec<ObjectId> = list.local.iter().map(|b| b.target).collect();
    tips.extend(
        list.remotes
            .iter()
            .flat_map(|r| r.branches.iter().map(|b| b.target)),
    );
    tips.extend(
        list.tags
            .iter()
            .filter(|t| t.peeled_kind == gix::object::Kind::Commit)
            .map(|t| t.target),
    );
    tips.sort();
    tips.dedup();
    // One walk for all tips: a merge base per tip would take minutes in a repository with thousands of tags, and
    // the confirmation waits for this answer.
    gitcthulhu_git::refs::is_reachable_from(repo, id, &tips)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ChangeKind;
    use crate::test_support::fixture::Fixture;

    /// A deleted tracked file next to an untracked copy stays a deletion plus an untracked file (the costly worktree
    /// rename pass does not run); a staged rename is still found.
    #[test]
    fn status_tracks_staged_renames_only() {
        let fx = Fixture::new();
        fx.write("old.txt", "the same content\n");
        fx.write("moved.txt", "staged rename\n");
        fx.commit_all("base");
        std::fs::remove_file(fx.path().join("old.txt")).expect("removable");
        fx.write("copy.txt", "the same content\n");
        fx.git(&["mv", "moved.txt", "renamed.txt"]);
        let repo = Repo::open(&fx.runtime(), fx.path()).expect("the fixture opens");
        let status = status(&repo, &CancelToken::new()).expect("status");
        let unstaged: Vec<_> = status
            .unstaged
            .iter()
            .map(|e| (e.path.to_string(), e.kind))
            .collect();
        assert_eq!(
            unstaged,
            [
                ("copy.txt".to_owned(), ChangeKind::Untracked),
                ("old.txt".to_owned(), ChangeKind::Deleted),
            ],
            "no worktree rename"
        );
        let staged: Vec<_> = status
            .staged
            .iter()
            .map(|e| (e.path.to_string(), e.kind))
            .collect();
        assert_eq!(
            staged,
            [("renamed.txt".to_owned(), ChangeKind::Renamed)],
            "the staged rename is still tracked"
        );
    }
}
