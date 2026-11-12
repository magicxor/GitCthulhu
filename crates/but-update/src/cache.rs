//! Caching layer for update check results to avoid redundant network requests.
//!
//! The outcome of the last check is persisted in the app cache database, which allows to:
//! - show a known update without making a network request
//! - track when the last successful update check occurred
//! - temporarily suppress (snooze) the notice for a configurable duration

use chrono::{DateTime, Utc};

use crate::{CheckError, check::suppression_expired};

/// The longest snooze, 30 days.
const MAX_SUPPRESSION_HOURS: u32 = 720;

fn cache_error(err: impl std::fmt::Display) -> CheckError {
    CheckError::Cache(err.to_string())
}

/// Returns the timestamp of the last successful update check, if available.
///
/// # Returns
///
/// * `Ok(Some(timestamp))` - The UTC timestamp of the last update check
/// * `Ok(None)` - No update check has been cached yet
/// * `Err(_)` - Failed to access the cache
pub fn last_checked(cache: &but_db::AppCacheHandle) -> Result<Option<DateTime<Utc>>, CheckError> {
    Ok(cached_result(cache)?.map(|cached| cached.checked_at))
}

/// Information about an available application update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    /// The version of the running application.
    pub current_version: semver::Version,
    /// The latest available version.
    pub available_version: semver::Version,
    /// Markdown-formatted release notes for the new version.
    pub release_notes: Option<String>,
    /// Download (or release page) URL.
    pub url: Option<String>,
    /// Detached signature of the download.
    pub signature: Option<String>,
}

fn cached_result(
    cache: &but_db::AppCacheHandle,
) -> Result<Option<but_db::cache::CachedCheckResult>, CheckError> {
    cache.update_check().try_get().map_err(cache_error)
}

/// Like [`available_update_at`], judged at the current time.
pub fn available_update(
    cache: &but_db::AppCacheHandle,
    current: &semver::Version,
    cache_scope: &str,
) -> Result<Option<AvailableUpdate>, CheckError> {
    available_update_at(cache, current, cache_scope, Utc::now())
}

/// Returns information about an available application update, if one exists and is not snoozed at `now`.
///
/// The cached check must say that an update is available and must have been produced for `cache_scope`
/// (see [`crate::CheckParams::cache_scope`]); a result of another version or channel is stale and ignored,
/// as is one whose version can no longer be parsed.
///
/// # Returns
///
/// * `Ok(Some(AvailableUpdate))` - An update is available and not snoozed
/// * `Ok(None)` - No update is available, no cache exists, the cache is stale or the update is snoozed
/// * `Err(_)` - Failed to access the cache
pub fn available_update_at(
    cache: &but_db::AppCacheHandle,
    current: &semver::Version,
    cache_scope: &str,
    now: DateTime<Utc>,
) -> Result<Option<AvailableUpdate>, CheckError> {
    let Some(cached) = cached_result(cache)? else {
        return Ok(None);
    };
    if cached.status.up_to_date || cached.status.valid_for_version.as_deref() != Some(cache_scope) {
        return Ok(None);
    }
    if let (Some(suppressed_at), Some(hours)) =
        (cached.suppressed_at, cached.suppress_duration_hours)
        && !suppression_expired(suppressed_at, hours, now)
    {
        return Ok(None);
    }
    let Ok(available_version) = semver::Version::parse(&cached.status.latest_version) else {
        return Ok(None);
    };
    Ok(Some(AvailableUpdate {
        current_version: current.clone(),
        available_version,
        release_notes: cached.status.release_notes,
        url: cached.status.url,
        signature: cached.status.signature,
    }))
}

/// Snooze the update notice for `hours`, 1 to 720 (30 days).
///
/// # Errors
///
/// [`CheckError::Cache`] if `hours` is out of range, no update check has been performed yet (nothing to
/// snooze), or the cache cannot be written.
pub fn suppress_update(cache: &mut but_db::AppCacheHandle, hours: u32) -> Result<(), CheckError> {
    if hours == 0 {
        return Err(CheckError::Cache(
            "Suppression duration must be at least 1 hour".into(),
        ));
    }
    if hours > MAX_SUPPRESSION_HOURS {
        return Err(CheckError::Cache(format!(
            "Suppression duration cannot exceed {MAX_SUPPRESSION_HOURS} hours (30 days)"
        )));
    }
    cache
        .update_check_mut()
        .and_then(|handle| handle.suppress(hours))
        .map_err(cache_error)
}
