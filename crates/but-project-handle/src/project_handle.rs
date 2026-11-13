use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A self-describing handle to the path of the project on disk, typically the `.git` directory.
///
/// Use it in places where no repository context is available.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ProjectHandle(String);

/// Lifecycle
impl ProjectHandle {
    /// Create a project handle from `path`.
    /// Note that this *should* be the git-dir as it's the most stable, and also
    /// allows to quickly check for duplication if this convention is followed.
    /// This is also needed to support bare repositories properly.
    pub fn from_path(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = gix::path::realpath(path)?;
        path_to_string(&path).map(Self)
    }

    /// Consume this handle and decode it into its absolute filesystem path.
    pub fn into_path(self) -> anyhow::Result<PathBuf> {
        self.to_path()
    }
}

/// Path conversion
impl ProjectHandle {
    /// Decode this handle into its absolute filesystem path.
    pub fn to_path(&self) -> anyhow::Result<PathBuf> {
        encoded_str_to_path(self.as_raw_str())
    }
}

impl ProjectHandle {
    fn as_raw_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for ProjectHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_raw_str())
    }
}

impl std::fmt::Debug for ProjectHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ProjectHandle").field(&self.0).finish()
    }
}

impl std::str::FromStr for ProjectHandle {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let path = encoded_str_to_path(value)?;
        path_to_string(&path).map(Self)
    }
}

impl TryFrom<&ProjectHandle> for PathBuf {
    type Error = anyhow::Error;

    fn try_from(value: &ProjectHandle) -> Result<Self, Self::Error> {
        value.to_path()
    }
}

impl TryFrom<ProjectHandle> for PathBuf {
    type Error = anyhow::Error;

    fn try_from(value: ProjectHandle) -> Result<Self, Self::Error> {
        value.into_path()
    }
}

fn encoded_str_to_path(encoded: &str) -> anyhow::Result<PathBuf> {
    let bytes = decode(encoded)?;
    let path = gix::path::from_byte_slice(&bytes)
        .map_err(anyhow::Error::from)
        .with_context(|| {
            format!("Encoded ProjectHandle payload is not a valid filesystem path: '{encoded}'")
        })?;
    if !path.is_absolute() {
        bail!(
            "ProjectHandle payload must decode to an absolute filesystem path, got '{}'",
            path.display()
        );
    }
    Ok(path.to_owned())
}

fn path_to_string(path: &Path) -> Result<String, anyhow::Error> {
    let bytes = gix::path::os_str_into_bstr(path.as_os_str())?;
    Ok(encode(bytes))
}

fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode(encoded: &str) -> anyhow::Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(encoded)
        .with_context(|| format!("ProjectHandle payload is not valid base64url: '{encoded}'"))
}

impl<'de> serde::Deserialize<'de> for ProjectHandle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::path_to_string;
    use std::path::Path;

    #[test]
    fn canonical_paths() -> anyhow::Result<()> {
        struct Case {
            os: &'static str,
            canonical_path: &'static str,
            expected: &'static str,
        }

        let cases = [
            Case {
                os: "linux",
                canonical_path: "/home/alice/src/gitbutler/repo/.git",
                expected: "L2hvbWUvYWxpY2Uvc3JjL2dpdGJ1dGxlci9yZXBvLy5naXQ",
            },
            Case {
                os: "macos",
                canonical_path: "/Users/alice/Library/Application Support/GitButler/repo/.git",
                expected: "L1VzZXJzL2FsaWNlL0xpYnJhcnkvQXBwbGljYXRpb24gU3VwcG9ydC9HaXRCdXRsZXIvcmVwby8uZ2l0",
            },
            Case {
                os: "windows",
                canonical_path: r"C:\Users\alice\AppData\Local\GitButler\repo\.git",
                expected: "QzpcVXNlcnNcYWxpY2VcQXBwRGF0YVxMb2NhbFxHaXRCdXRsZXJccmVwb1wuZ2l0",
            },
            Case {
                os: "windows-unc",
                canonical_path: r"\\server\share\GitButler\repo\.git",
                expected: "XFxzZXJ2ZXJcc2hhcmVcR2l0QnV0bGVyXHJlcG9cLmdpdA",
            },
        ];

        for case in cases {
            let path = Path::new(case.canonical_path);
            let encoded = path_to_string(path)?;
            assert_eq!(encoded, case.expected, "{}: readable mismatch", case.os);
        }

        Ok(())
    }

    mod encoded_str_to_path {
        use crate::project_handle::encoded_str_to_path;

        use crate::project_handle::{decode, encode};

        #[cfg(unix)]
        #[test]
        fn illformed_utf8_payload_round_trip() -> anyhow::Result<()> {
            use crate::project_handle::encode;
            use std::os::unix::ffi::OsStrExt as _;

            let bytes = b"/tmp/\xF1\xF2\xF3\xC0\xC1\xC2";
            #[expect(invalid_from_utf8)]
            let res = std::str::from_utf8(bytes);
            assert!(res.is_err(), "this is illformed UTF8");
            let encoded = encode(bytes);

            let decoded = encoded_str_to_path(&encoded)?;
            assert_eq!(decoded.as_os_str().as_bytes(), bytes);
            Ok(())
        }

        #[test]
        fn decoding_relative_paths_is_rejected() {
            let relative_inputs = ["a", ".", "..", "a/b", "./tmp", "../tmp", "tmp/../repo"];

            for relative in relative_inputs {
                let encoded = encode(relative.as_bytes());
                assert!(
                    encoded_str_to_path(&encoded).is_err(),
                    "expected relative payload to be rejected: '{relative}'"
                );
            }
        }

        /// The prefix that makes a path starting with `/` absolute on this platform.
        const ABSOLUTE_PREFIX: &str = if cfg!(windows) { "C:" } else { "" };

        #[test]
        fn requires_urlsafe_base64_payloads() {
            assert!(
                encoded_str_to_path(&encode(format!("{ABSOLUTE_PREFIX}/tmp").as_bytes())).is_ok()
            );
            assert!(encoded_str_to_path("%2Ftmp").is_err());
            assert!(encoded_str_to_path("a/b").is_err());
        }

        #[test]
        fn raw_codec_handles_full_byte_range_and_stays_url_safe() -> anyhow::Result<()> {
            let bytes: Vec<u8> = (0_u8..=255).collect();
            let encoded = encode(&bytes);
            assert!(
                encoded
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            );
            assert_eq!(decode(&encoded)?, bytes);
            Ok(())
        }

        #[test]
        fn accepts_urlsafe_text_bytes_for_absolute_paths() -> anyhow::Result<()> {
            let mut absolute_bytes = Vec::with_capacity(64);
            absolute_bytes.extend(ABSOLUTE_PREFIX.as_bytes());
            absolute_bytes
                .extend(b"/0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-._~");

            let encoded = encode(&absolute_bytes);
            assert_eq!(decode(&encoded)?, absolute_bytes);
            assert!(encoded_str_to_path(&encoded).is_ok());
            Ok(())
        }

        #[test]
        fn strict_malformed_input_handling() {
            assert!(encoded_str_to_path("%2Ftmp").is_err());
            assert!(encoded_str_to_path("not+base64").is_err());
            assert!(encoded_str_to_path("a/b").is_err());
            assert!(encoded_str_to_path("a=").is_err());
        }
    }
}
