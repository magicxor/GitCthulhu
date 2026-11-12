//! Forge backend calls (owned by X4 after G0b; spec 21 §2.10, §5.5). Milestone 1 ships the Git-side inputs
//! (remotes, default branch, commit messages); X4 adds the `ForgeHub` / `RepoForge` calls here behind
//! `cfg(feature = "services")`.

use super::{ForgeRemotes, GitError, ObjectId, Repo};

/// The remotes pull requests target and branches are pushed to (`remote::forge_remotes`).
pub fn forge_remotes(repo: &Repo) -> Result<ForgeRemotes, GitError> {
    gitcthulhu_git::remote::forge_remotes(repo)
}

/// The default branch of `remote` (`remote::default_branch`).
pub fn default_branch(repo: &Repo, remote: &str) -> Result<Option<String>, GitError> {
    gitcthulhu_git::remote::default_branch(repo, remote)
}

/// Full messages of `base..head`, oldest first, at most `limit` (`log::commit_messages`).
pub fn commit_messages(
    repo: &Repo,
    base: Option<ObjectId>,
    head: ObjectId,
    limit: usize,
) -> Result<Vec<String>, GitError> {
    gitcthulhu_git::log::commit_messages(repo, base, head, limit)
}

/// The number of connected forge accounts (no network, no keychain).
#[cfg(feature = "services")]
pub fn account_count(services: &super::services::ServicesHandle) -> Result<usize, String> {
    services
        .services()
        .forge
        .accounts()
        .map(|accounts| accounts.len())
        .map_err(|e| e.to_string())
}
