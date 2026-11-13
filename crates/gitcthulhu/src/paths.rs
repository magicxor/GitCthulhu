//! Application directories under the `gitcthulhu` identity (spec 21 §7.1).
//!
//! Nothing here points into a GitButler directory. When `GITCTHULHU_HOME` is set, all four directories are
//! subdirectories of it (tests and portable installs). Unit tests of this crate without the variable use a
//! per-process directory under the system temp directory instead of the real platform directories.

use std::path::PathBuf;

/// Directory name used under every platform directory.
pub const APP_DIR: &str = "gitcthulhu";
/// Environment variable that redirects every application directory (used by tests).
pub const HOME_ENV: &str = "GITCTHULHU_HOME";

fn home_override() -> Option<PathBuf> {
    let from_env = std::env::var_os(HOME_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    // Unit tests never touch the user's real directories, even when a test forgets to set `GITCTHULHU_HOME`:
    // debounced saves (settings, UI state) would otherwise land in `%APPDATA%\gitcthulhu`.
    #[cfg(test)]
    let fallback = Some(test_home().clone());
    #[cfg(not(test))]
    let fallback = None;
    from_env.or(fallback)
}

/// Process-wide scratch home of the unit tests (one per test binary run).
#[cfg(test)]
fn test_home() -> &'static PathBuf {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        std::env::temp_dir()
            .join("gitcthulhu-tests")
            .join(std::process::id().to_string())
    })
}

fn platform_dir(dir: Option<PathBuf>, override_leaf: &str) -> PathBuf {
    if let Some(home) = home_override() {
        return home.join(override_leaf);
    }
    dir.map(|d| d.join(APP_DIR))
        // Without any platform directory the working directory is the only writable place we can name.
        .unwrap_or_else(|| PathBuf::from(format!(".{APP_DIR}")).join(override_leaf))
}

/// Directory of `settings.json`, `state.json` and `repos.json` (`%APPDATA%\gitcthulhu` on Windows).
pub fn config_dir() -> PathBuf {
    platform_dir(dirs::config_dir(), "config")
}

/// Directory for application data (`dirs::data_local_dir()/gitcthulhu`).
pub fn data_dir() -> PathBuf {
    platform_dir(dirs::data_local_dir(), "data")
}

/// Directory for caches (`dirs::cache_dir()/gitcthulhu`).
pub fn cache_dir() -> PathBuf {
    platform_dir(dirs::cache_dir(), "cache")
}

/// Directory of the rolling log files: `<data>/logs`.
pub fn log_dir() -> PathBuf {
    if let Some(home) = home_override() {
        return home.join("logs");
    }
    data_dir().join("logs")
}

/// `<config>/settings.json`.
pub fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

/// `<config>/state.json`.
pub fn state_file() -> PathBuf {
    config_dir().join("state.json")
}

/// `<config>/repos.json`.
pub fn repos_file() -> PathBuf {
    config_dir().join("repos.json")
}

/// Creates the four application directories.
pub fn ensure_dirs() -> std::io::Result<()> {
    for dir in [config_dir(), data_dir(), cache_dir(), log_dir()] {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn app_dir_names_use_the_gitcthulhu_identity() {
        // Not asserting the override here: environment variables are process-global and tests run in parallel.
        for dir in [config_dir(), data_dir(), cache_dir()] {
            assert!(
                home_override().is_some()
                    || dir.ends_with(APP_DIR)
                    || dir.starts_with(format!(".{APP_DIR}")),
                "{} must live under the gitcthulhu identity",
                dir.display()
            );
        }
        assert!(
            log_dir().ends_with("logs"),
            "logs live in a `logs` directory, got {}",
            log_dir().display()
        );
    }

    #[test]
    fn tests_never_use_the_real_user_directories() {
        let home = home_override().expect("unit tests always have an overridden home");
        for dir in [config_dir(), data_dir(), cache_dir(), log_dir()] {
            assert!(
                dir.starts_with(&home),
                "{} must be below the test home {}",
                dir.display(),
                home.display()
            );
        }
        if let Some(real) = dirs::config_dir() {
            assert!(
                !settings_file().starts_with(real.join(APP_DIR)),
                "settings saved by tests must not reach the user's config directory"
            );
        }
    }
}
