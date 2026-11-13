//! Update check against a static manifest. 20 section 8.5 (= 16 B7).
//!
//! A thin, blocking facade over the slimmed `but-update`: it fetches the release manifest on the shared
//! runtime (cancellable), compares versions with semver, and keeps the outcome in the app cache database so
//! [`UpdateService::cached`] answers without the network. The interval policy (at startup, then every 24 h,
//! 0 = off) belongs to the GUI.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use gitcthulhu_git::CancelToken;

use crate::{Runtime, ServicesConfig, UpdateChannel};

/// An update that is newer than the running version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    /// The new version.
    pub version: semver::Version,
    /// Release notes (markdown).
    pub notes: Option<String>,
    /// Download URL (`https`), or the release page when the manifest has no build for this platform.
    pub url: String,
    /// Detached signature of the download, if the manifest has one for this platform.
    pub signature: Option<String>,
}

/// Errors of the update service.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// No manifest URL is configured.
    #[error("update checks are disabled")]
    Disabled,
    /// Transport failure.
    #[error("network error: {0}")]
    Network(String),
    /// The manifest URL is unusable or the manifest could not be parsed.
    #[error("invalid update manifest: {0}")]
    Manifest(String),
    /// The cache database failed.
    #[error("update cache error: {0}")]
    Cache(String),
    /// The caller passed a value outside the accepted range, for example a snooze of 0 hours.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The operation was cancelled.
    #[error("cancelled")]
    Cancelled,
}

impl From<but_update::CheckError> for UpdateError {
    fn from(err: but_update::CheckError) -> Self {
        match err {
            but_update::CheckError::Network(msg) => UpdateError::Network(msg),
            but_update::CheckError::Manifest(msg) => UpdateError::Manifest(msg),
            but_update::CheckError::Cache(msg) => UpdateError::Cache(msg),
            // `CheckError` is non-exhaustive; a new kind is at least a failure of the check itself.
            other => UpdateError::Network(other.to_string()),
        }
    }
}

/// The longest snooze, 30 days.
const MAX_SNOOZE_HOURS: u32 = 720;

/// The update service.
///
/// The cache database is only locked for the milliseconds a read or write takes, never while the manifest
/// is being downloaded, so [`UpdateService::cached`] and [`UpdateService::snooze`] stay responsive during a
/// check.
pub struct UpdateService {
    rt: Runtime,
    cache_dir: PathBuf,
    /// Opened on first use; the single connection of this service.
    cache: Mutex<Option<but_db::AppCacheHandle>>,
    /// Held for the duration of a check so that concurrent callers do not request the manifest twice. An
    /// async mutex, so that waiting for it can be cancelled.
    check_in_flight: tokio::sync::Mutex<()>,
    /// The number of checks whose outcome reached the cache; tells a caller that waited for another
    /// thread's check whether that check produced a fresh answer.
    completed_checks: AtomicU64,
    /// The manifest URL with `{channel}` still in it.
    manifest_template: Option<String>,
    version: semver::Version,
    /// The channel checks look at; changes with [`UpdateService::set_channel`].
    target: Mutex<Target>,
    user_agent: String,
}

/// What a check looks at: the manifest of the channel, and the scope its outcome is cached for.
#[derive(Clone)]
struct Target {
    manifest_url: Option<String>,
    /// What a cached check is valid for: the running version and the channel.
    cache_scope: String,
}

impl Target {
    fn new(template: Option<&str>, version: &semver::Version, channel: UpdateChannel) -> Self {
        let channel = match channel {
            UpdateChannel::Release => "release",
            UpdateChannel::Nightly => "nightly",
        };
        Target {
            manifest_url: template.map(|url| url.replace("{channel}", channel)),
            cache_scope: format!("{version}/{channel}"),
        }
    }
}

impl UpdateService {
    /// Create the service from the shared configuration.
    ///
    /// `update_manifest_url` may contain `{channel}`, which becomes `release` or `nightly` according to
    /// `update_channel`. Nothing is read or fetched here.
    pub fn new(rt: Runtime, cfg: &ServicesConfig) -> Self {
        UpdateService {
            rt,
            cache_dir: cfg.cache_dir.clone(),
            cache: Mutex::new(None),
            check_in_flight: tokio::sync::Mutex::new(()),
            completed_checks: AtomicU64::new(0),
            manifest_template: cfg.update_manifest_url.clone(),
            target: Mutex::new(Target::new(
                cfg.update_manifest_url.as_deref(),
                &cfg.app_version,
                cfg.update_channel,
            )),
            version: cfg.app_version.clone(),
            user_agent: cfg.user_agent.clone(),
        }
    }

    /// Run `f` on the app cache database, opening it on first use. Opening is infallible: it falls back to
    /// an in-memory database, in which case nothing is remembered across restarts.
    fn with_cache<T>(&self, f: impl FnOnce(&mut but_db::AppCacheHandle) -> T) -> T {
        let mut guard = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let cache = guard
            .get_or_insert_with(|| but_db::AppCacheHandle::new_in_directory(Some(&self.cache_dir)));
        f(cache)
    }

    /// Switch the channel that later checks look at. Nothing is fetched: the caller runs a check when it
    /// wants the new channel's answer. [`UpdateService::cached`] and [`UpdateService::cached_at`] only
    /// report outcomes that were produced for the channel now selected, so until that check finishes they
    /// answer `None`. A check that is running keeps asking the channel it started with.
    pub fn set_channel(&self, channel: UpdateChannel) {
        *self.target.lock().unwrap_or_else(PoisonError::into_inner) =
            Target::new(self.manifest_template.as_deref(), &self.version, channel);
    }

