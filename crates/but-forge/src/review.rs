use std::{
    fmt::Display,
    path::{self},
};

use anyhow::{Context as _, Error, Result};
use but_github::CredentialCheckResult;
use but_gitlab::GitLabProjectId;
use but_utils::list_files;
use serde::{Deserialize, Serialize};

use crate::{forge::ForgeName, resolve_account};

/// Get a list of available review template paths for a project
///
/// The paths are relative to the root path
pub fn available_review_templates(root_path: &path::Path, forge_name: &ForgeName) -> Vec<String> {
    let ReviewTemplateFunctions {
        is_review_template,
        get_root,
        supported_template_directories,
        ..
    } = get_review_template_functions(forge_name);

    let forge_root_path = get_root(root_path);
    let forge_root_path = forge_root_path.as_path();

    // let walked_paths = list_files(forge_root_path, &[forge_root_path]).unwrap_or_default();

    supported_template_directories
        .iter()
        .flat_map(|dir| match dir {
            SupportedTemplateDirectory::ProjectRoot => {
                list_files(root_path, &[root_path], false, Some(root_path)).unwrap_or_default()
            }
            SupportedTemplateDirectory::ForgeRoot => {
                list_files(forge_root_path, &[root_path], true, Some(root_path)).unwrap_or_default()
            }
            SupportedTemplateDirectory::Custom(custom_dir) => {
                let custom_path = root_path.join(custom_dir);
                list_files(custom_path.as_path(), &[root_path], true, Some(root_path))
                    .unwrap_or_default()
            }
        })
        .filter_map(|entry| {
            let path_entry = entry.as_path();
            let path_str = path_entry.to_string_lossy();

            if is_review_template(&path_str) {
                return Some(path_str.to_string());
            }
            None
        })
        .collect()
}

