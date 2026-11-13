//! Plain data of the forge backend: no service types, so the GUI compiles without the `services` feature and
//! never names the services crate.

use std::fmt;

use crate::backend::Sensitive;

/// A forge the app knows. Azure DevOps is detected for web URLs only (no API integration).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Forge {
    /// github.com and GitHub Enterprise Server.
    GitHub,
    /// gitlab.com and self-hosted GitLab.
    GitLab,
    /// Bitbucket Cloud.
    Bitbucket,
    /// Azure DevOps.
    Azure,
}

impl Forge {
    /// The name shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Forge::GitHub => "GitHub",
            Forge::GitLab => "GitLab",
            Forge::Bitbucket => "Bitbucket",
            Forge::Azure => "Azure DevOps",
        }
    }

    /// Whether pull requests and checks can be used through the API.
    pub fn has_api(self) -> bool {
        !matches!(self, Forge::Azure)
    }
}

/// An opaque account identity (the services crate's `ForgeUser`, serialised).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountId(pub(crate) String);

impl AccountId {
    /// The serialised identity (what `repos.json` stores as the preferred account).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A connected account, as the Accounts page and the HTTPS offer show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    /// Its forge.
    pub forge: Forge,
    /// The name shown in the row: the login, or the email of a Bitbucket account.
    pub name: String,
    /// How it signed in: "token", "browser sign-in", "on ghe.example.com", "API token".
    pub caption: String,
    /// The avatar URL the forge reported, if any.
    pub avatar_url: Option<String>,
    /// The identity to pass back to the account calls.
    pub id: AccountId,
}

/// A new account to add (the tokens never reach logs: `Debug` is redacted).
pub enum NewAccount {
    /// github.com with a personal access token.
    GitHubToken {
        /// The token.
        token: Sensitive,
    },
    /// A GitHub Enterprise Server instance with a personal access token.
    GitHubEnterprise {
        /// The API base URL, e.g. `https://ghe.example.com/api/v3`.
        api_url: String,
        /// The token.
        token: Sensitive,
    },
    /// gitlab.com with a personal access token.
    GitLabToken {
        /// The token.
        token: Sensitive,
    },
    /// A self-hosted GitLab instance with a personal access token.
    GitLabSelfHosted {
        /// The instance URL (the `/api/v4` suffix is optional).
        url: String,
        /// The token.
        token: Sensitive,
    },
    /// Bitbucket Cloud with an Atlassian API token.
    BitbucketToken {
        /// The Atlassian account email.
        email: String,
        /// The token.
        token: Sensitive,
    },
}

impl fmt::Debug for NewAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            NewAccount::GitHubToken { .. } => "GitHubToken",
            NewAccount::GitHubEnterprise { .. } => "GitHubEnterprise",
            NewAccount::GitLabToken { .. } => "GitLabToken",
            NewAccount::GitLabSelfHosted { .. } => "GitLabSelfHosted",
            NewAccount::BitbucketToken { .. } => "BitbucketToken",
        };
        write!(f, "NewAccount::{name}(***)")
    }
}

/// How an account stands after asking its forge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Validity {
    /// The token works.
    Valid,
    /// The forge rejected the token (or could not be reached).
    Invalid,
    /// No token is stored (it was removed from the keychain).
    NoCredentials,
}

/// The code of a GitHub device-flow sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCodeInfo {
    /// The code the user enters in the browser.
    pub user_code: String,
    /// The code the app polls with.
    pub device_code: String,
    /// Where the user enters `user_code`.
    pub verification_uri: String,
    /// The minimum polling interval in seconds.
    pub interval: u64,
    /// Seconds until the code expires.
    pub expires_in: u64,
}

/// The answer of one device-flow poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevicePoll {
    /// The user has not confirmed yet.
    Pending,
    /// Signed in.
    SignedIn(AccountInfo),
}

/// Why a forge call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The forge could not be reached, or failed on its side: may pass by itself.
    Offline,
    /// The API rate limit was hit.
    RateLimited {
        /// Unix time when the limit resets, if known.
        reset_at: Option<i64>,
    },
    /// The token is invalid, expired or revoked.
    Auth,
    /// The token lacks a permission.
    Permission,
    /// No connected account for the repository's host.
    NoAccount,
    /// The remote belongs to no supported forge.
    NotAForge,
    /// The resource does not exist.
    NotFound,
    /// The call was cancelled.
    Cancelled,
    /// The integrations are not available in this build or are not started yet.
    Unavailable,
    /// Anything else.
    Other,
}

/// A failed forge call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What the user can read.
    pub message: String,
    /// How the GUI reacts.
    pub kind: FailureKind,
}

impl Failure {
    /// A failure of kind `Other`.
    pub fn other(message: impl Into<String>) -> Self {
        Failure {
            message: message.into(),
            kind: FailureKind::Other,
        }
    }

    /// The integrations are missing.
    pub fn unavailable() -> Self {
        Failure {
            message: "The forge integrations are not available".into(),
            kind: FailureKind::Unavailable,
        }
    }

    /// Whether polling stops until the user fixes the account setup.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.kind,
            FailureKind::Auth
                | FailureKind::Permission
                | FailureKind::NoAccount
                | FailureKind::NotAForge
        )
    }

    /// Whether asking again later is worth it.
    pub fn may_pass(&self) -> bool {
        matches!(self.kind, FailureKind::Offline)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The state of a pull request as a badge shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrState {
    /// Open.
    Open,
    /// Open and marked as draft.
    Draft,
    /// Merged.
    Merged,
    /// Closed without merging.
    Closed,
}

impl PrState {
    /// The lower-case word of the tooltip.
    pub fn word(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Draft => "draft",
            PrState::Merged => "merged",
            PrState::Closed => "closed",
        }
    }
}

