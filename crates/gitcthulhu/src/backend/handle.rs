//! The open repository of a tab: `Repo` + optional `Oplog` + watcher (spec 21 §2.4).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::SharedString;
use parking_lot::{Mutex, RwLock};

use super::{
    CancelToken, FileStamp, GitError, GitRunner, GitRuntime, Oplog, OplogError, OplogOptions, Repo,
    RepoExclusiveGuard, RepoWatcher, RetentionPolicy, SuppressGuard, WatchEvent,
};
use crate::settings::Settings;

/// An open repository: everything the background closures of a tab need. `Send + Sync`; shared as
/// `Arc<RepoHandle>`.
pub struct RepoHandle {
    /// The repository.
    pub repo: Repo,
    /// `RwLock` only for `set_options` (`&mut`); every other call takes `read()`.
    pub(super) oplog: Option<RwLock<Oplog>>,
    /// Why the oplog is unavailable (unsupported repository or open error); `None` when it is available.
    pub oplog_unavailable: Option<SharedString>,
    watcher: Mutex<Option<RepoWatcher>>,
}

/// The backend of a tab.
#[derive(Clone)]
pub enum TabBackend {
    /// A real repository.
    Real(Arc<RepoHandle>),
    /// Tests: operations and reloads are recorded instead of executed; scripted outcomes complete operations
    /// (`test_support::script_op`). Every read wrapper is unreachable through this variant.
    #[cfg(test)]
    Inert(Arc<crate::test_support::InertBackend>),
}

impl TabBackend {
    /// The repository handle, `None` for the inert test backend.
    pub fn handle(&self) -> Option<&Arc<RepoHandle>> {
        match self {
            TabBackend::Real(handle) => Some(handle),
            #[cfg(test)]
            TabBackend::Inert(_) => None,
        }
    }

    /// The repository, `None` for the inert test backend.
    pub fn repo(&self) -> Option<&Repo> {
        self.handle().map(|h| &h.repo)
    }
}

/// The undo settings of [`Settings`] as the oplog needs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndoSettings {
    /// `OplogOptions::enabled` ("Record undo history").
    pub enabled: bool,
    /// `RetentionPolicy::max_entries`.
    pub keep_entries: usize,
    /// `RetentionPolicy::max_age` in days.
    pub keep_days: u32,
    /// `OplogOptions::capture_untracked`.
    pub capture_untracked: bool,
}

impl UndoSettings {
    /// The undo keys of `settings`.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.record_undo_history,
            keep_entries: settings.undo_keep_entries,
            keep_days: settings.undo_keep_days,
            capture_untracked: settings.undo_capture_untracked,
        }
    }
}

