//! Process-tree termination (Job Object / process group) and stale lock cleanup (§1.6).
//!
//! - *Windows*: right after spawn the child is assigned to a fresh Job Object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Cancel calls `TerminateJobObject`; closing the job when the command ends
//!   kills stragglers. Children inherit the job, so ssh, git-remote-https, credential helpers, git-lfs and hooks are
//!   covered. The race window between spawn and assignment is accepted.
//! - *Unix*: the child is spawned with `process_group(0)`. Cancel sends `killpg(SIGTERM)`, then `SIGKILL` after 3 s.
//!
//! After a cancelled command, lock files it may have left are deleted, but only if they are provably its own: they
//! did not exist before the command was spawned, they were created since, they are unchanged since right before
//! the kill (same file id, mtime and size) and no other process holds them open, see [`remove_stale_locks`].
#![allow(unsafe_code)] // Job Object FFI (Windows) and file ids (Windows); see the crate docs.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// How long to wait for a process tree to disappear after it was told to terminate.
const TREE_GONE_TIMEOUT: Duration = Duration::from_secs(5);
/// Unix: time between `SIGTERM` and `SIGKILL`.
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(3);
/// Lock files Git may leave behind when it is killed, relative to the per-worktree and the common git dir.
const LOCK_FILES: &[&str] = &[
    "index.lock",
    "HEAD.lock",
    "ORIG_HEAD.lock",
    "packed-refs.lock",
    "config.lock",
    "shallow.lock",
    // `git commit-graph write` (the background write of large repositories can be cancelled by closing the tab):
    // a leftover lock makes every later commit-graph write, `git gc` and `git maintenance` fail.
    "objects/info/commit-graph.lock",
    "objects/info/commit-graphs/commit-graph-chain.lock",
];
/// A lock counts as created by the command if its mtime is at most this much older than the command's start.
/// Covers file systems with coarse timestamps (FAT: 2 s) and kernels whose cached clock lags behind the wall clock.
/// A lock that already existed when the command was spawned is excluded by [`snapshot_locks`], not by the time.
const MTIME_SLACK: Duration = Duration::from_secs(2);

