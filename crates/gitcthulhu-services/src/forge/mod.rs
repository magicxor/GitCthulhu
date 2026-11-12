//! Forge integrations (GitHub, GitLab, Bitbucket). 20 section 8.3 (= 15 section 6.2 with the 8.3 adjustments).
//!
//! Stub: the implementation is owned by WP-S1. The full facade surface is declared so callers can be written
//! against it now. Every fallible method returns [`ForgeError::NotImplemented`]; methods without an error
//! channel return an empty answer (`None`, an empty list, no capabilities).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use but_secret::Sensitive;
use gitcthulhu_git::{CancelToken, ForgeRemotes};

use crate::Runtime;

pub use but_forge::{
    CiCheck, ForgeAccountValidity, ForgeCapabilities, ForgeInfo, ForgeName, ForgeReview, ForgeUser,
    RepoInfo, ReviewAssociation, ReviewMergeMethod, ReviewMergeStatus, ReviewUpdatePayload,
};

/// Directories used by the forge storage.
#[derive(Debug, Clone)]
pub struct ForgeDirs {
    /// `.../gitcthulhu`.
    pub app_data: PathBuf,
    /// The cache directory.
    pub cache: PathBuf,
}

/// Static forge configuration.
#[derive(Debug, Clone)]
pub struct ForgeConfig {
    /// GitCthulhu's own GitHub OAuth app; `None` means personal access tokens only (15 §0.3).
    pub github_oauth_client_id: Option<String>,
    /// The HTTP user agent, `"GitCthulhu/<version>"`.
    pub user_agent: String,
}

/// A signed-in forge account (filled in by WP-S1).
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    /// Which forge.
    pub forge: ForgeName,
    /// The account identity (includes the host for Enterprise/self-hosted accounts).
    pub user: ForgeUser,
    /// A display label.
    pub label: String,
    /// The avatar, if the forge reports one.
    pub avatar_url: Option<String>,
}

/// Per-repository forge preferences, stored in app data (15 §6.1).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RepoForgePrefs {
    /// Force a forge for self-hosted installations.
    pub forge_override: Option<ForgeName>,
    /// The account to prefer when several match the host.
    pub preferred_user: Option<ForgeUser>,
    /// A pull-request template path, relative to the worktree.
    pub template_path: Option<String>,
}

/// What the GUI hands to [`ForgeHub::open_repo`].
#[derive(Debug, Clone)]
pub struct RepoForgeInput {
    /// The repository directory.
    pub repo_dir: PathBuf,
    /// Base and push remotes from `gitcthulhu_git::remote::forge_remotes`.
    pub remotes: ForgeRemotes,
    /// Per-repository preferences.
    pub prefs: RepoForgePrefs,
}

/// Whether an account matches the repository's forge (no network, no keychain).
#[derive(Debug, Clone, PartialEq)]
pub enum RepoAccountStatus {
    /// This account will be used.
    Matching(Account),
    /// The repository is on a forge, but no account matches its host.
    NoMatchingAccount,
    /// The remote does not belong to a recognised forge.
    NotAForge,
}

/// The device-flow code of a GitHub sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    /// The code to enter in the browser.
    pub user_code: String,
    /// The code to poll with.
    pub device_code: String,
    /// Where the user enters `user_code`.
    pub verification_uri: String,
    /// The minimum polling interval in seconds.
    pub interval: u64,
    /// Seconds until the code expires.
    pub expires_in: u64,
}

/// Result of polling a device flow.
#[derive(Debug, Clone, PartialEq)]
pub enum DevicePoll {
    /// The user has not confirmed yet.
    Pending,
    /// Signed in.
    SignedIn(Account),
}

/// Outcome of a review refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshOutcome {
    /// A fingerprint of the cached review list; unchanged means nothing to reload.
    pub fingerprint: u64,
    /// Unix time of the most recent merge among the cached reviews.
    pub last_merged: Option<i64>,
}

/// Parameters of [`RepoForge::create_review`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateReview {
    /// The title.
    pub title: String,
    /// The body.
    pub body: String,
    /// The branch with the changes.
    pub source_branch: String,
    /// The branch to merge into.
    pub target_branch: String,
    /// Open as draft.
    pub draft: bool,
}

/// The conversation of a pull request (empty for GitLab/Bitbucket; filled in by WP-S1).
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    /// Conversation comments.
    pub comments: Vec<but_forge::ForgeReviewComment>,
    /// Review threads on code.
    pub threads: Vec<but_forge::ForgeReviewThread>,
    /// Review submissions (approve, request changes, comment).
    pub submissions: Vec<but_forge::ForgeReviewSubmission>,
    /// Reactions on the description.
    pub reactions: Vec<but_forge::ForgeReviewReaction>,
}