impl Default for UndoSettings {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

pub(super) fn oplog_options_for(repo: &Repo, undo: &UndoSettings) -> OplogOptions {
    OplogOptions {
        enabled: undo.enabled,
        retention: RetentionPolicy {
            max_entries: undo.keep_entries,
            max_age: Duration::from_secs(u64::from(undo.keep_days) * 86_400),
        },
        capture_untracked: undo.capture_untracked,
        git: Arc::new(CliGitRunner { repo: repo.clone() }),
        app_version: env!("CARGO_PKG_VERSION").into(),
        ..OplogOptions::default()
    }
}

/// Opens the repository at (or above) `path` and its oplog. Blocking. An oplog that cannot be opened leaves the
/// repository usable: `oplog_unavailable` holds the reason.
///
/// Errors (from `Repo::open`): `NotFound`, `NotARepository`, `UnsafeRepository`, `Unsupported`, `Io`.
pub fn open_repo(
    rt: &Arc<GitRuntime>,
    path: &Path,
    undo: &UndoSettings,
) -> Result<RepoHandle, GitError> {
    let repo = Repo::open(rt, path)?;
    Ok(handle_for(repo, undo))
}

/// `git init` at `path`, then [`open_repo`].
pub fn init_repo(
    rt: &Arc<GitRuntime>,
    path: &Path,
    undo: &UndoSettings,
) -> Result<RepoHandle, GitError> {
    let repo = Repo::init(rt, path, None)?;
    Ok(handle_for(repo, undo))
}

/// Wraps an already opened repository (clone, init) with its oplog.
pub fn handle_for(repo: Repo, undo: &UndoSettings) -> RepoHandle {
    let (oplog, oplog_unavailable) =
        match Oplog::open(&repo.local(), oplog_options_for(&repo, undo)) {
            Ok(oplog) => (Some(RwLock::new(oplog)), None),
            Err(err) => {
                tracing::info!("undo history unavailable for {:?}: {err}", repo.workdir());
                (None, Some(SharedString::from(oplog_unavailable_text(&err))))
            }
        };
    RepoHandle {
        repo,
        oplog,
        oplog_unavailable,
        watcher: Mutex::new(None),
    }
}

fn oplog_unavailable_text(err: &OplogError) -> String {
    match err {
        OplogError::Unsupported(reason) => reason.to_string(),
        other => other.to_string(),
    }
}

/// `git config --global --add safe.directory <path>` (UnsafeRepository recovery, spec 21 §2.9). Blocking.
pub fn add_safe_directory(rt: &GitRuntime, path: &Path) -> Result<(), GitError> {
    gitcthulhu_git::config::add_safe_directory(rt, path)
}

/// The oplog's `GitRunner` (17 §4.1) over `gitcthulhu_git::cli::run_simple`, so the user's Git path and environment
/// rules apply to the oplog's `git` calls too. `workdir` is the oplog's view of the same worktree and otherwise
/// ignored (the command runs in the repository's own worktree).
struct CliGitRunner {
    repo: Repo,
}

impl GitRunner for CliGitRunner {
    fn run(&self, workdir: &Path, args: &[&str]) -> Result<Vec<u8>, OplogError> {
        if let Some(own) = self.repo.workdir()
            && !same_dir(own, workdir)
        {
            tracing::warn!(
                "oplog git runner called for {} but the repository's worktree is {}",
                workdir.display(),
                own.display()
            );
        }
        gitcthulhu_git::cli::run_simple(&self.repo, args, &CancelToken::new()).map_err(|e| {
            OplogError::GitCommand {
                args: args.join(" "),
                stderr: e
                    .stderr()
                    .map(str::to_owned)
                    .unwrap_or_else(|| e.to_string()),
            }
        })
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

impl RepoHandle {
    /// Starts the repository watcher; `on_event` runs on the watcher thread (send into a channel, nothing else).
    /// A second call replaces the running watcher.
    pub fn start_watcher(
        &self,
        on_event: impl Fn(WatchEvent) + Send + 'static,
    ) -> Result<(), GitError> {
        let watcher = RepoWatcher::start(&self.repo, on_event)?;
        *self.watcher.lock() = Some(watcher);
        Ok(())
    }

    /// Stops the watcher (no-op when none runs).
    pub fn stop_watcher(&self) {
        // Drop outside the lock: dropping joins the watcher thread.
        let watcher = self.watcher.lock().take();
        drop(watcher);
    }

    /// Holds watcher events back while an own operation runs (they are delivered when the guard drops).
    pub fn suppress_watcher(&self) -> Option<SuppressGuard> {
        self.watcher.lock().as_ref().map(RepoWatcher::suppress)
    }

    /// Tells the watcher that the index stamp after our own `refresh_index` is not a change (20 §7.3).
    pub fn ignore_index_stamp(&self, stamp: FileStamp) {
        if let Some(watcher) = self.watcher.lock().as_ref() {
            watcher.ignore_index_stamp(stamp);
        }
    }

    /// `Repo::try_lock_exclusive(timeout)`: the repository's exclusive guard (`!Send`: take and drop it inside one
    /// background closure).
    pub fn try_lock(&self, timeout: Duration) -> Option<RepoExclusiveGuard> {
        self.repo.try_lock_exclusive(timeout)
    }

    /// The tab name: the worktree's last path segment, or the git dir's with a `.git` suffix stripped (07 §2.2).
    pub fn display_name(&self) -> SharedString {
        let dir = self.repo.workdir().unwrap_or_else(|| self.repo.git_dir());
        SharedString::from(display_name_of(dir))
    }

    /// The worktree directory; `None` for a bare repository.
    pub fn workdir(&self) -> Option<PathBuf> {
        self.repo.workdir().map(Path::to_path_buf)
    }

    /// Whether this repository has an oplog.
    pub fn has_oplog(&self) -> bool {
        self.oplog.is_some()
    }
}

/// The tab name of a repository directory (07 §2.2): its last segment, `.git` stripped.
pub fn display_name_of(dir: &Path) -> String {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    match name.strip_suffix(".git") {
        Some(stripped) if !stripped.is_empty() => stripped.to_owned(),
        _ => name,
    }
}

impl Drop for RepoHandle {
    fn drop(&mut self) {
        self.stop_watcher();
    }
}
