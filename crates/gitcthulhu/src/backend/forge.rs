//! Forge backend calls (owned by X4; spec 21 §2.10, §5.5, 20 §8.3). The Git-side inputs (remotes, default branch,
//! commit messages) are thin wrappers; the service calls (`ForgeHub` / `RepoForge`) are behind
//! `cfg(feature = "services")` and answer [`Failure::unavailable`] without it.
//!
//! Every function blocks (keychain, SQLite cache, HTTP) and is called through `backend::bg`; none of them may run on
//! the UI thread. The types in [`types`] are plain data: the GUI never names a service type.

use super::{ForgeRemotes, GitError, ObjectId, Repo};

mod types;

#[cfg(all(any(test, feature = "qa"), feature = "services"))]
pub(crate) mod testing;

pub use types::*;

use super::services::ServicesHandle;
#[cfg(feature = "services")]
use gitcthulhu_services::forge as svc;

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

/// The most commit messages the dialog reads for a description.
pub const MAX_COMMIT_MESSAGES: usize = 50;
/// The most branches whose CI checks one refresh asks for (one request each).
#[cfg_attr(not(feature = "services"), allow(dead_code))]
const MAX_CHECKED_BRANCHES: usize = 20;

/// The number of connected forge accounts (no network, no keychain).
#[cfg(feature = "services")]
pub fn account_count(services: &ServicesHandle) -> Result<usize, String> {
    services
        .services()
        .forge
        .accounts()
        .map(|accounts| accounts.len())
        .map_err(|e| e.to_string())
}

/// The owner part of a remote URL: `acme` of `https://github.com/acme/widgets.git`,
/// `git@github.com:acme/widgets.git` and `ssh://git@host:22/acme/widgets`; nested GitLab groups stay joined with
/// `/`. `None` when the URL has no owner/repository path.
pub fn remote_owner(url: &str) -> Option<String> {
    let url = url.trim();
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.split_once(':')?.1,
    };
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, _repo) = path.rsplit_once('/')?;
    (!owner.is_empty()).then(|| owner.to_owned())
}

