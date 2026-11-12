//! Update check against a static manifest. 20 section 8.5. Stub: the implementation is owned by WP-S3.

use gitcthulhu_git::CancelToken;

use crate::{Runtime, ServicesConfig};

/// An update that is newer than the running version.
#[derive(Debug, Clone)]
pub struct AvailableUpdate {
    /// The new version.
    pub version: semver::Version,
    /// Release notes.
    pub notes: Option<String>,
    /// Download URL.
    pub url: String,
    /// Detached signature, if the manifest has one.
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
    /// The manifest could not be parsed.
    #[error("invalid update manifest: {0}")]
    Manifest(String),
    /// The cache database failed.
    #[error("update cache error: {0}")]
    Cache(String),
    /// The operation was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// The operation is not implemented in this skeleton (WP-S3 replaces it).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// The update service.
pub struct UpdateService {
    #[allow(dead_code)]
    rt: Runtime,
    manifest_url: Option<String>,
}

impl UpdateService {
    /// Create the service from the shared configuration.
    pub fn new(rt: Runtime, cfg: &ServicesConfig) -> Self {
        UpdateService {
            rt,
            manifest_url: cfg.update_manifest_url.clone(),
        }
    }

    /// Fetch the manifest and compare versions.
    pub fn check_now(&self, _cancel: &CancelToken) -> Result<Option<AvailableUpdate>, UpdateError> {
        if self.manifest_url.is_none() {
            return Err(UpdateError::Disabled);
        }
        Err(UpdateError::NotImplemented("update::check_now"))
    }

    /// The update found by the last check, from the cache.
    pub fn cached(&self) -> Option<AvailableUpdate> {
        None
    }

    /// Hide update notices for `hours` (1..=720).
    pub fn snooze(&self, _hours: u32) -> Result<(), UpdateError> {
        Err(UpdateError::NotImplemented("update::snooze"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UpdateChannel;

    fn config(update_manifest_url: Option<String>) -> ServicesConfig {
        let dir = std::env::temp_dir().join("gitcthulhu-services-update-stub");
        ServicesConfig {
            app_data_dir: dir.join("data"),
            cache_dir: dir.join("cache"),
            user_agent: "GitCthulhu/0.0.0".into(),
            app_version: semver::Version::new(0, 0, 0),
            github_oauth_client_id: None,
            update_manifest_url,
            update_channel: UpdateChannel::Release,
        }
    }

    #[test]
    fn check_now_is_disabled_without_manifest_url() {
        let rt = Runtime::new().expect("runtime builds");
        let disabled = UpdateService::new(rt.clone(), &config(None));
        assert!(
            matches!(
                disabled.check_now(&CancelToken::new()),
                Err(UpdateError::Disabled)
            ),
            "dev builds without a manifest URL never check"
        );
        let enabled =
            UpdateService::new(rt, &config(Some("https://example.invalid/m.json".into())));
        assert!(
            matches!(
                enabled.check_now(&CancelToken::new()),
                Err(UpdateError::NotImplemented(_))
            ),
            "with a manifest URL the stub reports that checking is not implemented yet"
        );
    }
}
