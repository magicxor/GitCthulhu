//! Application-wide directories under the `gitcthulhu` identity.
//!
//! The layout matches the GUI's own `paths` module (`crates/gitcthulhu/src/paths.rs`): every directory is named
//! [`APP_DIR`] below the platform's directory of its kind. When the [`HOME_ENV`] environment variable is set,
//! all of them are subdirectories of it instead (tests and portable installs).
//!
//! Nothing here points into a GitButler directory.
use std::path::PathBuf;

use anyhow::Context as _;

/// Directory name used under every platform directory, and the application identifier.
pub const APP_DIR: &str = "gitcthulhu";
/// Environment variable that redirects every application directory.
pub const HOME_ENV: &str = "GITCTHULHU_HOME";

fn home_override() -> Option<PathBuf> {
    std::env::var_os(HOME_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn platform_dir(dir: Option<PathBuf>, override_leaf: &str, what: &str) -> anyhow::Result<PathBuf> {
    if let Some(home) = home_override() {
        return Ok(home.join(override_leaf));
    }
    dir.map(|d| d.join(APP_DIR))
        .with_context(|| format!("Could not determine the platform's {what} directory"))
}

/// The directory to store application-wide data in (`<data_local_dir>/gitcthulhu`, or `$GITCTHULHU_HOME/data`).
pub fn app_data_dir() -> anyhow::Result<PathBuf> {
    platform_dir(dirs::data_local_dir(), "data", "local data")
}

/// The directory to store logs in (`<app_data_dir>/logs`, or `$GITCTHULHU_HOME/logs`).
pub fn app_log_dir() -> anyhow::Result<PathBuf> {
    if let Some(home) = home_override() {
        return Ok(home.join("logs"));
    }
    Ok(app_data_dir()?.join("logs"))
}

/// The directory to store application-wide settings in (`<config_dir>/gitcthulhu`, or `$GITCTHULHU_HOME/config`).
pub fn app_config_dir() -> anyhow::Result<PathBuf> {
    platform_dir(dirs::config_dir(), "config", "configuration")
}

/// The directory for non-essential data that can be regenerated or re-downloaded, such as update check metadata
/// (`<cache_dir>/gitcthulhu`, or `$GITCTHULHU_HOME/cache`).
///
/// Unlike data stored in [`app_data_dir`], cached data should not be backed up, can be safely deleted to free up
/// disk space, and has no user-visible impact if cleared.
pub fn app_cache_dir() -> anyhow::Result<PathBuf> {
    platform_dir(dirs::cache_dir(), "cache", "cache")
}

/// Returns the application identifier, [`APP_DIR`].
pub fn identifier() -> &'static str {
    APP_DIR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_directory_uses_the_gitcthulhu_identity() -> anyhow::Result<()> {
        if home_override().is_some() {
            // The redirect is a valid configuration, but then the platform directories aren't used.
            return Ok(());
        }
        for dir in [app_data_dir()?, app_config_dir()?, app_cache_dir()?] {
            assert!(
                dir.ends_with(APP_DIR),
                "{} must live under the gitcthulhu identity",
                dir.display()
            );
        }
        assert!(
            app_log_dir()?.ends_with(PathBuf::from(APP_DIR).join("logs")),
            "logs live in the data directory"
        );
        Ok(())
    }
}