pub enum SupportedTemplateDirectory {
    ProjectRoot,
    ForgeRoot,
    Custom(&'static str),
}

pub struct ReviewTemplateFunctions {
    /// Check if a file is a review template
    pub is_review_template: fn(&str) -> bool,
    /// Get the forge directory path
    pub get_root: fn(&path::Path) -> path::PathBuf,
    /// Check if a relative path is a valid review template path
    ///
    /// First argument is the relative path to the file
    /// Second argument is the root path of the project
    pub is_valid_review_template_path: fn(&path::Path) -> bool,
    /// The supported template directories
    pub supported_template_directories: &'static [SupportedTemplateDirectory],
}

pub fn get_review_template_functions(forge_name: &ForgeName) -> ReviewTemplateFunctions {
    match forge_name {
        ForgeName::GitHub => ReviewTemplateFunctions {
            is_review_template: is_review_template_github,
            get_root: get_github_directory_path,
            is_valid_review_template_path: is_valid_review_template_path_github,
            supported_template_directories: &[
                SupportedTemplateDirectory::ForgeRoot,
                SupportedTemplateDirectory::ProjectRoot,
                SupportedTemplateDirectory::Custom("docs"),
            ],
        },
        ForgeName::GitLab => ReviewTemplateFunctions {
            is_review_template: is_review_template_gitlab,
            get_root: get_gitlab_directory_path,
            is_valid_review_template_path: is_valid_review_template_path_gitlab,
            supported_template_directories: &[SupportedTemplateDirectory::ForgeRoot],
        },
        ForgeName::Bitbucket => ReviewTemplateFunctions {
            is_review_template: is_review_template_bitbucket,
            get_root: get_bitbucket_directory_path,
            is_valid_review_template_path: is_valid_review_template_path_bitbucket,
            supported_template_directories: &[SupportedTemplateDirectory::ForgeRoot],
        },
        ForgeName::Azure => ReviewTemplateFunctions {
            is_review_template: is_review_template_azure,
            get_root: get_azure_directory_path,
            is_valid_review_template_path: is_valid_review_template_path_azure,
            supported_template_directories: &[SupportedTemplateDirectory::ForgeRoot],
        },
    }
}

fn get_github_directory_path(root_path: &path::Path) -> path::PathBuf {
    let mut path = root_path.to_path_buf();
    path.push(".github");
    path
}

fn is_review_template_github(path_str: &str) -> bool {
    let normalized_path = path_str.replace('\\', "/");
    normalized_path == "PULL_REQUEST_TEMPLATE.md"
        || normalized_path == "pull_request_template.md"
        || normalized_path.contains(".github/PULL_REQUEST_TEMPLATE")
            && normalized_path.ends_with(".md")
        || normalized_path.contains(".github/pull_request_template")
            && normalized_path.ends_with(".md")
        || normalized_path.contains("docs/PULL_REQUEST_TEMPLATE")
            && normalized_path.ends_with(".md")
        || normalized_path.contains("docs/pull_request_template")
            && normalized_path.ends_with(".md")
}

fn is_valid_review_template_path_github(path: &path::Path) -> bool {
    is_review_template_github(path.to_str().unwrap_or_default())
}

fn get_gitlab_directory_path(root_path: &path::Path) -> path::PathBuf {
    let mut path = root_path.to_path_buf();
    path.push(".gitlab");
    path
}

fn is_review_template_gitlab(path_str: &str) -> bool {
    let normalized_path = path_str.replace('\\', "/");
    normalized_path.contains(".gitlab/merge_request_templates/") && normalized_path.ends_with(".md")
}

fn is_valid_review_template_path_gitlab(path: &path::Path) -> bool {
    is_review_template_gitlab(path.to_str().unwrap_or_default())
}

fn get_bitbucket_directory_path(root_path: &path::Path) -> path::PathBuf {
    // TODO: implement
    root_path.to_path_buf()
}

fn is_review_template_bitbucket(_path_str: &str) -> bool {
    // TODO: implement
    false
}

fn is_valid_review_template_path_bitbucket(_path: &path::Path) -> bool {
    // TODO: implement
    false
}

fn get_azure_directory_path(root_path: &path::Path) -> path::PathBuf {
    // TODO: implement
    root_path.to_path_buf()
}

fn is_review_template_azure(_path_str: &str) -> bool {
    // TODO: implement
    false
}

fn is_valid_review_template_path_azure(_path: &path::Path) -> bool {
    // TODO: implement
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeReviewLabel {
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
}

impl From<but_github::GitHubPrLabel> for ForgeReviewLabel {
    fn from(label: but_github::GitHubPrLabel) -> Self {
        ForgeReviewLabel {
            name: label.name,
            description: label.description,
            color: label.color,
        }
    }
}

impl From<but_gitlab::GitLabLabel> for ForgeReviewLabel {
    fn from(label: but_gitlab::GitLabLabel) -> Self {
        ForgeReviewLabel {
            name: label.name,
            description: None,
            color: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Represents a user from a forge platform (e.g., GitHub, GitLab).
///
/// This structure contains information about a user account on a forge platform,
/// including their identification details and profile information.
pub struct ForgeReviewUser {
    /// The unique numeric identifier for the user on the forge platform
    pub id: i64,
    /// The user's login username
    pub login: String,
    /// The user's display name, if available
    pub name: Option<String>,
    /// The user's email address, if publicly available
    pub email: Option<String>,
    /// URL to the user's profile avatar image, if available
    pub avatar_url: Option<String>,
    /// Indicates whether this account is a bot account
    pub is_bot: bool,
}

impl Display for ForgeReviewUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "login: {}, name: {} ",
            self.login,
            self.name.as_deref().unwrap_or("N/A")
        )
    }
}

impl From<but_github::GitHubUser> for ForgeReviewUser {
    fn from(user: but_github::GitHubUser) -> Self {
        ForgeReviewUser {
            id: user.id,
            login: user.login,
            name: user.name,
            email: user.email,
            avatar_url: user.avatar_url,
            is_bot: user.is_bot,
        }
    }
}

impl From<but_gitlab::GitLabUser> for ForgeReviewUser {
    fn from(user: but_gitlab::GitLabUser) -> Self {
        ForgeReviewUser {
            id: user.id,
            login: user.username,
            name: user.name,
            email: user.email,
            avatar_url: user.avatar_url,
            is_bot: user.is_bot,
        }
    }
}

impl From<but_bitbucket::BitbucketUser> for ForgeReviewUser {
    fn from(user: but_bitbucket::BitbucketUser) -> Self {
        ForgeReviewUser {
            id: user.id,
            login: user.username,
            name: user.name,
            email: user.email,
            avatar_url: user.avatar_url,
            is_bot: user.is_bot,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Represents a review (pull request/merge request) from a forge platform (GitHub, GitLab, etc.).
///
/// Contains metadata and state information about a code review, including its location,
/// participants, labels, and timestamps for various lifecycle events.
pub struct ForgeReview {
    /// The URL to view this review in a web browser
    pub html_url: String,
    /// The unique identifier number for this review within its repository.
    /// This can be a PR or MR number.
    pub number: i64,
    /// The title/summary of the review
    pub title: String,
    /// The detailed description or body text of the review, if provided.
    pub body: Option<String>,
    /// The user who created this review.
    pub author: Option<ForgeReviewUser>,
    /// Labels or tags applied to categorize this review.
    pub labels: Vec<ForgeReviewLabel>,
    /// Whether this review is in draft state (not ready for final review).
    pub draft: bool,
    /// The name of the branch containing the proposed changes.
    /// This is the short name of the branch (e.g., "feature-branch")
    pub source_branch: String,
    /// The name of the branch that will receive the changes when merged.
    /// This is the short name of the branch (e.g., "main" or "develop")
    pub target_branch: String,
    /// The git commit SHA that this review is based on.
    pub sha: String,
    /// Commits on the target branch that represent this review having landed.
    pub integration_commit_shas: Vec<String>,
    /// ISO 8601 timestamp of when the review was created.
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of when the review was last modified.
    pub modified_at: Option<String>,
    /// ISO 8601 timestamp of when the review was merged, if applicable.
    pub merged_at: Option<String>,
    /// ISO 8601 timestamp of when the review was closed, if applicable.
    pub closed_at: Option<String>,
    /// SSH URL for cloning the repository containing this review.
    pub repository_ssh_url: Option<String>,
    /// HTTPS URL for cloning the repository containing this review.
    pub repository_https_url: Option<String>,
    /// The owner (user or organization) of the repository from which the branch originates.
    /// In the case of a fork, this will be the fork owner's username.
    pub repo_owner: Option<String>,
    /// Whether the source/head repository for this review is a fork.
    pub head_repo_is_fork: bool,
    /// Users who have been requested to review or have reviewed this code.
    pub reviewers: Vec<ForgeReviewUser>,
    /// Whether auto-merge (merge once the forge's requirements pass) is enabled.
    pub auto_merge_enabled: bool,
    /// The platform-specific symbol for this review type (e.g., "#" for GitHub pull requests and "!" for MRs).
    pub unit_symbol: String,
    /// The timestamp when this review was last fetched from the forge.
    pub last_sync_at: chrono::NaiveDateTime,
}

impl ForgeReview {
    /// Whether the review is still open (not merged or closed)
    pub fn is_open(&self) -> bool {
        self.merged_at.is_none() && self.closed_at.is_none()
    }

    /// Whether the review has been merged
    pub fn is_merged(&self) -> bool {
        self.merged_at.is_some()
    }

    /// Whether the review points to the given commit ID and has been merged
    pub fn is_merged_at_commit(&self, commit_id: &str) -> bool {
        self.is_merged() && self.sha == commit_id
    }

    /// The struct version for persistence compatibility purposes
    pub fn struct_version() -> i32 {
        4
    }
}

impl Display for ForgeReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}: {}\n - author: {}\n - description: {}\n - created at: {}\n",
            self.unit_symbol,
            self.number,
            self.title,
            self.author
                .as_ref()
                .map(|a| a.to_string())
                .unwrap_or("-unknown-".to_string()),
            self.body.as_deref().unwrap_or("-no description-"),
            self.created_at.as_deref().unwrap_or("-unknown-"),
        )
    }
}

impl From<but_github::PullRequest> for ForgeReview {
    fn from(pr: but_github::PullRequest) -> Self {
        ForgeReview {
            html_url: pr.html_url,
            number: pr.number,
            title: pr.title,
            body: pr.body,
            author: pr.author.map(ForgeReviewUser::from),
            labels: pr.labels.into_iter().map(ForgeReviewLabel::from).collect(),
            draft: pr.draft,
            source_branch: pr.source_branch,
            target_branch: pr.target_branch,
            sha: pr.sha,
            integration_commit_shas: pr.integration_commit_shas,
            created_at: pr.created_at,
            modified_at: pr.modified_at,
            merged_at: pr.merged_at,
            closed_at: pr.closed_at,
            repository_ssh_url: pr.repository_ssh_url,
            repository_https_url: pr.repository_https_url,
            repo_owner: pr.repo_owner,
            head_repo_is_fork: pr.head_repo_is_fork,
            reviewers: pr
                .requested_reviewers
                .into_iter()
                .map(ForgeReviewUser::from)
                .collect(),
            auto_merge_enabled: pr.auto_merge_enabled,
            unit_symbol: "#".to_string(),
            last_sync_at: chrono::Local::now().naive_local(),
        }
    }
}

impl From<but_gitlab::MergeRequest> for ForgeReview {
    fn from(mr: but_gitlab::MergeRequest) -> Self {
        ForgeReview {
            html_url: mr.web_url,
            number: mr.iid,
            title: mr.title,
            body: mr.description,
            author: mr.author.map(ForgeReviewUser::from),
            labels: mr.labels.into_iter().map(ForgeReviewLabel::from).collect(),
            draft: mr.draft,
            source_branch: mr.source_branch,
            target_branch: mr.target_branch,
            sha: mr.sha,
            integration_commit_shas: mr.integration_commit_shas,
            created_at: mr.created_at,
            modified_at: mr.updated_at,
            merged_at: mr.merged_at,
            closed_at: mr.closed_at,
            repository_ssh_url: mr.repository_ssh_url,
            repository_https_url: mr.repository_https_url,
            repo_owner: mr.repo_owner,
            head_repo_is_fork: mr.source_project_is_fork,
            reviewers: mr
                .reviewers
                .into_iter()
                .map(ForgeReviewUser::from)
                .collect(),
            auto_merge_enabled: mr.auto_merge_enabled,
            unit_symbol: "!".to_string(),
            last_sync_at: chrono::Local::now().naive_local(),
        }
    }
}

impl From<but_bitbucket::BitbucketPullRequest> for ForgeReview {
    fn from(pr: but_bitbucket::BitbucketPullRequest) -> Self {
        let merged_at = pr.merged_at();
        let closed_at = pr.closed_at();
        let integration_commit_shas = pr.merge_commit_hash.clone().into_iter().collect();
        ForgeReview {
            html_url: pr.html_url,
            number: pr.id,
            title: pr.title,
            body: pr.description,
            author: pr.author.map(ForgeReviewUser::from),
            // Bitbucket Cloud pull requests don't carry labels.
            labels: Vec::new(),
            draft: pr.draft,
            source_branch: pr.source_branch,
            target_branch: pr.target_branch,
            sha: pr.source_commit_hash,
            integration_commit_shas,
            created_at: pr.created_on,
            modified_at: pr.updated_on,
            merged_at,
            closed_at,
            repository_ssh_url: None,
            repository_https_url: None,
            repo_owner: pr.repo_owner,
            head_repo_is_fork: pr.head_repo_is_fork,
            reviewers: pr
                .reviewers
                .into_iter()
                .map(ForgeReviewUser::from)
                .collect(),
            // Bitbucket Cloud has no auto-merge.
            auto_merge_enabled: false,
            unit_symbol: "#".to_string(),
            last_sync_at: chrono::Local::now().naive_local(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum CacheConfig {
    CacheOnly,
    CacheWithFallback {
        max_age_seconds: u64,
    },
    #[default]
    NoCache,
}

/// List the open reviews (e.g. pull requests) for a given forge repository.
///
/// Dropping the returned future abandons the request (and closes its connection); the cache is written
/// only after the forge has answered, so a cancelled call leaves it untouched.
pub async fn list_forge_reviews_with_cache(
    preferred_forge_user: Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    storage: &but_forge_storage::Controller,
    db: &mut but_db::DbHandle,
    cache_config: Option<CacheConfig>,
) -> Result<Vec<ForgeReview>> {
    let cache_config = cache_config.unwrap_or_default();
    let reviews = match cache_config {
        // The cache also retains settled reviews for the branch association;
        // the listing serves only the open ones, matching a fresh sync.
        CacheConfig::CacheOnly => crate::list_cached_forge_reviews(db)?
            .into_iter()
            .filter(ForgeReview::is_open)
            .collect(),
        CacheConfig::CacheWithFallback { max_age_seconds } => {
            let cached = crate::db::reviews_from_cache(db)?;
            if let Some(reviews) =
                cached.fresh_rows(max_age_seconds, chrono::Local::now().naive_local())
            {
                return Ok(reviews.into_iter().filter(ForgeReview::is_open).collect());
            }
            match sync_listed_reviews(preferred_forge_user, forge_repo_info, storage, db).await {
                Ok(reviews) => reviews,
                Err(err) => {
                    let cached_open: Vec<ForgeReview> = crate::list_cached_forge_reviews(db)?
                        .into_iter()
                        .filter(ForgeReview::is_open)
                        .collect();
                    if serves_stale_reviews(&err, !cached_open.is_empty()) {
                        cached_open
                    } else {
                        return Err(err);
                    }
                }
            }
        }
        CacheConfig::NoCache => {
            sync_listed_reviews(preferred_forge_user, forge_repo_info, storage, db).await?
        }
    };
    Ok(reviews)
}

/// Whether a failed live refresh should keep serving the last known open
/// reviews instead of surfacing the error. Explicit `NoCache` reads never
/// come here and still surface every failure.
///
/// Missing credentials say nothing about the forge's state, so the cached
/// listing stays valid even when it is empty. A network error is transient,
/// but an empty cache has no "confirmed empty at time T" watermark worth
/// serving, so cold listings still surface the failure. Anything else
/// (404, invalid tokens, API errors) can require user action and is
/// returned. The providers tag their errors with these codes.
fn serves_stale_reviews(err: &anyhow::Error, has_open_reviews: bool) -> bool {
    use but_error::AnyhowContextExt as _;
    match err.custom_context().map(|ctx| ctx.code) {
        Some(but_error::Code::ForgeNotAuthenticated) => true,
        Some(but_error::Code::NetworkError) => has_open_reviews,
        _ => false,
    }
}

/// Fetch the open listing and fold it into the cache, recording the fate of
/// any cached open review the listing no longer contains.
///
/// Such a review has merged, been closed, or been deleted on the forge, and
/// the reconcile pass in [`crate::db::cache_reviews`] would delete its row
/// while still marked open — losing the branch association for good, leaving
/// an integrated branch with no way back to its landed review. When that is
/// about to happen (and only then — most syncs change nothing and cost no
/// extra request), one page of the forge's most recently updated settled
/// reviews covers everything that merged or closed since the last sync. The
/// fates ride in the same reconcile transaction, after the listing, so a
/// stale open listing cannot overwrite a fate just learned. A failed sweep
/// degrades to the pre-sweep behavior — the vanished row is deleted once its
/// grace expires — rather than blocking the sync.
async fn sync_listed_reviews(
    preferred_forge_user: Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    storage: &but_forge_storage::Controller,
    db: &mut but_db::DbHandle,
) -> Result<Vec<ForgeReview>> {
    let reviews = list_forge_reviews(&preferred_forge_user, forge_repo_info, storage).await?;
    let cached_states = crate::db::cached_review_states(db).unwrap_or_default();
    let mut to_cache = reviews.clone();
    if open_review_vanished(&cached_states, &reviews) {
        let settled =
            list_recently_settled_reviews(&preferred_forge_user, forge_repo_info, storage)
                .await
                .unwrap_or_default();
        to_cache.extend(
            fates_to_record(&settled, &cached_states)
                .into_iter()
                .cloned(),
        );
    }
    crate::db::cache_reviews(db, &to_cache).ok();
    Ok(reviews)
}

/// Whether some review the cache still holds as open is absent from the
/// fresh open listing — the moment its fate needs recording.
fn open_review_vanished(cached_states: &[(i64, bool)], listing: &[ForgeReview]) -> bool {
    let listed = listing
        .iter()
        .map(|review| review.number)
        .collect::<std::collections::HashSet<_>>();
    cached_states
        .iter()
        .any(|(number, settled)| !settled && !listed.contains(number))
}

/// The settled reviews worth recording ahead of a reconcile pass: those the
/// cache tracks as open, whose fate is now known. Reviews the cache never
/// saw are left out — associations key on branch name, so recording other
/// people's reviews would claim same-named local branches — and reviews
/// already settled in the cache need no re-write.
fn fates_to_record<'a>(
    settled: &'a [ForgeReview],
    cached_states: &[(i64, bool)],
) -> Vec<&'a ForgeReview> {
    let cached_open = cached_states
        .iter()
        .filter(|(_, settled)| !settled)
        .map(|(number, _)| *number)
        .collect::<std::collections::HashSet<_>>();
    settled
        .iter()
        .filter(|review| !review.is_open() && cached_open.contains(&review.number))
        .collect()
}

/// One page of the forge's most recently updated merged/closed reviews,
/// newest first. Forges without an implementation report nothing, which
/// leaves their reconcile behavior as it was.
async fn list_recently_settled_reviews(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    storage: &but_forge_storage::Controller,
) -> Result<Vec<ForgeReview>> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    let preferred_forge_user = resolve_account(preferred_forge_user, forge_repo_info, storage)?;
    let reviews = match forge {
        ForgeName::GitHub => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.github().cloned());
            but_github::pr::list_recently_closed(preferred_account.as_ref(), owner, repo, storage)
                .await?
                .into_iter()
                .map(ForgeReview::from)
                .collect()
        }
        ForgeName::GitLab => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.gitlab().cloned());
            let project_id = GitLabProjectId::new(owner, repo);
            but_gitlab::mr::list_recently_closed(preferred_account.as_ref(), project_id, storage)
                .await?
                .into_iter()
                .map(ForgeReview::from)
                .collect()
        }
        ForgeName::Bitbucket => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.bitbucket().cloned());
            but_bitbucket::pr::list_recently_closed(
                preferred_account.as_ref(),
                owner,
                repo,
                storage,
            )
            .await?
            .into_iter()
            .map(ForgeReview::from)
            .collect()
        }
        _ => Vec::new(),
    };
    Ok(reviews)
}

