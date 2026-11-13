//! Forge, AI and update services (spec 21 §2.10). `gitcthulhu-services` owns the tokio runtime and exposes blocking
//! calls; the GUI makes them inside `backend::bg` and never creates a runtime itself.

#[cfg(feature = "services")]
use std::sync::Arc;

use crate::settings::Settings;

/// The running services (feature `services`).
#[cfg(feature = "services")]
#[derive(Clone)]
pub struct ServicesHandle {
    pub(crate) services: Arc<gitcthulhu_services::Services>,
    /// GitHub's device flow is offered (`ServicesConfig::github_oauth_client_id` is set).
    pub github_device_flow: bool,
    /// Update checks are possible (`ServicesConfig::update_manifest_url` is set).
    pub updates_enabled: bool,
}

/// Placeholder of the handle in builds without the `services` feature; never constructed.
#[cfg(not(feature = "services"))]
#[derive(Clone)]
pub struct ServicesHandle {
    _private: (),
}

#[cfg(not(feature = "services"))]
impl ServicesHandle {
    /// Without the `services` feature there is nothing to switch.
    pub fn set_update_channel(&self, _channel: crate::settings::UpdateChannel) {}
}

#[cfg(feature = "services")]
impl ServicesHandle {
    /// The services (for the region backend files `backend/{ai,forge,update}.rs`).
    pub(crate) fn services(&self) -> &Arc<gitcthulhu_services::Services> {
        &self.services
    }

    /// Makes later update checks look at `channel` at once (no restart). Nothing is fetched: the caller runs a
    /// check when it wants the channel's answer. It only swaps a value under a lock that is never held across I/O,
    /// so it may run on the UI thread (which keeps quick successive changes in order).
    pub fn set_update_channel(&self, channel: crate::settings::UpdateChannel) {
        self.services.update.set_channel(match channel {
            crate::settings::UpdateChannel::Release => gitcthulhu_services::UpdateChannel::Release,
            crate::settings::UpdateChannel::Nightly => gitcthulhu_services::UpdateChannel::Nightly,
        });
    }
}

/// Starts the services (blocking: builds the runtime and opens the forge storage; call it inside `backend::bg`).
/// Without the `services` feature it fails with "built without integrations".
#[cfg(feature = "services")]
pub fn start_services(settings: &Settings) -> anyhow::Result<ServicesHandle> {
    use gitcthulhu_services::{Services, ServicesConfig, UpdateChannel};

    let version = env!("CARGO_PKG_VERSION");
    let github_oauth_client_id = option_env!("GITCTHULHU_GITHUB_CLIENT_ID")
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let update_manifest_url = option_env!("GITCTHULHU_UPDATE_MANIFEST_URL")
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let config = ServicesConfig {
        app_data_dir: crate::paths::data_dir(),
        cache_dir: crate::paths::cache_dir(),
        user_agent: format!("GitCthulhu/{version}"),
        app_version: semver::Version::parse(version)?,
        github_oauth_client_id: github_oauth_client_id.clone(),
        update_manifest_url: update_manifest_url.clone(),
        update_channel: match settings.update_channel {
            crate::settings::UpdateChannel::Release => UpdateChannel::Release,
            crate::settings::UpdateChannel::Nightly => UpdateChannel::Nightly,
        },
    };
    let services = Services::new(config)?;
    Ok(ServicesHandle {
        services: Arc::new(services),
        github_device_flow: github_oauth_client_id.is_some(),
        updates_enabled: update_manifest_url.is_some(),
    })
}

/// Starts the services (blocking). Without the `services` feature it fails with "built without integrations".
#[cfg(not(feature = "services"))]
pub fn start_services(_settings: &Settings) -> anyhow::Result<ServicesHandle> {
    anyhow::bail!("built without integrations")
}
