//! Resolution of a usable `bash` for fixture scripts, notably on Windows.
//!
//! On Windows, a plain `bash` frequently resolves to the WSL shim in `System32`, which cannot run
//! fixture scripts. Rust's `Command::new("bash")` even searches `System32` before `PATH`, so the
//! shim wins whenever WSL is installed. Git for Windows ships a real bash next to the `git`
//! executable, which is what we want instead - both for scripts we spawn and for nested `bash`
//! calls made by fixture scripts, which is why its directory is also prepended to `PATH`.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// Name of the environment variable that overrides the bash executable used for tests.
///
/// If set and non-empty, it must name an existing file, or [`bash_program()`] panics.
pub const BASH_OVERRIDE_ENV_VAR: &str = "GITCTHULHU_TEST_BASH";

/// Return the `bash` program to use for fixture scripts.
///
/// Resolution order: `GITCTHULHU_TEST_BASH` (if set and non-empty), then, on Windows, the bash of
/// Git for Windows, i.e. `<git --exec-path>/../../../bin/bash.exe` and then `…/usr/bin/bash.exe`,
/// and finally plain `bash` for a `PATH` search. Apart from the override, this is the very bash
/// `gix-testtools` runs fixture scripts with.
///
/// # Panics
///
/// If `GITCTHULHU_TEST_BASH` is set and non-empty but doesn't name an existing file, or if
/// `git --exec-path` can't be run on Windows.
pub fn bash_program() -> &'static Path {
    static BASH: OnceLock<PathBuf> = OnceLock::new();
    BASH.get_or_init(|| resolve_bash(std::env::var_os(BASH_OVERRIDE_ENV_VAR)))
        .as_path()
}

fn resolve_bash(override_value: Option<OsString>) -> PathBuf {
    if let Some(bash) = override_value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        assert!(
            bash.is_file(),
            "{BASH_OVERRIDE_ENV_VAR}='{}' must name an existing bash executable",
            bash.display()
        );
        return bash;
    }
    gix_testtools::bash_program().to_owned()
}

/// Return `PATH` with the directory of [`bash_program()`] prepended, if that program has a
/// directory component and it isn't already the first `PATH` entry. Otherwise return the current
/// `PATH` unchanged.
pub fn path_with_bash_dir() -> OsString {
    prepend_dir_to_path(
        bash_program().parent(),
        std::env::var_os("PATH").unwrap_or_default(),
    )
}

fn prepend_dir_to_path(dir: Option<&Path>, path: OsString) -> OsString {
    let Some(dir) = dir.filter(|dir| !dir.as_os_str().is_empty()) else {
        return path;
    };
    if std::env::split_paths(&path).next().as_deref() == Some(dir) {
        return path;
    }
    let paths = std::iter::once(dir.to_owned()).chain(std::env::split_paths(&path));
    std::env::join_paths(paths).unwrap_or(path)
}

/// Make nested `bash` calls of fixture scripts find the bash of [`bash_program()`] first, by
/// prepending its directory to this process' `PATH`. Does this only once, and only on Windows,
/// where a `PATH` search can otherwise find the WSL shim.
///
/// This is invoked by all fixture-creating functions of this crate. Call it as well before using
/// `gix_testtools::scripted_fixture_*` directly. Without a resolved bash directory, this is a no-op.
pub fn prepare_fixture_environment() {
    #[cfg(windows)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: modifying the environment is always safe on Windows, even with other threads
            // reading it concurrently, as documented on `std::env::set_var()`.
            unsafe { std::env::set_var("PATH", path_with_bash_dir()) };
        });
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn override_names_the_bash_to_use() {
        let this_file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("bash.rs");
        assert_eq!(
            resolve_bash(Some(this_file.clone().into())),
            this_file,
            "an existing file is used as is"
        );
        assert_eq!(
            resolve_bash(Some(OsString::new())),
            resolve_bash(None),
            "an empty override is the same as no override"
        );
    }

    #[test]
    #[should_panic(expected = "GITCTHULHU_TEST_BASH='does-not-exist' must name an existing bash")]
    fn override_naming_a_missing_file_panics() {
        resolve_bash(Some("does-not-exist".into()));
    }

    #[test]
    fn bash_dir_is_prepended_once() {
        let dir = Path::new("bash-dir");
        let path = std::env::join_paths(["a", "b"]).expect("valid paths");
        let expected = std::env::join_paths(["bash-dir", "a", "b"]).expect("valid paths");
        let prepended = prepend_dir_to_path(Some(dir), path.clone());
        assert_eq!(prepended, expected, "the bash dir comes first");
        assert_eq!(
            prepend_dir_to_path(Some(dir), prepended),
            expected,
            "an already leading bash dir isn't added again"
        );
        assert_eq!(
            prepend_dir_to_path(Some(Path::new("")), path.clone()),
            path,
            "a bash without directory leaves PATH alone"
        );
        assert_eq!(prepend_dir_to_path(None, path.clone()), path);
    }

    #[test]
    fn resolved_bash_runs_scripts_and_nested_bash() {
        let out = std::process::Command::new(bash_program())
            .env("PATH", path_with_bash_dir())
            .args(["-c", "echo outer; bash -c 'echo nested; uname -s'"])
            .output()
            .expect("bash can be spawned");
        assert!(
            out.status.success(),
            "bash must run a script with a nested bash call: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut lines = stdout.lines();
        assert_eq!(lines.next(), Some("outer"));
        assert_eq!(lines.next(), Some("nested"));
        let kernel = lines.next().unwrap_or_default();
        if cfg!(windows) {
            assert!(
                kernel.starts_with("MINGW") || kernel.starts_with("MSYS"),
                "nested bash must be Git for Windows' bash, not WSL, got '{kernel}'"
            );
        } else {
            assert!(!kernel.is_empty(), "nested bash prints the kernel name");
        }
    }

    #[test]
    #[cfg(windows)]
    fn windows_uses_git_for_windows_bash() {
        if std::env::var_os(BASH_OVERRIDE_ENV_VAR).is_some() {
            return;
        }
        let bash = bash_program();
        assert!(
            bash.is_absolute() && bash.is_file(),
            "Git for Windows bash must be found, got '{}'",
            bash.display()
        );
        assert!(
            !bash
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("system32"),
            "the WSL shim in System32 must not be used, got '{}'",
            bash.display()
        );
    }
}