/// Optimistically cache a single review — e.g. one just created via
/// [`create_forge_review`] — so it appears in cache-only projections before the
/// next full review-list sync.
///
/// Unlike [`list_forge_reviews_with_cache`], this upserts a single row and never
/// deletes other cached reviews. The reconcile pass protects such a freshly
/// written row from deletion for a short grace window, so it survives even if the
/// forge's own list endpoint hasn't caught up to the new review yet.
pub fn cache_review(db: &mut but_db::DbHandle, review: &ForgeReview) -> Result<()> {
    crate::db::upsert_review(db, review)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForgeAccountValidity {
    Valid,
    Invalid,
    NoCredentials,
}

impl From<but_github::CredentialCheckResult> for ForgeAccountValidity {
    fn from(value: but_github::CredentialCheckResult) -> Self {
        match value {
            CredentialCheckResult::Invalid => ForgeAccountValidity::Invalid,
            CredentialCheckResult::NoCredentials => ForgeAccountValidity::NoCredentials,
            CredentialCheckResult::Valid => ForgeAccountValidity::Valid,
        }
    }
}

impl From<but_gitlab::CredentialCheckResult> for ForgeAccountValidity {
    fn from(value: but_gitlab::CredentialCheckResult) -> Self {
        match value {
            but_gitlab::CredentialCheckResult::Invalid => ForgeAccountValidity::Invalid,
            but_gitlab::CredentialCheckResult::NoCredentials => ForgeAccountValidity::NoCredentials,
            but_gitlab::CredentialCheckResult::Valid => ForgeAccountValidity::Valid,
        }
    }
}

impl From<but_bitbucket::CredentialCheckResult> for ForgeAccountValidity {
    fn from(value: but_bitbucket::CredentialCheckResult) -> Self {
        match value {
            but_bitbucket::CredentialCheckResult::Invalid => ForgeAccountValidity::Invalid,
            but_bitbucket::CredentialCheckResult::NoCredentials => {
                ForgeAccountValidity::NoCredentials
            }
            but_bitbucket::CredentialCheckResult::Valid => ForgeAccountValidity::Valid,
        }
    }
}

/// Check whether there's an account that would be used for this repository is authenticated.
pub async fn check_forge_account_is_valid(
    preferred_forge_user: Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    storage: &but_forge_storage::Controller,
) -> Result<ForgeAccountValidity> {
    let Some(account) = crate::find_forge_account(&preferred_forge_user, forge_repo_info, storage)?
    else {
        return if forge_repo_info.forge == ForgeName::Azure {
            Err(Error::msg(format!(
                "Checking reviews for forge {:?} is not implemented yet",
                forge_repo_info.forge
            )))
        } else {
            Ok(ForgeAccountValidity::NoCredentials)
        };
    };
    match account {
        crate::ForgeUser::GitHub(account) => but_github::check_credentials(&account, storage)
            .await
            .map(Into::into),
        crate::ForgeUser::GitLab(account) => but_gitlab::check_credentials(&account, storage)
            .await
            .map(Into::into),
        crate::ForgeUser::Bitbucket(account) => but_bitbucket::check_credentials(&account, storage)
            .await
            .map(Into::into),
    }
}

async fn list_forge_reviews(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    storage: &but_forge_storage::Controller,
) -> Result<Vec<ForgeReview>> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    let preferred_forge_user = resolve_account(preferred_forge_user, forge_repo_info, storage)?;
    let reviews = match forge {
        ForgeName::GitHub => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.github().cloned());
            but_github::pr::list(preferred_account.as_ref(), owner, repo, storage)
                .await?
                .into_iter()
                .map(ForgeReview::from)
                .collect::<Vec<ForgeReview>>()
        }
        ForgeName::GitLab => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.gitlab().cloned());
            let project_id = GitLabProjectId::new(owner, repo);
            but_gitlab::mr::list(preferred_account.as_ref(), project_id, storage)
                .await?
                .into_iter()
                .map(ForgeReview::from)
                .collect::<Vec<ForgeReview>>()
        }
        ForgeName::Bitbucket => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.bitbucket().cloned());
            but_bitbucket::pr::list(preferred_account.as_ref(), owner, repo, storage)
                .await?
                .into_iter()
                .map(ForgeReview::from)
                .collect::<Vec<ForgeReview>>()
        }
        _ => {
            return Err(Error::msg(format!(
                "Listing reviews for forge {forge:?} is not implemented yet.",
            )));
        }
    };
    Ok(reviews)
}