/// The process tree of one spawned git child.
pub(crate) struct ProcessTree {
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl ProcessTree {
    /// Put the just-spawned `child` (and so all its descendants) under our control.
    pub(crate) fn attach(child: &tokio::process::Child) -> ProcessTree {
        #[cfg(windows)]
        {
            let job = child
                .raw_handle()
                .and_then(|handle| match windows_job::assign_new_job(handle) {
                    Ok(job) => Some(job),
                    Err(err) => {
                        tracing::warn!(
                            %err,
                            "Could not put git into a job object; cancelling will only stop git itself"
                        );
                        None
                    }
                });
            ProcessTree { job }
        }
        #[cfg(unix)]
        {
            // `process_group(0)` made the child the leader of a group whose id is its pid.
            ProcessTree {
                pgid: child.id().and_then(|pid| i32::try_from(pid).ok()),
            }
        }
    }

    /// Terminate the whole tree and wait until it is gone (bounded by internal timeouts).
    pub(crate) async fn terminate(&mut self, child: &mut tokio::process::Child) {
        #[cfg(windows)]
        {
            match &self.job {
                Some(job) => {
                    if let Err(err) = windows_job::terminate(job) {
                        tracing::warn!(%err, "TerminateJobObject failed; killing git only");
                        child.start_kill().ok();
                    }
                }
                None => {
                    child.start_kill().ok();
                }
            }
            wait_with_timeout(child, TREE_GONE_TIMEOUT).await;
            if let Some(job) = &self.job {
                let deadline = tokio::time::Instant::now() + TREE_GONE_TIMEOUT;
                while windows_job::active_processes(job).is_some_and(|n| n > 0)
                    && tokio::time::Instant::now() < deadline
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            let Some(pgid) = self.pgid else {
                child.start_kill().ok();
                wait_with_timeout(child, TREE_GONE_TIMEOUT).await;
                return;
            };
            let group = Pid::from_raw(pgid);
            killpg(group, Signal::SIGTERM).ok();
            if tokio::time::timeout(TERM_GRACE, child.wait())
                .await
                .is_err()
            {
                killpg(group, Signal::SIGKILL).ok();
                wait_with_timeout(child, TREE_GONE_TIMEOUT).await;
            }
            // Wait for the rest of the group (hooks, ssh, ...) to go away; escalate if it lingers.
            let group_alive = || killpg(group, None::<Signal>).is_ok();
            let deadline = tokio::time::Instant::now() + TERM_GRACE;
            while group_alive() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if group_alive() {
                killpg(group, Signal::SIGKILL).ok();
                let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
                while group_alive() && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    }

    /// The command ended: release the tree. On Windows this closes the job, which kills stragglers.
    pub(crate) fn release(&mut self) {
        #[cfg(windows)]
        {
            self.job = None;
        }
        #[cfg(unix)]
        {
            self.pgid = None;
        }
    }
}

async fn wait_with_timeout(child: &mut tokio::process::Child, timeout: Duration) {
    if tokio::time::timeout(timeout, child.wait()).await.is_err() {
        tracing::warn!("A killed git process did not exit within {timeout:?}");
    }
}

/// Identity of a file system object: Windows volume serial + 128-bit file id, Unix `(dev, ino)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    volume: u64,
    file: u128,
}

/// What a lock file looked like right before the kill.
#[derive(Debug, Clone)]
pub(crate) struct LockObservation {
    path: PathBuf,
    identity: Identity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    id: FileId,
    mtime: SystemTime,
    size: u64,
}

/// A lock file that existed before the command was spawned: it belongs to somebody else.
#[derive(Debug, Clone)]
pub(crate) struct PreexistingLock {
    path: PathBuf,
    id: FileId,
}

/// The lock files that exist in `dirs` right now. Call right before spawning the command.
pub(crate) fn snapshot_locks(dirs: &[PathBuf]) -> Vec<PreexistingLock> {
    candidates(dirs)
        .filter_map(|path| {
            let identity = identity(&path)?;
            Some(PreexistingLock {
                path,
                id: identity.id,
            })
        })
        .collect()
}

/// Every candidate lock file path in `dirs` (a directory listed twice is visited once).
fn candidates(dirs: &[PathBuf]) -> impl Iterator<Item = PathBuf> + '_ {
    let mut seen_dirs: Vec<&Path> = Vec::new();
    dirs.iter()
        .filter(move |dir| {
            if seen_dirs.contains(&dir.as_path()) {
                false
            } else {
                seen_dirs.push(dir);
                true
            }
        })
        .flat_map(|dir| LOCK_FILES.iter().map(move |name| dir.join(name)))
}

/// The lock files that exist in `dirs`, were created since `started` (minus [`MTIME_SLACK`]) and are not in `before`
/// (the locks that existed when the command was spawned). Call right before terminating the process tree.
pub(crate) fn observe_locks(
    dirs: &[PathBuf],
    started: SystemTime,
    before: &[PreexistingLock],
) -> Vec<LockObservation> {
    candidates(dirs)
        .filter_map(|path| {
            let identity = identity(&path)?;
            let existed_before = before
                .iter()
                .any(|lock| lock.path == path && lock.id == identity.id);
            (!existed_before && identity.mtime + MTIME_SLACK >= started)
                .then_some(LockObservation { path, identity })
        })
        .collect()
}

/// Delete every observed lock file that still exists with the **same** file id, mtime and size and that no other
/// process holds open; a lock that a terminal `git` created in the meantime, or that it is still working with, is
/// never touched. Returns the deleted paths. Call after the process tree is gone, so that only foreign processes
/// can still hold a lock.
pub(crate) fn remove_stale_locks(observed: &[LockObservation]) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for lock in observed {
        match identity(&lock.path) {
            Some(now) if now == lock.identity => {
                if held_open_by_another_process(&lock.path) {
                    tracing::info!(path = %lock.path.display(), "Lock file is in use by another process; keeping it");
                    continue;
                }
                match std::fs::remove_file(&lock.path) {
                    Ok(()) => {
                        tracing::info!(path = %lock.path.display(), "Removed a lock file left by a cancelled git command");
                        removed.push(lock.path.clone());
                    }
                    Err(err) => {
                        tracing::warn!(path = %lock.path.display(), %err, "Could not remove a stale lock file");
                    }
                }
            }
            Some(_) => {
                tracing::info!(path = %lock.path.display(), "Lock file was replaced after the kill; keeping it");
            }
            None => {}
        }
    }
    removed
}

/// Whether some process has `path` open. A git that is still working holds its lock file open until it renames or
/// deletes it, while the lock of a killed git is orphaned. Where this cannot be determined (macOS and other Unixes
/// without `/proc`) the answer is `false`; the other conditions of [`remove_stale_locks`] still apply there.
fn held_open_by_another_process(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        // Asking for no sharing at all fails while any other handle is open, whatever its own share mode.
        match std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
        {
            Ok(_) => false,
            Err(err) => matches!(
                err.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
            ),
        }
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(target) = std::fs::canonicalize(path) else {
            return false;
        };
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return false;
        };
        procs
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .bytes()
                    .all(|b| b.is_ascii_digit())
            })
            .any(|entry| {
                let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
                    return false;
                };
                fds.filter_map(Result::ok)
                    .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|link| link == target))
            })
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        false
    }
}

