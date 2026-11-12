//! Repository watcher (WP-B9): turns file system changes into "what must the GUI reload" ([`RefreshSet`]).
//!
//! `gitbutler-filemonitor` watches the worktree (ignored files dropped), the git dir and, for a linked worktree, the
//! common dir, and debounces the notifications. A mapping thread classifies the changed paths, unions the categories
//! of a batch and delivers one [`WatchEvent::Changed`]. While one of our own operations runs ([`RepoWatcher::suppress`])
//! batches are held back, never dropped, and delivered once after a grace period.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};

use gitbutler_filemonitor::{Checkout, FileMonitorHandle, InternalEvent, WatchMode};
use parking_lot::Mutex;

use crate::{FileStamp, GitError, RefreshSet, Repo, RepoKind, Result, WatchEvent};

/// How long after the last [`SuppressGuard`] is dropped events are still held back. Covers the debounce tail of our
/// own writes: three 250 ms ticks plus a margin.
const GRACE_PERIOD: Duration = Duration::from_millis(1000);

/// How often the mapping thread looks at its timers and control flags.
const POLL: Duration = Duration::from_millis(40);

/// Events of one debounced batch are sent back to back; after the first one we wait this long for stragglers so they
/// end up in the same [`WatchEvent`].
const BATCH_WINDOW: Duration = Duration::from_millis(30);

/// An upper bound for gathering one batch, so a steady stream cannot postpone the delivery forever.
const BATCH_MAX: Duration = Duration::from_millis(500);

/// How many changed paths a [`WatchEvent::Changed`] samples for diagnostics.
const SAMPLE_PATHS: usize = 8;

type Callback = Box<dyn Fn(WatchEvent) + Send + 'static>;

/// What the mapping thread and the handles share.
struct Shared {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The number of live [`SuppressGuard`]s.
    suppressed: usize,
    /// Changes accumulated while suppressed or in the grace period.
    held: RefreshSet,
    held_paths: Vec<PathBuf>,
    /// When the grace period after the last guard ends.
    grace_ends: Option<Instant>,
    /// Deliver `held` as soon as the thread notices.
    flush_requested: bool,
    /// The stamp of the index as our own `refresh_index` wrote it.
    ignored_index_stamp: Option<FileStamp>,
    /// The thread must end.
    stop: bool,
}

/// Watches a repository and reports what the GUI must reload.
pub struct RepoWatcher {
    shared: Arc<Shared>,
    monitor: Option<FileMonitorHandle>,
    thread: Option<JoinHandle<()>>,
}

impl RepoWatcher {
    /// Starts watching `repo` (worktree + git dir + common dir). `on_event` is called on the watcher thread.
    ///
    /// Returns after the watches are installed. If they cannot be (for example the operating system's limit of
    /// watches is exhausted), the watcher is inert and `on_event` receives a [`WatchEvent::Error`]; the GUI should
    /// then refresh on window focus instead. The watch strategy follows `GITCTHULHU_WATCH_MODE` (`auto`, `legacy` or
    /// `modern`).
    pub fn start(
        repo: &Repo,
        on_event: impl Fn(WatchEvent) + Send + 'static,
    ) -> Result<RepoWatcher> {
        let mode = WatchMode::from_env_or_settings("auto", |name| std::env::var(name).ok());
        Self::start_with_mode(repo, mode, on_event)
    }

