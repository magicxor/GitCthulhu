//! Platform-agnostic installation logic

use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::ui;

/// The name of the application binary the installer puts into `~/.local/bin`.
pub(crate) const APP_BINARY_NAME: &str = "gitcthulhu";

/// Returns the path to the application binary for the given home directory.
///
/// Only the Linux install flow puts a bare binary there; macOS installs an app bundle.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn app_binary_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".local/bin").join(APP_BINARY_NAME)
}

/// Validate that the installed binary at `path` is an executable file.
///
/// The binary is a GUI program without a `--version` mode, so it is not run: running it would
/// open the application window.
pub(crate) fn validate_installed_binary(path: &Path) -> bool {
    fs::metadata(path)
        .map(|md| md.is_file() && md.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Verify the minisign signature of an installable against `public_key_b64`, the release
/// signing key the installer was configured with.
pub(crate) fn verify_signature(
    installable: &Path,
    signature_b64: &str,
    public_key_b64: &str,
    temp_dir: &Path,
) -> Result<()> {
    crate::ui::info("Verifying download signature...");

    // Validate signature is not empty - this is a security requirement
    if signature_b64.trim().is_empty() {
        bail!("Signature is empty - refusing to verify without a valid signature");
    }

    let public_key = minisign_verify::PublicKey::from_base64(public_key_b64.trim())
        .context("Failed to parse the release signing public key")?;

    // Decode signature from base64 and write to file
    // The signature format from the API is base64-encoded minisign signature file content
    use base64::{Engine, engine::general_purpose::STANDARD};
    let signature_bytes = STANDARD
        .decode(signature_b64)
        .context("Failed to decode signature from base64")?;

    // Write signature to temp file for minisign to parse
    let signature_file = temp_dir.join("signature.minisig");
    fs::write(&signature_file, &signature_bytes)?;

    // Read it back as a string (minisign signatures are text files)
    let signature_str =
        fs::read_to_string(&signature_file).context("Failed to read signature file as string")?;

    // Parse the signature
    let signature =
        minisign_verify::Signature::decode(&signature_str).context("Failed to parse signature")?;

    // Use streaming verification to avoid loading entire file into memory
    let mut file =
        File::open(installable).context("Failed to open installable for verification")?;
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|e| anyhow!("Failed to initialize signature verifier: {e}"))?;

    // Read and verify file in 64KB chunks
    let mut buffer = [0u8; 65536];
    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        verifier.update(&buffer[..bytes_read]);
    }

    // Finalize verification
    verifier.finalize().map_err(|e| {
        anyhow!("Signature verification failed - the download may have been tampered with: {e}")
    })?;

    ui::info("Signature verification passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// A syntactically valid minisign public key that signed nothing the tests download.
    const TEST_PUBLIC_KEY: &str = "RWTrOEI+im1XYA9RBwyxnzFN/evFzJhU1lbQ70LVayWH3WRo7xQnRLD2";

    #[test]
    fn test_validate_installed_binary_needs_an_executable_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let binary = temp_dir.path().join(APP_BINARY_NAME);
        assert!(
            !validate_installed_binary(&binary),
            "a missing binary is not installed"
        );

        fs::write(&binary, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            !validate_installed_binary(&binary),
            "a file without the executable bit can't be run"
        );

        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            validate_installed_binary(&binary),
            "an executable file is installed"
        );
        assert!(
            !validate_installed_binary(temp_dir.path()),
            "a directory is not a binary"
        );
    }

    #[test]
    fn test_app_binary_path() {
        assert_eq!(
            app_binary_path(Path::new("/home/user")),
            Path::new("/home/user/.local/bin/gitcthulhu")
        );
    }

    #[test]
    fn test_verify_signature_rejects_an_invalid_public_key() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test.tar.gz");
        File::create(&test_file).unwrap();

        let err = verify_signature(&test_file, "c2ln", "not a key", temp_dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("public key"),
            "a broken key is reported as such: {err}"
        );
    }

    #[test]
    fn test_verify_signature_empty() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test.tar.gz");

        // Create a dummy file
        let mut file = File::create(&test_file).unwrap();
        file.write_all(&[0x1f, 0x8b, 0x08, 0x00]).unwrap();

        // Empty signature should fail with clear error
        let result = verify_signature(&test_file, "", TEST_PUBLIC_KEY, temp_dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));

        // Whitespace-only signature should also fail
        let result = verify_signature(&test_file, "   ", TEST_PUBLIC_KEY, temp_dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }
}
