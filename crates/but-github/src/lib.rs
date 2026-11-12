use std::collections::HashMap;

use anyhow::{Context as _, Result};
use but_secret::Sensitive;
use serde::{Deserialize, Serialize};

pub mod checks;
mod client;
mod graphql;
pub mod pr;
pub mod stacks;
pub use client::{
    AutoMergeState, CheckRun, CommentReactions, CreatePullRequestParams, GitHubClient,
    GitHubPrLabel, GitHubRepoPermissions, GitHubRepository, GitHubUser, HttpStatusError,
    MergeMethod, MergePullRequestParams, PullRequest, PullRequestComment, PullRequestMergeStatus,
    PullRequestReview, PullRequestReviewThread, PullRequestReviewThreadComment, Reaction,
    SetPullRequestAutoMergeParams, SetPullRequestDraftStateParams, UpdatePullRequestParams,
};
mod token;
pub use token::{GithubAccountIdentifier, get_gh_access_token};

/// Where a user enters the device-flow code when GitHub does not say.
const DEFAULT_VERIFICATION_URI: &str = "https://github.com/login/device";
/// GitHub's documented minimum polling interval of the device flow, in seconds.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
/// How long a device code lives when GitHub does not say, in seconds.
const DEFAULT_EXPIRES_IN_SECS: u64 = 900;

/// The user agent of the OAuth requests.
const OAUTH_USER_AGENT: &str = "gitcthulhu-github-integration";
/// The timeout of one OAuth request.
const OAUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Verification {
    /// The code the user types in at `verification_uri`.
    pub user_code: String,
    /// The code to poll with.
    pub device_code: String,
    /// The page where `user_code` is entered.
    #[serde(default = "default_verification_uri")]
    pub verification_uri: String,
    /// The minimum number of seconds between two polls.
    #[serde(default = "default_interval")]
    pub interval: u64,
    /// The number of seconds until `device_code` expires.
    #[serde(default = "default_expires_in")]
    pub expires_in: u64,
}

fn default_verification_uri() -> String {
    DEFAULT_VERIFICATION_URI.to_owned()
}

fn default_interval() -> u64 {
    DEFAULT_POLL_INTERVAL_SECS
}

fn default_expires_in() -> u64 {
    DEFAULT_EXPIRES_IN_SECS
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(Verification);

/// The device flow has not finished yet: the user has not confirmed (`authorization_pending`) or polling is
/// too fast (`slow_down`). Poll again later; this is not a failure.
///
/// It is the root of the `anyhow::Error` that [`check_github_auth_status`] returns in these two cases, so
/// callers recognise it with `downcast_ref`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("GitHub returned an error: {status} ({description})")]
pub struct DeviceFlowPending {
    /// GitHub's status string, `authorization_pending` or `slow_down`.
    pub status: String,
    /// GitHub's description of the status.
    pub description: String,
    /// Whether GitHub asks for slower polling.
    pub slow_down: bool,
    /// The new minimum polling interval in seconds, when GitHub names one.
    pub interval: Option<u64>,
}

/// Detect GitHub's OAuth error shape (e.g. `device_flow_disabled`, `authorization_pending`) before falling back to the expected payload, so the real cause surfaces instead of a generic "missing field" serde error.
fn parse_github_oauth_response<T: serde::de::DeserializeOwned>(body: &str) -> Result<T> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("Response body was not valid JSON")?;
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        let description = value
            .get("error_description")
            .and_then(serde_json::Value::as_str);
        let description = description.unwrap_or("no description");
        return Err(match device_flow_context(error) {
            Some(context) => anyhow::anyhow!("GitHub returned an error: {error} ({description})")
                .context(context),
            None => anyhow::Error::new(DeviceFlowPending {
                status: error.to_owned(),
                description: description.to_owned(),
                slow_down: error == "slow_down",
                interval: value.get("interval").and_then(serde_json::Value::as_u64),
            }),
        });
    }
    serde_json::from_value(value).context("Response body did not match expected schema")
}

