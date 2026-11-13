//! Configuration and platform detection

use std::{env, path::PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};

/// The environment variable naming the version to install, like the first argument does.
pub const VERSION_ENV: &str = "GITCTHULHU_VERSION";
/// The environment variable with the base URL of the release API, like `https://releases.example.com/releases`.
///
/// There is deliberately no default: the installer only talks to the release server it is told about.
pub const RELEASE_URL_ENV: &str = "GITCTHULHU_RELEASE_URL";
/// The environment variable with the base64 minisign public key that release artifacts are signed with.
///
/// There is deliberately no default, so a download can only be verified against a key the caller trusts.
pub const RELEASE_PUBLIC_KEY_ENV: &str = "GITCTHULHU_RELEASE_PUBLIC_KEY";

/// Channel type for the installation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Release,
    Nightly,
}

impl Channel {
    pub fn display_name(&self) -> &str {
        match self {
            Channel::Release => "Release",
            Channel::Nightly => "Nightly",
        }
    }
}

/// A validated version string for GitCthulhu installations.
///
/// This type guarantees that the version string:
/// - Is not empty
/// - Does not start with '-' (not a flag)
/// - Contains only semver-compatible characters (alphanumeric, '.', '-', '+')
/// - Contains at least one alphanumeric character
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version(String);

impl Version {
    /// Create a new Version from a string, validating the format.
    ///
    /// # Errors
    /// Returns an error if the version string is invalid.
    pub fn new(version: String) -> Result<Self> {
        Self::validate(&version)?;
        Ok(Version(version))
    }

    /// Validate a version string format
    fn validate(version: &str) -> Result<()> {
        // Reject empty strings
        if version.is_empty() {
            bail!("Invalid version: empty string. Usage: but-installer [version|nightly]");
        }

        // Reject if it looks like a flag
        if version.starts_with('-') {
            bail!("Invalid version: {version}. Usage: but-installer [version|nightly]");
        }

        // Only allow semver-compatible characters
        if !version
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '+')
        {
            bail!(
                "Invalid version format: {version}. Version must contain only alphanumeric characters, dots, hyphens, and plus signs."
            );
        }

        // Must contain at least one alphanumeric character
        if !version.chars().any(|c| c.is_alphanumeric()) {
            bail!(
                "Invalid version format: {version}. Version must contain at least one alphanumeric character."
            );
        }

        Ok(())
    }

    /// Get the version string as a &str
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Version {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Version request for the installer
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionRequest {
    /// Install the latest release version
    Release,
    /// Install the latest nightly build
    Nightly,
    /// Install a specific version (e.g., "0.18.7")
    Specific(Version),
}

impl VersionRequest {
    /// Parse a version string into a VersionRequest
    pub fn from_string(version: Option<String>) -> Result<Self> {
        match version.as_deref() {
            None => Ok(VersionRequest::Release),
            Some("nightly") => Ok(VersionRequest::Nightly),
            Some(version_str) => {
                let version = Version::new(version_str.to_string())?;
                Ok(VersionRequest::Specific(version))
            }
        }
    }
}

/// Where releases come from and how they are verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSource {
    /// The base URL of the release API: the latest release is at the URL itself, the latest nightly below
    /// `nightly` and a specific version below `version/<version>`.
    ///
    /// Always `https`, and its host is the only host release information is accepted from.
    pub base_url: url::Url,
    /// The base64 minisign public key that release artifacts must be signed with.
    pub public_key: String,
}

impl ReleaseSource {
    /// Create a release source from an explicit `base_url` and `public_key`.
    ///
    /// # Errors
    /// Returns an error if `base_url` isn't an `https` URL with a host, or if `public_key` is empty.
    pub fn new(base_url: &str, public_key: &str) -> Result<Self> {
        let parsed = url::Url::parse(base_url.trim())
            .with_context(|| format!("Invalid release URL: {base_url}"))?;
        if parsed.scheme() != "https" {
            bail!("The release URL must use HTTPS: {base_url}");
        }
        if parsed.host_str().is_none_or(str::is_empty) {
            bail!("The release URL has no host: {base_url}");
        }
        // Paths are appended to the base URL, which a query or fragment would swallow.
        if parsed.query().is_some() || parsed.fragment().is_some() {
            bail!("The release URL must not have a query or fragment: {base_url}");
        }
        if public_key.trim().is_empty() {
            bail!("The release signing public key is empty");
        }
        Ok(ReleaseSource {
            base_url: parsed,
            public_key: public_key.trim().to_owned(),
        })
    }