/// Counts of CI check states (port of `summarize_checks`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChecksSummary {
    /// Passed, skipped or neutral.
    pub passed: usize,
    /// In progress.
    pub running: usize,
    /// Failed, timed out, cancelled or needing action.
    pub failed: usize,
    /// Waiting for a runner.
    pub queued: usize,
}

/// How to check a pull request out locally (20 §8.3 describes how the GUI executes it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    /// The remote to add if it does not exist yet.
    pub remote_name: String,
    /// Its URL (SSH or HTTPS like the base remote).
    pub remote_url: String,
    /// The refspec to fetch.
    pub fetch_refspec: String,
    /// The suggested local branch name.
    pub local_branch: String,
}

/// Errors of the forge facade.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ForgeError {
    /// No account for this forge or host.
    #[error("not authenticated")]
    NotAuthenticated,
    /// The remote does not belong to a supported forge.
    #[error("forge not recognized")]
    Unrecognized,
    /// Transport failure.
    #[error("network error: {0}")]
    Network(String),
    /// The API rate limit was hit; `reset_at` is a unix timestamp if known.
    #[error("rate limited")]
    RateLimited {
        /// Unix time when the limit resets.
        reset_at: Option<i64>,
    },
    /// Token expired, revoked or otherwise invalid.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The token lacks a permission.
    #[error("permission denied: {0}")]
    Permission(String),
    /// The resource does not exist.
    #[error("not found")]
    NotFound,
    /// The operation was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// The operation is not implemented in this skeleton (WP-S1 removes this variant).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// Anything else.
    #[error(transparent)]
    Other(anyhow::Error),
}

impl ForgeError {
    /// Whether the failure may pass by itself, so asking again later is worth it (port of `may_pass`).
    pub fn may_pass(&self) -> bool {
        matches!(
            self,
            ForgeError::Network(_) | ForgeError::RateLimited { .. }
        )
    }

    /// Whether polling should stop until the user fixes the account setup.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ForgeError::NotAuthenticated
                | ForgeError::Unrecognized
                | ForgeError::Auth(_)
                | ForgeError::Permission(_)
        )
    }
}

fn not_implemented<T>(what: &'static str) -> Result<T, ForgeError> {
    Err(ForgeError::NotImplemented(what))
}

/// Process-wide forge entry point. Cheap to clone.
#[derive(Clone)]
pub struct ForgeHub {
    #[allow(dead_code)]
    rt: Runtime,
    #[allow(dead_code)]
    dirs: ForgeDirs,
    #[allow(dead_code)]
    cfg: ForgeConfig,
}

impl ForgeHub {
    /// Create the hub on the shared runtime.
    pub fn new(rt: Runtime, dirs: ForgeDirs, cfg: ForgeConfig) -> Result<Self, ForgeError> {
        Ok(ForgeHub { rt, dirs, cfg })
    }

    /// All signed-in accounts.
    pub fn accounts(&self) -> Result<Vec<Account>, ForgeError> {
        not_implemented("forge::accounts")
    }

    /// The account to use for HTTPS requests to `host` (20 §6.3 optional HTTPS answer).
    pub fn account_for_host(&self, _host: &str) -> Option<Account> {
        None
    }

    /// Start a GitHub device-flow sign-in (needs `ForgeConfig::github_oauth_client_id`).
    pub fn github_device_start(&self) -> Result<DeviceCode, ForgeError> {
        not_implemented("forge::github_device_start")
    }

    /// Poll a GitHub device flow.
    pub fn github_device_poll(
        &self,
        _code: &DeviceCode,
        _cancel: &CancelToken,
    ) -> Result<DevicePoll, ForgeError> {
        not_implemented("forge::github_device_poll")
    }

    /// Sign in to github.com with a personal access token.
    pub fn add_github_pat(&self, _token: Sensitive<String>) -> Result<Account, ForgeError> {
        not_implemented("forge::add_github_pat")
    }

    /// Sign in to a GitHub Enterprise instance.
    pub fn add_github_enterprise(
        &self,
        _api_url: &str,
        _token: Sensitive<String>,
    ) -> Result<Account, ForgeError> {
        not_implemented("forge::add_github_enterprise")
    }

    /// Sign in to gitlab.com with a personal access token.
    pub fn add_gitlab_pat(&self, _token: Sensitive<String>) -> Result<Account, ForgeError> {
        not_implemented("forge::add_gitlab_pat")
    }

    /// Sign in to a self-hosted GitLab instance.
    pub fn add_gitlab_self_hosted(
        &self,
        _url: &str,
        _token: Sensitive<String>,
    ) -> Result<Account, ForgeError> {
        not_implemented("forge::add_gitlab_self_hosted")
    }