/// A static, code-bearing context for terminal device-flow statuses, so the API serializes
/// guidance instead of GitHub's description. Pending statuses stay as they are: callers poll
/// through them, and the provider text remains the inner error for the CLI.
fn device_flow_context(status: &str) -> Option<but_error::Context> {
    use but_error::{Code, Context};
    match status {
        "authorization_pending" | "slow_down" => None,
        "expired_token" => Some(Context::new_static(
            Code::GitHubDeviceCodeExpired,
            "The GitHub device code has expired",
        )),
        "access_denied" => Some(Context::new_static(
            Code::GitHubDeviceAccessDenied,
            "Authorization was denied on GitHub",
        )),
        _ => Some(Context::new_static(
            Code::GitHubDeviceFlowRejected,
            "GitHub rejected the device authorization request",
        )),
    }
}

/// An HTTP client for the OAuth endpoints.
fn oauth_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(OAUTH_USER_AGENT)
        .timeout(OAUTH_TIMEOUT)
        .build()
        .context("Failed to create the OAuth client")
}

/// Start the device flow of the OAuth app `client_id`.
pub async fn init_github_device_oauth(client_id: &str) -> Result<Verification> {
    let mut req_body = HashMap::new();
    req_body.insert("client_id", client_id);
    req_body.insert("scope", "repo");

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let client = oauth_client()?;
    let res = client
        .post("https://github.com/login/device/code")
        .headers(headers)
        .json(&req_body)
        .send()
        .await
        .context("Failed to send request")?;

    let rsp_body = res.text().await.context("Failed to get response body")?;

    parse_github_oauth_response(&rsp_body)
}

#[derive(Debug, Clone)]
pub struct AuthStatusResponse {
    /// The access token.
    /// This is only shared with the FrontEnd temporarily as we undergo the migration to having all API calls
    /// made to the forges from the Rustend.
    pub access_token: Sensitive<String>,
    pub login: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub host: Option<String>,
}

/// Ask GitHub whether the user confirmed `device_code` of the OAuth app `client_id`. When they did, the
/// access token is stored and the signed-in user returned.
///
/// While the user has not confirmed yet the error is a [`DeviceFlowPending`]; an expired, denied or
/// rejected flow carries a [`but_error::Context`] with a `GitHubDevice*` code.
pub async fn check_github_auth_status(
    client_id: &str,
    device_code: &str,
    storage: &but_forge_storage::Controller,
) -> Result<AuthStatusResponse> {
    #[derive(Debug, Deserialize, Serialize, Clone, Default)]
    struct AccessTokenContainer {
        access_token: String,
    }

    let mut req_body = HashMap::new();
    req_body.insert("client_id", client_id);
    req_body.insert("device_code", device_code);
    req_body.insert("grant_type", "urn:ietf:params:oauth:grant-type:device_code");

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let client = oauth_client()?;
    let res = client
        .post("https://github.com/login/oauth/access_token")
        .headers(headers)
        .json(&req_body)
        .send()
        .await
        .context("Failed to send request")?;

    let rsp_body = res.text().await.context("Failed to get response body")?;

    let access_token =
        Sensitive(parse_github_oauth_response::<AccessTokenContainer>(&rsp_body)?.access_token);

    let user = fetch_and_persist_oauth_user_data(&access_token, storage).await?;

    Ok(AuthStatusResponse {
        access_token,
        login: user.login,
        name: user.name,
        email: user.email,
        host: None,
    })
}

/// Cache the user profile so it's available offline.
fn cache_user_profile(
    account: &GithubAccountIdentifier,
    user: &client::AuthenticatedUser,
    storage: &but_forge_storage::Controller,
) {
    let profile = but_forge_storage::settings::CachedProfile {
        avatar_url: user.avatar_url.clone(),
        name: user.name.clone(),
        email: user.email.clone(),
    };
    let key = account.cache_key();
    let existing = storage.cached_profile(&key).ok().flatten();
    if existing.as_ref() == Some(&profile) {
        return;
    }
    if let Err(err) = storage.set_cached_profile(&key, Some(profile)) {
        tracing::warn!(?account, "Failed to update cached GitHub profile: {err}");
    }
}