    /// Like [`RepoWatcher::start()`] with an explicit watch strategy. For tests.
    #[doc(hidden)]
    pub fn start_with_mode(
        repo: &Repo,
        mode: WatchMode,
        on_event: impl Fn(WatchEvent) + Send + 'static,
    ) -> Result<RepoWatcher> {
        // A bare repository has no worktree; its git dir takes the role of one.
        let root = repo.workdir().unwrap_or_else(|| repo.git_dir()).to_owned();
        let common_dir = matches!(repo.kind(), RepoKind::LinkedWorktree { .. })
            .then(|| repo.common_dir().to_owned());
        let git_dir = repo.git_dir().to_owned();

        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
        });
        let (tx, rx) = mpsc::channel();
        let (monitor, startup_error) = match gitbutler_filemonitor::spawn(
            &root,
            common_dir.as_deref(),
            || Ok(Vec::new()),
            tx,
            mode,
        ) {
            Ok(monitor) => (Some(monitor), None),
            Err(err) => (None, Some(format!("{err:#}"))),
        };

        let worker = Worker {
            shared: shared.clone(),
            git_dir,
            rx,
            on_event: Box::new(on_event),
            startup_error,
        };
        let thread = std::thread::Builder::new()
            .name("gitcthulhu-watch".into())
            .spawn(move || worker.run())
            .map_err(|err| {
                GitError::Other(
                    anyhow::Error::new(err)
                        .context("Failed to start the repository watcher thread"),
                )
            })?;
        Ok(RepoWatcher {
            shared,
            monitor,
            thread: Some(thread),
        })
    }

    /// While the guard lives, events are accumulated instead of delivered; dropping it schedules delivery after a
    /// grace period.
    pub fn suppress(&self) -> SuppressGuard {
        let mut state = self.shared.state.lock();
        state.suppressed += 1;
        state.grace_ends = None;
        SuppressGuard {
            shared: self.shared.clone(),
        }
    }

    /// Deliver accumulated events now, and make the file monitor release what it still debounces.
    ///
    /// Delivery happens on the watcher thread, shortly after this call.
    pub fn flush(&self) {
        if let Some(monitor) = &self.monitor {
            monitor.flush().ok();
        }
        self.shared.state.lock().flush_requested = true;
    }

    /// The GUI's own `status::refresh_index` wrote the index with this stamp: an `index` change whose current stamp
    /// equals it contributes nothing to the batch (it is not a user change). Replaced by each call.
    pub fn ignore_index_stamp(&self, stamp: FileStamp) {
        self.shared.state.lock().ignored_index_stamp = Some(stamp);
    }
}

impl Drop for RepoWatcher {
    fn drop(&mut self) {
        self.shared.state.lock().stop = true;
        // Dropping the handle stops the monitor thread, which closes the channel.
        drop(self.monitor.take());
        if let Some(thread) = self.thread.take() {
            // Dropped from within `on_event`: the thread sees `stop` once the callback returns, and joining
            // itself would deadlock.
            if thread.thread().id() == std::thread::current().id() {
                return;
            }
            // The thread only panics if the callback does; there is nobody to tell.
            thread.join().ok();
        }
    }
}

/// Holds events back while alive.
pub struct SuppressGuard {
    shared: Arc<Shared>,
}

impl Drop for SuppressGuard {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock();
        state.suppressed = state.suppressed.saturating_sub(1);
        if state.suppressed == 0 {
            state.grace_ends = Some(Instant::now() + GRACE_PERIOD);
        }
    }
}

/// The mapping thread.
struct Worker {
    shared: Arc<Shared>,
    git_dir: PathBuf,
    rx: Receiver<InternalEvent>,
    on_event: Callback,
    startup_error: Option<String>,
}

/// What a batch of file system events asks the GUI to reload.
#[derive(Default)]
struct Batch {
    refresh: RefreshSet,
    paths: Vec<PathBuf>,
}

impl Batch {
    fn add(&mut self, refresh: RefreshSet, path: PathBuf) {
        if refresh.is_empty() {
            return;
        }
        self.refresh |= refresh;
        if self.paths.len() < SAMPLE_PATHS {
            self.paths.push(path);
        }
    }
}

impl Worker {
    fn run(self) {
        if let Some(message) = &self.startup_error {
            (self.on_event)(WatchEvent::Error {
                message: message.clone(),
            });
        }
        loop {
            if self.shared.state.lock().stop {
                return;
            }
            self.deliver_held_if_due();
            match self.rx.recv_timeout(POLL) {
                Ok(first) => {
                    let batch = self.gather_batch(first);
                    self.handle(batch);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // Without a monitor there is nothing to wait for but the timers and the end.
                    if self.shared.state.lock().stop {
                        return;
                    }
                    std::thread::sleep(POLL);
                }
            }
        }
    }

    /// The events of one debounced batch, mapped.
    fn gather_batch(&self, first: InternalEvent) -> Batch {
        let mut batch = Batch::default();
        self.map_event(first, &mut batch);
        let started = Instant::now();
        while started.elapsed() < BATCH_MAX {
            match self.rx.recv_timeout(BATCH_WINDOW) {
                Ok(event) => self.map_event(event, &mut batch),
                Err(_) => break,
            }
        }
        batch
    }

