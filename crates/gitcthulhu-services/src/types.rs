//! Configuration types shared by all services (20 §8.2).

use std::path::PathBuf;

/// Which release stream the update check follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum UpdateChannel {
    /// Stable releases.
    #[default]
    Release,
    /// Nightly builds.
    Nightly,
}

/// Everything [`crate::Services::new`] needs from the application.
#[derive(Debug, Clone)]
pub struct ServicesConfig {
    /// The app data directory (`.../gitcthulhu`, the `but-path` identity from D3).
    pub app_data_dir: PathBuf,
    /// The cache directory.
    pub cache_dir: PathBuf,
    /// The HTTP user agent, `"GitCthulhu/<version>"`.
    pub user_agent: String,
    /// The running application's version.
    pub app_version: semver::Version,
    /// OAuth client id for GitHub's device flow. `None` until GitCthulhu has its own OAuth app: PAT only.
    pub github_oauth_client_id: Option<String>,
    /// The static update manifest. `None` disables update checks (dev builds).
    pub update_manifest_url: Option<String>,
    /// The update stream.
    pub update_channel: UpdateChannel,
}