    /// Sign in to Bitbucket with an API token.
    pub fn add_bitbucket_token(
        &self,
        _email: &str,
        _token: Sensitive<String>,
    ) -> Result<Account, ForgeError> {
        not_implemented("forge::add_bitbucket_token")
    }

    /// Remove an account and its stored token.
    pub fn forget(&self, _user: &ForgeUser) -> Result<(), ForgeError> {
        not_implemented("forge::forget")
    }

    /// Check whether an account's token is still valid.
    pub fn check(&self, _user: &ForgeUser) -> Result<ForgeAccountValidity, ForgeError> {
        not_implemented("forge::check")
    }

    /// Download an image (avatars), with a timeout and a byte cap.
    pub fn fetch_image(&self, _url: &str) -> Result<Vec<u8>, ForgeError> {
        not_implemented("forge::fetch_image")
    }

    /// Detect the forge of `remote_url`.
    pub fn detect(
        &self,
        _remote_url: &str,
        _forge_override: Option<ForgeName>,
    ) -> Option<ForgeInfo> {
        None
    }

    /// Open the per-repository handle.
    pub fn open_repo(&self, input: RepoForgeInput) -> RepoForge {
        RepoForge {
            hub: self.clone(),
            input,
        }
    }
}

/// The per-repository handle a repository tab holds.
#[derive(Clone)]
pub struct RepoForge {
    #[allow(dead_code)]
    hub: ForgeHub,
    #[allow(dead_code)]
    input: RepoForgeInput,
}

impl RepoForge {
    /// The detected forge; `None` = "No forge detected" (offer the override picker).
    pub fn info(&self) -> Option<&ForgeInfo> {
        None
    }

    /// What the forge supports.
    pub fn capabilities(&self) -> ForgeCapabilities {
        ForgeCapabilities {
            checks: false,
            repo_info: false,
            pr_service: false,
            list_service: false,
            review_comments: false,
            review_management: false,
        }
    }

    /// Which account would be used (no network, no keychain).
    pub fn account_status(&self) -> RepoAccountStatus {
        RepoAccountStatus::NotAForge
    }

    /// Web URL of a commit.
    pub fn url_commit(&self, _sha: &str) -> Option<String> {
        None
    }

    /// Web URL of a pull request.
    pub fn url_review(&self, _number: i64) -> Option<String> {
        None
    }

    /// Web URL that starts a new pull request from `head_branch` into `base` (fork-aware).
    pub fn url_new_review(&self, _base: &str, _head_branch: &str) -> Option<String> {
        None
    }

    /// Web URL of a branch.
    pub fn url_branch(&self, _branch: &str) -> Option<String> {
        None
    }

    /// The reviews in the cache (no network).
    pub fn cached_reviews(&self) -> Result<Vec<ForgeReview>, ForgeError> {
        not_implemented("forge::cached_reviews")
    }

    /// The pull request of a remote branch (e.g. `origin/feature`), from the cache.
    pub fn review_for_branch(
        &self,
        _repo: &gix::Repository,
        _remote_branch: &str,
    ) -> Option<ReviewAssociation> {
        None
    }

    /// Refresh the review list from the forge.
    pub fn refresh_reviews(&self, _cancel: &CancelToken) -> Result<RefreshOutcome, ForgeError> {
        not_implemented("forge::refresh_reviews")
    }

    /// One pull request, refreshed into the cache.
    pub fn review(&self, _number: i64) -> Result<ForgeReview, ForgeError> {
        not_implemented("forge::review")
    }

    /// Open a pull request.
    pub fn create_review(&self, _p: CreateReview) -> Result<ForgeReview, ForgeError> {
        not_implemented("forge::create_review")
    }

    /// Change title, body, target and similar fields of a pull request.
    pub fn update_review(&self, _number: i64, _u: ReviewUpdatePayload) -> Result<(), ForgeError> {
        not_implemented("forge::update_review")
    }

    /// Merge a pull request.
    pub fn merge_review(
        &self,
        _number: i64,
        _m: Option<ReviewMergeMethod>,
    ) -> Result<(), ForgeError> {
        not_implemented("forge::merge_review")
    }

    /// Turn auto-merge on or off.
    pub fn set_auto_merge(
        &self,
        _number: i64,
        _on: bool,
        _m: Option<ReviewMergeMethod>,
    ) -> Result<(), ForgeError> {
        not_implemented("forge::set_auto_merge")
    }

    /// Mark a pull request as draft or ready.
    pub fn set_draft(&self, _number: i64, _draft: bool) -> Result<(), ForgeError> {
        not_implemented("forge::set_draft")
    }

    /// Whether a pull request can be merged.
    pub fn merge_status(&self, _number: i64) -> Result<ReviewMergeStatus, ForgeError> {
        not_implemented("forge::merge_status")
    }

