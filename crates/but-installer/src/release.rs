//! Release fetching and validation

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::{config::InstallerConfig, http::create_client};

/// Release information from the release API
#[derive(Debug, Deserialize)]
pub struct Release {
    pub version: String,
    pub platforms: HashMap<String, PlatformInfo>,
}

#[derive(Debug, Deserialize)]
pub struct PlatformInfo {
    pub url: Option<String>,

    #[cfg(target_os = "macos")]
    pub signature: String,
}

pub(crate) fn fetch_release(config: &InstallerConfig) -> Result<Release> {
    let url = config.releases_url();
    let mut easy = create_client()?;

    easy.url(&url)
        .with_context(|| format!("Failed to set URL: {url}"))?;

    let mut response_data = Vec::new();
    {
        let mut transfer = easy.transfer();
        transfer
            .write_function(|data| {
                response_data.extend_from_slice(data);
                Ok(data.len())
            })
            .context("Failed to set write function")?;
        transfer
            .perform()
            .with_context(|| format!("Failed to fetch release information from {url}"))?;
    }

    let response_code = easy
        .response_code()
        .context("Failed to get response code")?;

    if response_code != 200 {
        match &config.version_request {
            crate::config::VersionRequest::Specific(version) => {
                bail!(
                    "Failed to fetch release information for version {version}. Version may not exist. HTTP {response_code}"
                );
            }
            crate::config::VersionRequest::Nightly => {
                bail!("Failed to fetch nightly release information. HTTP {response_code}");
            }
            crate::config::VersionRequest::Release => {
                bail!("Failed to fetch release information from {url}. HTTP {response_code}");
            }
        }
    }

    // Validate the effective URL after following redirects
    // This protects against malicious redirects to untrusted domains or insecure protocols
    let effective_url = easy
        .effective_url()
        .context("Failed to get effective URL")?
        .ok_or_else(|| anyhow!("Effective URL is missing"))?;

    validate_api_url(effective_url, config.source.host()).with_context(|| {
        format!("Release API was redirected to an untrusted URL: {effective_url}")
    })?;

    let release: Release =
        serde_json::from_slice(&response_data).context("Failed to parse release information")?;

    // Verify we got the version we requested (skip check for nightly)
    if let crate::config::VersionRequest::Specific(ref requested) = config.version_request
        && release.version != requested.as_str()
    {
        bail!(
            "API returned version {} but requested version {}",
            release.version,
            requested
        );
    }

    Ok(release)
}

/// Common URL validation logic for release URLs.
///
/// Validates HTTPS protocol, parses URL, and checks the host against a predicate.
fn validate_release_url(
    url: &str,
    url_type: &str,
    is_host_valid: impl Fn(&str) -> bool,
) -> Result<()> {
    // Only allow HTTPS URLs
    if !url.starts_with("https://") {
        bail!("{url_type} must use HTTPS: {url}");
    }

    // Extract host from URL
    let url_parsed =
        url::Url::parse(url).with_context(|| format!("Invalid {} URL", url_type.to_lowercase()))?;
    let host = url_parsed
        .host_str()
        .ok_or_else(|| anyhow!("No host in {} URL", url_type.to_lowercase()))?;

    // Validate host using the provided predicate
    if !is_host_valid(host) {
        bail!("{url_type} is not from the trusted release host: {url}");
    }

    Ok(())
}

/// Validates that an API URL is served by `trusted_host`, the host of the configured release URL.
///
/// API endpoints may not be redirected to any other host, not even a subdomain.
pub(crate) fn validate_api_url(url: &str, trusted_host: &str) -> Result<()> {
    validate_release_url(url, "API URL", |host| host == trusted_host)
}

/// Validates that a download URL is served by `trusted_host` or one of its subdomains.
///
/// This is more permissive than API validation to allow CDN distribution and mirrors below the
/// release host (e.g. `cdn.<trusted_host>`), which are under the same operator's control.
pub(crate) fn validate_download_url(url: &str, trusted_host: &str) -> Result<()> {
    validate_release_url(url, "Download URL", |host| {
        host == trusted_host
            || host
                .strip_suffix(trusted_host)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host of the test release URL; it is never contacted.
    const HOST: &str = "releases.example.com";

    #[test]
    fn test_validate_api_url() {
        // Valid API URLs - only the configured host
        assert!(validate_api_url("https://releases.example.com/releases", HOST).is_ok());
        assert!(validate_api_url("https://releases.example.com/releases/nightly", HOST).is_ok());
        assert!(
            validate_api_url("https://releases.example.com/releases/version/0.18.7", HOST).is_ok()
        );

        // Invalid - wrong protocol
        assert!(validate_api_url("http://releases.example.com/releases", HOST).is_err());

        // Invalid - other hosts of the same domain (even if trusted for downloads)
        assert!(validate_api_url("https://cdn.releases.example.com/releases", HOST).is_err());
        assert!(validate_api_url("https://example.com/releases", HOST).is_err());

        // Invalid - untrusted domains
        assert!(validate_api_url("https://evil.com/releases", HOST).is_err());
        assert!(validate_api_url("https://releases.example.com.evil.com/releases", HOST).is_err());
    }

    #[test]
    fn test_validate_download_url() {
        // Valid - the release host itself
        assert!(validate_download_url("https://releases.example.com/file.tar.gz", HOST).is_ok());

        // Valid - any subdomain of the release host
        assert!(
            validate_download_url("https://cdn.releases.example.com/file.tar.gz", HOST).is_ok()
        );
        assert!(
            validate_download_url("https://a.mirror.releases.example.com/file.tar.gz", HOST)
                .is_ok()
        );

        // Invalid - wrong protocol
        assert!(validate_download_url("http://releases.example.com/file.tar.gz", HOST).is_err());

        // Invalid - parent domains and siblings
        assert!(validate_download_url("https://example.com/file.tar.gz", HOST).is_err());
        assert!(validate_download_url("https://other.example.com/file.tar.gz", HOST).is_err());

        // Invalid - subdomain spoofing attempts
        assert!(
            validate_download_url("https://releases.example.com.evil.com/file.tar.gz", HOST)
                .is_err()
        );
        assert!(
            validate_download_url("https://evilreleases.example.com/file.tar.gz", HOST).is_err()
        );

        // Invalid - untrusted domains
        assert!(validate_download_url("https://evil.com/file.tar.gz", HOST).is_err());
    }

    #[test]
    fn test_release_parsing_allows_null_platform_url() {
        let json = r#"{
            "version": "0.5.1",
            "platforms": {
                "darwin-aarch64": {
                    "url": "https://releases.example.com/file.tar.gz",
                    "signature": "sig-darwin"
                },
                "windows-x86_64": {
                    "url": null,
                    "signature": "sig-windows"
                }
            }
        }"#;

        let release: Release = serde_json::from_str(json).unwrap();

        let darwin = release.platforms.get("darwin-aarch64").unwrap();
        let windows = release.platforms.get("windows-x86_64").unwrap();

        assert_eq!(
            darwin.url.as_deref(),
            Some("https://releases.example.com/file.tar.gz")
        );
        assert!(windows.url.is_none());
    }
}