/// The combined state of the CI checks of a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CiState {
    /// Every check passed.
    Passed,
    /// Something is running or waiting.
    Running,
    /// A check failed.
    Failed,
}

/// Counts of CI check states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckCounts {
    /// Passed, skipped or neutral.
    pub passed: usize,
    /// In progress.
    pub running: usize,
    /// Failed, timed out, cancelled or needing action.
    pub failed: usize,
    /// Waiting for a runner.
    pub queued: usize,
}

impl CheckCounts {
    /// The traffic-light state; `None` without checks.
    pub fn state(&self) -> Option<CiState> {
        if self.failed > 0 {
            Some(CiState::Failed)
        } else if self.running > 0 || self.queued > 0 {
            Some(CiState::Running)
        } else if self.passed > 0 {
            Some(CiState::Passed)
        } else {
            None
        }
    }

    /// "3 passed, 1 failed" (zero counts are left out); `None` without checks.
    pub fn describe(&self) -> Option<String> {
        let parts: Vec<String> = [
            (self.passed, "passed"),
            (self.failed, "failed"),
            (self.running, "running"),
            (self.queued, "queued"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// A pull request found for a branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewBadge {
    /// The number.
    pub number: i64,
    /// The unit symbol: `#` for pull requests, `!` for GitLab merge requests.
    pub symbol: String,
    /// The state.
    pub state: PrState,
    /// The title (known for open reviews, which are read from the cache in full).
    pub title: Option<String>,
    /// The CI checks of the review's head branch.
    pub checks: CheckCounts,
    /// The web URL of the review.
    pub url: Option<String>,
    /// For a merged review: the commit its head merged at (lower-case hex).
    pub merged_head: Option<String>,
}

/// One local branch to find a pull request for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchQuery {
    /// The branch's full ref name.
    pub local: String,
    /// The remote-tracking branch's short name (`origin/feature`): the upstream, else the push remote with the
    /// branch name.
    pub remote_key: String,
    /// The branch tip (hex), to recognise a merged review of exactly this tip.
    pub tip: String,
}

/// What a refresh of the forge changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// Fingerprint of the cached reviews; unchanged = nothing to redraw.
    pub fingerprint: u64,
    /// Unix time of the most recent merge among the cached reviews.
    pub last_merged: Option<i64>,
}

/// Whether an account matches the repository's forge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountMatch {
    /// This account is used (its label).
    Matching(String),
    /// The repository is on a forge, but no account matches its host.
    NoMatchingAccount,
    /// The remote belongs to no forge with an API.
    NotAForge,
}

/// What the app knows about the forge of a repository (no network).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoFacts {
    /// The detected forge; `None` = "No forge detected".
    pub forge: Option<Forge>,
    /// The web host, e.g. `github.com`.
    pub host: String,
    /// "pull request" or "merge request".
    pub unit_name: String,
    /// `#` or `!`.
    pub unit_symbol: String,
    /// Which account the repository would use.
    pub account: AccountMatch,
    /// The forge reports CI checks.
    pub checks: bool,
    /// Pull requests can be listed and created through the API.
    pub reviews: bool,
    /// The remote pull requests target.
    pub base_remote: Option<String>,
    /// The remote branches are pushed to.
    pub push_remote: Option<String>,
    /// The owner shown before the branch name in the head line (`owner:branch`).
    pub head_owner: Option<String>,
}

impl RepoFacts {
    /// Facts of a repository without a forge.
    pub fn none() -> Self {
        RepoFacts {
            forge: None,
            host: String::new(),
            unit_name: "pull request".into(),
            unit_symbol: "#".into(),
            account: AccountMatch::NotAForge,
            checks: false,
            reviews: false,
            base_remote: None,
            push_remote: None,
            head_owner: None,
        }
    }
}

/// Per-repository forge preferences (`repos.json`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoPrefsInput {
    /// `github`, `gitlab` or `bitbucket` (or a host name containing it): forces the forge.
    pub forge_override: Option<String>,
    /// The preferred account (serialised identity, opaque).
    pub preferred_user: Option<serde_json::Value>,
    /// The pull request template path, relative to the worktree.
    pub template_path: Option<String>,
}

/// A pull request to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewReview {
    /// The title.
    pub title: String,
    /// The description.
    pub body: String,
    /// The branch with the changes (the name on the remote).
    pub source_branch: String,
    /// The branch to merge into.
    pub target_branch: String,
    /// Open as draft.
    pub draft: bool,
}

/// A pull request the forge created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedReview {
    /// The number.
    pub number: i64,
    /// The unit symbol.
    pub symbol: String,
    /// The title.
    pub title: String,
    /// The web URL.
    pub url: Option<String>,
}

/// How to check a pull request out locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckoutPlanInfo {
    /// The remote to use (added first when it does not exist).
    pub remote_name: String,
    /// Its URL.
    pub remote_url: String,
    /// The refspec to fetch.
    pub fetch_refspec: String,
    /// The suggested local branch name.
    pub local_branch: String,
    /// The ref the fetch writes: the commit the branch starts at.
    pub fetched_ref: String,
    /// The remote-tracking branch to set as upstream, when the fetched ref is one.
    pub upstream: Option<String>,
}

/// What the create-PR dialog starts from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrDraft {
    /// The template path that was used, if any.
    pub template: Option<String>,
    /// The template text, if one was found.
    pub template_text: Option<String>,
    /// The default base branch.
    pub base: String,
    /// Full messages of the commits of the branch, oldest first (at most 50).
    pub commit_messages: Vec<String>,
}
