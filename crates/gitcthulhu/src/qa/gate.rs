//! The sandbox gate: with feature `qa` the application refuses to run unless the process environment points every
//! data location at a QA sandbox created by `tools/qa/new-sandbox.ps1`.
//!
//! The gate checks the environment the app is really started with, before any setting or repository is read:
//!
//! * [`SANDBOX_ENV`] names the sandbox directory, which holds the marker file [`MARKER_FILE`] written by the tool
//!   and is neither a file-system root nor the user's real profile directory (or above it);
//! * `GITCTHULHU_HOME` (every app directory), the user profile variables (`USERPROFILE`, `APPDATA`, `LOCALAPPDATA`
//!   on Windows, `HOME` and the `XDG_*` variables elsewhere) and `GIT_CONFIG_GLOBAL` lie inside the sandbox, so the
//!   user's real settings, Git configuration and credential helpers are never read;
//! * `GIT_CONFIG_NOSYSTEM=1` and `GIT_ALLOW_PROTOCOL=file` keep the system configuration out and forbid every
//!   network protocol (the fixture remotes are local bare repositories);
//! * every repository a scenario opens must lie inside the sandbox ([`Sandbox::check_repository`]).
//!
//! The checks are pure functions over an environment lookup, so they are unit-tested without touching the process.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The environment variable that names the sandbox directory.
pub const SANDBOX_ENV: &str = "GITCTHULHU_QA_SANDBOX";
/// The marker file `new-sandbox.ps1` writes into the sandbox root.
pub const MARKER_FILE: &str = ".qa-sandbox";
/// The first line of the marker file.
pub const MARKER_TEXT: &str = "gitcthulhu-qa-sandbox v1";
/// The process exit code of a refusal.
pub const EXIT_REFUSED: i32 = 3;

/// The variables that must lie inside the sandbox when the platform uses them.
#[cfg(windows)]
const PROFILE_VARS: &[&str] = &["USERPROFILE", "APPDATA", "LOCALAPPDATA"];
/// The variables that must lie inside the sandbox when the platform uses them.
#[cfg(not(windows))]
const PROFILE_VARS: &[&str] = &["HOME"];

/// Variables that must lie inside the sandbox when they are set at all.
const OPTIONAL_VARS: &[&str] = &[
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
];

/// A validated QA sandbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// A sandbox over `root` without running the gate: for unit tests of verbs only.
    #[cfg(test)]
    pub(crate) fn for_tests(root: PathBuf) -> Self {
        Self { root }
    }

    /// The sandbox directory (as named by [`SANDBOX_ENV`]).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory of the fixture repositories (`<sandbox>/repos`).
    pub fn repos_dir(&self) -> PathBuf {
        self.root.join("repos")
    }

    /// `Ok` when `path` lies inside the sandbox (a repository must never be opened elsewhere).
    pub fn check_repository(&self, path: &Path) -> Result<(), String> {
        if is_inside(&self.root, path) {
            Ok(())
        } else {
            Err(format!(
                "{} is outside the QA sandbox {}; scenarios only open fixture repositories",
                path.display(),
                self.root.display()
            ))
        }
    }
}

/// Runs the gate on the process environment.
pub fn check_process_env() -> Result<Sandbox, String> {
    check(&|name| std::env::var_os(name), real_profile().as_deref())
}

/// The user's real profile directory as the OS knows it, independent of the (sandboxed) environment variables:
/// `FOLDERID_Profile` on Windows. Elsewhere `HOME` is the only source and the sandbox overrides it, so none.
fn real_profile() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::home_dir()
    } else {
        None
    }
}

