//! Update-check and avatar backend calls (owned by X5; spec 21 §2.10, §5.6). Every call here blocks: make it
//! inside `backend::bg`. The service calls are behind `cfg(feature = "services")`; without the feature they fail
//! with "built without integrations" (the placeholder `ServicesHandle` is never constructed anyway).
//!
//! There is no default manifest URL: `ServicesHandle::updates_enabled` is true only for builds that were given
//! one (`GITCTHULHU_UPDATE_MANIFEST_URL` at compile time), see [`manifest_url`].

use std::fmt;

use super::CancelToken;
use super::services::ServicesHandle;

/// An update that is newer than the running version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateNotice {
    /// The new version, as written in the manifest (`1.2.3`, without a leading `v`).
    pub version: String,
    /// Where the download (or the release page) is. Always `https` (the update service guarantees it).
    pub url: String,
}

/// Why an update call did not produce an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateCheckError {
    /// No manifest URL is configured: nothing is ever fetched.
    Disabled,
    /// The check was cancelled.
    Cancelled,
    /// The manifest could not be fetched or understood, or the cache failed (the text is for the user).
    Failed(String),
}

impl fmt::Display for UpdateCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateCheckError::Disabled => f.write_str("update checks are disabled"),
            UpdateCheckError::Cancelled => f.write_str("cancelled"),
            UpdateCheckError::Failed(text) => f.write_str(text),
        }
    }
}

/// What a download of an image came back with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageFetch {
    /// The image bytes.
    Found(Vec<u8>),
    /// The server answered "no such image" (a Gravatar address without a picture): remember that.
    NotFound,
    /// The server could not be reached or failed: try again later (the text is for the log).
    Failed(String),
}

/// The manifest URL this build was compiled with (`GITCTHULHU_UPDATE_MANIFEST_URL`); `None` for builds without one.
/// May contain `{channel}`, which the update service replaces with `release` or `nightly`.
pub fn manifest_url() -> Option<&'static str> {
    option_env!("GITCTHULHU_UPDATE_MANIFEST_URL").filter(|url| !url.is_empty())
}

/// Whether this build can offer the Nightly channel: its manifest URL has a `{channel}` placeholder.
pub fn nightly_channel_available() -> bool {
    manifest_url().is_some_and(|url| url.contains("{channel}"))
}

/// Fetches the manifest and returns the update to announce (`None` when the running version is current or the
/// update is snoozed). Blocks up to the service's 30 s timeout; `cancel` interrupts the wait.
#[cfg(feature = "services")]
pub fn check_now(
    services: &ServicesHandle,
    cancel: &CancelToken,
) -> Result<Option<UpdateNotice>, UpdateCheckError> {
    use gitcthulhu_services::update::UpdateError;
    services
        .services()
        .update
        .check_now(cancel)
        .map(|found| found.map(notice_of))
        .map_err(|err| match err {
            UpdateError::Disabled => UpdateCheckError::Disabled,
            UpdateError::Cancelled => UpdateCheckError::Cancelled,
            other => UpdateCheckError::Failed(other.to_string()),
        })
}

/// Without the `services` feature there is nothing to ask.
#[cfg(not(feature = "services"))]
pub fn check_now(
    _services: &ServicesHandle,
    _cancel: &CancelToken,
) -> Result<Option<UpdateNotice>, UpdateCheckError> {
    Err(UpdateCheckError::Failed(
        "built without integrations".into(),
    ))
}

/// The update found by the last check, from the cache (no network); `None` when there is none, it is snoozed
/// or checks are disabled.
#[cfg(feature = "services")]
pub fn cached(services: &ServicesHandle) -> Option<UpdateNotice> {
    services.services().update.cached().map(notice_of)
}

/// Without the `services` feature there is no cache.
#[cfg(not(feature = "services"))]
pub fn cached(_services: &ServicesHandle) -> Option<UpdateNotice> {
    None
}

/// Hides the notice for `hours` (1 to 720). Needs a finished check (the service remembers the snooze next to the
/// check's outcome).
#[cfg(feature = "services")]
pub fn snooze(services: &ServicesHandle, hours: u32) -> Result<(), UpdateCheckError> {
    use gitcthulhu_services::update::UpdateError;
    services
        .services()
        .update
        .snooze(hours)
        .map_err(|err| match err {
            UpdateError::Disabled => UpdateCheckError::Disabled,
            UpdateError::Cancelled => UpdateCheckError::Cancelled,
            other => UpdateCheckError::Failed(other.to_string()),
        })
}

/// Without the `services` feature there is nothing to snooze.
#[cfg(not(feature = "services"))]
pub fn snooze(_services: &ServicesHandle, _hours: u32) -> Result<(), UpdateCheckError> {
    Err(UpdateCheckError::Failed(
        "built without integrations".into(),
    ))
}

#[cfg(feature = "services")]
fn notice_of(update: gitcthulhu_services::update::AvailableUpdate) -> UpdateNotice {
    UpdateNotice {
        version: update.version.to_string(),
        url: update.url,
    }
}

/// Fetches an image (an avatar) through the forge HTTP stack (30 s timeout, 16 MiB cap).
#[cfg(feature = "services")]
pub fn fetch_image(services: &ServicesHandle, url: &str) -> ImageFetch {
    use gitcthulhu_services::forge::ForgeError;
    match services.services().forge.fetch_image(url) {
        Ok(bytes) => ImageFetch::Found(bytes),
        Err(ForgeError::NotFound) => ImageFetch::NotFound,
        Err(err) => ImageFetch::Failed(err.to_string()),
    }
}

/// Without the `services` feature there is no HTTP stack.
#[cfg(not(feature = "services"))]
pub fn fetch_image(_services: &ServicesHandle, _url: &str) -> ImageFetch {
    ImageFetch::Failed("built without integrations".into())
}

/// Services whose update check reads `manifest_url` (a loopback URL of a test or QA server; `None` disables update
/// checks) and pretend to run `app_version`, keeping their data below `data_dir` and `cache_dir`. Blocking (builds the
/// runtime): call it inside `backend::bg` or a test.
#[cfg(all(feature = "services", any(test, feature = "qa")))]
pub fn services_with_manifest(
    data_dir: std::path::PathBuf,
    cache_dir: std::path::PathBuf,
    manifest_url: Option<&str>,
    app_version: &str,
) -> anyhow::Result<ServicesHandle> {
    use std::sync::Arc;

    use gitcthulhu_services::{Services, ServicesConfig, UpdateChannel};
    let config = ServicesConfig {
        app_data_dir: data_dir,
        cache_dir,
        user_agent: format!("GitCthulhu/{app_version}"),
        app_version: semver::Version::parse(app_version)?,
        github_oauth_client_id: None,
        update_manifest_url: manifest_url.map(str::to_owned),
        update_channel: UpdateChannel::Release,
    };
    Ok(ServicesHandle {
        services: Arc::new(Services::new(config)?),
        github_device_flow: false,
        updates_enabled: manifest_url.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn error_text_is_readable() {
        assert_eq!(
            UpdateCheckError::Failed("network error: down".into()).to_string(),
            "network error: down"
        );
        assert_eq!(
            UpdateCheckError::Disabled.to_string(),
            "update checks are disabled"
        );
    }

    #[test]
    fn this_build_has_no_default_manifest_url() {
        // The environment variable is only set by a release build; a plain build never points anywhere.
        if option_env!("GITCTHULHU_UPDATE_MANIFEST_URL").is_none() {
            assert_eq!(manifest_url(), None);
            assert!(!nightly_channel_available());
        }
    }
}