    fn map_event(&self, event: InternalEvent, batch: &mut Batch) {
        match event {
            InternalEvent::ProjectFilesChange(Checkout::Main, paths) => {
                for path in paths {
                    let refresh = if path == Path::new(".gitmodules") {
                        RefreshSet::STATUS | RefreshSet::SUBMODULES
                    } else {
                        RefreshSet::STATUS
                    };
                    batch.add(refresh, path);
                }
            }
            InternalEvent::GitFilesChange(Checkout::Main, paths) => {
                for path in paths {
                    let refresh = self.refresh_for_git_path(&path);
                    batch.add(refresh, path);
                }
            }
            InternalEvent::CommonGitFilesChange(paths) => {
                for path in paths {
                    let refresh = self.refresh_for_git_path(&path);
                    batch.add(refresh, path);
                }
            }
            // Other checkouts are not watched for this repository.
            InternalEvent::ProjectFilesChange(Checkout::Linked(_), _)
            | InternalEvent::GitFilesChange(Checkout::Linked(_), _) => {}
        }
    }

    fn refresh_for_git_path(&self, relative: &Path) -> RefreshSet {
        if relative == Path::new("index") {
            // Our own `refresh_index` rewrote the index: not a change the user made.
            let ignored = self.shared.state.lock().ignored_index_stamp;
            if ignored.is_some() && file_stamp(&self.git_dir.join("index")) == ignored {
                return RefreshSet::empty();
            }
        }
        refresh_for_git_path(relative)
    }

    /// Deliver a batch now, or hold it if one of our own operations is (or just was) running.
    fn handle(&self, batch: Batch) {
        if batch.refresh.is_empty() {
            return;
        }
        {
            let mut state = self.shared.state.lock();
            if state.suppressed > 0 || state.grace_ends.is_some() {
                state.held |= batch.refresh;
                for path in batch.paths {
                    if state.held_paths.len() < SAMPLE_PATHS {
                        state.held_paths.push(path);
                    }
                }
                return;
            }
        }
        (self.on_event)(WatchEvent::Changed {
            refresh: batch.refresh,
            during_own_operation: false,
            sample_paths: batch.paths,
        });
    }

    /// Deliver what was held back once the grace period is over, or when a flush was requested.
    fn deliver_held_if_due(&self) {
        let (refresh, paths) = {
            let mut state = self.shared.state.lock();
            let grace_over = state.suppressed == 0
                && state
                    .grace_ends
                    .is_some_and(|deadline| deadline <= Instant::now());
            if !grace_over && !state.flush_requested {
                return;
            }
            state.flush_requested = false;
            if grace_over {
                state.grace_ends = None;
            }
            (
                std::mem::take(&mut state.held),
                std::mem::take(&mut state.held_paths),
            )
        };
        if refresh.is_empty() {
            return;
        }
        (self.on_event)(WatchEvent::Changed {
            refresh,
            during_own_operation: true,
            sample_paths: paths,
        });
    }
}

