//! The error type of the forge facade and the classification of the errors the forge crates produce.
//!
//! `but-github`, `but-gitlab` and `but-bitbucket` report failures as `anyhow::Error`s: a
//! [`but_error::Context`] with a [`but_error::Code`] when the crate recognised the failure, an HTTP status
//! error (one type per crate) when the forge answered with a failure status, a `reqwest::Error` when the
//! transport failed. [`ForgeError::from`] folds all three into the variants the GUI matches on.

use but_error::{AnyhowContextExt as _, Code};

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
    /// Transport failure, or the forge failing on its side (HTTP 5xx).
    #[error("network error: {0}")]
    Network(String),
    /// The API rate limit was hit; `reset_at` is a unix timestamp if known.
    #[error("rate limited")]
    RateLimited {
        /// Unix time when the limit resets.
        reset_at: Option<i64>,
    },
    /// Token expired, revoked or otherwise invalid; a device-flow sign-in that expired, was denied or rejected.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The token lacks a permission (or an organisation's policy refuses it).
    #[error("permission denied: {0}")]
    Permission(String),
    /// The resource does not exist.
    #[error("not found")]
    NotFound,
    /// The operation was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// Anything else.
    #[error(transparent)]
    Other(anyhow::Error),
}

impl ForgeError {
    /// Whether the failure may pass by itself, so asking again later is worth it (port of the old GUI's
    /// `may_pass`): the forge could not be reached, or it failed on its side, as with a 502.
    ///
    /// Every other failure stays until the user acts (a missing account, a revoked token), or asking again
    /// makes it worse, as it does with a rate limit: for that, wait until `reset_at`.
    pub fn may_pass(&self) -> bool {
        matches!(self, ForgeError::Network(_))
    }

    /// Whether polling should stop until the user fixes the account setup: there is no account for the
    /// repository, or the token is invalid or lacks a permission.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ForgeError::NotAuthenticated
                | ForgeError::Unrecognized
                | ForgeError::Auth(_)
                | ForgeError::Permission(_)
        )
    }

    /// An error that does not fit any other variant.
    pub(crate) fn other(message: impl std::fmt::Display) -> Self {
        ForgeError::Other(anyhow::anyhow!("{message}"))
    }
}

impl From<crate::Cancelled> for ForgeError {
    fn from(_: crate::Cancelled) -> Self {
        ForgeError::Cancelled
    }
}

impl From<anyhow::Error> for ForgeError {
    fn from(err: anyhow::Error) -> Self {
        classify(err)
    }
}

/// The message a user may read for `err`: the one of its [`but_error::Context`] if it has one, else the
/// outermost error's.
fn message_of(err: &anyhow::Error) -> String {
    err.custom_context()
        .and_then(|context| context.message.map(|message| message.into_owned()))
        .unwrap_or_else(|| err.to_string())
}

/// The status of the HTTP failure `err` stems from, whichever forge crate it came from. The status error
/// may be anywhere in the chain of causes (a crate may wrap it in an error of its own).
fn http_status(err: &anyhow::Error) -> Option<reqwest::StatusCode> {
    err.chain().find_map(|cause| {
        cause
            .downcast_ref::<but_github::HttpStatusError>()
            .map(|e| e.status)
            .or_else(|| {
                cause
                    .downcast_ref::<but_gitlab::HttpStatusError>()
                    .map(|e| e.status)
            })
            .or_else(|| {
                cause
                    .downcast_ref::<but_bitbucket::HttpStatusError>()
                    .map(|e| e.status)
            })
    })
}

/// Whether `err` is a connectivity failure: DNS, connect, timeout, or a connection dropped mid-response.
/// A body that arrived in full but did not parse is the forge's fault, not the network's.
fn is_transport_failure(err: &reqwest::Error) -> bool {
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

/// Fold an error of the forge crates into a [`ForgeError`].
///
/// A [`but_error::Code`] attached by the forge crates decides first; then the HTTP status the forge
/// answered with (401 is `Auth`, 403 `Permission` unless the body names a rate limit, 404 `NotFound`, 429
/// `RateLimited`, 5xx `Network` so that [`ForgeError::may_pass()`] holds for it); then transport failures
/// (`Network`). Everything else is `Other`.
pub(crate) fn classify(err: anyhow::Error) -> ForgeError {
    if let Some(context) = err.custom_context() {
        let message = message_of(&err);
        match context.code {
            Code::NetworkError => return ForgeError::Network(message),
            Code::GitHubRateLimited => return ForgeError::RateLimited { reset_at: None },
            Code::GitHubTokenExpired
            | Code::GitLabUnauthorized
            | Code::GitHubDeviceCodeExpired
            | Code::GitHubDeviceAccessDenied
            | Code::GitHubDeviceFlowRejected => return ForgeError::Auth(message),
            Code::GitHubOrgOAuthRestricted
            | Code::GitHubOrgSamlRestricted
            | Code::GitHubInsufficientPermissions
            | Code::GitHubTokenLifetimeRestricted
            | Code::GitLabForbidden => return ForgeError::Permission(message),
            Code::GitLabProjectNotFound => return ForgeError::NotFound,
            Code::ForgeNotAuthenticated => return ForgeError::NotAuthenticated,
            Code::ForgeUnrecognized => return ForgeError::Unrecognized,
            _ => {}
        }
    }
    if let Some(status) = http_status(&err) {
        let mentions_rate_limit = || {
            err.chain().any(|cause| {
                cause
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("rate limit")
            })
        };
        match status.as_u16() {
            401 => return ForgeError::Auth(message_of(&err)),
            403 if mentions_rate_limit() => return ForgeError::RateLimited { reset_at: None },
            403 => return ForgeError::Permission(message_of(&err)),
            404 => return ForgeError::NotFound,
            429 => return ForgeError::RateLimited { reset_at: None },
            500..=599 => {
                return ForgeError::Network(format!("the forge answered with HTTP {status}"));
            }
            _ => {}
        }
    }
    if let Some(transport) = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
        && is_transport_failure(transport)
    {
        return ForgeError::Network(message_of(&err));
    }
    ForgeError::Other(err)
}