/// The state of a pull request from the cache's association.
#[cfg(feature = "services")]
fn pr_state(is_open: bool, draft: bool, merged: bool) -> PrState {
    if is_open {
        if draft { PrState::Draft } else { PrState::Open }
    } else if merged {
        PrState::Merged
    } else {
        PrState::Closed
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Services: accounts
// ---------------------------------------------------------------------------------------------------------------

#[cfg(feature = "services")]
fn failure_of(err: svc::ForgeError) -> Failure {
    let kind = match &err {
        svc::ForgeError::NotAuthenticated => FailureKind::NoAccount,
        svc::ForgeError::Unrecognized => FailureKind::NotAForge,
        svc::ForgeError::Network(_) => FailureKind::Offline,
        svc::ForgeError::RateLimited { reset_at } => FailureKind::RateLimited {
            reset_at: *reset_at,
        },
        svc::ForgeError::Auth(_) => FailureKind::Auth,
        svc::ForgeError::Permission(_) => FailureKind::Permission,
        svc::ForgeError::NotFound => FailureKind::NotFound,
        svc::ForgeError::Cancelled => FailureKind::Cancelled,
        _ => FailureKind::Other,
    };
    let message = match &err {
        svc::ForgeError::NotAuthenticated => {
            "No connected account for this repository. Add one in Settings > Accounts.".to_owned()
        }
        svc::ForgeError::Unrecognized => {
            "This remote does not belong to a supported forge.".to_owned()
        }
        svc::ForgeError::Network(m) => format!("The forge could not be reached: {m}"),
        svc::ForgeError::RateLimited { .. } => "The forge's rate limit was reached.".to_owned(),
        svc::ForgeError::NotFound => "The forge does not know this resource.".to_owned(),
        svc::ForgeError::Cancelled => "Cancelled".to_owned(),
        other => format!("{other:#}"),
    };
    Failure { message, kind }
}

#[cfg(feature = "services")]
fn forge_of(name: svc::ForgeName) -> Forge {
    match name {
        svc::ForgeName::GitHub => Forge::GitHub,
        svc::ForgeName::GitLab => Forge::GitLab,
        svc::ForgeName::Bitbucket => Forge::Bitbucket,
        svc::ForgeName::Azure => Forge::Azure,
    }
}

/// `ghe.example.com[:port]` of a host that may be a full API URL.
#[cfg(feature = "services")]
fn display_host(host: &str) -> &str {
    let rest = host.split_once("://").map_or(host, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

#[cfg(feature = "services")]
fn account_info(account: svc::Account) -> Option<AccountInfo> {
    use gitcthulhu_services::forge::ForgeUser;
    let (name, caption) = match &account.user {
        ForgeUser::GitHub(id) => {
            let caption = match id.custom_host() {
                Some(host) => format!("on {}", display_host(&host)),
                None if account.label.ends_with("(token)") => "token".to_owned(),
                None => "browser sign-in".to_owned(),
            };
            (id.username().to_owned(), caption)
        }
        ForgeUser::GitLab(id) => {
            let caption = if account.label.contains(" on ") {
                account
                    .label
                    .split_once(" on ")
                    .map(|(_, host)| format!("on {host}"))
                    .unwrap_or_else(|| "token".to_owned())
            } else {
                "token".to_owned()
            };
            (id.username().to_owned(), caption)
        }
        ForgeUser::Bitbucket(id) => (id.email().to_owned(), "API token".to_owned()),
    };
    let key = serde_json::to_string(&account.user).ok()?;
    Some(AccountInfo {
        forge: forge_of(account.forge),
        name,
        caption,
        avatar_url: account.avatar_url,
        id: AccountId(key),
    })
}

#[cfg(feature = "services")]
fn user_of(id: &AccountId) -> Result<svc::ForgeUser, Failure> {
    serde_json::from_str(&id.0).map_err(|e| Failure::other(format!("unknown account: {e}")))
}

/// Whether the build offers GitHub's device flow (it has an OAuth client id).
pub fn device_flow_available(services: &ServicesHandle) -> bool {
    #[cfg(feature = "services")]
    {
        services.github_device_flow
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = services;
        false
    }
}

/// All connected accounts, GitHub first (the account list only: no network, no keychain).
pub fn accounts(services: &ServicesHandle) -> Result<Vec<AccountInfo>, Failure> {
    #[cfg(feature = "services")]
    {
        let accounts = services.services().forge.accounts().map_err(failure_of)?;
        Ok(accounts.into_iter().filter_map(account_info).collect())
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = services;
        Err(Failure::unavailable())
    }
}

/// Signs in: the token is checked with the forge, then stored in the keychain.
pub fn add_account(services: &ServicesHandle, new: NewAccount) -> Result<AccountInfo, Failure> {
    #[cfg(feature = "services")]
    {
        use but_secret::Sensitive as Secret;
        let hub = &services.services().forge;
        let secret = |token: &super::Sensitive| Secret(token.expose().to_owned());
        let account = match &new {
            NewAccount::GitHubToken { token } => hub.add_github_pat(secret(token)),
            NewAccount::GitHubEnterprise { api_url, token } => {
                hub.add_github_enterprise(api_url, secret(token))
            }
            NewAccount::GitLabToken { token } => hub.add_gitlab_pat(secret(token)),
            NewAccount::GitLabSelfHosted { url, token } => {
                hub.add_gitlab_self_hosted(url, secret(token))
            }
            NewAccount::BitbucketToken { email, token } => {
                hub.add_bitbucket_token(email, secret(token))
            }
        }
        .map_err(failure_of)?;
        account_info(account).ok_or_else(|| Failure::other("the account cannot be shown"))
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (services, new);
        Err(Failure::unavailable())
    }
}

/// Removes an account with its token and cached profile.
pub fn forget_account(services: &ServicesHandle, id: &AccountId) -> Result<(), Failure> {
    #[cfg(feature = "services")]
    {
        let user = user_of(id)?;
        services.services().forge.forget(&user).map_err(failure_of)
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (services, id);
        Err(Failure::unavailable())
    }
}

/// Asks the forge whether the account's token still works.
pub fn check_account(services: &ServicesHandle, id: &AccountId) -> Result<Validity, Failure> {
    #[cfg(feature = "services")]
    {
        let user = user_of(id)?;
        let validity = services.services().forge.check(&user).map_err(failure_of)?;
        Ok(match validity {
            svc::ForgeAccountValidity::Valid => Validity::Valid,
            svc::ForgeAccountValidity::Invalid => Validity::Invalid,
            svc::ForgeAccountValidity::NoCredentials => Validity::NoCredentials,
        })
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (services, id);
        Err(Failure::unavailable())
    }
}

/// Starts a GitHub device-flow sign-in (only when the build has an OAuth client id).
pub fn device_start(services: &ServicesHandle) -> Result<DeviceCodeInfo, Failure> {
    #[cfg(feature = "services")]
    {
        let code = services
            .services()
            .forge
            .github_device_start()
            .map_err(failure_of)?;
        Ok(DeviceCodeInfo {
            user_code: code.user_code,
            device_code: code.device_code,
            verification_uri: code.verification_uri,
            interval: code.interval,
            expires_in: code.expires_in,
        })
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = services;
        Err(Failure::unavailable())
    }
}

/// Polls a device flow once; the hub keeps GitHub's polling interval (an early call answers `Pending`).
pub fn device_poll(
    services: &ServicesHandle,
    code: &DeviceCodeInfo,
    cancel: &super::CancelToken,
) -> Result<DevicePoll, Failure> {
    #[cfg(feature = "services")]
    {
        let code = svc::DeviceCode {
            user_code: code.user_code.clone(),
            device_code: code.device_code.clone(),
            verification_uri: code.verification_uri.clone(),
            interval: code.interval,
            expires_in: code.expires_in,
        };
        match services
            .services()
            .forge
            .github_device_poll(&code, cancel)
            .map_err(failure_of)?
        {
            svc::DevicePoll::Pending => Ok(DevicePoll::Pending),
            svc::DevicePoll::SignedIn(account) => account_info(account)
                .map(DevicePoll::SignedIn)
                .ok_or_else(|| Failure::other("the account cannot be shown")),
        }
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (services, code, cancel);
        Err(Failure::unavailable())
    }
}

/// The account that answers HTTPS prompts of `host` (the account list only: no network, no keychain).
pub fn account_for_host(services: &ServicesHandle, host: &str) -> Option<AccountInfo> {
    #[cfg(feature = "services")]
    {
        services
            .services()
            .forge
            .account_for_host(host)
            .and_then(account_info)
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (services, host);
        None
    }
}

/// The user name and token that answer an HTTPS prompt (20 §6.3). Reads the keychain.
pub fn https_credential(
    services: &ServicesHandle,
    id: &AccountId,
) -> Result<Option<(String, super::Sensitive)>, Failure> {
    #[cfg(feature = "services")]
    {
        let user = user_of(id)?;
        let credential = services
            .services()
            .forge
            .https_credential(&user)
            .map_err(failure_of)?;
        Ok(credential.map(|(name, token)| (name, super::Sensitive::new(token.0))))
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (services, id);
        Err(Failure::unavailable())
    }
}

/// Fetches an image through the forge HTTP stack (blocking).
pub fn fetch_image(services: &ServicesHandle, url: &str) -> Result<Vec<u8>, String> {
    #[cfg(feature = "services")]
    {
        services
            .services()
            .forge
            .fetch_image(url)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (services, url);
        Err("The forge integrations are not available".to_owned())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Services: one repository
// ---------------------------------------------------------------------------------------------------------------

/// The forge of one repository (cheap to clone). Open it again after an account was added or removed.
#[derive(Clone)]
pub struct RepoHandle {
    #[cfg(feature = "services")]
    forge: svc::RepoForge,
    facts: RepoFacts,
    #[cfg_attr(not(feature = "services"), allow(dead_code))]
    worktree: std::path::PathBuf,
    #[cfg_attr(not(feature = "services"), allow(dead_code))]
    template_path: Option<String>,
}

impl RepoHandle {
    /// What the app knows about the forge of the repository.
    pub fn facts(&self) -> &RepoFacts {
        &self.facts
    }
}

/// Detects the forge of `repo` and opens its handle (no network; reads the account list).
pub fn open_repo(
    services: &ServicesHandle,
    repo: &Repo,
    prefs: &RepoPrefsInput,
) -> Result<RepoHandle, Failure> {
    #[cfg(feature = "services")]
    {
        let remotes = forge_remotes(repo).map_err(|e| Failure::other(e.to_string()))?;
        let worktree = repo
            .workdir()
            .unwrap_or_else(|| repo.git_dir())
            .to_path_buf();
        let forge_override = prefs
            .forge_override
            .as_deref()
            .and_then(|text| {
                let text = text.to_ascii_lowercase();
                ["github", "gitlab", "bitbucket"]
                    .into_iter()
                    .find(|kind| text.contains(kind))
                    .map(str::to_owned)
            })
            .map(|kind| match kind.as_str() {
                "github" => svc::ForgeName::GitHub,
                "gitlab" => svc::ForgeName::GitLab,
                _ => svc::ForgeName::Bitbucket,
            });
        let preferred_user = prefs
            .preferred_user
            .clone()
            .and_then(|value| serde_json::from_value(value).ok());
        let input = svc::RepoForgeInput {
            repo_dir: worktree.clone(),
            remotes: remotes.clone(),
            prefs: svc::RepoForgePrefs {
                forge_override,
                preferred_user,
                template_path: prefs.template_path.clone(),
            },
        };
        let forge = services.services().forge.open_repo(input);
        let facts = match forge.info() {
            None => RepoFacts::none(),
            Some(info) => {
                let name = forge_of(info.name.clone());
                let account = match forge.account_status() {
                    svc::RepoAccountStatus::Matching(account) => {
                        AccountMatch::Matching(account.label)
                    }
                    svc::RepoAccountStatus::NoMatchingAccount => AccountMatch::NoMatchingAccount,
                    svc::RepoAccountStatus::NotAForge => AccountMatch::NotAForge,
                };
                let host = info
                    .base_url
                    .split_once("://")
                    .map_or(info.base_url.as_str(), |(_, rest)| rest)
                    .split('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                RepoFacts {
                    forge: Some(name),
                    host,
                    unit_name: info.unit.name.to_lowercase(),
                    unit_symbol: info.unit.symbol.clone(),
                    account,
                    checks: info.capabilities.checks,
                    reviews: name.has_api()
                        && info.capabilities.pr_service
                        && info.capabilities.list_service,
                    base_remote: remotes.base.as_ref().map(|r| r.name.clone()),
                    push_remote: remotes.push.as_ref().map(|r| r.name.clone()),
                    head_owner: remotes
                        .push
                        .as_ref()
                        .or(remotes.base.as_ref())
                        .and_then(|r| remote_owner(&r.url)),
                }
            }
        };
        Ok(RepoHandle {
            forge,
            facts,
            worktree,
            template_path: prefs.template_path.clone(),
        })
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (services, repo, prefs);
        Err(Failure::unavailable())
    }
}

impl RepoHandle {
    /// The web URL of a review.
    pub fn url_review(&self, number: i64) -> Option<String> {
        #[cfg(feature = "services")]
        {
            self.forge.url_review(number)
        }
        #[cfg(not(feature = "services"))]
        {
            let _ = number;
            None
        }
    }

    /// The web URL of a commit.
    pub fn url_commit(&self, sha: &str) -> Option<String> {
        #[cfg(feature = "services")]
        {
            self.forge.url_commit(sha)
        }
        #[cfg(not(feature = "services"))]
        {
            let _ = sha;
            None
        }
    }

    /// The web URL that starts a new review of `head_branch` into `base` (fork-aware).
    pub fn url_new_review(&self, base: &str, head_branch: &str) -> Option<String> {
        #[cfg(feature = "services")]
        {
            self.forge.url_new_review(base, head_branch)
        }
        #[cfg(not(feature = "services"))]
        {
            let _ = (base, head_branch);
            None
        }
    }

    /// Refreshes the reviews and the CI checks of the open reviews of `queries` from the forge into the cache.
    /// Network failures that are about one branch's checks (no permission, unknown ref) do not fail the refresh.
    pub fn refresh(
        &self,
        repo: &Repo,
        queries: &[BranchQuery],
        cancel: &super::CancelToken,
    ) -> Result<RefreshReport, Failure> {
        #[cfg(feature = "services")]
        {
            use std::collections::{HashMap, HashSet};
            let outcome = self.forge.refresh_reviews(cancel).map_err(failure_of)?;
            if self.facts.checks {
                let associations = self
                    .forge
                    .review_associations(&repo.local())
                    .map_err(failure_of)?;
                let open: HashMap<i64, svc::ForgeReview> = self
                    .forge
                    .cached_reviews()
                    .map_err(failure_of)?
                    .into_iter()
                    .map(|review| (review.number, review))
                    .collect();
                let mut refs: HashSet<String> = open
                    .values()
                    .map(|review| review.source_branch.clone())
                    .collect();
                let mut asked = HashSet::new();
                for query in queries {
                    if asked.len() >= MAX_CHECKED_BRANCHES {
                        break;
                    }
                    let Some((number, true, _)) = associations.get(&query.remote_key) else {
                        continue;
                    };
                    let Some(review) = i64::try_from(*number).ok().and_then(|n| open.get(&n))
                    else {
                        continue;
                    };
                    if !asked.insert(review.source_branch.clone()) {
                        continue;
                    }
                    let head = review.head_repo_is_fork.then_some(review.sha.as_str());
                    match self
                        .forge
                        .refresh_checks(&review.source_branch, head, cancel)
                    {
                        Ok(_) => {}
                        Err(svc::ForgeError::NotFound | svc::ForgeError::Permission(_)) => {}
                        Err(other) => return Err(failure_of(other)),
                    }
                }
                refs.extend(asked);
                self.forge.prune_checks(&refs).map_err(failure_of)?;
            }
            Ok(RefreshReport {
                fingerprint: outcome.fingerprint,
                last_merged: outcome.last_merged,
            })
        }
        #[cfg(not(feature = "services"))]
        {
            let _ = (repo, queries, cancel);
            Err(Failure::unavailable())
        }
    }

    /// The pull requests of `queries` from the cache (no network): `(branch full name, badge)`, in the order of the
    /// queries. A merged review shows only on a branch whose tip is exactly where it merged.
    pub fn badges(
        &self,
        repo: &Repo,
        queries: &[BranchQuery],
    ) -> Result<Vec<(String, ReviewBadge)>, Failure> {
        #[cfg(feature = "services")]
        {
            use std::collections::HashMap;
            let associations = self
                .forge
                .review_associations(&repo.local())
                .map_err(failure_of)?;
            let open: HashMap<i64, svc::ForgeReview> = self
                .forge
                .cached_reviews()
                .map_err(failure_of)?
                .into_iter()
                .map(|review| (review.number, review))
                .collect();
            let mut found = Vec::new();
            for query in queries {
                let Some((number, is_open, merged_head)) = associations.get(&query.remote_key)
                else {
                    continue;
                };
                let Ok(number) = i64::try_from(*number) else {
                    continue;
                };
                let review = open.get(&number);
                let state = pr_state(
                    *is_open,
                    review.is_some_and(|r| r.draft),
                    merged_head.is_some(),
                );
                let merged_head = merged_head.clone().map(|sha| sha.to_ascii_lowercase());
                if state == PrState::Merged
                    && merged_head.as_deref() != Some(&query.tip.to_ascii_lowercase())
                {
                    continue;
                }
                let checks = match (review, self.facts.checks) {
                    (Some(review), true) => self
                        .forge
                        .cached_checks(&review.source_branch)
                        .map(|checks| svc::RepoForge::checks_summary(&checks))
                        .map(|sum| CheckCounts {
                            passed: sum.passed,
                            running: sum.running,
                            failed: sum.failed,
                            queued: sum.queued,
                        })
                        .unwrap_or_default(),
                    _ => CheckCounts::default(),
                };
                found.push((
                    query.local.clone(),
                    ReviewBadge {
                        number,
                        symbol: self.facts.unit_symbol.clone(),
                        state,
                        title: review.map(|r| r.title.clone()),
                        checks,
                        url: self.forge.url_review(number),
                        merged_head,
                    },
                ));
            }
            Ok(found)
        }
        #[cfg(not(feature = "services"))]
        {
            let _ = (repo, queries);
            Err(Failure::unavailable())
        }
    }

    /// Opens a pull request. The new review is cached right away, so the badge shows before the next refresh.
    pub fn create_review(&self, review: NewReview) -> Result<CreatedReview, Failure> {
        #[cfg(feature = "services")]
        {
            let created = self
                .forge
                .create_review(svc::CreateReview {
                    title: review.title,
                    body: review.body,
                    source_branch: review.source_branch,
                    target_branch: review.target_branch,
                    draft: review.draft,
                })
                .map_err(failure_of)?;
            let url = (!created.html_url.is_empty())
                .then(|| created.html_url.clone())
                .or_else(|| self.forge.url_review(created.number));
            Ok(CreatedReview {
                number: created.number,
                symbol: self.facts.unit_symbol.clone(),
                title: created.title,
                url,
            })
        }
        #[cfg(not(feature = "services"))]
        {
            let _ = review;
            Err(Failure::unavailable())
        }
    }

    /// How to check pull request `number` out (the review comes from the cache, else from the forge).
    pub fn checkout_plan(&self, number: i64) -> Result<CheckoutPlanInfo, Failure> {
        #[cfg(feature = "services")]
        {
            let plan = self.forge.checkout_plan(number).map_err(failure_of)?;
            Ok(CheckoutPlanInfo {
                remote_name: plan.remote_name,
                remote_url: plan.remote_url,
                fetch_refspec: plan.fetch_refspec,
                local_branch: plan.local_branch,
                fetched_ref: plan.fetched_ref,
                upstream: plan.upstream,
            })
        }
        #[cfg(not(feature = "services"))]
        {
            let _ = number;
            Err(Failure::unavailable())
        }
    }

    /// The pull request templates of the worktree (relative paths) and the text of the one to start from: the
    /// repository's configured template, else the first found.
    pub fn template(&self) -> Option<(String, String)> {
        #[cfg(feature = "services")]
        {
            let found = self.forge.templates(&self.worktree);
            let choice = self
                .template_path
                .clone()
                .filter(|path| found.contains(path))
                .or_else(|| found.into_iter().next())?;
            let text = self.forge.read_template(&self.worktree, &choice).ok()?;
            Some((choice, text))
        }
        #[cfg(not(feature = "services"))]
        {
            None
        }
    }

    /// The default base branch: the remote's default branch, else `main`.
    pub fn default_target(&self, repo: &Repo) -> String {
        let remote = self.facts.base_remote.clone().unwrap_or_default();
        let default = default_branch(repo, &remote).ok().flatten();
        #[cfg(feature = "services")]
        {
            self.forge.default_review_target(default.as_deref())
        }
        #[cfg(not(feature = "services"))]
        {
            default.unwrap_or_else(|| "main".to_owned())
        }
    }

    /// What the create-PR dialog starts from for `head` against `base` on the base remote.
    pub fn draft(&self, repo: &Repo, head: ObjectId, base: Option<&str>) -> PrDraft {
        let base = base
            .map(str::to_owned)
            .unwrap_or_else(|| self.default_target(repo));
        let template = self.template();
        PrDraft {
            commit_messages: self.messages_since(repo, head, &base),
            template: template.as_ref().map(|(path, _)| path.clone()),
            template_text: template.map(|(_, text)| text),
            base,
        }
    }

    /// The messages of the commits of `head` that the base branch does not have (oldest first); empty when the base
    /// branch is not known locally.
    pub fn messages_since(&self, repo: &Repo, head: ObjectId, base: &str) -> Vec<String> {
        let Some(remote) = &self.facts.base_remote else {
            return Vec::new();
        };
        let spec = format!("refs/remotes/{remote}/{base}");
        let Ok(tip) = gitcthulhu_git::refs::resolve_commit(repo, &spec) else {
            return Vec::new();
        };
        commit_messages(repo, Some(tip), head, MAX_COMMIT_MESSAGES).unwrap_or_default()
    }
}

/// Whether a remote of that name is configured.
pub fn remote_exists(repo: &Repo, name: &str) -> bool {
    use bstr::ByteSlice as _;
    repo.local()
        .remote_names()
        .iter()
        .any(|existing| existing.to_str_lossy() == name)
}

/// Checks a pull request out (20 §8.3): adds the remote if it is new, fetches the plan's refspec, creates the local
/// branch at the fetched commit and checks it out (never `--track`: the pull request's own ref is outside the
/// remote's fetch refspec), and sets the upstream only when the fetched ref is a remote-tracking branch. Needs the
/// repository guard; the caller records it as a branch creation with checkout.
pub fn checkout_pull_request(
    repo: &Repo,
    perm: &mut super::RepoExclusive,
    ctx: &super::OpContext,
    plan: &CheckoutPlanInfo,
) -> Result<super::OpOutcome<super::FullName>, GitError> {
    use gitcthulhu_git::{
        FetchOptions, FetchTarget, StartPoint, TagFetch, branch, network, refs, remote,
    };

    let mut log = super::CommandLog::default();
    let mut refresh = super::RefreshSet::empty();
    let mut warnings = Vec::new();
    macro_rules! absorb {
        ($outcome:expr) => {{
            let outcome = $outcome;
            refresh |= outcome.refresh;
            log.commands.extend(outcome.log.commands);
            log.hooks.extend(outcome.log.hooks);
            warnings.extend(outcome.warnings);
            outcome.value
        }};
    }
    if !remote_exists(repo, &plan.remote_name) {
        absorb!(remote::add_remote(
            repo,
            perm,
            ctx,
            &plan.remote_name,
            &plan.remote_url,
            None
        )?);
        refresh |= super::RefreshSet::REMOTES | super::RefreshSet::CONFIG;
    }
    absorb!(network::fetch(
        repo,
        ctx,
        &FetchOptions {
            target: FetchTarget::Remote(plan.remote_name.clone()),
            prune: Some(false),
            tags: TagFetch::None,
            refspecs: vec![plan.fetch_refspec.clone()],
        },
    )?);
    refresh |= super::RefreshSet::REMOTES | super::RefreshSet::LOG | super::RefreshSet::BRANCHES;
    let commit = refs::resolve_commit(repo, &plan.fetched_ref)?;
    let created = absorb!(branch::create_branch(
        repo,
        perm,
        ctx,
        &plan.local_branch,
        &StartPoint::Commit(commit),
        true,
    )?);
    if let Some(upstream) = &plan.upstream {
        let upstream =
            super::FullName::try_from(upstream.clone()).map_err(|_| GitError::InvalidName {
                what: "ref",
                name: upstream.clone(),
            })?;
        absorb!(branch::set_upstream(
            repo,
            perm,
            ctx,
            &created,
            Some(&upstream)
        )?);
    }
    refresh |= super::RefreshSet::CONFIG;
    Ok(super::OpOutcome {
        value: created,
        refresh,
        log,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn remote_owner_reads_https_scp_and_ssh_urls() {
        for (url, owner) in [
            ("https://github.com/acme/widgets.git", Some("acme")),
            ("https://github.com/acme/widgets", Some("acme")),
            ("git@github.com:acme/widgets.git", Some("acme")),
            (
                "ssh://git@ghe.example.com:22/acme/widgets.git",
                Some("acme"),
            ),
            ("https://gitlab.com/group/sub/proj.git", Some("group/sub")),
            ("https://github.com/widgets", None),
            ("", None),
        ] {
            assert_eq!(remote_owner(url).as_deref(), owner, "owner of {url:?}");
        }
    }

    #[test]
    fn check_counts_make_a_traffic_light_and_a_description() {
        let none = CheckCounts::default();
        assert_eq!((none.state(), none.describe()), (None, None));
        let mixed = CheckCounts {
            passed: 3,
            failed: 1,
            running: 0,
            queued: 2,
        };
        assert_eq!(mixed.state(), Some(CiState::Failed), "a failure wins");
        assert_eq!(
            mixed.describe().as_deref(),
            Some("3 passed, 1 failed, 2 queued")
        );
        let waiting = CheckCounts {
            passed: 2,
            queued: 1,
            ..CheckCounts::default()
        };
        assert_eq!(waiting.state(), Some(CiState::Running));
        let ok = CheckCounts {
            passed: 2,
            ..CheckCounts::default()
        };
        assert_eq!(ok.state(), Some(CiState::Passed));
    }

    #[test]
    fn new_account_debug_hides_tokens() {
        let account = NewAccount::GitHubToken {
            token: super::super::Sensitive::new("ghp_secret".into()),
        };
        assert!(!format!("{account:?}").contains("ghp_secret"));
    }
}
