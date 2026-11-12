use std::{collections::BTreeMap, env, time::Duration};

use chrono::{DateTime, Utc};
use reqwest::{
    header::{HeaderMap, HeaderValue, USER_AGENT},
    redirect,
};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A manifest is a few hundred bytes; anything much larger is not one.
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// Why an update check or a cache operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CheckError {
    /// The manifest could not be downloaded (DNS, TLS, timeout, HTTP status, redirect to a forbidden target).
    #[error("{0}")]
    Network(String),
    /// The manifest URL is unusable, or the manifest is not valid or lacks what the platform needs.
    #[error("{0}")]
    Manifest(String),
    /// The cache database failed or the request is invalid for the cache (for example snooze hours out of range).
    #[error("{0}")]
    Cache(String),
}

/// What a check needs to know about the running application.
#[derive(Debug, Clone, Copy)]
pub struct CheckParams<'a> {
    /// Where the manifest lives. Must be `https`, or `http` for a loopback host (tests, local mirrors).
    pub manifest_url: &'a str,
    /// The running application's version.
    pub current_version: &'a semver::Version,
    /// What the cached outcome is valid for, typically the version plus the release channel. A cached
    /// outcome produced for another scope is ignored by [`crate::available_update`].
    pub cache_scope: &'a str,
    /// The HTTP user agent, `"GitCthulhu/<version>"`.
    pub user_agent: &'a str,
}

/// One downloadable build in the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PlatformEntry {
    /// The download URL. `null` or missing: nothing is published for this platform.
    #[serde(default)]
    pub url: Option<String>,
    /// The detached signature of the download (base64 of the `.minisig` text).
    #[serde(default)]
    pub signature: Option<String>,
}

/// The parsed release manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The latest published version.
    pub version: semver::Version,
    /// Markdown release notes.
    pub notes: Option<String>,
    /// A page about the release, used as the link when the platform has no download of its own.
    pub release_url: Option<String>,
    /// Builds by platform key (see [`platform_key`]).
    pub platforms: BTreeMap<String, PlatformEntry>,
}

#[derive(Deserialize)]
struct RawManifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    release_url: Option<String>,
    #[serde(default)]
    platforms: BTreeMap<String, PlatformEntry>,
}

impl Manifest {
    /// Parse a manifest. A leading `v` of the version is accepted.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, CheckError> {
        let raw: RawManifest = serde_json::from_slice(bytes)
            .map_err(|err| CheckError::Manifest(format!("not a valid update manifest: {err}")))?;
        let version = semver::Version::parse(raw.version.strip_prefix('v').unwrap_or(&raw.version))
            .map_err(|err| {
                CheckError::Manifest(format!("manifest version '{}': {err}", raw.version))
            })?;
        Ok(Manifest {
            version,
            notes: raw.notes,
            release_url: raw.release_url,
            platforms: raw.platforms,
        })
    }

    /// Compare with `current` for `platform` (a [`platform_key`]).
    ///
    /// The manifest is newer only by semver precedence, so equal or older versions never offer an update,
    /// `0.10.0` beats `0.9.0` and build metadata (`1.0.0+build.2`) is ignored. A newer manifest that publishes nothing for `platform` (no entry, a `null`
    /// url and no `release_url`) is reported as up to date: there is nothing the user could act on.
    /// A download URL that is not `https` is rejected as a [`CheckError::Manifest`], because it ends up in a
    /// browser or an installer.
    pub fn status_for(
        &self,
        current: &semver::Version,
        platform: &str,
    ) -> Result<CheckUpdateStatus, CheckError> {
        let entry = self.platforms.get(platform);
        let own_url = entry.and_then(|entry| entry.url.clone());
        // `Ord` of `semver::Version` also orders build metadata (`1.0.0+b` > `1.0.0`), semver precedence
        // ignores it: a rebuild of the running version is not an update.
        let offer = if self.version.cmp_precedence(current) == std::cmp::Ordering::Greater {
            own_url
                .clone()
                .or_else(|| self.release_url.clone())
                .map(|url| {
                    require_https(&url)?;
                    // A signature belongs to the platform build, not to the release page fallback.
                    let signature = own_url
                        .is_some()
                        .then(|| entry.and_then(|entry| entry.signature.clone()))
                        .flatten();
                    Ok((url, signature))
                })
                .transpose()?
        } else {
            None
        };
        Ok(match offer {
            Some((url, signature)) => CheckUpdateStatus {
                up_to_date: false,
                latest_version: self.version.to_string(),
                release_notes: self.notes.clone(),
                url: Some(url),
                signature,
            },
            None => CheckUpdateStatus {
                up_to_date: true,
                latest_version: self.version.to_string(),
                release_notes: None,
                url: None,
                signature: None,
            },
        })
    }
}

