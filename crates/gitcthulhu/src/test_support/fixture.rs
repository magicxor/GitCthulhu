//! Throw-away Git repositories for fixture tests (spec 21 §8.1): the real `git` executable, isolated from the
//! developer's configuration (`GIT_CONFIG_GLOBAL` / `GIT_CONFIG_NOSYSTEM`, own HOME), like the backend's tests (20 §10).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use gitcthulhu_git::repo::TestIsolation;
use gitcthulhu_git::{GitRuntime, RuntimeConfig};

/// The global configuration of every fixture (also applied to gix opens, which do not read the global file).
const BASE_CONFIG: &[(&str, &str)] = &[
    ("user.name", "Test User"),
    ("user.email", "test@example.com"),
    ("init.defaultBranch", "main"),
    ("core.autocrlf", "false"),
    ("commit.gpgsign", "false"),
    ("tag.gpgsign", "false"),
    ("protocol.file.allow", "always"),
    ("advice.detachedHead", "false"),
];

/// Variables of the test process that would redirect a child git.
const SCRUBBED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
];

/// A repository in a temporary directory (`<tmp>/repo`, branch `main`, no commits).
pub struct Fixture {
    /// The temporary world: `repo/`, `home/`, `gitconfig`.
    pub dir: tempfile::TempDir,
    repo: PathBuf,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixture {
    /// `git init -b main` in a fresh temporary directory.
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("gcg")
            .tempdir()
            .expect("a temporary directory can be created");
        std::fs::create_dir_all(dir.path().join("home")).expect("home can be created");
        std::fs::create_dir_all(dir.path().join("repo")).expect("the repository can be created");
        let mut text = String::new();
        for (key, value) in BASE_CONFIG {
            let (section, name) = key.split_once('.').expect("keys have a section");
            // `protocol.file.allow` = `[protocol "file"] allow`.
            let (section, name) = match name.split_once('.') {
                Some((sub, name)) => (format!("{section} \"{sub}\""), name),
                None => (section.to_owned(), name),
            };
            text.push_str(&format!("[{section}]\n\t{name} = {value}\n"));
        }
        std::fs::write(dir.path().join("gitconfig"), text)
            .expect("the global config can be written");
        let repo = dir.path().join("repo");
        let fixture = Fixture { dir, repo };
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture
    }

    /// The repository's worktree.
    pub fn path(&self) -> &Path {
        &self.repo
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn global_config(&self) -> PathBuf {
        self.dir.path().join("gitconfig")
    }

    /// Writes `text` to `path` (relative to the worktree), creating parent directories.
    pub fn write(&self, path: &str, text: &str) {
        let full = self.repo.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("parent directories can be created");
        }
        std::fs::write(&full, text).expect("the file can be written");
    }

    /// Runs `git <args>` in the worktree and returns its stdout. Panics with stderr when git fails.
    pub fn git(&self, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", self.global_config())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .env("XDG_CONFIG_HOME", self.home().join(".config"));
        for var in SCRUBBED_ENV {
            cmd.env_remove(var);
        }
        let output = cmd.output().expect("git can be started");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// `git add -A` + `git commit -m msg`.
    pub fn commit_all(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }

    /// A Git runtime isolated like the fixture's own git calls.
    pub fn runtime(&self) -> Arc<GitRuntime> {
        GitRuntime::new(RuntimeConfig {
            login_shell_env: false,
            test_isolation: Some(TestIsolation {
                home: self.home(),
                global_config: self.global_config(),
                overrides: BASE_CONFIG
                    .iter()
                    .filter(|(key, _)| *key != "core.autocrlf")
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            }),
            ..RuntimeConfig::default()
        })
        .expect("git is installed and recent enough for the tests")
    }
}
