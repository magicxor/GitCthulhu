//! The process-wide [`GitRuntime`] (git executable, version, helper programs, private tokio runtime) and the
//! [`Repo`] handle every other module operates on.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use but_core::sync::{RepoExclusiveGuard, RepoSharedGuard};
use parking_lot::RwLock;

use crate::{
    CommandRecord, GitError, GitVersion, HostKeyPolicy, OpClass, PostState, Result, cli::GitOutput,
    error::classify,
};

/// Configuration of the [`GitRuntime`].
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// User override; else `gix::path::env::exe_invocation()`.
    pub git_path: Option<PathBuf>,
    /// Askpass helper. `None` => `<dir of current_exe>/gitcthulhu-askpass[.exe]` (the GUI's bin, §1.8).
    /// Backend tests pass `env!("CARGO_BIN_EXE_gitcthulhu-git-askpass")`.
    pub askpass_exe: Option<PathBuf>,
    /// Rebase editor helper. `None` => `<dir of current_exe>/gitcthulhu-editor[.exe]`.
    /// Backend tests pass `env!("CARGO_BIN_EXE_gitcthulhu-git-editor")`.
    pub editor_exe: Option<PathBuf>,
    /// How unknown SSH host keys are treated; default `AcceptNew`.
    pub host_key_policy: HostKeyPolicy,
    /// Merge the login shell's `PATH` into the environment of child processes; default true on macOS/Linux, ignored
    /// on Windows.
    pub login_shell_env: bool,
    /// `-c key=value` pairs added to every Network command (GUI proxy / TLS settings: `http.proxy`, `http.sslVerify`;
    /// replaces Gitnuro's JVM proxy properties, 05 §6.5).
    pub network_config: Vec<(String, String)>,
    /// Tests only: isolate every child git and every gix open from the developer's config (§10).
    #[doc(hidden)]
    pub test_isolation: Option<TestIsolation>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            git_path: None,
            askpass_exe: None,
            editor_exe: None,
            host_key_policy: HostKeyPolicy::default(),
            login_shell_env: default_login_shell_env(),
            network_config: Vec::new(),
            test_isolation: None,
        }
    }
}

/// Whether the login shell's environment is merged by default: on macOS/Linux, where GUI apps started from a
/// launcher miss the user's `PATH`.
fn default_login_shell_env() -> bool {
    cfg!(not(windows))
}

/// How tests cut the backend off from the developer's own Git configuration.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct TestIsolation {
    /// Used as `HOME` / `USERPROFILE` / `XDG_CONFIG_HOME` parent of every child.
    pub home: PathBuf,
    /// `GIT_CONFIG_GLOBAL` of every child.
    pub global_config: PathBuf,
    /// `key=value` configuration applied to every gix open (gix does not read the global file when isolated). Child
    /// processes read the same settings from `global_config`.
    pub overrides: Vec<(String, String)>,
}

/// Process-wide. Owns the private tokio runtime (2 worker threads, `enable_all`, thread name "gitcthulhu-git")
/// used only by cli.rs (child processes, askpass servers) and watch.rs.
pub struct GitRuntime {
    config: RuntimeConfig,
    git_path: PathBuf,
    version: GitVersion,
    askpass_path: PathBuf,
    editor_path: PathBuf,
    base_env: Vec<(OsString, OsString)>,
    tokio: Option<tokio::runtime::Runtime>,
}

impl std::fmt::Debug for GitRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitRuntime")
            .field("git_path", &self.git_path)
            .field("version", &self.version)
            .field("askpass_path", &self.askpass_path)
            .field("editor_path", &self.editor_path)
            .finish_non_exhaustive()
    }
}

impl Drop for GitRuntime {
    fn drop(&mut self) {
        // Dropping a tokio runtime from within an async context panics; the last `Arc` may well be released on a
        // runtime worker of the application. Shut down without blocking instead.
        if let Some(runtime) = self.tokio.take() {
            runtime.shutdown_background();
        }
    }
}

/// Where the helper programs are, and whether they exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperStatus {
    /// `Err` = expected path, missing.
    pub askpass: Result<PathBuf, PathBuf>,
    /// `Err` = expected path, missing.
    pub editor: Result<PathBuf, PathBuf>,
}