async fn get_forge_review_inner(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    storage: &but_forge_storage::Controller,
) -> Result<ForgeReview> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    let preferred_forge_user = resolve_account(preferred_forge_user, forge_repo_info, storage)?;
    match forge {
        ForgeName::GitHub => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let pr =
                but_github::pr::get(preferred_account, owner, repo, review_number, storage).await?;
            Ok(ForgeReview::from(pr))
        }
        ForgeName::GitLab => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.gitlab());
            let project_id = GitLabProjectId::new(owner, repo);
            let mr =
                but_gitlab::mr::get(preferred_account, project_id, review_number, storage).await?;
            Ok(ForgeReview::from(mr))
        }
        ForgeName::Bitbucket => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.bitbucket());
            let pr = but_bitbucket::pr::get(preferred_account, owner, repo, review_number, storage)
                .await?;
            Ok(ForgeReview::from(pr))
        }
        _ => Err(Error::msg(format!(
            "Getting reviews for forge {forge:?} is not implemented yet.",
        ))),
    }
}

/// Forge-agnostic runtime merge state for a review. Always fetched
/// fresh from the forge; not cached. Used by the UI to render the
/// merge-button hint and comment count without forcing every review
/// consumer to subscribe to those expensive fields.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewMergeStatus {
    /// Forge-reported merge state. GitHub strings: `clean`, `dirty`,
    /// `unknown`, `blocked`, `behind`, `unstable`, `has_hooks`,
    /// `draft`. GitLab strings: `can_be_merged`, `cannot_be_merged`,
    /// `checking`, etc. `None` when the forge hasn't computed it.
    /// Used by the UI only to surface a specific reason tooltip.
    pub mergeable_state: Option<String>,
    pub comments_count: i64,
    /// Forge-normalized: whether merging is allowed. Drives the merge
    /// button without forcing the UI to know per-forge state strings.
    pub is_mergeable: bool,
}

/// A top-level comment on a review's conversation thread. Fetched fresh
/// from the forge; not cached. Diff-anchored review comments are not
/// part of this type.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeReviewComment {
    /// Forge-assigned identifier of the comment.
    pub id: i64,
    /// The comment text, as forge-flavored markdown.
    pub body: String,
    /// The comment's author.
    pub author: Option<ForgeReviewUser>,
    /// ISO 8601 timestamp of when the comment was created.
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of the comment's last edit.
    pub modified_at: Option<String>,
    /// The URL to view this comment in a web browser.
    pub html_url: String,
    /// Reaction tallies on this comment, nonzero kinds only.
    pub reactions: Vec<ForgeReviewReactionCount>,
}

/// One reaction kind's tally. `kind` is the forge's native reaction name
/// (GitHub: `+1`, `laugh`, …) — an open set, since forges like GitLab
/// allow arbitrary award emoji.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeReviewReactionCount {
    pub kind: String,
    pub count: i64,
}

fn github_reaction_counts(
    reactions: but_github::CommentReactions,
) -> Vec<ForgeReviewReactionCount> {
    [
        ("+1", reactions.plus_one),
        ("-1", reactions.minus_one),
        ("laugh", reactions.laugh),
        ("confused", reactions.confused),
        ("heart", reactions.heart),
        ("hooray", reactions.hooray),
        ("rocket", reactions.rocket),
        ("eyes", reactions.eyes),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .map(|(kind, count)| ForgeReviewReactionCount {
        kind: kind.to_owned(),
        count,
    })
    .collect()
}

impl From<but_github::PullRequestComment> for ForgeReviewComment {
    fn from(comment: but_github::PullRequestComment) -> Self {
        ForgeReviewComment {
            id: comment.id,
            body: comment.body,
            author: comment.author.map(ForgeReviewUser::from),
            created_at: comment.created_at,
            modified_at: comment.modified_at,
            html_url: comment.html_url,
            reactions: github_reaction_counts(comment.reactions),
        }
    }
}

/// The verdict a submitted review carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ForgeReviewSubmissionState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

/// A submitted review (approval, change request, or review comment) on a
/// review. Fetched fresh from the forge; not cached. The caller's own
/// unsubmitted (pending) drafts are excluded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeReviewSubmission {
    /// Forge-assigned identifier of the submission.
    pub id: i64,
    /// Who submitted the review.
    pub author: Option<ForgeReviewUser>,
    /// The verdict of this submission.
    pub state: ForgeReviewSubmissionState,
    /// The summary text accompanying the submission, if any.
    pub body: Option<String>,
    /// ISO 8601 timestamp of when the review was submitted.
    pub submitted_at: Option<String>,
    /// The URL to view this submission in a web browser.
    pub html_url: String,
    /// The reactions left on the submission, with who left each.
    pub reactions: Vec<ForgeReviewReaction>,
}

/// Map GitHub's review-state string to the forge-agnostic verdict.
/// `PENDING` (the caller's own unsubmitted draft) and unknown future states
/// map to `None` and are omitted from listings.
fn github_submission_state(raw: &str) -> Option<ForgeReviewSubmissionState> {
    match raw {
        "APPROVED" => Some(ForgeReviewSubmissionState::Approved),
        "CHANGES_REQUESTED" => Some(ForgeReviewSubmissionState::ChangesRequested),
        "COMMENTED" => Some(ForgeReviewSubmissionState::Commented),
        "DISMISSED" => Some(ForgeReviewSubmissionState::Dismissed),
        _ => None,
    }
}

/// List the submitted reviews on a review, oldest first. Each call hits
/// the forge fresh (no DB cache).
pub async fn list_review_submissions(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    storage: &but_forge_storage::Controller,
) -> Result<Vec<ForgeReviewSubmission>> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    match forge {
        ForgeName::GitHub => {
            let preferred_forge_user =
                resolve_account(preferred_forge_user, forge_repo_info, storage)?;
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let reviews = but_github::pr::list_pr_reviews(
                preferred_account,
                owner,
                repo,
                review_number,
                storage,
            )
            .await?;
            Ok(reviews
                .into_iter()
                .filter_map(|review| {
                    let state = github_submission_state(&review.state)?;
                    Some(ForgeReviewSubmission {
                        id: review.id,
                        author: review.author.map(ForgeReviewUser::from),
                        state,
                        body: review.body.filter(|body| !body.trim().is_empty()),
                        submitted_at: review.submitted_at,
                        html_url: review.html_url,
                        reactions: github_reactions(review.reactions),
                    })
                })
                .collect())
        }
        // Read as empty rather than erroring; see list_review_comments.
        _ => Ok(Vec::new()),
    }
}

/// List the top-level conversation comments on a review, oldest first.
/// Each call hits the forge fresh (no DB cache).
pub async fn list_review_comments(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    storage: &but_forge_storage::Controller,
) -> Result<Vec<ForgeReviewComment>> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    match forge {
        ForgeName::GitHub => {
            let preferred_forge_user =
                resolve_account(preferred_forge_user, forge_repo_info, storage)?;
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let comments = but_github::pr::list_comments(
                preferred_account,
                owner,
                repo,
                review_number,
                storage,
            )
            .await?;
            Ok(comments.into_iter().map(Into::into).collect())
        }
        // Read as empty rather than erroring: the UI polls this for every
        // open review, and a forge without comment support shouldn't turn
        // that into a permanent failure loop.
        _ => Ok(Vec::new()),
    }
}

/// Which side of the diff a thread hangs on: `old` line numbers count in
/// the pre-image, `new` in the post-image. Same axis as a local diff
/// comment, so both can be anchored the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeReviewThreadSide {
    Old,
    New,
}

/// A diff-anchored conversation on a review: where in the diff it hangs,
/// whether it has been resolved, and the comments left in it. Fetched
/// fresh from the forge; not cached.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeReviewThread {
    /// The forge's thread identifier, opaque and only meaningful to it.
    pub id: String,
    /// Path the thread hangs on, as the forge spells it.
    pub path: String,
    /// The line in the current diff, or `None` once the thread is outdated.
    pub line: Option<i64>,
    /// First line of a multi-line thread; equals `line` for a single one.
    pub start_line: Option<i64>,
    /// Where the thread was left, which survives the diff moving on.
    pub original_line: Option<i64>,
    pub side: ForgeReviewThreadSide,
    /// Whether someone marked the conversation done.
    pub is_resolved: bool,
    /// Whether the diff the thread was left on has since changed.
    pub is_outdated: bool,
    pub comments: Vec<ForgeReviewThreadComment>,
}

/// One comment inside a review thread.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeReviewThreadComment {
    /// Forge-assigned identifier of the comment.
    pub id: i64,
    /// The comment text, as forge-flavored markdown.
    pub body: String,
    pub author: Option<ForgeReviewUser>,
    /// ISO 8601 timestamp of when the comment was created.
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of the comment's last edit.
    pub modified_at: Option<String>,
    /// The URL to view this comment in a web browser.
    pub html_url: String,
    /// The diff the comment was anchored to, as a unified hunk.
    pub diff_hunk: Option<String>,
    /// The review submission this comment was posted under, which is what
    /// files it under a verdict rather than standing on its own.
    pub review_id: Option<i64>,
}

impl From<but_github::PullRequestReviewThread> for ForgeReviewThread {
    fn from(thread: but_github::PullRequestReviewThread) -> Self {
        ForgeReviewThread {
            id: thread.id,
            path: thread.path,
            line: thread.line,
            start_line: thread.start_line,
            original_line: thread.original_line,
            side: if thread.diff_side == "LEFT" {
                ForgeReviewThreadSide::Old
            } else {
                ForgeReviewThreadSide::New
            },
            is_resolved: thread.is_resolved,
            is_outdated: thread.is_outdated,
            comments: thread
                .comments
                .into_iter()
                .map(ForgeReviewThreadComment::from)
                .collect(),
        }
    }
}