/// Fetch the authenticated user data from GitHub and persist the access token. (OAuth)
async fn fetch_and_persist_oauth_user_data(
    access_token: &Sensitive<String>,
    storage: &but_forge_storage::Controller,
) -> Result<client::AuthenticatedUser, anyhow::Error> {
    let gh = client::GitHubClient::new(access_token).context("Failed to create GitHub client")?;
    let user = gh
        .get_authenticated()
        .await
        .context("Failed to get authenticated user")?;
    let account_id = token::GithubAccountIdentifier::oauth(&user.login);
    token::persist_gh_access_token(&account_id, access_token, storage)
        .context("Failed to persist access token")?;
    cache_user_profile(&account_id, &user, storage);
    Ok(user)
}

/// Store a PAT access token and fetch the associated user data.
pub async fn store_pat(
    access_token: &Sensitive<String>,
    storage: &but_forge_storage::Controller,
) -> Result<AuthStatusResponse> {
    let user = fetch_and_persist_pat_user_data(access_token, storage).await?;
    Ok(AuthStatusResponse {
        access_token: access_token.clone(),
        login: user.login,
        name: user.name,
        email: user.email,
        host: None,
    })
}

/// Fetch the authenticated user data from GitHub and persist the access token. (PAT)
async fn fetch_and_persist_pat_user_data(
    access_token: &Sensitive<String>,
    storage: &but_forge_storage::Controller,
) -> Result<client::AuthenticatedUser, anyhow::Error> {
    let gh = client::GitHubClient::new(access_token).context("Failed to create GitHub client")?;
    let user = gh
        .get_authenticated()
        .await
        .context("Failed to get authenticated user")?;
    let account_id = token::GithubAccountIdentifier::pat(&user.login);
    token::persist_gh_access_token(&account_id, access_token, storage)
        .context("Failed to persist access token")?;
    cache_user_profile(&account_id, &user, storage);
    Ok(user)
}

/// Store an Enterprise access token and fetch the associated user data.
pub async fn store_enterprise_pat(
    host: &str,
    access_token: &Sensitive<String>,
    storage: &but_forge_storage::Controller,
) -> Result<AuthStatusResponse> {
    let user = fetch_and_persist_enterprise_user_data(host, access_token, storage).await?;
    Ok(AuthStatusResponse {
        access_token: access_token.clone(),
        login: user.login,
        name: user.name,
        email: user.email,
        host: Some(host.to_owned()),
    })
}

/// Fetch the authenticated user data from GitHub and persist the access token. (Enterprise)
async fn fetch_and_persist_enterprise_user_data(
    host: &str,
    access_token: &Sensitive<String>,
    storage: &but_forge_storage::Controller,
) -> Result<client::AuthenticatedUser, anyhow::Error> {
    let gh = client::GitHubClient::new_with_host_override(access_token, host)
        .context("Failed to create GitHub client")?;
    let user = gh
        .get_authenticated()
        .await
        .context("Failed to get authenticated user")?;
    let account_id = token::GithubAccountIdentifier::enterprise(&user.login, host);
    token::persist_gh_access_token(&account_id, access_token, storage)
        .context("Failed to persist access token")?;
    cache_user_profile(&account_id, &user, storage);
    Ok(user)
}

pub fn forget_gh_access_token(
    account: &GithubAccountIdentifier,
    storage: &but_forge_storage::Controller,
) -> Result<()> {
    token::delete_gh_access_token(account, storage).context("Failed to delete access token")
}

#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub access_token: Sensitive<String>,
    pub login: String,
    pub avatar_url: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
}

