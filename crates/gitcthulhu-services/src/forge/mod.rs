//! Forge integrations: GitHub, GitLab and Bitbucket accounts, pull and merge requests, CI checks.
//! 20 section 8.3 (= 15 section 6.2 with the 8.3 adjustments).
//!
//! * [`ForgeHub`] is the process-wide entry point: accounts (sign-in with a browser or a token, forget,
//!   validity), forge detection from a remote URL, image download. It owns the forge storage and shares the
//!   [`Runtime`](crate::Runtime) with the other services.
//! * [`RepoForge`] is what a repository tab holds: the detected forge of the repository, web URLs, the
//!   review (pull request) cache, reviews and CI checks from the forge, PR checkout plans.
//!
//! Everything blocks and must be called from a background thread. The methods that wait for the network and
//! are polled by the GUI take a [`CancelToken`](crate::CancelToken): cancelling drops the request (which
//! closes its connection) and answers [`ForgeError::Cancelled`].
//!
//! The review and check caches live in one SQLite file per forge repository below `ForgeDirs::cache`; the
//! accounts and cached profiles are `forge_settings.json` in `ForgeDirs::app_data`, the tokens are in the
//! system keychain (`but-secret`).

mod checkout;
mod checks;
mod error;
mod hub;
mod image;
mod repo;

use std::path::PathBuf;

use gitcthulhu_git::ForgeRemotes;

pub use but_forge::{
    CiCheck, CiConclusion, CiOutput, CiStatus, ForgeAccountValidity, ForgeCapabilities, ForgeInfo,
    ForgeName, ForgeRepoInfo, ForgeReview, ForgeReviewComment, ForgeReviewLabel,
    ForgeReviewReaction, ForgeReviewSubmission, ForgeReviewThread, ForgeReviewUser, ForgeUnitInfo,
    ForgeUser, RepoInfo, RepoPermissions, ReviewAssociation, ReviewMergeMethod, ReviewMergeStatus,
    ReviewState, ReviewUpdatePayload,
};
pub use error::ForgeError;
pub use hub::ForgeHub;
pub use repo::RepoForge;

/// Directories used by the forge storage.
#[derive(Debug, Clone)]
pub struct ForgeDirs {
    /// `.../gitcthulhu`: holds the account list (`forge_settings.json`).
    pub app_data: PathBuf,
    /// The cache directory: holds the review and check caches.
    pub cache: PathBuf,
}

/// Static forge configuration.
#[derive(Debug, Clone)]
pub struct ForgeConfig {
    /// GitCthulhu's own GitHub OAuth app; `None` means personal access tokens only (15 §0.3).
    pub github_oauth_client_id: Option<String>,
    /// The HTTP user agent of image downloads, `"GitCthulhu/<version>"`. The forge API clients identify as
    /// `gitcthulhu-<forge>-integration`.
    pub user_agent: String,
}

/// A signed-in forge account.
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

/// The conversation of a pull request (empty for GitLab and Bitbucket, which have no conversation API here).
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

/// How to check a pull request out locally (20 section 8.3 describes how the GUI executes the plan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    /// The remote to add if it does not exist yet. An existing remote that already fetches from the
    /// pull request's repository is reused, so then this is its name.
    pub remote_name: String,
    /// Its URL (SSH or HTTPS like the base remote).
    pub remote_url: String,
    /// The refspec to fetch, e.g. `+refs/heads/feature:refs/remotes/octocat/feature`.
    pub fetch_refspec: String,
    /// The suggested local branch name.
    pub local_branch: String,
    /// The full name of the ref the fetch writes: the commit the new branch starts from.
    pub fetched_ref: String,
    /// The remote-tracking branch to set as the new branch's upstream. `None` when `fetched_ref` is not a
    /// remote-tracking branch (the pull request's own ref `refs/pull/N/head` or `refs/merge-requests/N/head`,
    /// used when the head branch is gone), where `git branch --set-upstream-to` would fail.
    pub upstream: Option<String>,
}