    fn target(&self) -> Target {
        self.target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn require_manifest_url(&self) -> Result<Target, UpdateError> {
        let target = self.target();
        if target.manifest_url.is_some() {
            Ok(target)
        } else {
            Err(UpdateError::Disabled)
        }
    }

    fn params<'a>(
        &'a self,
        manifest_url: &'a str,
        cache_scope: &'a str,
    ) -> but_update::CheckParams<'a> {
        but_update::CheckParams {
            manifest_url,
            current_version: &self.version,
            cache_scope,
            user_agent: &self.user_agent,
        }
    }

    fn current_update(&self, cache_scope: &str) -> Result<Option<AvailableUpdate>, UpdateError> {
        let found = self
            .with_cache(|cache| but_update::available_update(cache, &self.version, cache_scope))?;
        Ok(found.and_then(|update| {
            Some(AvailableUpdate {
                version: update.available_version,
                notes: update.release_notes,
                url: update.url?,
                signature: update.signature,
            })
        }))
    }

    /// Fetch the manifest, compare it with the running version and remember the outcome.
    ///
    /// Returns the update to announce: `Some` only when the manifest is newer by semver precedence and has a
    /// download (or release page) for this platform, and the user has not snoozed it (see
    /// [`UpdateService::snooze`]). The wait is interrupted by `cancel` ([`UpdateError::Cancelled`]); the
    /// request itself times out after 30 s.
    ///
    /// If another thread of this service is checking at this very moment, this call waits for that check
    /// (cancellable) and returns its outcome instead of requesting the manifest a second time. Only if that
    /// check failed, was cancelled or asked another channel does this call request the manifest itself.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Disabled`] without a manifest URL, [`UpdateError::Network`] when the manifest cannot
    /// be downloaded, [`UpdateError::Manifest`] when it is not a usable manifest, [`UpdateError::Cache`]
    /// when the outcome cannot be stored. After an error the previously cached outcome is unchanged.
    pub fn check_now(&self, cancel: &CancelToken) -> Result<Option<AvailableUpdate>, UpdateError> {
        let target = self.require_manifest_url()?;
        let manifest_url = target
            .manifest_url
            .as_deref()
            .ok_or(UpdateError::Disabled)?;
        let cache_scope = target.cache_scope.as_str();
        let completed_before = self.completed_checks.load(Ordering::SeqCst);
        let _in_flight = match self.check_in_flight.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                // Another thread is checking right now: wait for it rather than asking the server twice.
                let guard = self
                    .rt
                    .block_on_cancellable(cancel, self.check_in_flight.lock())
                    .map_err(|_| UpdateError::Cancelled)?;
                // Its outcome answers this call only if it was for the same channel: after
                // [`UpdateService::set_channel`] the waiting call asks the new channel itself.
                if self.completed_checks.load(Ordering::SeqCst) != completed_before
                    && self.stored_check(cache_scope).is_some()
                {
                    return self.current_update(cache_scope);
                }
                guard
            }
        };
        let params = self.params(manifest_url, cache_scope);
        let status = self
            .rt
            .block_on_cancellable(cancel, but_update::fetch_status(&params))
            .map_err(|_| UpdateError::Cancelled)??;
        self.with_cache(|cache| but_update::record_status(cache, &params, &status))?;
        self.completed_checks.fetch_add(1, Ordering::SeqCst);
        self.current_update(cache_scope)
    }

    /// The update found by the last check, from the cache, without any network access.
    ///
    /// `None` when nothing was found, the update is snoozed, the cached check belongs to another version or
    /// channel, the cache is unreadable, or update checks are disabled.
    pub fn cached(&self) -> Option<AvailableUpdate> {
        let target = self.require_manifest_url().ok()?;
        self.current_update(&target.cache_scope).ok().flatten()
    }

    /// When the last check finished, from the cache, without any network access. It survives restarts.
    ///
    /// `None` when no check was made yet, the stored check belongs to another version or channel (see
    /// [`UpdateService::set_channel`]), the cache is unreadable, or update checks are disabled.
    pub fn cached_at(&self) -> Option<std::time::SystemTime> {
        let target = self.require_manifest_url().ok()?;
        self.stored_check(&target.cache_scope)
            .map(|stored| stored.checked_at.into())
    }

    /// The stored outcome of the last check, if it was made for `cache_scope` (version and channel).
    fn stored_check(&self, cache_scope: &str) -> Option<but_db::cache::CachedCheckResult> {
        self.with_cache(|cache| cache.update_check().try_get())
            .ok()
            .flatten()
            .filter(|stored| stored.status.valid_for_version.as_deref() == Some(cache_scope))
    }

    /// Hide the update notice for `hours` (1..=720). A later check keeps a running snooze while it finds the
    /// same version; a newer published version ends the snooze.
    ///
    /// # Errors
    ///
    /// [`UpdateError::InvalidArgument`] for hours outside 1..=720, [`UpdateError::Disabled`] without a
    /// manifest URL, [`UpdateError::Cache`] if no check has been made yet or the cache cannot be written.
    pub fn snooze(&self, hours: u32) -> Result<(), UpdateError> {
        self.require_manifest_url()?;
        if !(1..=MAX_SNOOZE_HOURS).contains(&hours) {
            return Err(UpdateError::InvalidArgument(format!(
                "snooze must be 1 to {MAX_SNOOZE_HOURS} hours, got {hours}"
            )));
        }
        Ok(self.with_cache(|cache| but_update::suppress_update(cache, hours))?)
    }
}