pub async fn get_gh_user(
    account: &GithubAccountIdentifier,
    storage: &but_forge_storage::Controller,
) -> Result<Option<AuthenticatedUser>> {
    if let Some(access_token) = token::get_gh_access_token(account, storage)? {
        let gh = account
            .client(&access_token)
            .context("Failed to create GitHub client")?;
        match gh.get_authenticated().await {
            Ok(user) => {
                cache_user_profile(account, &user, storage);
                Ok(Some(AuthenticatedUser {
                    access_token,
                    login: user.login,
                    avatar_url: user.avatar_url,
                    name: user.name,
                    email: user.email,
                }))
            }
            Err(client_err) => {
                let cache_key = account.cache_key();
                // Check if this is a network error — return cached data if available.
                if let Some(reqwest_err) = client_err.downcast_ref::<reqwest::Error>()
                    && is_network_error(reqwest_err)
                {
                    match storage.cached_profile(&cache_key) {
                        Ok(Some(cached)) => {
                            return Ok(Some(AuthenticatedUser {
                                access_token,
                                login: account.username().to_owned(),
                                avatar_url: cached.avatar_url,
                                name: cached.name,
                                email: cached.email,
                            }));
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::warn!("Failed to read cached GitHub profile: {err}");
                        }
                    }
                    return Err(client_err.context(but_error::Context::new_static(
                        but_error::Code::NetworkError,
                        "Unable to connect to GitHub.",
                    )));
                }
                // Check if this is an auth error (401/403) — clear cached profile.
                if let Some(http_err) = client_err.downcast_ref::<client::HttpStatusError>()
                    && matches!(
                        http_err.status,
                        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
                    )
                    && let Err(err) = storage.set_cached_profile(&cache_key, None)
                {
                    tracing::warn!("Failed to clear cached GitHub profile: {err}");
                }
                Err(client_err.context("Failed to get authenticated user"))
            }
        }
    } else {
        Ok(None)
    }
}

/// Check if an error is a network connectivity error.
///
/// This includes DNS resolution failures, connection timeouts, connection
/// refused, and connections dropped while the response body was being read.
/// reqwest wraps both body I/O failures and malformed payloads as the same
/// decode kind, so the source chain decides: a serde cause means the payload
/// was malformed, anything else means the transport failed mid-response.
pub(crate) fn is_network_error(err: &reqwest::Error) -> bool {
    if err.is_timeout() || err.is_connect() || err.is_request() {
        return true;
    }
    if !err.is_decode() {
        return false;
    }
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        if cause.downcast_ref::<serde_json::Error>().is_some() {
            return false;
        }
        source = cause.source();
    }
    true
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum CredentialCheckResult {
    Valid,
    Invalid,
    NoCredentials,
}

/// Check the validity of the stored credentials for the given GitHub account.
pub async fn check_credentials(
    account: &GithubAccountIdentifier,
    storage: &but_forge_storage::Controller,
) -> Result<CredentialCheckResult> {
    if let Some(access_token) = token::get_gh_access_token(account, storage)? {
        let gh = account
            .client(&access_token)
            .context("Failed to create GitHub client")?;
        match gh.get_authenticated().await {
            Ok(_) => Ok(CredentialCheckResult::Valid),
            Err(_) => Ok(CredentialCheckResult::Invalid),
        }
    } else {
        Ok(CredentialCheckResult::NoCredentials)
    }
}

pub fn list_known_github_accounts(
    storage: &but_forge_storage::Controller,
) -> Result<Vec<token::GithubAccountIdentifier>> {
    token::list_known_github_accounts(storage).context("Failed to list known GitHub usernames")
}

pub fn clear_all_github_tokens(storage: &but_forge_storage::Controller) -> Result<()> {
    token::clear_all_github_accounts(storage).context("Failed to clear all GitHub tokens")
}

/// JSON serialization types for GitHub API responses.
///
/// This module contains serializable versions of GitHub authentication types
/// that expose sensitive data (like access tokens) as plain strings for API responses.
pub mod json {
    use serde::Serialize;

    use crate::{AuthStatusResponse, AuthenticatedUser};