/// What a change of `relative`, a path relative to a git dir (or the common dir), asks the GUI to reload.
fn refresh_for_git_path(relative: &Path) -> RefreshSet {
    let mut components = relative.components().map(|c| c.as_os_str());
    let Some(first) = components.next() else {
        return RefreshSet::empty();
    };
    let rest: Vec<_> = components.collect();
    let first = first.to_string_lossy();
    let second = rest.first().map(|name| name.to_string_lossy());
    match (&*first, rest.len()) {
        ("index", 0) => RefreshSet::STATUS,
        ("HEAD" | "ORIG_HEAD", 0) => {
            RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::REPO_STATE
        }
        ("logs", _) => match (second.as_deref(), rest.len()) {
            (Some("HEAD"), 1) => {
                RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::REPO_STATE
            }
            (Some("refs"), 2) if rest[1] == "stash" => RefreshSet::STASHES | RefreshSet::LOG,
            _ => RefreshSet::empty(),
        },
        ("refs", _) => match second.as_deref() {
            Some("heads") => RefreshSet::BRANCHES | RefreshSet::LOG | RefreshSet::WORKTREES,
            Some("remotes") => RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::LOG,
            Some("tags") => RefreshSet::TAGS | RefreshSet::LOG,
            Some("stash") => RefreshSet::STASHES | RefreshSet::LOG,
            Some("gitcthulhu") => RefreshSet::UNDO,
            Some(_) => RefreshSet::LOG,
            None => RefreshSet::empty(),
        },
        ("FETCH_HEAD", 0) => RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::LOG,
        ("packed-refs", 0) => {
            RefreshSet::BRANCHES | RefreshSet::REMOTES | RefreshSet::TAGS | RefreshSet::LOG
        }
        ("config", 0) => {
            RefreshSet::CONFIG | RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::SUBMODULES
        }
        ("MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "REBASE_HEAD" | "BISECT_LOG", 0)
        | ("rebase-merge" | "rebase-apply" | "sequencer", _) => {
            RefreshSet::REPO_STATE | RefreshSet::STATUS
        }
        ("modules", _) => RefreshSet::SUBMODULES,
        ("worktrees", _) => RefreshSet::WORKTREES | RefreshSet::BRANCHES,
        ("gitdir", 0) => RefreshSet::WORKTREES,
        _ => RefreshSet::empty(),
    }
}

/// The stamp of a file: modification time in nanoseconds since the Unix epoch, and size. `None` if it cannot be read.
pub(crate) fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let mtime_ns = match modified.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => after.as_nanos() as i128,
        Err(before) => -(before.duration().as_nanos() as i128),
    };
    Some(FileStamp {
        mtime_ns,
        size: metadata.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(path: &str) -> RefreshSet {
        refresh_for_git_path(Path::new(path))
    }

    #[test]
    fn git_paths_map_to_the_categories_of_the_spec() {
        use RefreshSet as R;
        let cases: &[(&str, RefreshSet)] = &[
            ("index", R::STATUS),
            ("HEAD", R::STATUS | R::LOG | R::BRANCHES | R::REPO_STATE),
            (
                "logs/HEAD",
                R::STATUS | R::LOG | R::BRANCHES | R::REPO_STATE,
            ),
            (
                "ORIG_HEAD",
                R::STATUS | R::LOG | R::BRANCHES | R::REPO_STATE,
            ),
            ("refs/heads/feature/x", R::BRANCHES | R::LOG | R::WORKTREES),
            (
                "refs/remotes/origin/main",
                R::REMOTES | R::BRANCHES | R::LOG,
            ),
            ("FETCH_HEAD", R::REMOTES | R::BRANCHES | R::LOG),
            ("refs/tags/v1", R::TAGS | R::LOG),
            ("refs/stash", R::STASHES | R::LOG),
            ("logs/refs/stash", R::STASHES | R::LOG),
            ("refs/gitcthulhu/undo/1", R::UNDO),
            ("packed-refs", R::BRANCHES | R::REMOTES | R::TAGS | R::LOG),
            (
                "config",
                R::CONFIG | R::REMOTES | R::BRANCHES | R::SUBMODULES,
            ),
            ("MERGE_HEAD", R::REPO_STATE | R::STATUS),
            ("CHERRY_PICK_HEAD", R::REPO_STATE | R::STATUS),
            ("REVERT_HEAD", R::REPO_STATE | R::STATUS),
            ("REBASE_HEAD", R::REPO_STATE | R::STATUS),
            ("BISECT_LOG", R::REPO_STATE | R::STATUS),
            ("rebase-merge/git-rebase-todo", R::REPO_STATE | R::STATUS),
            ("rebase-apply", R::REPO_STATE | R::STATUS),
            ("sequencer/todo", R::REPO_STATE | R::STATUS),
            ("modules/sub/HEAD", R::SUBMODULES),
            ("worktrees/linked", R::WORKTREES | R::BRANCHES),
            ("gitdir", R::WORKTREES),
        ];
        for (path, expected) in cases {
            assert_eq!(map(path), *expected, "mapping of {path}");
        }
    }

    #[test]
    fn unknown_git_paths_map_to_nothing() {
        for path in [
            "COMMIT_EDITMSG",
            "objects/ab/cd",
            "logs/refs/heads/main",
            "",
        ] {
            assert!(map(path).is_empty(), "{path} is not interesting");
        }
    }
}
