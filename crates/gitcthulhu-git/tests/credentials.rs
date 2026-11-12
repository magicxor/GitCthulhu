//! Tests of the public credential types (spec §2.4): prompt classification, `Sensitive`, `NoPrompter`.

use gitcthulhu_git::{
    CancelToken, CredentialPrompter, CredentialRequest, CredentialResponse, NoPrompter, PromptKind,
    Sensitive, credentials::classify,
};

fn request(kind: PromptKind) -> CredentialRequest {
    CredentialRequest {
        raw_prompt: String::new(),
        kind,
        operation: "Fetch".into(),
        repo: None,
        attempt: 1,
        cancel: CancelToken::new(),
    }
}

#[test]
fn classify_table() {
    let cases: &[(&str, PromptKind)] = &[
        (
            "Username for 'https://github.com': ",
            PromptKind::HttpsUsername {
                url: "https://github.com".into(),
            },
        ),
        (
            "Password for 'https://alice@github.com': ",
            PromptKind::HttpsPassword {
                url: "https://alice@github.com".into(),
                username: Some("alice".into()),
            },
        ),
        (
            "Enter passphrase for key '/home/a/.ssh/id_ed25519': ",
            PromptKind::SshPassphrase {
                key_path: Some("/home/a/.ssh/id_ed25519".into()),
            },
        ),
        (
            "Enter passphrase for \"/home/a/.ssh/id_rsa\": ",
            PromptKind::SshPassphrase {
                key_path: Some("/home/a/.ssh/id_rsa".into()),
            },
        ),
        (
            "alice@host's password: ",
            PromptKind::SshPassword {
                user_host: "alice@host".into(),
            },
        ),
        (
            "Enter PIN for ED25519-SK key /k: ",
            PromptKind::SshPin {
                device: Some("ED25519-SK key /k".into()),
            },
        ),
        (
            "The authenticity of host 'h (1.2.3.4)' can't be established.\nRSA key fingerprint is SHA256:xyz.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ",
            PromptKind::HostKey {
                host: "h".into(),
                key_type: Some("RSA".into()),
                fingerprint: Some("SHA256:xyz".into()),
            },
        ),
        (
            "Confirm user presence for key ED25519-SK SHA256:abc",
            PromptKind::SshPin {
                device: Some("ED25519-SK SHA256:abc".into()),
            },
        ),
        (
            // `ssh-keygen -Y sign` (SSH commit signing) asks without naming the key.
            "Enter passphrase: ",
            PromptKind::SshPassphrase { key_path: None },
        ),
        (
            "Password for 'https://github.com': ",
            PromptKind::HttpsPassword {
                url: "https://github.com".into(),
                username: None,
            },
        ),
        ("What? ", PromptKind::Other),
    ];
    for (prompt, expected) in cases {
        assert_eq!(&classify(prompt), expected, "classification of {prompt:?}");
    }
}

#[test]
fn sensitive_is_masked_in_debug_output() {
    let secret = Sensitive::new("hunter2".to_owned());
    assert_eq!(format!("{secret:?}"), "***", "Debug masks the secret");
    let response = CredentialResponse::Answer(secret);
    assert!(
        matches!(response, CredentialResponse::Answer(ref s) if s.expose() == "hunter2"),
        "expose still returns the value"
    );
}

#[test]
fn no_prompter_always_cancels() {
    let response = NoPrompter.prompt(&request(PromptKind::HttpsUsername { url: "u".into() }));
    assert!(
        matches!(response, CredentialResponse::Cancel),
        "the null prompter refuses every prompt"
    );
}
