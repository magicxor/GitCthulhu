//! [`ForgeHub`]: accounts, sign-in flows, forge detection and image downloads.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use but_forge::{ForgeInfo, ForgeName, ForgeUser, all_forge_accounts};
use but_forge_storage::Controller;
use but_secret::Sensitive;
use gitcthulhu_git::CancelToken;

use super::{
    Account, DeviceCode, DevicePoll, ForgeAccountValidity, ForgeConfig, ForgeDirs, ForgeError,
    RepoForge, RepoForgeInput, image,
};
use crate::Runtime;

/// How much slower GitHub's device flow is polled after `slow_down` when it names no new interval.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// Process-wide forge entry point. Cheap to clone.
#[derive(Clone)]
pub struct ForgeHub {
    pub(super) inner: Arc<Inner>,
}

pub(super) struct Inner {
    pub(super) rt: Runtime,
    pub(super) storage: Controller,
    pub(super) dirs: ForgeDirs,
    cfg: ForgeConfig,
    /// Forge refreshes run one at a time: they serialise keychain reads (one prompt) and respect rate limits.
    pub(super) refresh: tokio::sync::Mutex<()>,
    /// The device flows in progress by device code: they keep the polling interval GitHub asks for.
    device_flows: Mutex<HashMap<String, DeviceFlow>>,
    http: reqwest::Client,
}

