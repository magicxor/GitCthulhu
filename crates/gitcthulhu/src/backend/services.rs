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

#[cfg(feature = "services")]
impl ServicesHandle {
    /// The services (for the region backend files `backend/{ai,forge,update}.rs`).
    pub(crate) fn services(&self) -> &Arc<gitcthulhu_services::Services> {
        &self.services
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

/// `ai::SecretSource` over the OS keychain (`but_secret`, namespace "gitcthulhu" set by `Services::new`).
#[cfg(feature = "services")]
pub struct GuiSecretSource;

#[cfg(feature = "services")]
impl gitcthulhu_services::ai::SecretSource for GuiSecretSource {
    fn secret(&self, handle: &str) -> Option<String> {
        match but_secret::secret::retrieve(handle, but_secret::secret::Namespace::BuildKind) {
            Ok(secret) => secret.map(|s| s.0),
            Err(err) => {
                tracing::warn!("cannot read the secret {handle}: {err}");
                None
            }
        }
    }
}