impl From<but_github::PullRequestReviewThreadComment> for ForgeReviewThreadComment {
    fn from(comment: but_github::PullRequestReviewThreadComment) -> Self {
        ForgeReviewThreadComment {
            id: comment.id,
            body: comment.body,
            author: comment.author.map(ForgeReviewUser::from),
            created_at: comment.created_at,
            modified_at: comment.modified_at,
            html_url: comment.html_url,
            diff_hunk: comment.diff_hunk,
            review_id: comment.review_id,
        }
    }
}

/// List the diff-anchored review threads on a review, oldest first. Each
/// call hits the forge fresh (no DB cache).
pub async fn list_review_threads(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    storage: &but_forge_storage::Controller,
) -> Result<Vec<ForgeReviewThread>> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    match forge {
        ForgeName::GitHub => {
            let preferred_forge_user =
                resolve_account(preferred_forge_user, forge_repo_info, storage)?;
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let threads = but_github::pr::list_review_threads(
                preferred_account,
                owner,
                repo,
                review_number,
                storage,
            )
            .await?;
            Ok(threads.into_iter().map(Into::into).collect())
        }
        // Read as empty rather than erroring; see list_review_comments.
        _ => Ok(Vec::new()),
    }
}

/// One individual reaction, with who left it and the forge id that
/// addresses its removal. `kind` is the forge's native reaction name — an
/// open set; unknown kinds pass through rather than being dropped.
/// Fetched fresh; not cached.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeReviewReaction {
    pub id: i64,
    pub kind: String,
    pub user: Option<ForgeReviewUser>,
}

/// List the reactions on a review's description, oldest first. Each call
/// hits the forge fresh (no DB cache).
pub async fn list_review_reactions(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    storage: &but_forge_storage::Controller,
) -> Result<Vec<ForgeReviewReaction>> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    match forge {
        ForgeName::GitHub => {
            let preferred_forge_user =
                resolve_account(preferred_forge_user, forge_repo_info, storage)?;
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let reactions = but_github::pr::list_reactions(
                preferred_account,
                owner,
                repo,
                review_number,
                storage,
            )
            .await?;
            Ok(github_reactions(reactions))
        }
        // Read as empty rather than erroring; see list_review_comments.
        _ => Ok(Vec::new()),
    }
}

fn github_reactions(reactions: Vec<but_github::Reaction>) -> Vec<ForgeReviewReaction> {
    reactions.into_iter().map(github_reaction).collect()
}

fn github_reaction(reaction: but_github::Reaction) -> ForgeReviewReaction {
    ForgeReviewReaction {
        id: reaction.id,
        kind: reaction.content,
        user: reaction.user.map(ForgeReviewUser::from),
    }
}

/// Canonical clone URL for a review's base repo (the repo the PR
/// targets). `None` for forges without a URL-based fork model.
pub async fn get_review_base_repo_url(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    storage: &but_forge_storage::Controller,
) -> Result<Option<String>> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    match forge {
        ForgeName::GitHub => {
            let preferred_forge_user =
                resolve_account(preferred_forge_user, forge_repo_info, storage)?;
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let pr_number = review_number
                .try_into()
                .context("PR: Failed to cast usize to i64, somehow")?;
            but_github::GitHubClient::from_storage(storage, preferred_account)?
                .get_pull_request_base_repo_url(owner, repo, pr_number)
                .await
                .context("Failed to fetch PR base repo URL")
        }
        // None tells the UI to fall back to a branch-name-only check.
        ForgeName::GitLab | ForgeName::Bitbucket | ForgeName::Azure => Ok(None),
    }
}

/// Fetch the runtime merge state for a review. Each call hits the
/// forge fresh (no DB cache) so the UI can render up-to-date hints
/// without paying for the rest of the review payload.
pub async fn get_review_merge_status(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    storage: &but_forge_storage::Controller,
) -> Result<ReviewMergeStatus> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    let preferred_forge_user = resolve_account(preferred_forge_user, forge_repo_info, storage)?;
    match forge {
        ForgeName::GitHub => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let status = but_github::pr::get_merge_status(
                preferred_account,
                owner,
                repo,
                review_number,
                storage,
            )
            .await?;
            Ok(ReviewMergeStatus {
                mergeable_state: status.mergeable_state,
                comments_count: status.comments_count,
                is_mergeable: status.is_mergeable,
            })
        }
        ForgeName::GitLab => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.gitlab());
            let project_id = GitLabProjectId::new(owner, repo);
            let status = but_gitlab::mr::get_merge_status(
                preferred_account,
                project_id,
                review_number,
                storage,
            )
            .await?;
            Ok(ReviewMergeStatus {
                mergeable_state: status.mergeable_state,
                comments_count: status.comments_count,
                is_mergeable: status.is_mergeable,
            })
        }
        ForgeName::Bitbucket => {
            // Bitbucket has no dedicated mergeability endpoint; derive a basic
            // status from the PR's state and comment count.
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.bitbucket());
            let pr = but_bitbucket::pr::get(preferred_account, owner, repo, review_number, storage)
                .await?;
            Ok(ReviewMergeStatus {
                is_mergeable: pr.is_open(),
                // Bitbucket's state strings ("OPEN"/"MERGED"/…) don't map to the
                // forge-agnostic mergeable_state vocabulary the UI understands, so
                // leave it unset rather than feed a value it can't interpret.
                mergeable_state: None,
                comments_count: pr.comment_count,
            })
        }
        _ => Err(anyhow::anyhow!(
            "Merge status for forge {forge:?} is not implemented yet."
        )),
    }
}

/// Get a specific review (e.g. pull request) for a given forge repository
///
/// The resulting review will be cached.
pub async fn get_forge_review(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    db: &mut but_db::DbHandle,
    storage: &but_forge_storage::Controller,
) -> Result<ForgeReview> {
    let review = get_forge_review_inner(
        preferred_forge_user,
        forge_repo_info,
        review_number,
        storage,
    )
    .await?;

    // Cache the review and ignore any issues, if any.
    crate::db::upsert_review(db, &review).ok();
    Ok(review)
}

/// How to merge a review on the forge. GitHub honours all three;
/// other forges fall back to their default merge strategy when the
/// caller asks for `Squash`/`Rebase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReviewMergeMethod {
    #[default]
    Merge,
    Squash,
    Rebase,
}

impl From<&ReviewMergeMethod> for but_github::MergeMethod {
    fn from(value: &ReviewMergeMethod) -> Self {
        match value {
            ReviewMergeMethod::Merge => but_github::MergeMethod::Merge,
            ReviewMergeMethod::Squash => but_github::MergeMethod::Squash,
            ReviewMergeMethod::Rebase => but_github::MergeMethod::Rebase,
        }
    }
}

/// The values to update for an existing review.  Each `Some` is applied;
/// `None` leaves the field unchanged on the forge.
pub struct ReviewUpdatePayload {
    title: Option<String>,
    body: Option<String>,
    state: Option<ReviewState>,
    target_base: Option<String>,
}

impl ReviewUpdatePayload {
    /// Create a new instance of the parameters of the review to update.
    pub fn new(
        title: Option<String>,
        body: Option<String>,
        state: Option<ReviewState>,
        target_base: Option<String>,
    ) -> Self {
        Self {
            title,
            body,
            state,
            target_base,
        }
    }
}

/// Update arbitrary fields of a single review.
pub async fn update_review(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    payload: ReviewUpdatePayload,
    storage: &but_forge_storage::Controller,
) -> Result<()> {
    let ReviewUpdatePayload {
        title,
        body,
        state,
        target_base,
    } = payload;
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    let has_metadata_update = title.is_some() || body.is_some() || target_base.is_some();
    let should_decline = matches!(state, Some(ReviewState::Closed));
    if *forge == ForgeName::Bitbucket {
        if matches!(state, Some(ReviewState::Open)) {
            return Err(anyhow::anyhow!(
                "Bitbucket does not support reopening a declined pull request via the API."
            ));
        }
        if !has_metadata_update && !should_decline {
            return Ok(());
        }
    }
    let preferred_forge_user = resolve_account(preferred_forge_user, forge_repo_info, storage)?;
    match forge {
        ForgeName::GitHub => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let pr_number = review_number
                .try_into()
                .context("PR: Failed to cast usize to i64, somehow")?;
            let state_str = state.as_ref().map(|s| s.as_github_str());
            let params = but_github::UpdatePullRequestParams {
                owner,
                repo,
                pr_number,
                title: title.as_deref(),
                body: body.as_deref(),
                base: target_base.as_deref(),
                state: state_str,
            };
            but_github::pr::update(preferred_account, params, storage)
                .await
                .context("Failed to update PR")?;
            Ok(())
        }
        ForgeName::GitLab => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.gitlab());
            let project_id = GitLabProjectId::new(owner, repo);
            let mr_iid = review_number
                .try_into()
                .context("MR: Failed to cast usize to i64, somehow")?;
            // GitLab uses `state_event` ("close" / "reopen") rather than
            // a `state` field. Map the forge-agnostic ReviewState onto that.
            let state_event = state.as_ref().map(|s| s.as_gitlab_state_event());
            let params = but_gitlab::UpdateMergeRequestParams {
                project_id,
                mr_iid,
                title: title.as_deref(),
                description: body.as_deref(),
                target_branch: target_base.as_deref(),
                state_event,
            };
            but_gitlab::mr::update(preferred_account, params, storage).await?;
            Ok(())
        }
        ForgeName::Bitbucket => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.bitbucket());
            if has_metadata_update {
                let id = review_number
                    .try_into()
                    .context("PR: Failed to cast usize to i64, somehow")?;
                let params = but_bitbucket::UpdatePullRequestParams {
                    workspace: owner,
                    repo_slug: repo,
                    id,
                    title: title.as_deref(),
                    description: body.as_deref(),
                    target_branch: target_base.as_deref(),
                };
                but_bitbucket::pr::update(preferred_account, params, storage).await?;
            }
            if should_decline {
                but_bitbucket::pr::decline(preferred_account, owner, repo, review_number, storage)
                    .await?;
            }
            Ok(())
        }
        _ => Err(anyhow::anyhow!(
            "Updating pull requests for forge {forge:?} is not implemented yet."
        )),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewState {
    Open,
    Closed,
}

