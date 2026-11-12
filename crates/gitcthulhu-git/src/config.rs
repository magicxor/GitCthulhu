//! Reading and writing Git configuration (WP-B5). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use std::path::Path;

use crate::{
    ConfigScope, GitRuntime, Identity, IdentityInfo, OpContext, OpOutcome, PullMode, RefreshSet,
    Repo, RepoExclusive, Result, SigningConfig, internal::not_implemented,
};

/// The configured identities and the one `git commit` would use.
pub fn identity(repo: Option<&Repo>) -> Result<IdentityInfo> {
    not_implemented("config::identity")
}

/// Set or (with `None`) unset `user.name` / `user.email` in `scope`.
pub fn set_identity(
    repo: Option<&Repo>,
    scope: ConfigScope,
    identity: Option<&Identity>,
) -> Result<RefreshSet> {
    not_implemented("config::set_identity")
}

/// A configuration value.
pub fn get(repo: &Repo, key: &str) -> Result<Option<String>> {
    not_implemented("config::get")
}

/// A boolean configuration value.
pub fn get_bool(repo: &Repo, key: &str) -> Result<Option<bool>> {
    not_implemented("config::get_bool")
}

/// Set a configuration value; `None` => unset.
pub fn set(
    repo: Option<&Repo>,
    scope: ConfigScope,
    key: &str,
    value: Option<&str>,
) -> Result<RefreshSet> {
    not_implemented("config::set")
}

/// The effective signing configuration.
pub fn signing(repo: &Repo) -> Result<SigningConfig> {
    not_implemented("config::signing")
}

/// `pull.rebase` / `pull.ff` / `branch.<b>.rebase`.
pub fn pull_mode_default(repo: &Repo) -> Result<PullMode> {
    not_implemented("config::pull_mode_default")
}

/// `git config --global --add safe.directory <path>` (UnsafeRepository recovery).
pub fn add_safe_directory(rt: &GitRuntime, path: &Path) -> Result<()> {
    not_implemented("config::add_safe_directory")
}

/// Whether a commit-graph file exists.
///
/// Stub: `false`.
pub fn commit_graph_present(repo: &Repo) -> bool {
    false
}

/// `git commit-graph write --reachable --changed-paths` (offered once for large repos, §9).
pub fn write_commit_graph(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    not_implemented("config::write_commit_graph")
}