impl GitRuntime {
    /// Start the runtime: locate and version-check `git`, resolve the helper programs, build the base environment.
    ///
    /// Errors: `GitNotFound` (spawn failed), `GitTooOld`. A missing helper binary => Ok, a `tracing::warn!` naming the
    /// expected path, and `helper_status()` reports it (the GUI shows "helper missing" in About and in the credential
    /// error). Without askpass, prompts cannot be answered (Git fails → AuthFailed); without the editor helper,
    /// `rebase::start_interactive` returns `Unsupported { what: "editor helper missing" }`.
    pub fn new(config: RuntimeConfig) -> Result<Arc<GitRuntime>> {
        let git_path = config
            .git_path
            .clone()
            .unwrap_or_else(|| gix::path::env::exe_invocation().to_path_buf());
        let version = detect_version(&git_path, config.test_isolation.as_ref())?;
        if version < GitVersion::MIN {
            return Err(GitError::GitTooOld {
                found: version,
                required: GitVersion::MIN,
            });
        }

        let askpass_path = helper_path(config.askpass_exe.as_deref(), "gitcthulhu-askpass");
        let editor_path = helper_path(config.editor_exe.as_deref(), "gitcthulhu-editor");
        for (what, path) in [("askpass", &askpass_path), ("rebase editor", &editor_path)] {
            if !path.is_file() {
                tracing::warn!(
                    path = %path.display(),
                    "The {what} helper is missing; related features are disabled"
                );
            }
        }

        let base_env = base_env(config.login_shell_env);
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("gitcthulhu-git")
            .build()
            .map_err(|err| {
                GitError::Other(
                    anyhow::Error::new(err).context("Failed to start the git executor runtime"),
                )
            })?;
        Ok(Arc::new(GitRuntime {
            config,
            git_path,
            version,
            askpass_path,
            editor_path,
            base_env,
            tokio: Some(tokio),
        }))
    }

    /// The version of the git executable.
    pub fn version(&self) -> GitVersion {
        self.version
    }

    /// The git executable.
    pub fn git_path(&self) -> &Path {
        &self.git_path
    }

    /// Whether the askpass helper exists, so prompts can be answered.
    pub fn has_askpass(&self) -> bool {
        self.askpass_path.is_file()
    }

    /// Whether the rebase editor helper exists, so interactive rebases are possible.
    pub fn has_editor_helper(&self) -> bool {
        self.editor_path.is_file()
    }

    /// Where the helper programs are, and whether they exist.
    pub fn helper_status(&self) -> HelperStatus {
        let status = |path: &PathBuf| {
            if path.is_file() {
                Ok(path.clone())
            } else {
                Err(path.clone())
            }
        };
        HelperStatus {
            askpass: status(&self.askpass_path),
            editor: status(&self.editor_path),
        }
    }

    /// The path of the askpass helper, existing or not.
    #[allow(dead_code)] // Used by the executor (WP-B0b).
    pub(crate) fn askpass_exe(&self) -> &Path {
        &self.askpass_path
    }

    /// The path of the editor helper, existing or not.
    #[allow(dead_code)] // Used by the executor (WP-B0b).
    pub(crate) fn editor_exe(&self) -> &Path {
        &self.editor_path
    }

    /// The configuration this runtime was created with.
    #[allow(dead_code)] // Used by the executor (WP-B0b).
    pub(crate) fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// The process environment (plus the login shell's `PATH` on macOS/Linux) captured at startup.
    #[allow(dead_code)] // Used by the executor (WP-B0b).
    pub(crate) fn base_env(&self) -> &[(OsString, OsString)] {
        &self.base_env
    }

    /// The private tokio runtime that drives child processes and askpass servers.
    #[allow(dead_code)] // Used by the executor (WP-B0b).
    pub(crate) fn tokio(&self) -> &tokio::runtime::Runtime {
        self.tokio
            .as_ref()
            .expect("the runtime is only taken out when dropping")
    }

    /// The isolation of a test runtime; `None` in production.
    pub(crate) fn test_isolation(&self) -> Option<&TestIsolation> {
        self.config.test_isolation.as_ref()
    }