impl ReviewState {
    fn as_github_str(&self) -> &'static str {
        match self {
            ReviewState::Open => "open",
            ReviewState::Closed => "closed",
        }
    }

    fn as_gitlab_state_event(&self) -> &'static str {
        match self {
            ReviewState::Open => "reopen",
            ReviewState::Closed => "close",
        }
    }
}

/// Merge a review to it's target branch
pub async fn merge_review(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    merge_method: Option<ReviewMergeMethod>,
    storage: &but_forge_storage::Controller,
) -> Result<()> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    let preferred_forge_user = resolve_account(preferred_forge_user, forge_repo_info, storage)?;
    match forge {
        ForgeName::GitHub => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let pr_number = review_number
                .try_into()
                .context("PR: Failed to cast usize to i64, somehow")?;
            let params = but_github::MergePullRequestParams {
                owner,
                repo,
                pr_number,
                commit_message: None,
                commit_title: None,
                merge_method: merge_method.as_ref().map(Into::into),
            };
            but_github::pr::merge(preferred_account, params, storage).await
        }
        ForgeName::GitLab => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.gitlab());
            let project_id = GitLabProjectId::new(owner, repo);
            let mr_iid = review_number
                .try_into()
                .context("MR: Failed to cast usize to i64, somehow")?;
            let params = but_gitlab::MergeMergeRequestParams {
                project_id,
                mr_iid,
                squash: None,
            };

            but_gitlab::mr::merge(preferred_account, params, storage).await
        }
        ForgeName::Bitbucket => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.bitbucket());
            let id = review_number
                .try_into()
                .context("PR: Failed to cast usize to i64, somehow")?;
            let strategy = match merge_method {
                Some(ReviewMergeMethod::Squash) => but_bitbucket::MergeStrategy::Squash,
                Some(ReviewMergeMethod::Rebase) => but_bitbucket::MergeStrategy::RebaseFastForward,
                Some(ReviewMergeMethod::Merge) | None => but_bitbucket::MergeStrategy::MergeCommit,
            };
            let params = but_bitbucket::MergePullRequestParams {
                workspace: owner,
                repo_slug: repo,
                id,
                strategy,
            };
            but_bitbucket::pr::merge(preferred_account, params, storage).await
        }
        _ => Err(Error::msg(format!(
            "Merging reviews for forge {forge:?} is not implemented yet.",
        ))),
    }
}

/// Set a review to automatically merge when all prerequisites are met. GitHub merges it by
/// `merge_method`, or with `None` the way its merge box offers the user first. GitLab merges
/// as the project is set up to.
pub async fn set_review_auto_merge_state(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    enable: bool,
    merge_method: Option<ReviewMergeMethod>,
    storage: &but_forge_storage::Controller,
) -> Result<()> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    if *forge == ForgeName::Bitbucket {
        return Err(Error::msg(
            "Bitbucket Cloud does not support auto-merge for pull requests.",
        ));
    }
    let preferred_forge_user = resolve_account(preferred_forge_user, forge_repo_info, storage)?;

    match forge {
        ForgeName::GitHub => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let pr_number = review_number
                .try_into()
                .context("PR: Failed to cast usize to i64, somehow")?;
            let params = but_github::SetPullRequestAutoMergeParams {
                owner,
                repo,
                pr_number,
                state: match enable {
                    true => {
                        but_github::AutoMergeState::Enabled(merge_method.as_ref().map(Into::into))
                    }
                    false => but_github::AutoMergeState::Disabled,
                },
            };
            but_github::pr::set_auto_merge(preferred_account, params, storage).await
        }
        ForgeName::GitLab => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.gitlab());
            let project_id = GitLabProjectId::new(owner, repo);
            let mr_iid = review_number
                .try_into()
                .context("MR: Failed to cast usize to i64, somehow")?;
            let params = but_gitlab::SetMergeRequestAutoMergeParams {
                project_id,
                mr_iid,
                enabled: enable,
            };
            but_gitlab::mr::set_auto_merge(preferred_account, params, storage).await
        }

        _ => Err(Error::msg(format!(
            "Setting the auto-merge state of reviews for forge {forge:?} is not implemented yet.",
        ))),
    }
}

/// Set the draftiness of a review: Should it be a draft or is it ready to review?
pub async fn set_review_draftiness(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    review_number: usize,
    draft: bool,
    storage: &but_forge_storage::Controller,
) -> Result<()> {
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    let preferred_forge_user = resolve_account(preferred_forge_user, forge_repo_info, storage)?;

    match forge {
        ForgeName::GitHub => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let pr_number = review_number
                .try_into()
                .context("PR: Failed to cast usize to i64, somehow")?;
            let params = but_github::SetPullRequestDraftStateParams {
                owner,
                repo,
                pr_number,
                draft,
            };
            but_github::pr::set_draft_state(preferred_account, params, storage).await
        }
        ForgeName::GitLab => {
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.gitlab());
            let project_id = GitLabProjectId::new(owner, repo);
            let mr_iid = review_number
                .try_into()
                .context("MR: Failed to cast usize to i64, somehow")?;
            let params = but_gitlab::SetMergeRequestDraftStateParams {
                project_id,
                mr_iid,
                is_draft: draft,
            };
            but_gitlab::mr::set_draft_state(preferred_account, params, storage).await
        }
        ForgeName::Bitbucket => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.bitbucket());
            let id = review_number
                .try_into()
                .context("PR: Failed to cast usize to i64, somehow")?;
            let params = but_bitbucket::SetPullRequestDraftStateParams {
                workspace: owner,
                repo_slug: repo,
                id,
                is_draft: draft,
            };
            but_bitbucket::pr::set_draft_state(preferred_account, params, storage).await
        }
        _ => Err(Error::msg(format!(
            "Setting the draftiness of reviews for forge {forge:?} is not implemented yet.",
        ))),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateForgeReviewParams {
    pub title: String,
    pub body: String,
    pub source_branch: String,
    pub target_branch: String,
    pub draft: bool,
}

fn github_head_owner_and_repo<'a>(
    forge_repo_info: &'a crate::forge::ForgeRepoInfo,
    forge_push_repo_info: &'a Option<crate::forge::ForgeRepoInfo>,
) -> (&'a str, Option<&'a str>) {
    if let Some(forge_push_repo_info) = forge_push_repo_info
        && !forge_push_repo_info.same_repository(forge_repo_info)
    {
        // If there's a push repo defined, it means we're handling a fork.
        // The head owner is the repository were we push the branches to (the fork) and
        // the target repo (the one holding the base branch) is the original repository.
        (
            forge_push_repo_info.owner.as_str(),
            Some(forge_push_repo_info.repo.as_str()),
        )
    } else {
        // If there's no push repo, we assume the owner is the same as the owner of the target repo.
        // We don't need a `head_repo`` in that case.
        (forge_repo_info.owner.as_str(), None)
    }
}