fn require_https(url: &str) -> Result<(), CheckError> {
    match Url::parse(url) {
        Ok(parsed) if parsed.scheme() == "https" => Ok(()),
        _ => Err(CheckError::Manifest(format!(
            "download url '{url}' is not an https url"
        ))),
    }
}

/// The manifest key of a platform: `<os>-<arch>`, with Rust's `macos` spelled `darwin`.
///
/// Rust's `std::env::consts::OS` / `ARCH` values are the input, for example `("macos", "aarch64")` gives
/// `darwin-aarch64` and `("windows", "x86_64")` gives `windows-x86_64`.
pub fn platform_key(os: &str, arch: &str) -> String {
    let os = if os == "macos" { "darwin" } else { os };
    format!("{os}-{arch}")
}

/// [`platform_key`] of the running process.
pub fn current_platform_key() -> String {
    platform_key(env::consts::OS, env::consts::ARCH)
}

/// Information about the latest available version and whether an update is needed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CheckUpdateStatus {
    /// `true` if there is nothing newer to offer for this platform.
    pub up_to_date: bool,

    /// The latest published version (e.g., "0.18.3").
    pub latest_version: String,

    /// Markdown-formatted release notes of the newer version, only when an update is offered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_notes: Option<String>,

    /// Download (or release page) URL, only when an update is offered. Always `https`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Detached signature of the download, if the manifest has one for this platform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// Blocking [`check_status_async`]: runs the check on a scoped helper thread with its own current-thread
/// runtime, so it also works when the caller is inside another runtime.
///
/// There is no cancellation; the request times out after 30 seconds. Callers that need to cancel use
/// [`check_status_async`] and drop the future.
pub fn check_status(
    params: &CheckParams<'_>,
    cache: &mut but_db::AppCacheHandle,
) -> Result<Option<CheckUpdateStatus>, CheckError> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| CheckError::Network(format!("cannot start a runtime: {err}")))?;
                runtime.block_on(check_status_async(params, cache))
            })
            .join()
            .unwrap_or_else(|_| {
                Err(CheckError::Network(
                    "the update check thread panicked".into(),
                ))
            })
    })
}

/// Fetch the manifest and compare it with the running version for the current platform, without touching
/// the cache. Dropping the future cancels the request.
///
/// Pair it with [`record_status`] to remember the outcome. [`check_status_async`] does both under a
/// database lock for callers that want to deduplicate checks across processes.
pub async fn fetch_status(params: &CheckParams<'_>) -> Result<CheckUpdateStatus, CheckError> {
    let url = manifest_url(params.manifest_url)?;
    let client = http_client(params.user_agent, &url)?;
    let manifest = fetch_manifest(&client, url).await?;
    manifest.status_for(params.current_version, &current_platform_key())
}

/// Remember the outcome of [`fetch_status`] for `params.cache_scope`.
///
/// A snooze that is still running survives as long as the manifest still names the snoozed version; an
/// expired snooze, or one of an older version, is dropped.
pub fn record_status(
    cache: &mut but_db::AppCacheHandle,
    params: &CheckParams<'_>,
    status: &CheckUpdateStatus,
) -> Result<(), CheckError> {
    let mut trans = cache
        .deferred_transaction()
        .map_err(|err| CheckError::Cache(format!("cannot lock the update cache: {err}")))?;
    save_status(&mut trans, params, status)?;
    commit(trans)
}