/// Runs the gate on `env` (a variable lookup). `real_profile` is the user's real profile directory: the sandbox
/// must not be it or contain it (a marker dropped there would expose every repository and setting below it).
pub fn check(
    env: &dyn Fn(&str) -> Option<OsString>,
    real_profile: Option<&Path>,
) -> Result<Sandbox, String> {
    let get = |name: &str| env(name).filter(|v| !v.is_empty());
    let root = get(SANDBOX_ENV).map(PathBuf::from).ok_or_else(|| {
        format!(
            "{SANDBOX_ENV} is not set. A qa build only runs against a sandbox made by tools/qa/new-sandbox.ps1; \
             start it through tools/qa/run-gui.ps1"
        )
    })?;
    if !root.is_absolute() {
        return Err(format!(
            "{SANDBOX_ENV} must be an absolute path, got {}",
            root.display()
        ));
    }
    if root.parent().is_none() {
        return Err(format!(
            "{SANDBOX_ENV}={} is a file-system root, not a sandbox folder",
            root.display()
        ));
    }
    if let Some(profile) = real_profile
        && is_inside(&root, profile)
    {
        return Err(format!(
            "{SANDBOX_ENV}={} is or contains the real profile directory {}",
            root.display(),
            profile.display()
        ));
    }
    let marker = std::fs::read_to_string(root.join(MARKER_FILE)).map_err(|_| {
        format!(
            "{} has no {MARKER_FILE} marker: it was not created by tools/qa/new-sandbox.ps1",
            root.display()
        )
    })?;
    if !marker.starts_with(MARKER_TEXT) {
        return Err(format!(
            "the {MARKER_FILE} marker of {} is not a GitCthulhu QA marker",
            root.display()
        ));
    }

    let mut required = vec!["GITCTHULHU_HOME", "GIT_CONFIG_GLOBAL"];
    required.extend(PROFILE_VARS);
    for name in required {
        let value = get(name)
            .ok_or_else(|| format!("{name} is not set; it must point into the sandbox"))?;
        let path = PathBuf::from(value);
        if !is_inside(&root, &path) {
            return Err(format!(
                "{name}={} is outside the QA sandbox {}",
                path.display(),
                root.display()
            ));
        }
    }
    for name in OPTIONAL_VARS {
        if let Some(value) = get(name) {
            let path = PathBuf::from(value);
            if !is_inside(&root, &path) {
                return Err(format!(
                    "{name}={} is outside the QA sandbox {}",
                    path.display(),
                    root.display()
                ));
            }
        }
    }
    if get("GIT_CONFIG_NOSYSTEM").is_none_or(|v| v != "1") {
        return Err("GIT_CONFIG_NOSYSTEM=1 is required (no system Git configuration in QA)".into());
    }
    if get("GIT_ALLOW_PROTOCOL").is_none_or(|v| v != "file") {
        return Err(
            "GIT_ALLOW_PROTOCOL=file is required (QA never uses the network, only local bare remotes)"
                .into(),
        );
    }
    Ok(Sandbox { root })
}

/// Resolves `.` and `..` lexically and strips the Windows extended-length prefix, so paths compare without the
/// file system (the path may not exist yet).
fn normalize(path: &Path) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                let text = prefix.as_os_str().to_string_lossy().replace("\\\\?\\", "");
                parts.push(fold(&text));
            }
            Component::RootDir => parts.push("/".into()),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            Component::Normal(part) => parts.push(fold(&part.to_string_lossy())),
        }
    }
    parts
}

/// File names compare case-insensitively on Windows.
fn fold(text: &str) -> String {
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.to_owned()
    }
}