/// Create a new review (e.g. pull request) for a given forge repository
///
/// Some info on the push repo:
/// If there's a push repository specified and it's different from the main repository,
/// we assume we're opening a review from a fork. Both repositories must use the same
/// forge provider and server.
pub async fn create_forge_review(
    preferred_forge_user: &Option<crate::ForgeUser>,
    forge_repo_info: &crate::forge::ForgeRepoInfo,
    forge_push_repo_info: &Option<crate::forge::ForgeRepoInfo>,
    params: &CreateForgeReviewParams,
    storage: &but_forge_storage::Controller,
) -> Result<ForgeReview> {
    if matches!(
        forge_repo_info.forge,
        ForgeName::GitHub | ForgeName::GitLab | ForgeName::Bitbucket
    ) && let Some(push) = forge_push_repo_info
        && !push.same_server(forge_repo_info)
    {
        anyhow::bail!("Review base and push repositories must use the same forge server.");
    }
    let crate::forge::ForgeRepoInfo {
        forge, owner, repo, ..
    } = forge_repo_info;
    // Private SSH cannot constrain the API port. If the push URL supplies it, select
    // against that URL so the one client used for both repositories matches it.
    let account_repo = forge_push_repo_info
        .as_ref()
        .filter(|push| {
            push.forge == *forge
                && forge_repo_info.identity_port().is_none()
                && push.identity_port().is_some()
        })
        .unwrap_or(forge_repo_info);
    let preferred_forge_user = resolve_account(preferred_forge_user, account_repo, storage)?;
    match forge {
        ForgeName::GitHub => {
            let (head_owner, head_repo) =
                github_head_owner_and_repo(forge_repo_info, forge_push_repo_info);

            let head = format!("{}:{}", head_owner, params.source_branch);
            let pr_params = but_github::CreatePullRequestParams {
                owner,
                repo,
                title: &params.title,
                body: &params.body,
                head: &head,
                head_repo,
                base: &params.target_branch,
                draft: params.draft,
            };
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.github());
            let pr = but_github::pr::create(preferred_account, pr_params, storage).await?;
            Ok(ForgeReview::from(pr))
        }
        ForgeName::GitLab => {
            let project_id = GitLabProjectId::new(owner, repo);
            // If there's a push repo defined, we consider that the source repository.
            let source_project_id = forge_push_repo_info
                .as_ref()
                .map(|repo_info| GitLabProjectId::new(&repo_info.owner, &repo_info.repo));

            let mr_params = but_gitlab::CreateMergeRequestParams {
                project_id,
                title: &params.title,
                body: &params.body,
                source_branch: &params.source_branch,
                target_branch: &params.target_branch,
                source_project_id,
                draft: params.draft,
            };
            let preferred_account = preferred_forge_user.as_ref().and_then(|user| user.gitlab());
            let mr = but_gitlab::mr::create(preferred_account, mr_params, storage).await?;
            Ok(ForgeReview::from(mr))
        }
        ForgeName::Bitbucket => {
            let preferred_account = preferred_forge_user
                .as_ref()
                .and_then(|user| user.bitbucket());
            // When opening from a fork, the source repository is the push repo
            // (`workspace/repo_slug`).
            let source_repo_full_name = forge_push_repo_info
                .as_ref()
                .filter(|push| !push.same_repository(forge_repo_info))
                .map(|push| format!("{}/{}", push.owner, push.repo));

            let pr_params = but_bitbucket::CreatePullRequestParams {
                workspace: owner,
                repo_slug: repo,
                title: &params.title,
                body: &params.body,
                source_branch: &params.source_branch,
                target_branch: &params.target_branch,
                source_repo_full_name: source_repo_full_name.as_deref(),
                draft: params.draft,
            };
            let pr = but_bitbucket::pr::create(preferred_account, pr_params, storage).await?;
            Ok(ForgeReview::from(pr))
        }
        _ => Err(Error::msg(format!(
            "Creating reviews for forge {forge:?} is not implemented yet.",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn p(path: &str) -> &Path {
        Path::new(path)
    }

    fn cached_review(number: i64, merged_at: Option<&str>, closed_at: Option<&str>) -> ForgeReview {
        ForgeReview {
            html_url: format!("https://github.com/o/r/pull/{number}"),
            number,
            title: format!("PR {number}"),
            body: None,
            author: None,
            labels: vec![],
            draft: false,
            source_branch: format!("branch-{number}"),
            target_branch: "main".to_string(),
            sha: "0000000000000000000000000000000000000000".to_string(),
            integration_commit_shas: vec![],
            created_at: None,
            modified_at: None,
            merged_at: merged_at.map(Into::into),
            closed_at: closed_at.map(Into::into),
            repository_ssh_url: None,
            repository_https_url: None,
            repo_owner: None,
            head_repo_is_fork: false,
            reviewers: vec![],
            auto_merge_enabled: false,
            unit_symbol: "#".to_string(),
            last_sync_at: chrono::NaiveDateTime::default(),
        }
    }

    #[test]
    fn only_cached_open_reviews_get_their_fate_recorded() {
        let timestamp = Some("2026-08-24T00:00:00Z");
        let settled = vec![
            cached_review(1, timestamp, None), // merged, cache tracks it as open
            cached_review(2, timestamp, None), // merged, cache never saw it
            cached_review(3, None, timestamp), // closed unmerged, cache tracks it as open
            cached_review(4, timestamp, None), // merged, fate already recorded
            cached_review(5, None, None),      // still open (a forge listing quirk)
        ];
        let cached_states = [(1, false), (3, false), (4, true), (9, false)];

        let recorded: Vec<i64> = fates_to_record(&settled, &cached_states)
            .iter()
            .map(|review| review.number)
            .collect();

        assert_eq!(
            recorded,
            vec![1, 3],
            "a tracked review's fate is recorded once; unknown reviews would claim same-named local branches and already-settled rows need no re-write"
        );
    }

    #[test]
    fn a_sweep_is_due_exactly_when_a_cached_open_review_left_the_listing() {
        let listing = vec![cached_review(1, None, None)];

        assert!(
            !open_review_vanished(&[(1, false), (2, true)], &listing),
            "still-listed and already-settled rows demand no sweep request"
        );
        assert!(
            open_review_vanished(&[(1, false), (3, false)], &listing),
            "an open row missing from the listing is about to be deleted, so its fate must be swept first"
        );
    }

    #[test]
    fn only_outage_shaped_sync_failures_serve_the_stale_cache() {
        let not_authenticated =
            anyhow::anyhow!("no accounts").context(but_error::Context::new_static(
                but_error::Code::ForgeNotAuthenticated,
                "Not authenticated.",
            ));
        let network = anyhow::anyhow!("connection refused").context(
            but_error::Context::new_static(but_error::Code::NetworkError, "Unable to connect."),
        );
        let api_failure = anyhow::anyhow!("Failed to list open pull requests: 500");

        assert!(
            serves_stale_reviews(&not_authenticated, false),
            "missing credentials say nothing about the forge, even an empty cache stays valid"
        );
        assert!(
            serves_stale_reviews(&network, true),
            "a transient outage must not wipe out the last known open reviews"
        );
        assert!(
            !serves_stale_reviews(&network, false),
            "an empty cache has nothing usable, cold listings still surface the outage"
        );
        assert!(
            !serves_stale_reviews(&api_failure, true),
            "a forge-side failure can require user action and must be surfaced"
        );
    }

    #[test]
    fn github_submission_states_map_and_unknowns_drop() {
        use ForgeReviewSubmissionState as S;
        assert_eq!(github_submission_state("APPROVED"), Some(S::Approved));
        assert_eq!(
            github_submission_state("CHANGES_REQUESTED"),
            Some(S::ChangesRequested)
        );
        assert_eq!(github_submission_state("COMMENTED"), Some(S::Commented));
        assert_eq!(github_submission_state("DISMISSED"), Some(S::Dismissed));
        // The caller's own draft, and any state GitHub adds later, are omitted.
        assert_eq!(github_submission_state("PENDING"), None);
        assert_eq!(github_submission_state("APPROVED_WITH_COMMENTS"), None);
    }

    fn repo_info(owner: &str, repo: &str) -> crate::forge::ForgeRepoInfo {
        crate::forge::ForgeRepoInfo {
            forge: crate::forge::ForgeName::GitHub,
            host: "github.com".to_string(),
            port: None,
            owner: owner.to_string(),
            repo: repo.to_string(),
            protocol: "https".to_string(),
        }
    }

    fn review_params() -> CreateForgeReviewParams {
        CreateForgeReviewParams {
            title: "Review".into(),
            body: "private description".into(),
            source_branch: "topic".into(),
            target_branch: "main".into(),
            draft: false,
        }
    }

    fn unusable_storage(tmp: &tempfile::TempDir) -> but_forge_storage::Controller {
        let path = tmp.path().join("unusable-account-storage");
        std::fs::write(&path, "not a directory").unwrap();
        but_forge_storage::Controller::from_path(path)
    }

    #[test]
    fn live_reviews_require_an_account_for_the_repository_host() -> Result<()> {
        tokio::runtime::Runtime::new()?.block_on(async {
            use crate::test_support::{accounts, assert_auth_error};
            let (tmp, storage, accounts) = accounts();
            for account in &accounts {
                let mut repo = repo_info("owner", "repo");
                repo.forge = account.forge_name();
                repo.host = "forge.example.com".into();
                for preferred in std::iter::once(None).chain(accounts.iter().cloned().map(Some)) {
                    for error in [
                        list_forge_reviews(&preferred, &repo, &storage)
                            .await
                            .unwrap_err(),
                        create_forge_review(&preferred, &repo, &None, &review_params(), &storage)
                            .await
                            .unwrap_err(),
                    ] {
                        assert_auth_error(error);
                    }
                    for payload in [
                        ReviewUpdatePayload::new(Some(String::new()), None, None, None),
                        ReviewUpdatePayload::new(None, Some(String::new()), None, None),
                        ReviewUpdatePayload::new(None, None, None, Some("main".into())),
                        ReviewUpdatePayload::new(None, None, Some(ReviewState::Closed), None),
                        ReviewUpdatePayload::new(
                            Some("Updated title".into()),
                            None,
                            Some(ReviewState::Closed),
                            None,
                        ),
                    ] {
                        assert_auth_error(
                            update_review(&preferred, &repo, 1, payload, &storage)
                                .await
                                .unwrap_err(),
                        );
                    }
                    if repo.forge != ForgeName::Bitbucket {
                        assert_auth_error(
                            update_review(
                                &preferred,
                                &repo,
                                1,
                                ReviewUpdatePayload::new(None, None, None, None),
                                &storage,
                            )
                            .await
                            .unwrap_err(),
                        );
                    }
                    let validity = check_forge_account_is_valid(preferred, &repo, &storage).await?;
                    assert!(
                        matches!(validity, ForgeAccountValidity::NoCredentials),
                        "never validate a different provider or host"
                    );
                }
            }
            let storage = unusable_storage(&tmp);
            for forge in [ForgeName::GitHub, ForgeName::GitLab, ForgeName::Bitbucket] {
                let mut base = repo_info("owner", "repo");
                base.forge = forge;
                base.host = "base.example.com".into();
                let mut push = base.clone();
                push.host = "push.example.com".into();
                let error =
                    create_forge_review(&None, &base, &Some(push), &review_params(), &storage)
                        .await
                        .unwrap_err();
                assert!(
                    error.to_string().contains("same forge server"),
                    "cross-instance publishing must fail before reading account storage: {error:#}"
                );
            }
            let azure = crate::derive_forge_repo_info(
                "git@ssh.dev.azure.com:v3/org/project/repo.git",
                None,
                &[],
            )
            .unwrap();
            let error = create_forge_review(
                &None,
                &azure,
                &Some(repo_info("owner", "repo")),
                &review_params(),
                &storage,
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Creating reviews for forge Azure is not implemented yet.",
                "unsupported publishing must not read the push provider's account storage"
            );
            let storage = but_forge_storage::Controller::from_path(tmp.path());
            storage.add_gitlab_account(
                &but_forge_storage::settings::GitLabAccount::SelfHosted {
                    username: "alice".into(),
                    host: "https://gitlab.private.example:8443/api/v4".into(),
                    access_token_key: "unused-fork-token".into(),
                },
            )?;
            let base = crate::derive_forge_repo_info(
                "git@gitlab.private.example:target/repo.git",
                None,
                &[],
            )
            .unwrap();
            let push = crate::derive_forge_repo_info(
                "https://gitlab.private.example:9443/fork/repo.git",
                None,
                &[],
            )
            .unwrap();
            // The SSH base cannot constrain the port; the HTTP fork must reject the 8443 client.
            let error = create_forge_review(&None, &base, &Some(push), &review_params(), &storage)
                .await
                .unwrap_err();
            assert_auth_error(error);
            Ok(())
        })
    }

    #[test]
    fn test_github_head_owner_and_repo_without_push_repo() {
        let forge_repo_info = repo_info("target-owner", "target-repo");

        let (head_owner, head_repo) = github_head_owner_and_repo(&forge_repo_info, &None);

        assert_eq!(head_owner, "target-owner");
        assert_eq!(head_repo, None);
    }

    #[test]
    fn test_github_head_owner_and_repo_with_fork_push_repo() {
        let forge_repo_info = repo_info("target-owner", "target-repo");
        for (owner, repo) in [
            ("fork-owner", "fork-repo"),
            ("fork-owner", "target-repo"),
            ("target-owner", "fork-repo"),
        ] {
            let forge_push_repo_info = Some(repo_info(owner, repo));
            let (head_owner, head_repo) =
                github_head_owner_and_repo(&forge_repo_info, &forge_push_repo_info);

            assert_eq!(
                (head_owner, head_repo),
                (owner, Some(repo)),
                "a different owner or repository requires fork head parameters"
            );
        }
    }

    #[test]
    fn test_github_head_owner_and_repo_with_equal_push_repo() {
        let mut forge_repo_info = repo_info("target-owner", "target-repo");
        forge_repo_info.host = "github.private.example".into();
        forge_repo_info.port = Some(8443);
        for (protocol, port) in [("https", Some(8443)), ("ssh", Some(2222))] {
            let mut push = forge_repo_info.clone();
            push.protocol = protocol.into();
            push.port = port;
            let forge_push_repo_info = Some(push);
            let (head_owner, head_repo) =
                github_head_owner_and_repo(&forge_repo_info, &forge_push_repo_info);

            assert_eq!(
                (head_owner, head_repo),
                ("target-owner", None),
                "a private SSH port cannot distinguish API repositories or imply a fork"
            );
        }
        let mut push = forge_repo_info.clone();
        push.port = Some(9443);
        let forge_push_repo_info = Some(push);
        let (_, head_repo) = github_head_owner_and_repo(&forge_repo_info, &forge_push_repo_info);
        assert_eq!(
            head_repo,
            Some("target-repo"),
            "different known API ports must not identify the same repository"
        );
    }

    #[test]
    fn test_is_valid_review_template_path_github() {
        assert!(is_valid_review_template_path_github(p(
            ".github/PULL_REQUEST_TEMPLATE.md"
        )));
        assert!(is_valid_review_template_path_github(p(
            ".github/pull_request_template.md"
        )));
        assert!(is_valid_review_template_path_github(p(
            ".github/PULL_REQUEST_TEMPLATE/something.md"
        )));
        assert!(is_valid_review_template_path_github(p(
            ".docs/PULL_REQUEST_TEMPLATE.md"
        )));
        assert!(is_valid_review_template_path_github(p(
            "PULL_REQUEST_TEMPLATE.md"
        )));
        assert!(!is_valid_review_template_path_github(p("README.md"),));
    }

    #[test]
    fn test_is_valid_review_template_path_github_windows() {
        assert!(is_valid_review_template_path_github(p(
            ".github\\PULL_REQUEST_TEMPLATE.md"
        ),));
        assert!(is_valid_review_template_path_github(p(
            ".github\\pull_request_template.md"
        ),));
        assert!(is_valid_review_template_path_github(p(
            ".github\\PULL_REQUEST_TEMPLATE\\something.md"
        ),));
        assert!(is_valid_review_template_path_github(p(
            ".docs\\PULL_REQUEST_TEMPLATE.md"
        ),));
        assert!(is_valid_review_template_path_github(p(
            "PULL_REQUEST_TEMPLATE.md"
        ),));
        assert!(!is_valid_review_template_path_github(p("README.md"),));
    }

    #[test]
    fn test_is_valid_review_template_path_gitlab() {
        assert!(is_valid_review_template_path_gitlab(p(
            ".gitlab/merge_request_templates/Default.md"
        )));
        assert!(is_valid_review_template_path_gitlab(p(
            ".gitlab/merge_request_templates/Documentation.md"
        )));
        assert!(is_valid_review_template_path_gitlab(p(
            ".gitlab/merge_request_templates/Security Fix.md"
        )));
        assert!(!is_valid_review_template_path_gitlab(p("README.md")));
        assert!(!is_valid_review_template_path_gitlab(p(
            ".gitlab/issue_templates/Bug.md"
        )));
        assert!(!is_valid_review_template_path_gitlab(p(
            ".gitlab/merge_request_templates/Default.txt"
        )));
    }

    #[test]
    fn test_is_valid_review_template_path_gitlab_windows() {
        assert!(is_valid_review_template_path_gitlab(p(
            ".gitlab\\merge_request_templates\\Default.md"
        )));
        assert!(is_valid_review_template_path_gitlab(p(
            ".gitlab\\merge_request_templates\\Documentation.md"
        )));
        assert!(is_valid_review_template_path_gitlab(p(
            ".gitlab\\merge_request_templates\\Security Fix.md"
        )));
        assert!(!is_valid_review_template_path_gitlab(p("README.md")));
        assert!(!is_valid_review_template_path_gitlab(p(
            ".gitlab\\issue_templates\\Bug.md"
        )));
        assert!(!is_valid_review_template_path_gitlab(p(
            ".gitlab\\merge_request_templates\\Default.txt"
        )));
    }

    #[test]
    fn test_get_gitlab_directory_path() {
        let root_path = p("/path/to/project");
        let gitlab_path = get_gitlab_directory_path(root_path);
        assert_eq!(gitlab_path, p("/path/to/project/.gitlab"));
    }

    #[test]
    fn test_is_review_template_gitlab() {
        // Valid GitLab merge request templates
        assert!(is_review_template_gitlab(
            ".gitlab/merge_request_templates/Default.md"
        ));
        assert!(is_review_template_gitlab(
            ".gitlab/merge_request_templates/Documentation.md"
        ));
        assert!(is_review_template_gitlab(
            ".gitlab/merge_request_templates/Security Fix.md"
        ));

        // Invalid paths
        assert!(!is_review_template_gitlab("README.md"));
        assert!(!is_review_template_gitlab(".gitlab/issue_templates/Bug.md"));
        assert!(!is_review_template_gitlab(
            ".gitlab/merge_request_templates/Default.txt"
        ));
        assert!(!is_review_template_gitlab(
            "merge_request_templates/Default.md"
        ));

        // Windows path separators should work
        assert!(is_review_template_gitlab(
            ".gitlab\\merge_request_templates\\Default.md"
        ));
    }
}
