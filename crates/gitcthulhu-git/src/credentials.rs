//! Credential prompts: the [`CredentialPrompter`] trait the GUI implements, the request/response types and the
//! classification of Git/OpenSSH prompt texts.
//!
//! The types are final; [`classify`] is a stub (implemented by WP-B0c).

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
/// Stub (WP-B0c): every prompt is `Other`.
pub fn classify(raw_prompt: &str) -> PromptKind {
    let _ = raw_prompt;
    PromptKind::Other
}