/// Whether `path` is `root` or lies below it, lexically and (when it exists) after resolving links.
pub fn is_inside(root: &Path, path: &Path) -> bool {
    let lexical = |a: &Path, b: &Path| {
        let (a, b) = (normalize(a), normalize(b));
        b.len() >= a.len() && b[..a.len()] == a[..]
    };
    if !path.is_absolute() || !lexical(root, path) {
        return false;
    }
    match (root.canonicalize(), path.canonicalize()) {
        (Ok(real_root), Ok(real_path)) => lexical(&real_root, &real_path),
        // A path that does not exist yet cannot be a link out of the sandbox.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use std::collections::HashMap;

    use super::*;

    fn sandbox_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(MARKER_FILE), format!("{MARKER_TEXT}\n")).expect("marker");
        dir
    }

    /// A complete, valid environment for `root`.
    fn good_env(root: &Path) -> HashMap<String, OsString> {
        let inside = |leaf: &str| root.join(leaf).into_os_string();
        let mut env = HashMap::new();
        env.insert(SANDBOX_ENV.to_owned(), root.as_os_str().to_owned());
        env.insert("GITCTHULHU_HOME".into(), inside("app"));
        env.insert("GIT_CONFIG_GLOBAL".into(), inside("gitconfig"));
        env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        env.insert("GIT_ALLOW_PROTOCOL".into(), "file".into());
        for name in PROFILE_VARS {
            env.insert((*name).to_owned(), inside("profile"));
        }
        env
    }

    fn run(env: &HashMap<String, OsString>) -> Result<Sandbox, String> {
        check(&|name| env.get(name).cloned(), None)
    }

    #[test]
    fn the_real_profile_and_its_ancestors_are_never_a_sandbox() {
        let dir = sandbox_dir();
        let env = good_env(dir.path());
        let profile = dir.path().join("Users").join("someone");
        std::fs::create_dir_all(&profile).expect("profile dir");
        let err = check(&|name| env.get(name).cloned(), Some(&profile))
            .expect_err("the sandbox contains the profile");
        assert!(err.contains("profile"), "names the reason: {err}");
        let err = check(&|name| env.get(name).cloned(), Some(dir.path()))
            .expect_err("the sandbox is the profile");
        assert!(err.contains("profile"), "names the reason: {err}");
        let elsewhere = tempfile::tempdir().expect("other dir");
        assert!(
            check(&|name| env.get(name).cloned(), Some(elsewhere.path())).is_ok(),
            "an unrelated profile does not matter"
        );
    }

    #[test]
    fn a_file_system_root_is_never_a_sandbox() {
        let dir = sandbox_dir();
        let root = dir.path().ancestors().last().expect("a root").to_path_buf();
        let mut env = good_env(dir.path());
        env.insert(SANDBOX_ENV.to_owned(), root.into_os_string());
        let err = run(&env).expect_err("a root is refused");
        assert!(err.contains("root"), "names the reason: {err}");
    }

    #[test]
    fn a_complete_sandbox_environment_passes() {
        let dir = sandbox_dir();
        let sandbox = run(&good_env(dir.path())).expect("gate passes");
        assert_eq!(
            sandbox.root(),
            dir.path(),
            "the root is the named directory"
        );
        assert!(
            sandbox
                .check_repository(&sandbox.repos_dir().join("linear"))
                .is_ok(),
            "fixture repositories live in <sandbox>/repos"
        );
    }

    #[test]
    fn a_missing_sandbox_variable_refuses() {
        let dir = sandbox_dir();
        let mut env = good_env(dir.path());
        env.remove(SANDBOX_ENV);
        let err = run(&env).expect_err("refused");
        assert!(err.contains(SANDBOX_ENV), "names the variable: {err}");
    }

    #[test]
    fn a_directory_without_the_marker_refuses() {
        let dir = tempfile::tempdir().expect("temp dir");
        let err = run(&good_env(dir.path())).expect_err("refused");
        assert!(err.contains(MARKER_FILE), "names the marker: {err}");
    }

    #[test]
    fn a_foreign_marker_refuses() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(MARKER_FILE), "something else").expect("marker");
        assert!(run(&good_env(dir.path())).is_err(), "wrong marker text");
    }

    #[test]
    fn every_data_variable_must_stay_inside() {
        let dir = sandbox_dir();
        let other = tempfile::tempdir().expect("other dir");
        for name in ["GITCTHULHU_HOME", "GIT_CONFIG_GLOBAL", PROFILE_VARS[0]] {
            let mut env = good_env(dir.path());
            env.insert(name.to_owned(), other.path().as_os_str().to_owned());
            let err = run(&env).expect_err("refused");
            assert!(err.contains(name), "{name} is named: {err}");
        }
        let mut env = good_env(dir.path());
        env.insert("XDG_DATA_HOME".into(), other.path().as_os_str().to_owned());
        assert!(
            run(&env).is_err(),
            "an optional variable that is set is checked too"
        );
    }

    #[test]
    fn a_missing_profile_or_home_variable_refuses() {
        let dir = sandbox_dir();
        for name in ["GITCTHULHU_HOME", "GIT_CONFIG_GLOBAL", PROFILE_VARS[0]] {
            let mut env = good_env(dir.path());
            env.remove(name);
            assert!(run(&env).is_err(), "{name} is required");
        }
    }

    #[test]
    fn git_must_be_isolated_and_offline() {
        let dir = sandbox_dir();
        let mut env = good_env(dir.path());
        env.remove("GIT_CONFIG_NOSYSTEM");
        assert!(run(&env).is_err(), "the system configuration must be off");
        let mut env = good_env(dir.path());
        env.insert("GIT_ALLOW_PROTOCOL".into(), "file:https".into());
        assert!(run(&env).is_err(), "only the file protocol is allowed");
    }

    #[test]
    fn dot_dot_cannot_escape_the_sandbox() {
        let dir = sandbox_dir();
        let escape = dir
            .path()
            .join("repos")
            .join("..")
            .join("..")
            .join("elsewhere");
        assert!(
            !is_inside(dir.path(), &escape),
            "{} escapes",
            escape.display()
        );
        let sandbox = run(&good_env(dir.path())).expect("gate passes");
        assert!(
            sandbox.check_repository(&escape).is_err(),
            "repositories outside are refused"
        );
        assert!(
            !is_inside(dir.path(), Path::new("repos/linear")),
            "relative paths are never inside"
        );
    }

    #[test]
    fn a_sibling_with_the_same_prefix_is_outside() {
        let dir = sandbox_dir();
        let sibling = PathBuf::from(format!("{}-other", dir.path().display()));
        assert!(
            !is_inside(dir.path(), &sibling),
            "component-wise comparison"
        );
    }
}
