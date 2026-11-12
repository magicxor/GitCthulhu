use std::path::{Path, PathBuf};

use but_project_handle::ProjectHandle;

#[test]
fn round_trip() -> anyhow::Result<()> {
    let input = Path::new("/tmp/read me?/a+b/#test");
    let handle = ProjectHandle::from_path(input)?;
    let decoded = PathBuf::try_from(&handle)?;
    assert_eq!(decoded, gix::path::realpath(input)?);
    assert!(
        handle
            .to_string()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    );
    Ok(())
}

#[test]
fn from_path_and_decode_round_trip() -> anyhow::Result<()> {
    let input = Path::new("/tmp/read me?/a+b/#test");
    let handle = ProjectHandle::from_path(input)?;
    let decoded = handle.to_path()?;
    assert_eq!(decoded, gix::path::realpath(input)?);
    Ok(())
}

/// An absolute path on the platform the tests run on (`/tmp` alone is relative on Windows).
fn absolute(path: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{path}"))
    } else {
        PathBuf::from(path)
    }
}

#[test]
fn from_str_round_trip() -> anyhow::Result<()> {
    let encoded = if cfg!(windows) {
        "QzovdG1wL3JlYWQgbWU" // C:/tmp/read me
    } else {
        "L3RtcC9yZWFkIG1l" // /tmp/read me
    };
    let handle: ProjectHandle = encoded.parse()?;
    assert_eq!(
        handle.to_path()?,
        absolute("/tmp/read me"),
        "the handle decodes to the path it encodes"
    );
    Ok(())
}

#[test]
fn from_str_rejects_relative_payloads() {
    assert!("cmVsYXRpdmUvcGF0aA".parse::<ProjectHandle>().is_err());
}

#[test]
fn convenience_path_methods_round_trip() -> anyhow::Result<()> {
    let input = Path::new("/tmp/read me?/a+b/#test");
    let handle = ProjectHandle::from_path(input)?;
    let canonical = gix::path::realpath(input)?;

    assert_eq!(handle.to_path()?, canonical);
    assert_eq!(handle.into_path()?, canonical);
    Ok(())
}

#[cfg(unix)]
#[test]
fn minimal_illformed_utf8_round_trip() -> anyhow::Result<()> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let input: PathBuf = OsString::from_vec(b"/tmp/\xFF".into()).into();
    let handle = ProjectHandle::from_path(&input)?;

    let canonical = gix::path::realpath(&input)?;
    assert_eq!(handle.to_path()?, canonical);
    Ok(())
}

#[test]
fn display_and_debug_print() -> anyhow::Result<()> {
    let input = Path::new("/tmp/read me?/a+b/#test");
    let handle = ProjectHandle::from_path(input)?;
    let display = handle.to_string();

    assert_eq!(format!("{handle}"), display);
    assert_eq!(
        format!("{handle:?}"),
        format!("ProjectHandle(\"{display}\")")
    );
    Ok(())
}

#[test]
fn deserializes_from_a_json_string() -> anyhow::Result<()> {
    let encoded = if cfg!(windows) {
        "QzovdG1w" // C:/tmp
    } else {
        "L3RtcA" // /tmp
    };
    let handle: ProjectHandle = serde_json::from_str(&format!("\"{encoded}\""))?;
    assert_eq!(
        handle.to_string(),
        encoded,
        "the handle is its encoded string"
    );
    assert_eq!(
        handle.to_path()?,
        absolute("/tmp"),
        "and decodes to its path"
    );
    assert!(
        serde_json::from_str::<ProjectHandle>(r#""00000000-0000-0000-0000-000000000000""#).is_err(),
        "a UUID is not a project handle"
    );
    Ok(())
}