    /// Read the release source from [`RELEASE_URL_ENV`] and [`RELEASE_PUBLIC_KEY_ENV`].
    ///
    /// # Errors
    /// Returns an error if either is unset or invalid.
    pub fn from_env() -> Result<Self> {
        let base_url = env::var(RELEASE_URL_ENV).map_err(|_| {
            anyhow!(
                "No release server is configured. Set {RELEASE_URL_ENV} to the base URL of the GitCthulhu release API."
            )
        })?;
        let public_key = env::var(RELEASE_PUBLIC_KEY_ENV).map_err(|_| {
            anyhow!(
                "No release signing key is configured. Set {RELEASE_PUBLIC_KEY_ENV} to the base64 minisign public key of the GitCthulhu releases."
            )
        })?;
        Self::new(&base_url, &public_key)
    }

    /// The host that serves release information and downloads.
    pub fn host(&self) -> &str {
        self.base_url.host_str().unwrap_or_default()
    }

    /// The URL of the release information that `version_request` asks for.
    pub fn releases_url(&self, version_request: &VersionRequest) -> String {
        let base = self.base_url.as_str().trim_end_matches('/');
        match version_request {
            VersionRequest::Nightly => format!("{base}/nightly"),
            VersionRequest::Specific(version) => format!("{base}/version/{}", version.as_str()),
            VersionRequest::Release => base.to_owned(),
        }
    }
}

/// Configuration for the installer
pub struct InstallerConfig {
    pub version_request: VersionRequest,
    pub home_dir: PathBuf,
    pub platform: String,
    pub source: ReleaseSource,
}

impl InstallerConfig {
    /// Create a new installer config, reading version from command-line arguments or environment
    pub fn new() -> Result<Self> {
        // Validate argument count - only 0 or 1 positional arguments allowed
        let args: Vec<String> = env::args().collect();
        if args.len() > 2 {
            bail!(
                "Too many arguments. Usage: but-installer [version|nightly] or {VERSION_ENV}=<version> but-installer"
            );
        }

        // Get version from CLI argument (takes precedence) or the version environment variable
        let version_string = args.get(1).cloned().or_else(|| env::var(VERSION_ENV).ok());

        let version_request = VersionRequest::from_string(version_string)?;
        Self::new_with_version(version_request)
    }

    /// Create a new installer config with an explicit version request, reading the release source
    /// from the environment.
    pub(crate) fn new_with_version(version_request: VersionRequest) -> Result<Self> {
        let source = ReleaseSource::from_env()?;
        let home_dir =
            dirs::home_dir().ok_or_else(|| anyhow!("Failed to determine home directory"))?;

        // Detect platform
        let os = env::consts::OS;
        let arch = env::consts::ARCH;

        let platform = match (os, arch) {
            ("macos", "aarch64") => "darwin-aarch64",
            ("macos", "x86_64") => "darwin-x86_64",
            ("linux", "aarch64") => "linux-aarch64",
            ("linux", "x86_64") => "linux-x86_64",
            (os, arch) => bail!("unsupported OS or architecture: {os} {arch}"),
        };

        Ok(Self {
            version_request,
            home_dir,
            platform: platform.to_string(),
            source,
        })
    }