    /// Serializable version of [`AuthStatusResponse`], without the access token.
    ///
    /// The credential is stored by the backend as part of the call, so the caller is told
    /// who authenticated and nothing more. Field names are camelCase for JSON.
    #[derive(Debug, Serialize)]
    #[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
    #[serde(rename_all = "camelCase")]
    pub struct GithubAuthStatusResponse {
        pub login: String,
        pub name: Option<String>,
        pub email: Option<String>,
        /// The enterprise or self-hosted host, when there is one.
        pub host: Option<String>,
    }

    impl From<AuthStatusResponse> for GithubAuthStatusResponse {
        fn from(
            AuthStatusResponse {
                login,
                name,
                email,
                host,
                ..
            }: AuthStatusResponse,
        ) -> Self {
            GithubAuthStatusResponse {
                login,
                name,
                email,
                host,
            }
        }
    }

    #[cfg(feature = "export-schema")]
    but_schemars::register_sdk_type!(GithubAuthStatusResponse);

    /// Serializable version of [`AuthenticatedUser`] with exposed access token.
    ///
    /// This struct represents an authenticated GitHub user with their credentials
    /// exposed as plain strings for API responses. Field names are converted to camelCase for JSON.
    #[derive(Debug, Serialize)]
    #[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
    #[serde(rename_all = "camelCase")]
    pub struct GithubAuthenticatedUserSensitive {
        /// The GitHub access token as a plain string (sensitive data).
        pub access_token: String,
        /// The GitHub username/login.
        pub login: String,
        /// The URL to the user's avatar image, if available.
        pub avatar_url: Option<String>,
        /// The user's display name, if available.
        pub name: Option<String>,
        /// The user's email address, if available.
        pub email: Option<String>,
    }

    impl From<AuthenticatedUser> for GithubAuthenticatedUserSensitive {
        fn from(
            AuthenticatedUser {
                access_token,
                login,
                avatar_url,
                name,
                email,
            }: AuthenticatedUser,
        ) -> Self {
            GithubAuthenticatedUserSensitive {
                access_token: access_token.0,
                login,
                avatar_url,
                name,
                email,
            }
        }
    }

    #[cfg(feature = "export-schema")]
    but_schemars::register_sdk_type!(GithubAuthenticatedUserSensitive);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_network_error_with_reqwest_timeout() {
        // Create a reqwest error by making an actual HTTP request that will timeout
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(1))
            .build()
            .unwrap();

        // Try to connect to a non-routable IP address (should timeout)
        let result = client.get("http://192.0.2.1:80").send();

