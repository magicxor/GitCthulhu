//! Credential prompts: the [`CredentialPrompter`] trait the GUI implements, the request/response types and the
//! classification of Git/OpenSSH prompt texts.
//!
//! The types are final.

use std::path::PathBuf;

use crate::CancelToken;

/// What a prompt asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    /// `Username for 'https://github.com': `.
    HttpsUsername {
        /// The URL.
        url: String,
    },
    /// `Password for 'https://alice@github.com': `.
    HttpsPassword {
        /// The URL.
        url: String,
        /// The user name embedded in the URL.
        username: Option<String>,
    },
    /// `Enter passphrase for key '...': `.
    SshPassphrase {
        /// The key file.
        key_path: Option<String>,
    },
    /// `alice@host's password: `.
    SshPassword {
        /// `user@host`.
        user_host: String,
    },
    /// FIDO / PKCS#11 PIN or presence confirmation.
    SshPin {
        /// The device.
        device: Option<String>,
    },
    /// Unknown host key; answer "yes"/"no".
    HostKey {
        /// The host.
        host: String,
        /// The key type.
        key_type: Option<String>,
        /// The fingerprint.
        fingerprint: Option<String>,
    },
    /// Anything else.
    Other,
}

/// A prompt Git (or ssh) raised during a command.
#[derive(Debug, Clone)]
pub struct CredentialRequest {
    /// What it asks for.
    pub kind: PromptKind,
    /// Full text (multi-line for HostKey).
    pub raw_prompt: String,
    /// `OpContext::label`.
    pub operation: String,
    /// The repository, if any.
    pub repo: Option<PathBuf>,
    /// 1-based count of prompts of the same kind in this git invocation (>1 => "wrong password?").
    pub attempt: u32,
    /// The GUI closes its dialog when this fires.
    pub cancel: CancelToken,
}

/// The answer to a prompt.
pub enum CredentialResponse {
    /// The user's answer.
    Answer(Sensitive),
    /// The user declined.
    Cancel,
}

/// A secret string: zeroised on drop, `Debug` prints "***".
pub struct Sensitive(String);

impl Sensitive {
    /// Wrap a secret.
    pub fn new(s: String) -> Self {
        Sensitive(s)
    }

    /// The secret. Handle with care.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Sensitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

impl Drop for Sensitive {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        // Keep the zeroing from being optimised away.
        std::hint::black_box(&bytes);
    }
}

/// Answers prompts. Implemented by the GUI.
pub trait CredentialPrompter: Send + Sync {
    /// Called on a blocking runtime thread; may block (e.g. waiting for a GUI modal). Never called on the UI thread.
    fn prompt(&self, request: &CredentialRequest) -> CredentialResponse;
}

/// A prompter that always cancels.
pub struct NoPrompter;

impl CredentialPrompter for NoPrompter {
    fn prompt(&self, _: &CredentialRequest) -> CredentialResponse {
        CredentialResponse::Cancel
    }
}

/// Pure; maps OpenSSH / Git prompt texts (English) to [`PromptKind`].
///
/// | Prompt | Kind |
/// |---|---|
/// | `Username for 'https://github.com': ` | `HttpsUsername` |
/// | `Password for 'https://alice@github.com': ` | `HttpsPassword` (with the user name of the URL) |
/// | `Enter passphrase for key '/home/a/.ssh/id_ed25519': ` (also `"..."`) | `SshPassphrase` |
/// | `alice@host's password: ` | `SshPassword` |
/// | `Enter PIN for ...` / `Confirm user presence for key ...` | `SshPin` |
/// | `... Are you sure you want to continue connecting ...` | `HostKey` |
/// | anything else | `Other` |
pub fn classify(raw_prompt: &str) -> PromptKind {
    let prompt = raw_prompt.trim();

    if prompt.contains("Are you sure you want to continue connecting") {
        return classify_host_key(prompt);
    }
    if let Some(url) = prompt.strip_prefix("Username for ").and_then(quoted) {
        return PromptKind::HttpsUsername { url };
    }
    if let Some(url) = prompt.strip_prefix("Password for ").and_then(quoted) {
        let username = url_username(&url);
        return PromptKind::HttpsPassword { url, username };
    }
    if let Some(rest) = prompt
        .strip_prefix("Enter PIN for ")
        .or_else(|| prompt.strip_prefix("Confirm user presence for key "))
    {
        return PromptKind::SshPin {
            device: non_empty(rest.trim_end_matches(':').trim()),
        };
    }
    if let Some(rest) = prompt.strip_prefix("Enter passphrase") {
        // `Enter passphrase for key '<path>':`, `Enter passphrase for "<path>":`, `Enter passphrase for <path>:`,
        // or just `Enter passphrase:`.
        let key_path = rest.strip_prefix(" for").and_then(|rest| {
            let rest = rest.trim();
            let rest = rest.strip_prefix("key ").unwrap_or(rest).trim();
            quoted(rest).or_else(|| non_empty(rest.trim_end_matches(':').trim()))
        });
        return PromptKind::SshPassphrase { key_path };
    }
    if let Some(user_host) = prompt.strip_suffix("'s password:")
        && !user_host.is_empty()
        && !user_host.contains(char::is_whitespace)
    {
        return PromptKind::SshPassword {
            user_host: user_host.to_owned(),
        };
    }
    PromptKind::Other
}