    /// The options every gix open of this runtime uses.
    fn gix_open_options(&self) -> gix::open::Options {
        match &self.config.test_isolation {
            Some(isolation) => gix::open::Options::isolated().config_overrides(
                isolation
                    .overrides
                    .iter()
                    .map(|(key, value)| format!("{key}={value}")),
            ),
            None => gix::open::Options::default(),
        }
    }

    /// The trust-level dependent options for `gix::discover`.
    fn gix_trust_mapping(&self) -> gix::sec::trust::Mapping<gix::open::Options> {
        match &self.config.test_isolation {
            Some(_) => {
                let options = self.gix_open_options();
                gix::sec::trust::Mapping {
                    full: options.clone(),
                    reduced: options,
                }
            }
            None => Default::default(),
        }
    }

    /// A synchronous `git` command with the environment hygiene of §1.7 (no repository targeting variables, no
    /// prompts, English messages, no editor) and, in tests, the isolation of `RuntimeConfig::test_isolation`.
    ///
    /// For the few operations that run outside the async executor: the version probe and `git init`.
    pub(crate) fn blocking_git(&self, cwd: Option<&Path>) -> std::process::Command {
        blocking_git(&self.git_path, cwd, self.config.test_isolation.as_ref())
    }
}

/// Environment variables that would make a child git operate on another repository.
const REPOSITORY_TARGETING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Inherited tooling variables that must not reach a child git (§1.7): foreign askpass programs, `-c` overrides and
/// pathspec modes of a terminal or hook the application was started from.
const INHERITED_TOOLING_ENV: &[&str] = &[
    "GIT_TRACE2_EVENT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SSH_ASKPASS_REQUIRE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_EXEC_PATH",
    "GIT_REFLOG_ACTION",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// Prefixes of inherited tooling variables that must not reach a child git (§1.7).
const INHERITED_TOOLING_ENV_PREFIXES: &[&str] =
    &["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_", "VSCODE_GIT_"];

fn blocking_git(
    git_path: &Path,
    cwd: Option<&Path>,
    isolation: Option<&TestIsolation>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(git_path);
    for var in REPOSITORY_TARGETING_ENV.iter().chain(INHERITED_TOOLING_ENV) {
        cmd.env_remove(var);
    }
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| {
            INHERITED_TOOLING_ENV_PREFIXES
                .iter()
                .any(|prefix| key.starts_with(prefix))
        }) {
            cmd.env_remove(key);
        }
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("LANGUAGE", "en")
        .env("LC_MESSAGES", "C")
        .env("GIT_EDITOR", ":")
        .env("GIT_SEQUENCE_EDITOR", ":")
        .env("GIT_PAGER", "cat")
        .stdin(std::process::Stdio::null());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    if let Some(isolation) = isolation {
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &isolation.global_config)
            .env("HOME", &isolation.home)
            .env("USERPROFILE", &isolation.home)
            .env("XDG_CONFIG_HOME", isolation.home.join(".config"));
    }
    cmd.args([
        "--no-pager",
        "-c",
        "core.quotepath=false",
        "-c",
        "color.ui=false",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Run `git version` and parse its output.
fn detect_version(git_path: &Path, isolation: Option<&TestIsolation>) -> Result<GitVersion> {
    let output = blocking_git(git_path, None, isolation)
        .arg("version")
        .output()
        .map_err(|err| {
            tracing::debug!(%err, path = %git_path.display(), "Failed to spawn `git version`");
            GitError::GitNotFound {
                searched: git_path.display().to_string(),
            }
        })?;
    if !output.status.success() {
        return Err(GitError::Other(anyhow::anyhow!(
            "`{} version` failed with {}: {}",
            git_path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    GitVersion::parse(&text).ok_or_else(|| {
        GitError::Other(anyhow::anyhow!(
            "`{} version` printed something unexpected: {:?}",
            git_path.display(),
            text.trim()
        ))
    })
}

/// The configured helper path, else `<dir of current_exe>/<name>[.exe]`.
fn helper_path(configured: Option<&Path>, name: &str) -> PathBuf {
    if let Some(path) = configured {
        return path.to_owned();
    }
    let file_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    match std::env::current_exe() {
        Ok(exe) => exe.with_file_name(file_name),
        Err(_) => PathBuf::from(file_name),
    }
}

/// The process environment, with the login shell's `PATH` merged in on macOS/Linux so hooks find `node`/`npx` (husky)
/// when the app was started from Finder or a desktop launcher.
fn base_env(login_shell_env: bool) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    if cfg!(windows) || !login_shell_env {
        return env;
    }
    let Some(login_env) = but_core::cmd::extract_interactive_login_shell_environment() else {
        return env;
    };
    for (key, value) in login_env {
        if key == "PATH" {
            let existing = env.iter().position(|(k, _)| k == "PATH");
            let current = existing.map(|idx| env[idx].1.clone()).unwrap_or_default();
            let mut dirs: Vec<PathBuf> = std::env::split_paths(&value).collect();
            for dir in std::env::split_paths(&current) {
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
            if let Ok(joined) = std::env::join_paths(dirs) {
                match existing {
                    Some(idx) => env[idx].1 = joined,
                    None => env.push((key, joined)),
                }
            }
        } else if key == "SSH_AUTH_SOCK" && !env.iter().any(|(k, _)| k == "SSH_AUTH_SOCK") {
            env.push((key, value));
        }
    }
    env
}

/// What kind of repository a [`Repo`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoKind {
    /// An ordinary repository with its main worktree.
    Main,
    /// A linked worktree of another repository.
    LinkedWorktree {
        /// The directory name under `<common>/worktrees/`.
        id: String,
    },
    /// A submodule's repository.
    Submodule {
        /// The worktree of the superproject, if it could be determined.
        superproject: Option<PathBuf>,
    },
    /// A bare repository.
    Bare,
}

struct RepoInner {
    rt: Arc<GitRuntime>,
    repo: RwLock<gix::ThreadSafeRepository>,
    /// What `reload()` opens again: the worktree, or the git dir of a bare repository.
    open_path: PathBuf,
    workdir: Option<PathBuf>,
    git_dir: PathBuf,
    common_dir: PathBuf,
    kind: RepoKind,
}

/// A cheap-to-clone, `Send + Sync` handle to an open repository.
#[derive(Clone)]
pub struct Repo(Arc<RepoInner>);

impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo")
            .field("workdir", &self.0.workdir)
            .field("git_dir", &self.0.git_dir)
            .field("kind", &self.0.kind)
            .finish()
    }
}

impl Repo {
    /// Open the repository at or above `path`.
    ///
    /// Gitnuro 2.1 rules: `path` must exist (`NotFound`) and be a directory (`NotARepository`); discovers upwards
    /// (gix discovery with default trust), so a sub-directory opens its enclosing work tree; a submodule dir opens the
    /// submodule. Then reads `extensions.refStorage` from the repository config: any value other than absent/"files"
    /// => `Unsupported { what: "reftable ref storage (needs a newer gitoxide)" }` (the pinned gix opens such a
    /// repository without error but sees no refs: empty history, unborn HEAD, every file "staged").
    ///
    /// Errors: `NotFound`, `NotARepository`, `UnsafeRepository` (the repository is owned by someone else and not
    /// listed in `safe.directory`), `Unsupported`, `Io`.
    pub fn open(rt: &Arc<GitRuntime>, path: &Path) -> Result<Repo> {
        let metadata = std::fs::metadata(path).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => GitError::NotFound {
                what: "directory",
                name: path.display().to_string(),
            },
            _ => GitError::io(path, err),
        })?;
        if !metadata.is_dir() {
            return Err(GitError::NotARepository {
                path: path.to_owned(),
            });
        }
        let repo = gix::ThreadSafeRepository::discover_opts(
            path,
            // Like Git: honour GIT_CEILING_DIRECTORIES and GIT_DISCOVERY_ACROSS_FILESYSTEM.
            gix::discover::upwards::Options::default().apply_environment(),
            rt.gix_trust_mapping(),
        )
        .map_err(|err| map_discover_error(err, path))?;
        Self::from_gix(rt, repo, path)
    }

    /// `git init [-b <initial_branch>] <path>` then [`Repo::open()`]. Engine: git (templates/hooks).
    pub fn init(rt: &Arc<GitRuntime>, path: &Path, initial_branch: Option<&str>) -> Result<Repo> {
        std::fs::create_dir_all(path).map_err(|err| GitError::io(path, err))?;
        let mut cmd = rt.blocking_git(Some(path));
        cmd.arg("init");
        if let Some(branch) = initial_branch {
            cmd.args(["-b", branch]);
        }
        cmd.arg(".");
        let started = std::time::Instant::now();
        let output = cmd.output().map_err(|err| GitError::GitNotFound {
            searched: format!("{} ({err})", rt.git_path.display()),
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let args = match initial_branch {
                Some(branch) => format!("init -b {branch} ."),
                None => "init .".to_owned(),
            };
            let out = GitOutput {
                exit_code: output.status.code(),
                stdout: output.stdout,
                stderr: stderr.clone(),
                hooks: Vec::new(),
                record: CommandRecord {
                    args,
                    exit_code: output.status.code(),
                    duration: started.elapsed(),
                    stderr,
                },
                prompt_refused: None,
                removed_stale_locks: Vec::new(),
                warnings: Vec::new(),
            };
            return Err(classify(OpClass::Generic, &out, &PostState::default()));
        }
        Self::open(rt, path)
    }

    fn from_gix(
        rt: &Arc<GitRuntime>,
        repo: gix::ThreadSafeRepository,
        searched: &Path,
    ) -> Result<Repo> {
        let local = repo.to_thread_local();
        reject_unsupported_ref_storage(&local)?;
        reject_untrusted(&local, searched, rt)?;

        // Absolute and free of `..`: gix joins a linked worktree's `commondir` file (`../..`) onto its git dir, and
        // `common_dir` is the key of the in-process lock map, so a main worktree and its linked worktrees must
        // produce the identical path.
        let workdir = local.workdir().map(absolute_normalized);
        let git_dir = absolute_normalized(local.git_dir());
        let common_dir = absolute_normalized(local.common_dir());
        let kind = if local.is_bare() {
            RepoKind::Bare
        } else {
            match local.kind() {
                gix::repository::Kind::LinkedWorkTree => RepoKind::LinkedWorktree {
                    id: git_dir
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                },
                gix::repository::Kind::Submodule => RepoKind::Submodule {
                    superproject: superproject_of_submodule(rt, workdir.as_deref(), &common_dir),
                },
                _ => RepoKind::Main,
            }
        };
        let open_path = workdir.clone().unwrap_or_else(|| git_dir.clone());
        drop(local);
        Ok(Repo(Arc::new(RepoInner {
            rt: rt.clone(),
            repo: RwLock::new(repo),
            open_path,
            workdir,
            git_dir,
            common_dir,
            kind,
        })))
    }

    /// The work tree; `None` for a bare repository.
    pub fn workdir(&self) -> Option<&Path> {
        self.0.workdir.as_deref()
    }

    /// The per-worktree git directory.
    pub fn git_dir(&self) -> &Path {
        &self.0.git_dir
    }

    /// The git directory shared by all worktrees.
    pub fn common_dir(&self) -> &Path {
        &self.0.common_dir
    }

    /// What kind of repository this is.
    pub fn kind(&self) -> &RepoKind {
        &self.0.kind
    }

    /// The runtime this repository was opened with.
    pub fn runtime(&self) -> &Arc<GitRuntime> {
        &self.0.rt
    }

    /// Fresh thread-local gix repository (object cache 64 MiB). Cheap.
    pub fn local(&self) -> gix::Repository {
        let mut repo = self.0.repo.read().to_thread_local();
        repo.object_cache_size_if_unset(64 * 1024 * 1024);
        repo
    }

    /// Re-open from disk (config changes). Called automatically by our config-changing mutations.
    pub fn reload(&self) -> Result<()> {
        let fresh =
            gix::ThreadSafeRepository::open_opts(&self.0.open_path, self.0.rt.gix_open_options())
                .map_err(GitError::Gix)?;
        *self.0.repo.write() = fresh;
        Ok(())
    }

    /// The exclusive guard shared with the oplog (§1.5). Blocks until available. `!Send`.
    pub fn lock_exclusive(&self) -> RepoExclusiveGuard {
        but_core::sync::exclusive_repo_access(self.common_dir(), Some(&self.project_data_dir()))
    }

    /// Like `lock_exclusive`, but gives up after `timeout` (in-process and inter-process wait combined).
    pub fn try_lock_exclusive(&self, timeout: Duration) -> Option<RepoExclusiveGuard> {
        but_core::sync::try_exclusive_repo_access(
            self.common_dir(),
            Some(&self.project_data_dir()),
            timeout,
        )
    }

    /// A shared guard: many readers, no writer.
    pub fn lock_shared(&self) -> RepoSharedGuard {
        but_core::sync::shared_repo_access(self.common_dir())
    }

    /// The directory of the inter-process lock: `<common_dir>/gitcthulhu`.
    fn project_data_dir(&self) -> PathBuf {
        self.common_dir().join("gitcthulhu")
    }
}

/// Classify a failed discovery.
fn map_discover_error(err: gix::Error, searched: &Path) -> GitError {
    use gix::discover::upwards::Error as Upwards;
    match err.downcast_any_ref::<Upwards>() {
        Some(Upwards::NoTrustedGitRepository { candidate, .. }) => GitError::UnsafeRepository {
            path: candidate.clone(),
        },
        Some(_) => GitError::NotARepository {
            path: searched.to_owned(),
        },
        None => GitError::Gix(err),
    }
}

/// Reftable repositories open without error but show no refs: refuse them.
fn reject_unsupported_ref_storage(repo: &gix::Repository) -> Result<()> {
    let storage = repo.config_snapshot().string("extensions.refStorage");
    match storage {
        Some(value) if !value.to_string().eq_ignore_ascii_case("files") => {
            Err(GitError::Unsupported {
                what: "reftable ref storage (needs a newer gitoxide)",
            })
        }
        _ => Ok(()),
    }
}

/// A repository owned by someone else is only usable if `safe.directory` allows it, like Git itself does.
///
/// gix already applies Git's rules while opening: it reads `safe.directory` only from the system and global
/// configuration (never from the repository's own config, which its owner controls), interpolates
/// `~/` and `%(prefix)/`, honours `*` and `<dir>/*`, and raises the trust to `Full` when an entry matches. So any
/// trust below `Full` here means "owned by someone else and not allowed".
fn reject_untrusted(repo: &gix::Repository, searched: &Path, rt: &GitRuntime) -> Result<()> {
    if rt.config.test_isolation.is_some() || repo.git_dir_trust() == gix::sec::Trust::Full {
        return Ok(());
    }
    let own_path = repo.workdir().unwrap_or_else(|| repo.git_dir());
    Err(GitError::UnsafeRepository {
        path: if own_path.as_os_str().is_empty() {
            searched.to_owned()
        } else {
            absolute_normalized(own_path)
        },
    })
}

/// `p` made absolute (against the current directory) with `..` components resolved lexically.
fn absolute_normalized(p: &Path) -> PathBuf {
    let absolute = std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
    match gix::path::normalize(std::borrow::Cow::Borrowed(&absolute), &absolute) {
        Some(normalized) => normalized.into_owned(),
        None => absolute,
    }
}

/// The work tree of the repository that contains the submodule.
///
/// The superproject is the repository enclosing the submodule's work tree (for nested submodules that is the
/// intermediate submodule, not the top-level repository). It is accepted only if the submodule's git dir lives inside
/// its git dir (`<super-git-dir>/modules/...`). Falls back to `<super>/.git/modules/<name>` => `<super>`.
fn superproject_of_submodule(
    rt: &GitRuntime,
    workdir: Option<&Path>,
    common_dir: &Path,
) -> Option<PathBuf> {
    let discovered = workdir.and_then(Path::parent).and_then(|parent| {
        let superproject = gix::ThreadSafeRepository::discover_opts(
            parent,
            gix::discover::upwards::Options::default().apply_environment(),
            rt.gix_trust_mapping(),
        )
        .ok()?;
        let super_git_dir = absolute_normalized(superproject.git_dir());
        common_dir
            .starts_with(super_git_dir.join("modules"))
            .then(|| superproject.work_dir().map(absolute_normalized))
            .flatten()
    });
    discovered.or_else(|| {
        common_dir.ancestors().find_map(|ancestor| {
            if ancestor.file_name()? != "modules" {
                return None;
            }
            let git_dir = ancestor.parent()?;
            (git_dir.file_name()? == ".git").then(|| git_dir.parent().map(Path::to_owned))?
        })
    })
}
