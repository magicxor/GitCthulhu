//! Shared fixtures of the integration tests: throw-away repositories built with the real `git` executable.
//!
//! No shell scripts are involved (on the development machine `bash` may resolve to WSL's shim): every fixture is
//! built by spawning `git` directly or by feeding `git fast-import`.
#![allow(dead_code)] // Every test binary includes this module and uses a different part of it.

use std::{
    collections::VecDeque,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use gitcthulhu_git::{
    CancelToken, CredentialPrompter, CredentialRequest, CredentialResponse, GitRuntime, NoProgress,
    OpContext, Repo, RepoExclusiveGuard, RuntimeConfig, Sensitive, repo::TestIsolation,
};

/// Environment variables that would make a child git operate on another repository or load foreign settings.
const SCRUBBED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_TRACE2_EVENT",
    "GIT_EXEC_PATH",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// Prefixes of further inherited variables that carry `-c` overrides into a child git.
const SCRUBBED_ENV_PREFIXES: &[&str] = &["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"];

/// Keys that only the global gitconfig of the fixtures sets. They are *not* injected into gix opens as overrides,
/// because an override beats the repository's own configuration: a test that runs `git config core.autocrlf true` in
/// its repository must see that value through gix as well (gix's default for an unset `core.autocrlf` is `false`,
/// the value of the global file). See [`TestRepo::open_with_autocrlf`].
const GLOBAL_FILE_ONLY: &[&str] = &["core.autocrlf"];

/// Settings every fixture repository gets (also applied to gix opens, which do not read the global file when
/// isolated, except for [`GLOBAL_FILE_ONLY`]).
const BASE_CONFIG: &[(&str, &str)] = &[
    ("user.name", "Test User"),
    ("user.email", "test@example.com"),
    ("init.defaultBranch", "main"),
    ("core.autocrlf", "false"),
    ("commit.gpgsign", "false"),
    ("tag.gpgsign", "false"),
    ("protocol.file.allow", "always"),
    ("advice.detachedHead", "false"),
    ("advice.statusHints", "false"),
    ("advice.pushUpdateRejected", "false"),
    ("advice.addIgnoredFile", "false"),
];

/// A temporary world: a home directory with a global Git configuration, and a runtime isolated from the
/// developer's own configuration.
pub struct TestEnv {
    pub tmp: tempfile::TempDir,
    pub rt: Arc<GitRuntime>,
    pub home: PathBuf,
    pub global_config: PathBuf,
    clock: Arc<AtomicU64>,
}

impl TestEnv {
    /// Creates `<tmp>/home` and `<tmp>/gitconfig` and a runtime that uses only those.
    pub fn new() -> Self {
        let (tmp, home, global_config) = Self::world();
        TestEnv {
            rt: Self::runtime_for(&Self::config_for(&home, &global_config)),
            tmp,
            home,
            global_config,
            clock: Arc::new(AtomicU64::new(1_700_000_000)),
        }
    }

    fn world() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::Builder::new()
            .prefix("gc")
            .tempdir_in(canonical_temp_dir())
            .expect("a temporary directory can be created");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).expect("home can be created");
        let global_config = tmp.path().join("gitconfig");
        let mut text = String::new();
        for (key, value) in BASE_CONFIG {
            let (section, name) = key.split_once('.').expect("keys have a section");
            let (section, subsection) = match name.split_once('.') {
                Some((sub, name)) => (format!("{section} \"{sub}\""), name),
                None => (section.to_owned(), name),
            };
            text.push_str(&format!("[{section}]\n\t{subsection} = {value}\n"));
        }
        std::fs::write(&global_config, text).expect("the global config can be written");
        (tmp, home, global_config)
    }

    /// The runtime configuration the fixtures use, for tests that need to tweak it.
    pub fn runtime_config(&self) -> RuntimeConfig {
        Self::config_for(&self.home, &self.global_config)
    }

    fn config_for(home: &Path, global_config: &Path) -> RuntimeConfig {
        RuntimeConfig {
            askpass_exe: Some(env!("CARGO_BIN_EXE_gitcthulhu-git-askpass").into()),
            editor_exe: Some(env!("CARGO_BIN_EXE_gitcthulhu-git-editor").into()),
            login_shell_env: false,
            test_isolation: Some(TestIsolation {
                home: home.to_owned(),
                global_config: global_config.to_owned(),
                overrides: BASE_CONFIG
                    .iter()
                    .filter(|(key, _)| !GLOBAL_FILE_ONLY.contains(key))
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            }),
            ..RuntimeConfig::default()
        }
    }

    fn runtime_for(config: &RuntimeConfig) -> Arc<GitRuntime> {
        GitRuntime::new(config.clone()).expect("git is installed and recent enough for the tests")
    }

    /// A new runtime with a customised configuration (isolated like the default one).
    pub fn runtime_with(
        &self,
        tweak: impl FnOnce(&mut RuntimeConfig),
    ) -> gitcthulhu_git::Result<Arc<GitRuntime>> {
        let mut config = self.runtime_config();
        tweak(&mut config);
        GitRuntime::new(config)
    }

    /// A fresh directory `<tmp>/<name>` (not created).
    pub fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    /// `git init -b main` in `<tmp>/<name>`.
    pub fn repo(&self, name: &str) -> TestRepo {
        let path = self.path(name);
        std::fs::create_dir_all(&path).expect("repository directory can be created");
        let repo = TestRepo {
            path,
            rt: self.rt.clone(),
            home: self.home.clone(),
            global_config: self.global_config.clone(),
            clock: self.clock.clone(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    /// `git init --bare` in `<tmp>/<name>.git`; used through `file://` URLs.
    pub fn bare_remote(&self, name: &str) -> PathBuf {
        let path = self.path(&format!("{name}.git"));
        std::fs::create_dir_all(&path).expect("remote directory can be created");
        let repo = TestRepo {
            path: path.clone(),
            rt: self.rt.clone(),
            home: self.home.clone(),
            global_config: self.global_config.clone(),
            clock: self.clock.clone(),
        };
        repo.git(&["init", "-q", "--bare", "-b", "main"]);
        path
    }

    /// Run `git <args>` against the isolated global configuration, outside any repository.
    pub fn git_global(&self, args: &[&str]) -> String {
        let repo = TestRepo {
            path: self.tmp.path().to_owned(),
            rt: self.rt.clone(),
            home: self.home.clone(),
            global_config: self.global_config.clone(),
            clock: self.clock.clone(),
        };
        repo.git(args)
    }
}

impl Drop for TestEnv {
    /// Windows: a just-exited child (or the antivirus) may still hold a handle inside the directory for a moment, so
    /// deletion is retried 5 x 100 ms before `TempDir` makes its own (silent) last attempt.
    fn drop(&mut self) {
        for attempt in 0..5 {
            match std::fs::remove_dir_all(self.tmp.path()) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if attempt < 4 => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(err) => eprintln!(
                    "could not remove the test directory {}: {err}",
                    self.tmp.path().display()
                ),
            }
        }
    }
}

/// A repository built by `git`.
pub struct TestRepo {
    pub path: PathBuf,
    rt: Arc<GitRuntime>,
    home: PathBuf,
    global_config: PathBuf,
    clock: Arc<AtomicU64>,
}

impl TestRepo {
    /// A repository at `path` created by running `git <init_args>` there (for `--ref-format`, `--object-format`...).
    pub fn init_with(env: &TestEnv, path: &Path, init_args: &[&str]) -> TestRepo {
        std::fs::create_dir_all(path).expect("repository directory can be created");
        let repo = TestRepo {
            path: path.to_owned(),
            rt: env.rt.clone(),
            home: env.home.clone(),
            global_config: env.global_config.clone(),
            clock: env.clock.clone(),
        };
        repo.git(init_args);
        repo
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(self.rt.git_path());
        for var in SCRUBBED_ENV {
            cmd.env_remove(var);
        }
        for (key, _) in std::env::vars_os() {
            if key
                .to_str()
                .is_some_and(|key| SCRUBBED_ENV_PREFIXES.iter().any(|p| key.starts_with(p)))
            {
                cmd.env_remove(key);
            }
        }
        // A fixed, per-call incremented date keeps commit ids deterministic and ordered.
        let now = self.clock.fetch_add(60, Ordering::SeqCst);
        let date = format!("{now} +0000");
        cmd.current_dir(&self.path)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.global_config)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LANGUAGE", "en")
            .env("LC_MESSAGES", "C")
            .env("GIT_EDITOR", ":")
            .stdin(Stdio::null());
        cmd
    }

    /// Run `git <args>` with an isolated environment; returns stdout without the trailing newline and panics with
    /// stderr on failure.
    pub fn git(&self, args: &[&str]) -> String {
        let output = self.command(args).output().expect("git can be spawned");
        assert!(
            output.status.success(),
            "git {args:?} failed in {}: {}",
            self.path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_owned()
    }

    /// Run `git <args>` and return its raw stdout bytes; panics on failure.
    pub fn git_bytes(&self, args: &[&str]) -> Vec<u8> {
        let output = self.command(args).output().expect("git can be spawned");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    /// Run `git <args>`, expect a non-zero exit and return stderr.
    pub fn git_fail(&self, args: &[&str]) -> String {
        let output = self.command(args).output().expect("git can be spawned");
        assert!(
            !output.status.success(),
            "git {args:?} was expected to fail but printed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    /// Write a file (creating parent directories).
    pub fn write(&self, rela: &str, content: impl AsRef<[u8]>) {
        let path = self.path.join(rela);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent directories can be created");
        }
        std::fs::write(&path, content).expect("file can be written");
    }

    /// Whether the index holds sparse directory entries (`git ls-files --sparse` needs Git 2.35).
    pub fn index_is_sparse(&self) -> bool {
        gix::open(&self.path)
            .expect("the repository opens")
            .index_or_empty()
            .expect("the index can be read")
            .is_sparse()
    }

    /// Read a file.
    pub fn read(&self, rela: &str) -> Vec<u8> {
        std::fs::read(self.path.join(rela)).expect("file can be read")
    }

    /// Delete a file.
    pub fn remove(&self, rela: &str) {
        std::fs::remove_file(self.path.join(rela)).expect("file can be removed");
    }

    /// `git add -A` and `git commit`; returns the new commit.
    pub fn commit_all(&self, msg: &str) -> gix::ObjectId {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        let id = self.git(&["rev-parse", "HEAD"]);
        gix::ObjectId::from_hex(id.as_bytes()).expect("rev-parse prints a hex id")
    }

    /// Writes `.git/hooks/<name>` as `#!/bin/sh\n<script_body>` with LF line endings, executable on Unix.
    pub fn hook(&self, name: &str, script_body: &str) {
        let dir = self.path.join(".git").join("hooks");
        std::fs::create_dir_all(&dir).expect("hooks directory can be created");
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script_body}\n")).expect("hook can be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("hook can be made executable");
        }
    }

    /// The content of the index entry for `rela` (`git show :<rela>`).
    pub fn index_blob(&self, rela: &str) -> Vec<u8> {
        self.git_bytes(&["show", &format!(":{rela}")])
    }

    /// Feed a generated `git fast-import` stream (big graphs: 10k-100k commits, octopus merges, many tags).
    pub fn fast_import(&self, stream: &[u8]) {
        let stream = stream.to_vec();
        self.fast_import_with(move |stdin| stdin.write_all(&stream));
    }

    /// Like [`TestRepo::fast_import`], but `produce` writes the stream piece by piece, so a stream of hundreds of
    /// megabytes never has to exist in memory.
    pub fn fast_import_with(
        &self,
        produce: impl FnOnce(&mut std::process::ChildStdin) -> std::io::Result<()> + Send + 'static,
    ) {
        let mut cmd = self.command(&["fast-import", "--quiet"]);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("git fast-import can be spawned");
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let writer = std::thread::spawn(move || produce(&mut stdin));
        let output = child.wait_with_output().expect("fast-import finishes");
        writer
            .join()
            .expect("writer thread does not panic")
            .expect("the stream can be written");
        assert!(
            output.status.success(),
            "fast-import failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Set `core.autocrlf` in the repository's own configuration and open it: both the `git` children and the gix
    /// reads of the backend see `value` (`"true"`, `"input"`, `"false"`).
    pub fn open_with_autocrlf(&self, value: &str) -> Repo {
        self.git(&["config", "core.autocrlf", value]);
        self.open()
    }

    /// Open the repository with the backend under test.
    pub fn open(&self) -> Repo {
        Repo::open(&self.rt, &self.path).expect("the repository can be opened")
    }

    /// An operation context with the given cancel token and prompter, ignoring progress.
    pub fn op_ctx(&self, cancel: CancelToken, prompter: Arc<dyn CredentialPrompter>) -> OpContext {
        OpContext {
            cancel,
            progress: Arc::new(NoProgress),
            prompter,
            label: "test".into(),
        }
    }

    /// The exclusive guard of `repo`.
    pub fn exclusive(&self, repo: &Repo) -> RepoExclusiveGuard {
        repo.lock_exclusive()
    }
}

/// The temp directory in the form Git reports paths in: `TEMP` may be an 8.3 short path on Windows
/// (`C:\Users\RUNNER~1\...` on hosted runners) and is a symlink into `/private/var` on macOS, and Git
/// matches `includeIf "gitdir:..."` and prints worktree paths by the real one.
fn canonical_temp_dir() -> PathBuf {
    let temp = std::env::temp_dir();
    let Ok(canonical) = std::fs::canonicalize(&temp) else {
        return temp;
    };
    // Without the `\\?\` prefix Windows adds, which Git does not understand.
    match canonical
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
    {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ if cfg!(windows) => temp,
        _ => canonical,
    }
}

/// Whether the platform lets the current user create symbolic links (Windows needs Developer Mode).
pub fn symlinks_supported() -> bool {
    let dir = tempfile::Builder::new()
        .prefix("gc")
        .tempdir()
        .expect("a temporary directory can be created");
    let target = dir.path().join("target");
    if std::fs::write(&target, b"x").is_err() {
        return false;
    }
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(&target, dir.path().join("link"));
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(&target, dir.path().join("link"));
    result.is_ok()
}

/// Whether two paths denote the same file system object (tolerates 8.3 names and `\\?\` prefixes on Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// A prompter that answers from a queue (`None` = cancel) and records what it was asked.
#[derive(Default)]
pub struct TestPrompter {
    answers: poison_free::Mutex<VecDeque<Option<String>>>,
    requests: poison_free::Mutex<Vec<CredentialRequest>>,
}

impl TestPrompter {
    /// A prompter that will answer with `answers` in order, then cancel.
    pub fn new(answers: impl IntoIterator<Item = Option<&'static str>>) -> Self {
        TestPrompter {
            answers: poison_free::Mutex::new(
                answers.into_iter().map(|a| a.map(str::to_owned)).collect(),
            ),
            requests: Default::default(),
        }
    }

    /// The prompts seen so far.
    pub fn requests(&self) -> Vec<CredentialRequest> {
        self.requests.lock().clone()
    }
}

impl CredentialPrompter for TestPrompter {
    fn prompt(&self, request: &CredentialRequest) -> CredentialResponse {
        self.requests.lock().push(request.clone());
        match self.answers.lock().pop_front().flatten() {
            Some(answer) => CredentialResponse::Answer(Sensitive::new(answer)),
            None => CredentialResponse::Cancel,
        }
    }
}

/// A poison-ignoring `std::sync::Mutex` wrapper, so the support module needs no extra dependency.
mod poison_free {
    #[derive(Default)]
    pub struct Mutex<T>(std::sync::Mutex<T>);

    impl<T> Mutex<T> {
        pub fn new(value: T) -> Self {
            Mutex(std::sync::Mutex::new(value))
        }

        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }
}