        if let Err(reqwest_err) = result {
            assert!(
                is_network_error(&reqwest_err),
                "Should detect timeout/connection errors"
            );
        } else {
            panic!("Expected a network error but request succeeded");
        }
    }

    #[test]
    fn test_is_network_error_with_connection_error() {
        // Create a reqwest error
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(1))
            .build()
            .unwrap();

        let result = client.get("http://192.0.2.1:80").send();

        if let Err(reqwest_err) = result {
            assert!(
                is_network_error(&reqwest_err),
                "Should detect reqwest network errors"
            );
        } else {
            panic!("Expected a network error but request succeeded");
        }
    }

    #[test]
    fn device_flow_statuses_carry_static_context_but_keep_provider_text() {
        use but_error::{AnyhowContextExt as _, Code};
        let secret = "device_code=3584d274 user_code=WDJB-MJHT https://github.com/login/device";
        let rejected = Some((
            Code::GitHubDeviceFlowRejected,
            "GitHub rejected the device authorization request",
        ));
        let cases: [(&str, Option<(Code, &str)>); 9] = [
            ("authorization_pending", None),
            ("slow_down", None),
            (
                "expired_token",
                Some((
                    Code::GitHubDeviceCodeExpired,
                    "The GitHub device code has expired",
                )),
            ),
            (
                "access_denied",
                Some((
                    Code::GitHubDeviceAccessDenied,
                    "Authorization was denied on GitHub",
                )),
            ),
            ("incorrect_client_credentials", rejected),
            ("incorrect_device_code", rejected),
            ("unsupported_grant_type", rejected),
            ("device_flow_disabled", rejected),
            ("brand_new_status", rejected),
        ];
        for (status, expected) in cases {
            let body = format!(r#"{{"error":"{status}","error_description":"{secret}"}}"#);
            let err = parse_github_oauth_response::<serde_json::Value>(&body)
                .expect_err("an OAuth error status is a failure");
            match (err.custom_context(), expected) {
                (None, None) => {}
                (Some(ctx), Some((code, message))) => {
                    assert_eq!(ctx.code, code, "{status}");
                    assert_eq!(ctx.message.as_deref(), Some(message), "{status}");
                }
                (ctx, expected) => panic!("{status}: got {ctx:?}, expected {expected:?}"),
            }
            // The API layer's JSON error serializes the context message when present, else the chain.
            let api_message = err
                .custom_context_or_error_chain()
                .message
                .expect("always a message");
            if expected.is_some() {
                assert!(
                    !api_message.contains(secret) && !api_message.contains(status),
                    "{status}: provider detail must not reach the API: {api_message}"
                );
            } else {
                assert!(
                    api_message.contains(status),
                    "{status}: pending statuses keep today's message"
                );
            }
            // Alternate display (the CLI's) keeps the provider status as the inner error.
            let display = format!("{err:#}");
            assert!(
                display.contains(&format!("GitHub returned an error: {status} (")),
                "{status}: {display}"
            );
        }
    }

    #[test]
    fn verification_keeps_what_github_says_and_defaults_the_rest() {
        let full: Verification = parse_github_oauth_response(
            r#"{"device_code":"dc","user_code":"WDJB-MJHT","verification_uri":"https://ghe.example/login/device","expires_in":600,"interval":10}"#,
        )
        .expect("a device code response");
        assert_eq!(
            (
                full.device_code.as_str(),
                full.user_code.as_str(),
                full.verification_uri.as_str(),
                full.expires_in,
                full.interval
            ),
            (
                "dc",
                "WDJB-MJHT",
                "https://ghe.example/login/device",
                600,
                10
            ),
            "the page, lifetime and polling interval GitHub names are kept"
        );
        let minimal: Verification =
            parse_github_oauth_response(r#"{"device_code":"dc","user_code":"X"}"#)
                .expect("a bare device code response");
        assert_eq!(
            (
                minimal.verification_uri.as_str(),
                minimal.expires_in,
                minimal.interval
            ),
            ("https://github.com/login/device", 900, 5),
            "GitHub's documented defaults fill in what is missing"
        );
    }

    #[test]
    fn pending_device_flow_statuses_are_a_typed_error_with_the_new_interval() {
        let err = parse_github_oauth_response::<serde_json::Value>(
            r#"{"error":"slow_down","error_description":"too fast","interval":15}"#,
        )
        .expect_err("slow_down is not a result");
        let pending = err
            .downcast_ref::<DeviceFlowPending>()
            .expect("the error is the typed pending status");
        assert_eq!(
            (pending.slow_down, pending.interval),
            (true, Some(15)),
            "slow_down asks for the interval GitHub names"
        );
        let err = parse_github_oauth_response::<serde_json::Value>(
            r#"{"error":"authorization_pending"}"#,
        )
        .expect_err("pending is not a result");
        let pending = err.downcast_ref::<DeviceFlowPending>().expect("typed");
        assert_eq!(
            (pending.slow_down, pending.interval),
            (false, None),
            "authorization_pending asks for nothing"
        );
        let err = parse_github_oauth_response::<serde_json::Value>(r#"{"error":"expired_token"}"#)
            .expect_err("expired");
        assert!(
            err.downcast_ref::<DeviceFlowPending>().is_none(),
            "an expired flow is a failure, not a pending one"
        );
    }

    #[test]
    fn non_provider_parse_failures_are_not_contextualized() {
        use but_error::AnyhowContextExt as _;
        for body in ["not json", r#"{"unexpected":"shape"}"#] {
            let err = parse_github_oauth_response::<Verification>(body).expect_err("bad body");
            assert!(err.custom_context().is_none(), "{body:?}: {err:#}");
        }
    }
}