/// Fetch the manifest, compare it with the running version for the current platform and cache the outcome.
///
/// Returns `Ok(None)` without doing anything when the cache database is locked, which means another thread
/// or process is checking right now. The lock is held while the request runs, so dropping the future
/// (cancellation) releases it again. A failed check leaves the previous cached outcome and any snooze in
/// place. A snooze that has not expired survives a successful check that finds the same version.
///
/// Note that the lock also stalls every other connection that is being opened on the same database for
/// the duration of the request; callers that must stay responsive use [`fetch_status`] and
/// [`record_status`] instead.
pub async fn check_status_async(
    params: &CheckParams<'_>,
    cache: &mut but_db::AppCacheHandle,
) -> Result<Option<CheckUpdateStatus>, CheckError> {
    let url = manifest_url(params.manifest_url)?;
    let client = http_client(params.user_agent, &url)?;

    // Try to get a transaction without waiting for locks, assume it's us being busy
    // in another thread or process.
    let Some(mut trans) = cache
        .immediate_transaction_nonblocking()
        .map_err(|err| CheckError::Cache(format!("cannot lock the update cache: {err}")))?
    else {
        return Ok(None);
    };

    let manifest = fetch_manifest(&client, url).await?;
    let status = manifest.status_for(params.current_version, &current_platform_key())?;
    save_status(&mut trans, params, &status)?;
    commit(trans)?;
    Ok(Some(status))
}

fn commit(trans: but_db::Transaction<'_>) -> Result<(), CheckError> {
    trans
        .commit()
        .map_err(|err| CheckError::Cache(format!("cannot save the update check: {err}")))
}

fn save_status(
    trans: &mut but_db::Transaction<'_>,
    params: &CheckParams<'_>,
    status: &CheckUpdateStatus,
) -> Result<(), CheckError> {
    let now = Utc::now();
    // Preserve a snooze that is still running, but only for the version that was snoozed: "remind me later"
    // about 0.2.0 must not hide 0.3.0 when it is published during the snooze.
    let (suppressed_at, suppress_duration_hours) = trans
        .update_check()
        .get()
        .filter(|cached| cached.status.latest_version == status.latest_version)
        .and_then(|cached| cached.suppressed_at.zip(cached.suppress_duration_hours))
        .filter(|(suppressed_at, hours)| !suppression_expired(*suppressed_at, *hours, now))
        .unzip();
    let cached = but_db::cache::CachedCheckResult {
        checked_at: now,
        status: but_db::cache::CheckUpdateStatus {
            up_to_date: status.up_to_date,
            latest_version: status.latest_version.clone(),
            release_notes: status.release_notes.clone(),
            url: status.url.clone(),
            signature: status.signature.clone(),
            valid_for_version: Some(params.cache_scope.to_owned()),
        },
        suppressed_at,
        suppress_duration_hours,
    };
    trans
        .update_check_mut()
        .and_then(|handle| handle.save(&cached))
        .map_err(|err| CheckError::Cache(format!("cannot save the update check: {err}")))
}

pub(crate) fn suppression_expired(
    suppressed_at: DateTime<Utc>,
    hours: u32,
    now: DateTime<Utc>,
) -> bool {
    now > suppressed_at + chrono::Duration::hours(i64::from(hours))
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Only `https` (or `http` to the local machine) may be used to learn about updates.
fn transport_allowed(url: &Url) -> bool {
    url.scheme() == "https" || (url.scheme() == "http" && is_loopback(url))
}

fn manifest_url(raw: &str) -> Result<Url, CheckError> {
    let url = Url::parse(raw)
        .map_err(|err| CheckError::Manifest(format!("manifest url '{raw}': {err}")))?;
    if !transport_allowed(&url) {
        return Err(CheckError::Manifest(format!(
            "manifest url '{raw}' must be an https url"
        )));
    }
    Ok(url)
}

fn http_client(user_agent: &str, manifest_url: &Url) -> Result<reqwest::Client, CheckError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(user_agent)
            .map_err(|err| CheckError::Manifest(format!("invalid user agent: {err}")))?,
    );
    // A redirect may not downgrade to plain http, except when the manifest itself is served from the
    // local machine.
    let allow_plain_http = is_loopback(manifest_url);
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(REQUEST_TIMEOUT)
        .redirect(redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if attempt.url().scheme() == "https"
                || (allow_plain_http && transport_allowed(attempt.url()))
            {
                attempt.follow()
            } else {
                attempt.error("redirect to a non-https url")
            }
        }))
        .build()
        .map_err(|err| CheckError::Network(format!("cannot build the HTTP client: {err}")))
}

async fn fetch_manifest(client: &reqwest::Client, url: Url) -> Result<Manifest, CheckError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|err| CheckError::Network(format!("manifest request failed: {err}")))?
        .error_for_status()
        .map_err(|err| CheckError::Network(format!("manifest server returned an error: {err}")))?;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| CheckError::Network(format!("manifest download failed: {err}")))?
    {
        if body.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(CheckError::Manifest(format!(
                "the manifest is larger than {MAX_MANIFEST_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Manifest::parse(&body)
}
