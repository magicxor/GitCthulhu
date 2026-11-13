use but_update::{CheckError, Manifest, current_platform_key, platform_key};

use crate::support::version;

const MANIFEST: &str = r###"{
    "version": "0.5.1",
    "pub_date": "2026-12-01T00:00:00Z",
    "notes": "## What's new\n- Feature A",
    "unknown_field": [1, 2, 3],
    "platforms": {
        "windows-x86_64": {"url": "https://example.com/win.msi", "signature": "winsig", "sha256": "ab"},
        "linux-x86_64": {"url": null, "signature": "linuxsig"},
        "darwin-aarch64": {"url": "https://example.com/mac.tar.gz", "signature": "macsig"}
    }
}"###;

#[test]
fn manifest_parse() {
    let manifest = Manifest::parse(MANIFEST.as_bytes()).expect("a valid manifest parses");
    assert_eq!(manifest.version, version("0.5.1"), "the version is parsed");
    assert_eq!(
        manifest.notes.as_deref(),
        Some("## What's new\n- Feature A"),
        "notes are kept verbatim"
    );
    assert_eq!(
        manifest.platforms.len(),
        3,
        "all platforms are read, unknown fields ignored"
    );
    assert_eq!(
        manifest.platforms["linux-x86_64"].url, None,
        "an explicit null url means nothing is published"
    );
    assert_eq!(
        manifest.platforms["darwin-aarch64"].signature.as_deref(),
        Some("macsig")
    );

    let prefixed = Manifest::parse(br#"{"version": "v1.2.3"}"#).expect("a leading v is accepted");
    assert_eq!(
        prefixed.version,
        version("1.2.3"),
        "the v prefix is dropped"
    );
    assert!(
        prefixed.platforms.is_empty(),
        "platforms may be missing entirely"
    );
}

#[test]
fn manifest_parse_rejects_broken_input() {
    for (label, input) in [
        ("not json", &b"<html>404</html>"[..]),
        ("missing version", br#"{"platforms": {}}"#),
        ("version not a string", br#"{"version": 5}"#),
        ("version not semver", br#"{"version": "latest"}"#),
        ("empty", b""),
    ] {
        let err = Manifest::parse(input).expect_err(label);
        assert!(
            matches!(err, CheckError::Manifest(_)),
            "{label}: classified as a manifest error, got {err:?}"
        );
    }
}

#[test]
fn newer_semver_only() {
    let manifest = Manifest::parse(MANIFEST.as_bytes()).expect("manifest parses");
    let key = "windows-x86_64";
    for (current, expect_update) in [
        ("0.5.0", true),
        ("0.4.9", true),
        ("0.5.1", false),
        ("0.5.2", false),
        ("1.0.0", false),
        ("0.5.1-rc.1", true),
    ] {
        let status = manifest
            .status_for(&version(current), key)
            .expect("status computes");
        assert_eq!(
            !status.up_to_date, expect_update,
            "manifest 0.5.1 vs running {current}"
        );
    }

    let ten = Manifest::parse(br#"{"version": "0.10.0", "release_url": "https://example.com/r"}"#)
        .expect("manifest parses");
    assert!(
        !ten.status_for(&version("0.9.0"), key)
            .expect("status computes")
            .up_to_date,
        "0.10.0 is newer than 0.9.0 (numeric, not textual, comparison)"
    );
    let nine = Manifest::parse(br#"{"version": "0.9.0", "release_url": "https://example.com/r"}"#)
        .expect("manifest parses");
    assert!(
        nine.status_for(&version("0.10.0"), key)
            .expect("status computes")
            .up_to_date,
        "0.9.0 is not newer than 0.10.0"
    );
    for (published, current) in [
        ("1.0.0+build.2", "1.0.0"),
        ("1.0.0+build.2", "1.0.0+build.1"),
    ] {
        let rebuild = Manifest::parse(
            format!(r#"{{"version": "{published}", "release_url": "https://example.com/r"}}"#)
                .as_bytes(),
        )
        .expect("manifest parses");
        assert!(
            rebuild
                .status_for(&version(current), key)
                .expect("status computes")
                .up_to_date,
            "build metadata has no precedence: {published} is not newer than {current}"
        );
    }

    let status = manifest
        .status_for(&version("0.5.0"), key)
        .expect("status computes");
    assert_eq!(status.latest_version, "0.5.1");
    assert_eq!(status.url.as_deref(), Some("https://example.com/win.msi"));
    assert_eq!(status.signature.as_deref(), Some("winsig"));
    assert!(status.release_notes.is_some(), "notes ride along");
    let current = manifest
        .status_for(&version("0.5.1"), key)
        .expect("status computes");
    assert_eq!(
        (current.url, current.signature, current.release_notes),
        (None, None, None),
        "an up-to-date status carries no download"
    );
}

#[test]
fn platform_key_macos_maps_to_darwin() {
    assert_eq!(platform_key("macos", "aarch64"), "darwin-aarch64");
    assert_eq!(platform_key("macos", "x86_64"), "darwin-x86_64");
    assert_eq!(platform_key("windows", "x86_64"), "windows-x86_64");
    assert_eq!(platform_key("linux", "aarch64"), "linux-aarch64");
    assert!(
        !current_platform_key().contains("macos"),
        "the running platform never produces the macos spelling"
    );
}

#[test]
fn missing_platform_falls_back_to_release_page_or_nothing() {
    let current = version("0.1.0");
    let without_page = Manifest::parse(MANIFEST.as_bytes()).expect("manifest parses");
    assert!(
        without_page
            .status_for(&current, "linux-x86_64")
            .expect("status computes")
            .up_to_date,
        "a null url and no release page offer nothing to do"
    );
    assert!(
        without_page
            .status_for(&current, "freebsd-x86_64")
            .expect("status computes")
            .up_to_date,
        "an unlisted platform offers nothing to do"
    );

    let with_page = Manifest::parse(
        br#"{"version": "0.5.1", "release_url": "https://example.com/release", "platforms": {
            "linux-x86_64": {"url": null, "signature": "sig-of-a-build-we-do-not-link"}}}"#,
    )
    .expect("manifest parses");
    let status = with_page
        .status_for(&current, "linux-x86_64")
        .expect("status computes");
    assert!(!status.up_to_date, "the release page is something to open");
    assert_eq!(status.url.as_deref(), Some("https://example.com/release"));
    assert_eq!(
        status.signature, None,
        "a signature is only reported together with its own download"
    );
}

#[test]
fn download_urls_must_be_https() {
    let manifest = Manifest::parse(
        br#"{"version": "9.0.0", "platforms": {"linux-x86_64": {"url": "http://example.com/x.tar.gz"},
            "windows-x86_64": {"url": "file:///c:/x.msi"}}}"#,
    )
    .expect("manifest parses");
    for key in ["linux-x86_64", "windows-x86_64"] {
        let err = manifest
            .status_for(&version("0.1.0"), key)
            .expect_err("a plain http or file url is refused");
        assert!(
            matches!(err, CheckError::Manifest(_)),
            "{key}: classified as a manifest error, got {err:?}"
        );
    }
    assert!(
        manifest
            .status_for(&version("9.0.0"), "linux-x86_64")
            .expect("an old manifest is not inspected for urls")
            .up_to_date,
        "an up-to-date app never looks at the download url"
    );
}