    /// The URL of the release information for the requested version.
    pub fn releases_url(&self) -> String {
        self.source.releases_url(&self.version_request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_request_from_string_valid() {
        // Valid versions
        assert_eq!(
            VersionRequest::from_string(None).unwrap(),
            VersionRequest::Release
        );
        assert_eq!(
            VersionRequest::from_string(Some("nightly".to_string())).unwrap(),
            VersionRequest::Nightly
        );
        assert_eq!(
            VersionRequest::from_string(Some("1.0.0".to_string())).unwrap(),
            VersionRequest::Specific(Version::new("1.0.0".to_string()).unwrap())
        );
        assert_eq!(
            VersionRequest::from_string(Some("1.2.3-alpha".to_string())).unwrap(),
            VersionRequest::Specific(Version::new("1.2.3-alpha".to_string()).unwrap())
        );
        assert_eq!(
            VersionRequest::from_string(Some("2.0.0+build123".to_string())).unwrap(),
            VersionRequest::Specific(Version::new("2.0.0+build123".to_string()).unwrap())
        );
        assert_eq!(
            VersionRequest::from_string(Some("0.0.1-rc.1".to_string())).unwrap(),
            VersionRequest::Specific(Version::new("0.0.1-rc.1".to_string()).unwrap())
        );
    }

    #[test]
    fn test_version_request_from_string_invalid() {
        // Invalid versions
        assert!(VersionRequest::from_string(Some("".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("--help".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("-v".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("1.0.0/../../etc".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("1.0.0?foo=bar".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("1.0.0;rm -rf /".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("../../../etc/passwd".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("...".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("---".to_string())).is_err());
        assert!(VersionRequest::from_string(Some("+++".to_string())).is_err());
    }

    #[test]
    fn test_version_new_valid() {
        // Valid versions
        assert!(Version::new("1.0.0".to_string()).is_ok());
        assert!(Version::new("1.2.3-alpha".to_string()).is_ok());
        assert!(Version::new("2.0.0+build123".to_string()).is_ok());
        assert!(Version::new("0.0.1-rc.1".to_string()).is_ok());
    }

    #[test]
    fn test_version_new_invalid() {
        // Invalid versions
        assert!(Version::new("".to_string()).is_err());
        assert!(Version::new("--help".to_string()).is_err());
        assert!(Version::new("-v".to_string()).is_err());
        assert!(Version::new("1.0.0/../../etc".to_string()).is_err());
        assert!(Version::new("1.0.0?foo=bar".to_string()).is_err());
        assert!(Version::new("1.0.0;rm -rf /".to_string()).is_err());
        assert!(Version::new("../../../etc/passwd".to_string()).is_err());
        assert!(Version::new("...".to_string()).is_err());
        assert!(Version::new("---".to_string()).is_err());
        assert!(Version::new("+++".to_string()).is_err());
    }

    #[test]
    fn test_version_as_str() {
        let version = Version::new("1.2.3".to_string()).unwrap();
        assert_eq!(version.as_str(), "1.2.3");
    }

    #[test]
    fn test_version_display() {
        let version = Version::new("1.2.3".to_string()).unwrap();
        assert_eq!(format!("{version}"), "1.2.3");
    }

    #[test]
    fn test_version_as_ref() {
        let version = Version::new("1.2.3".to_string()).unwrap();
        let s: &str = version.as_ref();
        assert_eq!(s, "1.2.3");
    }

    /// The release server of the tests; it is never contacted.
    const TEST_RELEASE_URL: &str = "https://releases.example.com/releases";

    fn source() -> ReleaseSource {
        ReleaseSource::new(TEST_RELEASE_URL, "test-key").unwrap()
    }

    #[test]
    fn test_releases_url_per_version_request() {
        let source = source();
        assert_eq!(
            source.releases_url(&VersionRequest::Release),
            "https://releases.example.com/releases"
        );
        assert_eq!(
            source.releases_url(&VersionRequest::Nightly),
            "https://releases.example.com/releases/nightly"
        );
        assert_eq!(
            source.releases_url(&VersionRequest::Specific(
                Version::new("0.18.7".to_string()).unwrap()
            )),
            "https://releases.example.com/releases/version/0.18.7"
        );
        assert_eq!(
            ReleaseSource::new("https://releases.example.com/releases/", "test-key")
                .unwrap()
                .releases_url(&VersionRequest::Nightly),
            "https://releases.example.com/releases/nightly",
            "a trailing slash doesn't double up"
        );
        assert_eq!(source.host(), "releases.example.com");
    }

    #[test]
    fn test_release_source_is_strict() {
        assert!(
            ReleaseSource::new("http://releases.example.com/releases", "test-key").is_err(),
            "release information is only fetched over HTTPS"
        );
        assert!(
            ReleaseSource::new("not a url", "test-key").is_err(),
            "the URL must parse"
        );
        assert!(
            ReleaseSource::new(
                "https://releases.example.com/releases?channel=x",
                "test-key"
            )
            .is_err(),
            "a query would end up in front of the appended `/nightly` or `/version/<v>`"
        );
        assert!(
            ReleaseSource::new("https://releases.example.com/releases#top", "test-key").is_err(),
            "a fragment would swallow the appended path"
        );
        assert!(
            ReleaseSource::new(TEST_RELEASE_URL, "  ").is_err(),
            "downloads can't be verified without a key"
        );
        assert_eq!(
            ReleaseSource::new(TEST_RELEASE_URL, " test-key\n")
                .unwrap()
                .public_key,
            "test-key",
            "surrounding whitespace is not part of the key"
        );
    }

    #[test]
    fn test_channel_display_name() {
        assert_eq!(Channel::Release.display_name(), "Release");
        assert_eq!(Channel::Nightly.display_name(), "Nightly");
    }
}
