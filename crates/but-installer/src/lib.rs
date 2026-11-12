#![cfg(unix)]
//! GitCthulhu installer library
//!
//! This library provides functionality for installing GitCthulhu on Linux and macOS systems.
//! It handles downloading releases, verifying signatures, extracting archives,
//! and setting up the application and the `PATH` entry for its binary.
//!
//! The release server and the signing key are explicit configuration
//! ([`RELEASE_URL_ENV`], [`RELEASE_PUBLIC_KEY_ENV`]); there are no defaults.

mod config;
mod download;
mod http;
mod install;
mod release;
mod shell;
pub mod ui;

#[cfg(target_os = "linux")]
mod install_linux;

#[cfg(target_os = "macos")]
mod install_macos;

use anyhow::Result;
use config::{Channel, InstallerConfig};
// Re-export types for public API consumers
pub use config::{
    RELEASE_PUBLIC_KEY_ENV, RELEASE_URL_ENV, ReleaseSource, VERSION_ENV, Version, VersionRequest,
};
use install::APP_BINARY_NAME;
use release::fetch_release;
use shell::configure_shell;
use ui::{info, success};

#[cfg(target_os = "linux")]
use crate::install_linux::download_and_install_app;
#[cfg(target_os = "macos")]
use crate::install_macos::download_and_install_app;

/// Runs the complete GitCthulhu installation process with a specific version.
///
/// This is the main entry point that orchestrates:
/// - Fetching release information from the release API configured in [`RELEASE_URL_ENV`]
/// - Downloading the application and verifying it against [`RELEASE_PUBLIC_KEY_ENV`]
/// - Extracting and installing the app bundle
/// - Setting up the `PATH` entry for the application binary
///
/// # Arguments
/// * `version_request` - Version to install (Release, Nightly, or Specific version).
/// * `interactive` - Whether this is an interactive installation (e.g., from the install script).
///   When `true`, offers to configure the shell and prints usage instructions after installation.
///
/// Returns an error if any step fails. The function will attempt to rollback
/// changes on installation failure.
pub fn run_installation_with_version(
    version_request: VersionRequest,
    interactive: bool,
) -> Result<()> {
    let config = InstallerConfig::new_with_version(version_request)?;
    run_installation_impl(config, interactive)
}

/// The environment variable that forces a non-interactive installation when set to a non-empty value.
pub const NONINTERACTIVE_ENV: &str = "GITCTHULHU_NONINTERACTIVE";

/// Runs the complete GitCthulhu installation process.
///
/// Reads version from command-line arguments or the [`VERSION_ENV`] environment variable.
/// If neither is provided, installs the latest release.
///
/// This is the entry point for the standalone installer binary, and runs in interactive mode with
/// shell configuration if there is a terminal connected. Otherwise, or when [`NONINTERACTIVE_ENV`]
/// is set to a non-empty value (for scripts that wrap the installer), it runs non-interactively.
///
/// Returns an error if any step fails. The function will attempt to rollback
/// changes on installation failure.
pub fn run_installation() -> Result<()> {
    let config = InstallerConfig::new()?;
    let forced_noninteractive = std::env::var(NONINTERACTIVE_ENV).is_ok_and(|v| !v.is_empty());
    let interactive = ui::is_connected_to_terminal() && !forced_noninteractive;
    run_installation_impl(config, interactive)
}

fn run_installation_impl(config: InstallerConfig, interactive: bool) -> Result<()> {
    if interactive && !ui::is_connected_to_terminal() {
        anyhow::bail!("interactive mode requested, but there is no terminal to prompt on");
    }

    info(&format!("Detected platform: {}", config.platform));

    // Fetch release information
    let message = match &config.version_request {
        VersionRequest::Nightly => "Fetching latest nightly release information...",
        VersionRequest::Specific(_) => "Fetching release information...",
        VersionRequest::Release => "Fetching latest release information...",
    };
    info(message);

    let release = fetch_release(&config)?;

    // Display version information
    let channel = match &config.version_request {
        VersionRequest::Nightly => {
            info(&format!(
                "Installing latest nightly version: {}",
                release.version
            ));
            Some(Channel::Nightly)
        }
        VersionRequest::Specific(version) => {
            info(&format!("Installing version: {version}"));
            None
        }
        VersionRequest::Release => {
            info(&format!(
                "Installing latest release version: {}",
                release.version
            ));
            Some(Channel::Release)
        }
    };

    let platform_info = release
        .platforms
        .get(&config.platform)
        .ok_or_else(|| anyhow::anyhow!("Platform {} not found in release", config.platform))?;

    download_and_install_app(&config, platform_info, &release, channel)?;

    if interactive {
        info("Checking shell configuration");
        configure_shell(&config.home_dir)?;
    }

    ui::println_empty();
    success(&format!(
        "✓ GitCthulhu installation completed! ({}{})",
        channel
            .map(|c| format!("{} ", c.display_name()))
            .unwrap_or_default(),
        release.version
    ));
    ui::println_empty();

    if interactive {
        info(&format!(
            "Start GitCthulhu with: {APP_BINARY_NAME} [repository directory]"
        ));
    }

    Ok(())
}