/// The text between the leading quote character (`'` or `"`) and the last quote of the same kind.
fn quoted(text: &str) -> Option<String> {
    let text = text.trim();
    let quote = text.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let inner = &text[1..];
    non_empty(&inner[..inner.rfind(quote)?])
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

/// The user name in `scheme://user[:password]@host/...`.
fn url_username(url: &str) -> Option<String> {
    let authority = url.split_once("://")?.1.split('/').next()?;
    let (userinfo, _host) = authority.rsplit_once('@')?;
    non_empty(userinfo.split(':').next()?)
}

/// `The authenticity of host 'example.org (192.0.2.1)' can't be established.` and
/// `ED25519 key fingerprint is SHA256:xyz.`
fn classify_host_key(prompt: &str) -> PromptKind {
    const HOST_INTRO: &str = "The authenticity of host '";
    let host = prompt
        .find(HOST_INTRO)
        .map(|at| &prompt[at + HOST_INTRO.len()..])
        .and_then(|rest| rest.find('\'').map(|end| &rest[..end]))
        // Drop the ` (<ip>)` suffix.
        .map(|name| name.split(" (").next().unwrap_or(name))
        .unwrap_or_default()
        .to_owned();

    let mut key_type = None;
    let mut fingerprint = None;
    for line in prompt.lines() {
        if let Some((before, after)) = line.split_once(" key fingerprint is ") {
            key_type = non_empty(before.trim());
            fingerprint = non_empty(
                after
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches('.'),
            );
        }
    }
    PromptKind::HostKey {
        host,
        key_type,
        fingerprint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_https_username() {
        assert_eq!(
            classify("Username for 'https://github.com': "),
            PromptKind::HttpsUsername {
                url: "https://github.com".into()
            }
        );
    }

    #[test]
    fn classify_https_password_with_and_without_user() {
        assert_eq!(
            classify("Password for 'https://alice@github.com': "),
            PromptKind::HttpsPassword {
                url: "https://alice@github.com".into(),
                username: Some("alice".into())
            }
        );
        assert_eq!(
            classify("Password for 'https://github.com': "),
            PromptKind::HttpsPassword {
                url: "https://github.com".into(),
                username: None
            }
        );
    }

    #[test]
    fn classify_ssh_passphrase_variants() {
        let key = |p: &str| PromptKind::SshPassphrase {
            key_path: Some(p.into()),
        };
        assert_eq!(
            classify("Enter passphrase for key '/home/a/.ssh/id_ed25519': "),
            key("/home/a/.ssh/id_ed25519")
        );
        assert_eq!(
            classify(r#"Enter passphrase for "C:\Users\a\.ssh\id_rsa": "#),
            key(r"C:\Users\a\.ssh\id_rsa")
        );
        assert_eq!(
            classify("Enter passphrase for /home/a/.ssh/id_rsa: "),
            key("/home/a/.ssh/id_rsa")
        );
        assert_eq!(
            classify("Enter passphrase: "),
            PromptKind::SshPassphrase { key_path: None }
        );
    }

    #[test]
    fn classify_ssh_password() {
        assert_eq!(
            classify("alice@example.org's password: "),
            PromptKind::SshPassword {
                user_host: "alice@example.org".into()
            }
        );
    }

    #[test]
    fn classify_ssh_pin_and_presence() {
        assert_eq!(
            classify("Enter PIN for ED25519-SK key /home/a/.ssh/id_ed25519_sk: "),
            PromptKind::SshPin {
                device: Some("ED25519-SK key /home/a/.ssh/id_ed25519_sk".into())
            }
        );
        assert_eq!(
            classify("Confirm user presence for key ED25519-SK SHA256:abc"),
            PromptKind::SshPin {
                device: Some("ED25519-SK SHA256:abc".into())
            }
        );
    }

    #[test]
    fn classify_host_key_parses_host_type_and_fingerprint() {
        let prompt = "The authenticity of host 'example.org (192.0.2.1)' can't be established.\nED25519 key fingerprint is SHA256:abc123.\nThis key is not known by any other names.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert_eq!(
            classify(prompt),
            PromptKind::HostKey {
                host: "example.org".into(),
                key_type: Some("ED25519".into()),
                fingerprint: Some("SHA256:abc123".into())
            }
        );
    }

    #[test]
    fn classify_unknown_text_is_other() {
        assert_eq!(classify("Something else: "), PromptKind::Other);
        assert_eq!(classify(""), PromptKind::Other);
    }

    #[test]
    fn sensitive_debug_does_not_leak() {
        let secret = Sensitive::new("hunter2".into());
        assert_eq!(format!("{secret:?}"), "***", "Debug is masked");
        assert_eq!(secret.expose(), "hunter2", "expose gives the value");
    }
}