    /// Comments, threads, submissions and reactions of a pull request.
    pub fn conversation(&self, _number: i64) -> Result<Conversation, ForgeError> {
        not_implemented("forge::conversation")
    }

    /// The pull-request templates found in `worktree`.
    pub fn templates(&self, _worktree: &Path) -> Vec<String> {
        Vec::new()
    }

    /// Read the template `rel` (relative to `worktree`).
    pub fn read_template(&self, _worktree: &Path, _rel: &str) -> Result<String, ForgeError> {
        not_implemented("forge::read_template")
    }

    /// The default pull-request target: the remote's default branch (from
    /// `gitcthulhu_git::remote::default_branch`), else `"main"`.
    pub fn default_review_target(&self, remote_default_branch: Option<&str>) -> String {
        remote_default_branch.unwrap_or("main").to_owned()
    }

    /// Permissions, fork status and merge settings of the repository on the forge.
    pub fn repo_info(&self) -> Result<RepoInfo, ForgeError> {
        not_implemented("forge::repo_info")
    }

    /// How to check pull request `number` out locally.
    pub fn checkout_plan(&self, _number: i64) -> Result<CheckoutPlan, ForgeError> {
        not_implemented("forge::checkout_plan")
    }

    /// The CI checks of `reference` in the cache (no network).
    pub fn cached_checks(&self, _reference: &str) -> Result<Vec<CiCheck>, ForgeError> {
        not_implemented("forge::cached_checks")
    }

    /// Refresh the CI checks of `reference`; `head_commit` is needed for fork pull requests.
    pub fn refresh_checks(
        &self,
        _reference: &str,
        _head_commit: Option<&str>,
        _cancel: &CancelToken,
    ) -> Result<Vec<CiCheck>, ForgeError> {
        not_implemented("forge::refresh_checks")
    }

    /// Count `checks` by state.
    pub fn checks_summary(_checks: &[CiCheck]) -> ChecksSummary {
        ChecksSummary::default()
    }

    /// Drop cached checks of every reference not in `keep_refs`.
    pub fn prune_checks(&self, _keep_refs: &HashSet<String>) -> Result<(), ForgeError> {
        not_implemented("forge::prune_checks")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> ForgeHub {
        let dir = std::env::temp_dir().join("gitcthulhu-services-forge-stub");
        ForgeHub::new(
            Runtime::new().expect("runtime builds"),
            ForgeDirs {
                app_data: dir.join("data"),
                cache: dir.join("cache"),
            },
            ForgeConfig {
                github_oauth_client_id: None,
                user_agent: "GitCthulhu/0.0.0".into(),
            },
        )
        .expect("the stub hub always builds")
    }

    #[test]
    fn default_review_target_falls_back_to_main() {
        let repo = hub().open_repo(RepoForgeInput {
            repo_dir: PathBuf::from("."),
            remotes: ForgeRemotes {
                base: None,
                push: None,
            },
            prefs: RepoForgePrefs::default(),
        });
        assert_eq!(
            repo.default_review_target(Some("develop")),
            "develop",
            "the remote's default branch wins"
        );
        assert_eq!(
            repo.default_review_target(None),
            "main",
            "without a remote default branch the target is main"
        );
    }

    #[test]
    fn stub_methods_report_not_implemented() {
        let hub = hub();
        assert!(
            matches!(hub.accounts(), Err(ForgeError::NotImplemented(_))),
            "stubs fail loudly instead of pretending there are no accounts"
        );
        let repo = hub.open_repo(RepoForgeInput {
            repo_dir: PathBuf::from("."),
            remotes: ForgeRemotes {
                base: None,
                push: None,
            },
            prefs: RepoForgePrefs::default(),
        });
        assert!(
            matches!(
                repo.refresh_reviews(&CancelToken::new()),
                Err(ForgeError::NotImplemented("forge::refresh_reviews"))
            ),
            "the error names the missing method"
        );
    }

    #[test]
    fn error_classification() {
        assert!(
            ForgeError::Network("down".into()).may_pass(),
            "network errors pass"
        );
        assert!(
            ForgeError::RateLimited { reset_at: None }.may_pass(),
            "rate limits pass"
        );
        assert!(
            !ForgeError::NotAuthenticated.may_pass(),
            "missing accounts do not pass"
        );
        assert!(
            ForgeError::NotAuthenticated.is_terminal(),
            "missing accounts stop polling"
        );
        assert!(
            ForgeError::Auth("expired".into()).is_terminal(),
            "bad tokens stop polling"
        );
        assert!(
            !ForgeError::Network("down".into()).is_terminal(),
            "network errors keep polling"
        );
        assert!(
            !ForgeError::Cancelled.is_terminal(),
            "cancellation is not terminal"
        );
    }
}