/// The git directories of the repository whose work tree (or bare git dir) is `cwd`, for commands run without a
/// [`crate::Repo`]: `<cwd>/.git` (directory or `gitdir:` file) or `cwd` itself if it is a git dir, plus the common
/// dir named by `commondir`. Empty if `cwd` is not a repository root.
pub(crate) fn git_dirs_of_cwd(cwd: &Path) -> Vec<PathBuf> {
    let dot_git = cwd.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if dot_git.is_file() {
        let Ok(content) = std::fs::read_to_string(&dot_git) else {
            return Vec::new();
        };
        let Some(target) = content.trim().strip_prefix("gitdir:") else {
            return Vec::new();
        };
        cwd.join(target.trim())
    } else if cwd.join("HEAD").is_file() && cwd.join("objects").is_dir() {
        cwd.to_owned()
    } else {
        return Vec::new();
    };
    let mut dirs = vec![git_dir.clone()];
    if let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) {
        let common = git_dir.join(common.trim());
        dirs.push(std::fs::canonicalize(&common).unwrap_or(common));
    }
    dirs
}

/// The identity of the file at `path`, `None` if it does not exist or cannot be inspected.
fn identity(path: &Path) -> Option<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        Some(Identity {
            id: FileId {
                volume: meta.dev(),
                file: u128::from(meta.ino()),
            },
            mtime: meta.modified().ok()?,
            size: meta.len(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        // Attribute access only: never conflicts with the share mode of the git process that may still hold the
        // lock open, and never blocks its rename or delete.
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() {
            return None;
        }
        Some(Identity {
            id: windows_job::file_id(&file)?,
            mtime: meta.modified().ok()?,
            size: meta.len(),
        })
    }
}

#[cfg(windows)]
mod windows_job {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};

    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
    };

    use super::FileId;

    /// Create a kill-on-close job and assign `process` to it.
    pub(super) fn assign_new_job(process: RawHandle) -> windows::core::Result<OwnedHandle> {
        // SAFETY: plain FFI; the returned handle is owned by us and wrapped immediately.
        let job = unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null())? };
        // SAFETY: `job` is a valid, freshly created handle that nobody else owns.
        let job = unsafe { OwnedHandle::from_raw_handle(job.0) };
        let handle = HANDLE(job.as_raw_handle());
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a properly initialised struct of the size passed.
        unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
        }
        // SAFETY: both handles are valid for the duration of the call; `process` is owned by the tokio child.
        unsafe { AssignProcessToJobObject(handle, HANDLE(process))? };
        Ok(job)
    }

    /// Terminate every process in the job with exit code 1.
    pub(super) fn terminate(job: &OwnedHandle) -> windows::core::Result<()> {
        // SAFETY: `job` is a valid job handle.
        unsafe { TerminateJobObject(HANDLE(job.as_raw_handle()), 1) }
    }

    /// How many processes are still alive in the job.
    pub(super) fn active_processes(job: &OwnedHandle) -> Option<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `info` is a properly sized out buffer.
        unsafe {
            QueryInformationJobObject(
                Some(HANDLE(job.as_raw_handle())),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut info).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )
        }
        .ok()?;
        Some(info.ActiveProcesses)
    }

    /// Volume serial number and 128-bit file id of an open file.
    pub(super) fn file_id(file: &std::fs::File) -> Option<FileId> {
        let mut info = FILE_ID_INFO::default();
        // SAFETY: `file` is an open handle; `info` is a properly sized out buffer.
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileIdInfo,
                std::ptr::from_mut(&mut info).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        }
        .ok()?;
        Some(FileId {
            volume: info.VolumeSerialNumber,
            file: u128::from_le_bytes(info.FileId.Identifier),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_lock_is_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let started = SystemTime::now();
        let lock = dir.path().join("index.lock");
        std::fs::write(&lock, b"partial index").expect("lock can be written");
        let observed = observe_locks(
            &[dir.path().to_owned(), dir.path().to_owned()],
            started,
            &[],
        );
        assert_eq!(
            observed.len(),
            1,
            "the lock is observed once although the dir is listed twice"
        );

        assert_eq!(
            remove_stale_locks(&observed),
            std::slice::from_ref(&lock),
            "the untouched lock is deleted"
        );
        assert!(!lock.exists(), "and gone from disk");
    }

    #[test]
    fn commit_graph_lock_of_a_killed_write_is_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let info = dir.path().join("objects").join("info");
        std::fs::create_dir_all(&info).expect("objects/info");
        let started = SystemTime::now();
        let lock = info.join("commit-graph.lock");
        std::fs::write(&lock, b"partial graph").expect("lock can be written");
        let observed = observe_locks(&[dir.path().to_owned()], started, &[]);
        assert_eq!(observed.len(), 1, "the commit-graph lock is observed");
        assert_eq!(remove_stale_locks(&observed).len(), 1);
        assert!(!lock.exists(), "a killed commit-graph write leaves no lock");
    }

    #[test]
    fn replaced_lock_is_kept() {
        let dir = tempfile::tempdir().expect("temp dir");
        let started = SystemTime::now();
        let lock = dir.path().join("HEAD.lock");
        std::fs::write(&lock, b"old").expect("lock can be written");
        let observed = observe_locks(&[dir.path().to_owned()], started, &[]);
        assert_eq!(observed.len(), 1, "the lock is observed");

        // Keep the old file alive under another name so its file id cannot be reused by the new one.
        std::fs::rename(&lock, dir.path().join("moved")).expect("lock can be moved away");
        std::fs::write(&lock, b"a different git").expect("a new lock can be written");
        assert!(
            remove_stale_locks(&observed).is_empty(),
            "a lock re-created by another git is never touched"
        );
        assert!(lock.exists(), "the foreign lock survives");
    }

    #[test]
    fn old_locks_are_not_observed() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("index.lock"), b"x").expect("lock can be written");
        let much_later = SystemTime::now() + Duration::from_secs(3600);
        assert!(
            observe_locks(&[dir.path().to_owned()], much_later, &[]).is_empty(),
            "a lock older than the command belongs to someone else"
        );
    }

    #[test]
    fn locks_that_existed_before_the_spawn_are_not_observed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let dirs = [dir.path().to_owned()];
        let foreign = dir.path().join("index.lock");
        std::fs::write(&foreign, b"a terminal git's index").expect("lock can be written");
        let before = snapshot_locks(&dirs);
        assert_eq!(before.len(), 1, "the pre-existing lock is recorded");
        std::fs::write(dir.path().join("HEAD.lock"), b"ours").expect("second lock");

        let observed = observe_locks(&dirs, SystemTime::now() - Duration::from_secs(60), &before);
        assert_eq!(
            observed.len(),
            1,
            "only the lock created after the snapshot is observed, although both are recent"
        );
        assert!(
            remove_stale_locks(&observed)
                .iter()
                .all(|path| path.ends_with("HEAD.lock")),
            "the pre-existing lock is never deleted"
        );
        assert!(foreign.exists(), "the foreign lock survives");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn lock_held_open_by_another_process_is_kept() {
        let dir = tempfile::tempdir().expect("temp dir");
        let lock = dir.path().join("index.lock");
        std::fs::write(&lock, b"being written").expect("lock can be written");
        let observed = observe_locks(&[dir.path().to_owned()], SystemTime::now(), &[]);
        assert_eq!(observed.len(), 1, "the lock is observed");

        let holder = std::fs::File::open(&lock).expect("the lock can be held open");
        assert!(
            remove_stale_locks(&observed).is_empty(),
            "a lock somebody still has open is kept"
        );
        assert!(lock.exists(), "the held lock survives");
        drop(holder);
        assert_eq!(
            remove_stale_locks(&observed),
            std::slice::from_ref(&lock),
            "once nobody holds it, it is an orphan and goes"
        );
    }

    #[test]
    fn git_dirs_of_main_and_linked_worktrees() {
        let dir = tempfile::tempdir().expect("temp dir");
        let main = dir.path().join("main");
        std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt")).expect("dirs");
        assert_eq!(
            git_dirs_of_cwd(&main),
            [main.join(".git")],
            "a main worktree has one git dir"
        );

        let linked = dir.path().join("linked");
        std::fs::create_dir_all(&linked).expect("linked dir");
        let wt_git_dir = main.join(".git").join("worktrees").join("wt");
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", wt_git_dir.display()),
        )
        .expect(".git file");
        std::fs::write(wt_git_dir.join("commondir"), "../..\n").expect("commondir");
        let dirs = git_dirs_of_cwd(&linked);
        assert_eq!(dirs.len(), 2, "per-worktree and common dir: {dirs:?}");
        assert_eq!(dirs[0], wt_git_dir, "the per-worktree dir comes first");
        assert_eq!(
            std::fs::canonicalize(&dirs[1]).expect("exists"),
            std::fs::canonicalize(main.join(".git")).expect("exists"),
            "the common dir is resolved through `commondir`"
        );

        assert!(
            git_dirs_of_cwd(dir.path()).is_empty(),
            "not a repository root"
        );
    }
}