/// Where a device flow stands.
struct DeviceFlow {
    /// The minimum time between two polls.
    interval: Duration,
    /// Polls before this return `Pending` without asking GitHub.
    next_poll: Instant,
    /// When the device code expires.
    expires: Instant,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ForgeHub {
    /// Create the hub on the shared runtime.
    ///
    /// The account list is read from `dirs.app_data` (created when the first account is added); the
    /// keychain is not touched. The keychain namespace is the process-wide one that
    /// [`Services::new`](crate::Services::new) sets.
    pub fn new(rt: Runtime, dirs: ForgeDirs, cfg: ForgeConfig) -> Result<Self, ForgeError> {
        let http = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .build()
            .map_err(|err| ForgeError::Other(err.into()))?;
        Ok(ForgeHub {
            inner: Arc::new(Inner {
                rt,
                storage: Controller::from_path(&dirs.app_data),
                dirs,
                cfg,
                refresh: tokio::sync::Mutex::new(()),
                device_flows: Mutex::new(HashMap::new()),
                http,
            }),
        })
    }

    /// The known accounts as the [`ForgeUser`]s of the forge crates: the list only, no secrets.
    pub(super) fn users(&self) -> Result<Vec<ForgeUser>, ForgeError> {
        Ok(all_forge_accounts(&self.inner.storage)?)
    }

    /// The [`Account`] of `user`, with its label and the avatar of its cached profile.
    pub(super) fn account(&self, user: ForgeUser) -> Account {
        let avatar_url = self
            .inner
            .storage
            .cached_profile(&profile_key(&user))
            .ok()
            .flatten()
            .and_then(|profile| profile.avatar_url);
        Account {
            forge: user.forge_name(),
            label: label(&user),
            user,
            avatar_url,
        }
    }

    /// All signed-in accounts, GitHub first, then GitLab, then Bitbucket. Reads the account list only: no
    /// network, no keychain.
    pub fn accounts(&self) -> Result<Vec<Account>, ForgeError> {
        Ok(self
            .users()?
            .into_iter()
            .map(|user| self.account(user))
            .collect())
    }

    /// The account to use for HTTPS requests to `host` (20 section 6.3 optional HTTPS answer): the first
    /// account whose forge instance is `host`. `host` may carry a port (`git.example.com:8443`).
    ///
    /// Public accounts answer for `github.com`, `gitlab.com` and `bitbucket.org`; a GitHub Enterprise or
    /// self-hosted GitLab account for the host of its API URL (or the host without its `api.` prefix).
    /// `None` when no account matches, or the account list cannot be read.
    pub fn account_for_host(&self, host: &str) -> Option<Account> {
        let (host, port) = split_host_port(host.trim());
        if host.is_empty() {
            return None;
        }
        [ForgeName::GitHub, ForgeName::GitLab, ForgeName::Bitbucket]
            .into_iter()
            .find_map(|forge| {
                let repo = but_forge::ForgeRepoInfo {
                    forge,
                    host: host.to_owned(),
                    port,
                    owner: String::new(),
                    repo: String::new(),
                    protocol: "https".into(),
                };
                but_forge::find_forge_account(&None, &repo, &self.inner.storage)
                    .ok()
                    .flatten()
            })
            .map(|user| self.account(user))
    }

    /// The HTTPS answer for `user` (20 section 6.3): the user name Git is asked for and the account's token.
    /// Reads the keychain: blocking, off the UI thread only. `Ok(None)` when no token is stored.
    ///
    /// The user name is the login for GitHub, `oauth2` for GitLab and `x-bitbucket-api-token-auth` for
    /// Bitbucket Cloud.
    pub fn https_credential(
        &self,
        user: &ForgeUser,
    ) -> Result<Option<(String, Sensitive<String>)>, ForgeError> {
        let storage = &self.inner.storage;
        Ok(match user {
            ForgeUser::GitHub(id) => but_github::get_gh_access_token(id, storage)?
                .map(|token| (id.username().to_owned(), token)),
            ForgeUser::GitLab(id) => but_gitlab::get_gl_access_token(id, storage)?
                .map(|token| ("oauth2".to_owned(), token)),
            ForgeUser::Bitbucket(id) => but_bitbucket::get_bb_access_token(id, storage)?
                .map(|token| ("x-bitbucket-api-token-auth".to_owned(), token)),
        })
    }

    /// Start a GitHub device-flow sign-in (needs `ForgeConfig::github_oauth_client_id`): show the user
    /// `user_code` and `verification_uri`, then call [`Self::github_device_poll()`] until it answers.
    pub fn github_device_start(&self) -> Result<DeviceCode, ForgeError> {
        let client_id = self.client_id()?;
        let verification = self
            .inner
            .rt
            .block_on(but_github::init_github_device_oauth(client_id))?;
        let interval = Duration::from_secs(verification.interval);
        let now = Instant::now();
        {
            let mut flows = lock(&self.inner.device_flows);
            flows.retain(|_, flow| flow.expires > now);
            flows.insert(
                verification.device_code.clone(),
                DeviceFlow {
                    interval,
                    next_poll: now + interval,
                    expires: now + Duration::from_secs(verification.expires_in),
                },
            );
        }
        Ok(DeviceCode {
            user_code: verification.user_code,
            device_code: verification.device_code,
            verification_uri: verification.verification_uri,
            interval: verification.interval,
            expires_in: verification.expires_in,
        })
    }

    /// Poll a GitHub device flow. The polling interval GitHub asks for is kept here: a call that comes
    /// sooner (or after a `slow_down`) answers [`DevicePoll::Pending`] without a request.
    ///
    /// An expired, denied or rejected sign-in is [`ForgeError::Auth`]. When the user confirmed, the account
    /// is stored (the token goes to the keychain) and returned.
    pub fn github_device_poll(
        &self,
        code: &DeviceCode,
        cancel: &CancelToken,
    ) -> Result<DevicePoll, ForgeError> {
        let client_id = self.client_id()?;
        let now = Instant::now();
        {
            let mut flows = lock(&self.inner.device_flows);
            flows.retain(|_, flow| flow.expires > now);
            let flow = flows
                .entry(code.device_code.clone())
                // Started by another hub or before a restart: trust the code's own numbers.
                .or_insert_with(|| DeviceFlow {
                    interval: Duration::from_secs(code.interval),
                    next_poll: now,
                    expires: now + Duration::from_secs(code.expires_in),
                });
            if now < flow.next_poll {
                return Ok(DevicePoll::Pending);
            }
            flow.next_poll = now + flow.interval;
        }
        let answer = self.inner.rt.block_on_cancellable(
            cancel,
            but_github::check_github_auth_status(client_id, &code.device_code, &self.inner.storage),
        )?;
        match answer {
            Ok(signed_in) => {
                lock(&self.inner.device_flows).remove(&code.device_code);
                let user =
                    ForgeUser::GitHub(but_github::GithubAccountIdentifier::oauth(&signed_in.login));
                Ok(DevicePoll::SignedIn(self.account(user)))
            }
            Err(err) => match err.downcast_ref::<but_github::DeviceFlowPending>() {
                Some(pending) => {
                    if let Some(flow) = lock(&self.inner.device_flows).get_mut(&code.device_code) {
                        if pending.slow_down {
                            flow.interval = pending
                                .interval
                                .map_or(flow.interval + SLOW_DOWN_STEP, Duration::from_secs);
                        }
                        flow.next_poll = Instant::now() + flow.interval;
                    }
                    Ok(DevicePoll::Pending)
                }
                None => {
                    lock(&self.inner.device_flows).remove(&code.device_code);
                    Err(err.into())
                }
            },
        }
    }

    fn client_id(&self) -> Result<&str, ForgeError> {
        self.inner
            .cfg
            .github_oauth_client_id
            .as_deref()
            .ok_or_else(|| {
                ForgeError::other(
                    "Signing in with the browser needs a GitHub OAuth app; use a personal access token instead",
                )
            })
    }

    /// Sign in to github.com with a personal access token. The token is checked with GitHub, then stored in
    /// the keychain.
    pub fn add_github_pat(&self, token: Sensitive<String>) -> Result<Account, ForgeError> {
        let token = trimmed(token);
        let signed_in = self
            .inner
            .rt
            .block_on(but_github::store_pat(&token, &self.inner.storage))?;
        Ok(
            self.account(ForgeUser::GitHub(but_github::GithubAccountIdentifier::pat(
                &signed_in.login,
            ))),
        )
    }

    /// Sign in to a GitHub Enterprise instance whose API is at `api_url` (e.g.
    /// `https://ghe.example.com/api/v3`).
    pub fn add_github_enterprise(
        &self,
        api_url: &str,
        token: Sensitive<String>,
    ) -> Result<Account, ForgeError> {
        let api_url = api_base_url(api_url)?;
        let token = trimmed(token);
        let signed_in = self.inner.rt.block_on(but_github::store_enterprise_pat(
            &api_url,
            &token,
            &self.inner.storage,
        ))?;
        Ok(self.account(ForgeUser::GitHub(
            but_github::GithubAccountIdentifier::enterprise(&signed_in.login, &api_url),
        )))
    }

    /// Sign in to gitlab.com with a personal access token (scope `api`).
    pub fn add_gitlab_pat(&self, token: Sensitive<String>) -> Result<Account, ForgeError> {
        let token = trimmed(token);
        let signed_in = self
            .inner
            .rt
            .block_on(but_gitlab::store_pat(&token, &self.inner.storage))?;
        Ok(
            self.account(ForgeUser::GitLab(but_gitlab::GitlabAccountIdentifier::pat(
                &signed_in.username,
            ))),
        )
    }

    /// Sign in to a self-hosted GitLab instance at `url` (the `/api/v4` suffix is optional).
    pub fn add_gitlab_self_hosted(
        &self,
        url: &str,
        token: Sensitive<String>,
    ) -> Result<Account, ForgeError> {
        let url = url.trim();
        let token = trimmed(token);
        let signed_in = self.inner.rt.block_on(but_gitlab::store_selfhosted_pat(
            url,
            &token,
            &self.inner.storage,
        ))?;
        Ok(self.account(ForgeUser::GitLab(
            but_gitlab::GitlabAccountIdentifier::selfhosted(&signed_in.username, url),
        )))
    }

    /// Sign in to Bitbucket Cloud with an Atlassian API token and the account's email.
    pub fn add_bitbucket_token(
        &self,
        email: &str,
        token: Sensitive<String>,
    ) -> Result<Account, ForgeError> {
        let email = email.trim();
        let token = trimmed(token);
        self.inner.rt.block_on(but_bitbucket::store_api_token(
            email,
            &token,
            &self.inner.storage,
        ))?;
        Ok(self.account(ForgeUser::Bitbucket(
            but_bitbucket::BitbucketAccountIdentifier::apitoken(email),
        )))
    }

    /// Remove an account, its token and its cached profile.
    pub fn forget(&self, user: &ForgeUser) -> Result<(), ForgeError> {
        let storage = &self.inner.storage;
        match user {
            ForgeUser::GitHub(id) => but_github::forget_gh_access_token(id, storage)?,
            ForgeUser::GitLab(id) => but_gitlab::forget_gl_access_token(id, storage)?,
            ForgeUser::Bitbucket(id) => but_bitbucket::forget_bb_access_token(id, storage)?,
        }
        // The profile is only a cache; a failure to drop it must not fail the sign-out.
        storage.set_cached_profile(&profile_key(user), None).ok();
        Ok(())
    }

    /// Check whether an account's token is still valid: asks the forge. A failure to reach it counts as
    /// `Invalid`.
    pub fn check(&self, user: &ForgeUser) -> Result<ForgeAccountValidity, ForgeError> {
        let storage = &self.inner.storage;
        let validity = self.inner.rt.block_on(async {
            Ok::<_, anyhow::Error>(match user {
                ForgeUser::GitHub(id) => but_github::check_credentials(id, storage).await?.into(),
                ForgeUser::GitLab(id) => but_gitlab::check_credentials(id, storage).await?.into(),
                ForgeUser::Bitbucket(id) => {
                    but_bitbucket::check_credentials(id, storage).await?.into()
                }
            })
        })?;
        Ok(validity)
    }

    /// Download an image (an avatar, a picture in a pull request description) with a 30 second timeout and
    /// a cap of 16 MiB.
    ///
    /// GitHub answers as if a private repository's image were not there; an image on GitHub's own hosts
    /// that fails like that is asked again with the token of each github.com account (the keychain is read
    /// only then, so a public image never asks for it). The token goes to GitHub over HTTPS only.
    pub fn fetch_image(&self, url: &str) -> Result<Vec<u8>, ForgeError> {
        let url = image::parse(url)?;
        let anonymous = self
            .inner
            .rt
            .block_on(image::fetch(&self.inner.http, &url, None));
        match anonymous {
            Ok(bytes) => Ok(bytes),
            Err(err) if image::hidden(&err) && image::on_github(&url) => {
                self.signed_in_image(&url).ok_or_else(|| err.into())
            }
            Err(err) => Err(err.into()),
        }
    }

    /// What `url` serves to the first github.com account that may see it.
    fn signed_in_image(&self, url: &reqwest::Url) -> Option<Vec<u8>> {
        let storage = &self.inner.storage;
        let accounts = but_github::list_known_github_accounts(storage).ok()?;
        accounts
            .iter()
            // An Enterprise account's token is for its own server.
            .filter(|account| account.custom_host().is_none())
            .find_map(|account| {
                let token = but_github::get_gh_access_token(account, storage).ok()??;
                self.inner
                    .rt
                    .block_on(image::fetch(&self.inner.http, url, Some(&token.0)))
                    .ok()
            })
    }

    /// Detect the forge of `remote_url`, taking the known accounts (self-hosted hosts) and
    /// `forge_override` into account. `None` when the remote belongs to no forge.
    pub fn detect(&self, remote_url: &str, forge_override: Option<ForgeName>) -> Option<ForgeInfo> {
        let users = self.users().unwrap_or_default();
        but_forge::forge_info(remote_url, &users, forge_override)
    }

    /// Open the per-repository handle. It snapshots the accounts to detect the forge: open it again after
    /// an account was added or removed.
    pub fn open_repo(&self, input: RepoForgeInput) -> RepoForge {
        RepoForge::open(self.clone(), input)
    }
}

/// `token` without the whitespace a paste brings along.
fn trimmed(token: Sensitive<String>) -> Sensitive<String> {
    Sensitive(token.0.trim().to_owned())
}

/// The base URL of a GitHub Enterprise API: absolute http(s), without a trailing slash.
fn api_base_url(api_url: &str) -> Result<String, ForgeError> {
    let url = api_url.trim().trim_end_matches('/');
    let valid = url.split_once("://").is_some_and(|(scheme, rest)| {
        matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") && !rest.is_empty()
    });
    if valid {
        Ok(url.to_owned())
    } else {
        Err(ForgeError::other(format!(
            "'{api_url}' is not an API URL, expected something like https://ghe.example.com/api/v3"
        )))
    }
}

/// The key of the cached profile of `user` in the forge storage.
fn profile_key(user: &ForgeUser) -> String {
    match user {
        ForgeUser::GitHub(id) => id.cache_key(),
        ForgeUser::GitLab(id) => id.cache_key(),
        ForgeUser::Bitbucket(id) => id.cache_key(),
    }
}

/// The label of an account: `octocat`, `octocat (token)`, `octocat on ghe.example.com`.
fn label(user: &ForgeUser) -> String {
    use but_github::GithubAccountIdentifier as GitHub;
    use but_gitlab::GitlabAccountIdentifier as GitLab;
    match user {
        ForgeUser::GitHub(GitHub::OAuthUsername { username }) => username.clone(),
        ForgeUser::GitHub(GitHub::PatUsername { username }) => format!("{username} (token)"),
        ForgeUser::GitHub(GitHub::Enterprise { username, host }) => {
            format!("{username} on {}", display_host(host))
        }
        ForgeUser::GitLab(GitLab::PatUsername { username }) => username.clone(),
        ForgeUser::GitLab(GitLab::SelfHosted { username, host }) => {
            format!("{username} on {}", display_host(host))
        }
        ForgeUser::Bitbucket(id) => id.email().to_owned(),
    }
}

/// `host` (which may be a full API URL) as `ghe.example.com[:port]`.
fn display_host(host: &str) -> &str {
    let rest = host.split_once("://").map_or(host, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

/// `host` and its explicit port, if `host` has one.
fn split_host_port(host: &str) -> (&str, Option<u16>) {
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') => match port.parse() {
            Ok(port) => (name, Some(port)),
            Err(_) => (host, None),
        },
        _ => (host, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_tell_sign_in_kinds_apart() {
        use but_github::GithubAccountIdentifier as GitHub;
        use but_gitlab::GitlabAccountIdentifier as GitLab;
        let cases = [
            (ForgeUser::GitHub(GitHub::oauth("octocat")), "octocat"),
            (ForgeUser::GitHub(GitHub::pat("octocat")), "octocat (token)"),
            (
                ForgeUser::GitHub(GitHub::enterprise(
                    "octocat",
                    "https://ghe.example.com/api/v3",
                )),
                "octocat on ghe.example.com",
            ),
            (
                ForgeUser::GitLab(GitLab::selfhosted(
                    "tanuki",
                    "https://gl.example:8443/api/v4",
                )),
                "tanuki on gl.example:8443",
            ),
            (ForgeUser::GitLab(GitLab::pat("tanuki")), "tanuki"),
            (
                ForgeUser::Bitbucket(but_bitbucket::BitbucketAccountIdentifier::apitoken(
                    "a@example.com",
                )),
                "a@example.com",
            ),
        ];
        for (user, expected) in cases {
            assert_eq!(label(&user), expected, "label of {user:?}");
        }
    }

    #[test]
    fn host_and_port_split() {
        assert_eq!(split_host_port("github.com"), ("github.com", None));
        assert_eq!(
            split_host_port("git.example.com:8443"),
            ("git.example.com", Some(8443))
        );
        assert_eq!(
            split_host_port("git.example.com:http"),
            ("git.example.com:http", None),
            "a non-numeric suffix is no port"
        );
    }

    #[test]
    fn enterprise_api_urls_are_normalised_and_validated() {
        assert_eq!(
            api_base_url(" https://ghe.example.com/api/v3/ ").unwrap(),
            "https://ghe.example.com/api/v3",
            "whitespace and the trailing slash go"
        );
        for bad in ["ghe.example.com", "ftp://ghe.example.com", "https://", ""] {
            assert!(api_base_url(bad).is_err(), "'{bad}' is not an API base URL");
        }
    }
}
