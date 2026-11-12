//! The error model of the oplog (spec 17 §4.2, §3.7).

use std::path::{Path, PathBuf};

/// Errors of the oplog API.
#[derive(Debug, thiserror::Error)]
pub enum OplogError {
    /// The repository is of a kind the oplog cannot snapshot and restore.
    #[error("unsupported repository: {0}")]
    Unsupported(UnsupportedReason),
    /// The `gix::Repository` passed in belongs to another worktree than the [`Oplog`](crate::Oplog) handle.
    #[error("the repository handle belongs to a different worktree than this oplog")]
    WrongRepository,
    /// An interrupted undo/redo must be recovered first (`Oplog::recover`).
    #[error("an interrupted undo/redo must be finished or rolled back first")]
    InterruptedRestore,
    /// Capturing the repository state failed.
    #[error("could not record the repository state: {0}")]
    Capture(#[source] anyhow::Error),
    /// A file is held open by another program (Windows sharing or lock violation, access denied).
    #[error("'{}' is in use by another program (OS error {code}); close it and try again",
            path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "a file".into()))]
    FileLocked {
        /// The file, if known.
        path: Option<PathBuf>,
        /// The raw OS error code (32, 33 or 5 on Windows).
        code: i32,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A Git lock file exists (another Git process is running, or one was killed).
    #[error("another Git process is running in this repository ('{}' exists)", path.display())]
    RepositoryLocked {
        /// The lock file.
        path: PathBuf,
    },
    /// The oplog or a guarded ref changed concurrently; nothing was changed.
    #[error("the repository changed while undo was running; nothing was changed")]
    ConcurrentModification,
    /// An entry of the log cannot be read.
    #[error("oplog entry {commit} is corrupt: {reason}")]
    Corrupt {
        /// The entry commit.
        commit: gix::ObjectId,
        /// What is wrong with it.
        reason: String,
    },
    /// The git executable failed.
    #[error("git {args} failed: {stderr}")]
    GitCommand {
        /// The arguments, space-separated.
        args: String,
        /// Git's error output.
        stderr: String,
    },
    /// An I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Any other error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Why [`OplogError::Unsupported`] was returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// The repository has no worktree.
    BareRepository,
    /// `extensions.refStorage = reftable` (gix has no reftable backend).
    Reftable,
    /// `core.sparseCheckout = true` (a sparse worktree does not round-trip in v1).
    SparseCheckout,
}

impl std::fmt::Display for UnsupportedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            UnsupportedReason::BareRepository => "bare repository",
            UnsupportedReason::Reftable => "reftable ref storage",
            UnsupportedReason::SparseCheckout => "sparse checkout",
        })
    }
}

/// Windows error codes that mean "another program holds the file": `ERROR_ACCESS_DENIED`,
/// `ERROR_SHARING_VIOLATION`, `ERROR_LOCK_VIOLATION`.
pub(crate) const LOCKED_FILE_OS_ERRORS: [i32; 3] = [5, 32, 33];

/// Classify `err` (from a capture or a restore step in `repo`) per §3.7:
///
/// - a leftover Git lock file (`index.lock`, `HEAD.lock`) in the worktree's git dir → `RepositoryLocked`;
/// - a `std::io::Error` in the chain with one of [`LOCKED_FILE_OS_ERRORS`] → `FileLocked`, with the path taken from
///   the chain's context messages when one names an existing file below the workdir;
/// - anything else → `otherwise(err)`.
pub(crate) fn classify(
    repo: &gix::Repository,
    err: anyhow::Error,
    otherwise: impl FnOnce(anyhow::Error) -> OplogError,
) -> OplogError {
    if let Some(path) = leftover_lock_file(repo) {
        return OplogError::RepositoryLocked { path };
    }
    let locked_code = err.chain().find_map(|cause| {
        let io = cause.downcast_ref::<std::io::Error>()?;
        let code = io.raw_os_error()?;
        LOCKED_FILE_OS_ERRORS.contains(&code).then_some(code)
    });
    if let Some(code) = locked_code {
        let path = repo
            .workdir()
            .and_then(|workdir| path_from_context(&err, workdir));
        return OplogError::FileLocked {
            path,
            code,
            source: std::io::Error::from_raw_os_error(code),
        };
    }
    otherwise(err)
}

/// The Git lock files whose presence makes a capture or restore fail: the index lock of this worktree and the
/// lock of its `HEAD`.
fn leftover_lock_file(repo: &gix::Repository) -> Option<PathBuf> {
    [
        repo.index_path().with_extension("lock"),
        repo.git_dir().join("HEAD.lock"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Find a quoted path (`'…'` or `"…"`) in the context messages of `err` that names an existing file in `workdir`.
fn path_from_context(err: &anyhow::Error, workdir: &Path) -> Option<PathBuf> {
    err.chain().find_map(|cause| {
        let message = cause.to_string();
        for quote in ['\'', '"'] {
            let mut parts = message.split(quote);
            parts.next();
            while let Some(candidate) = parts.next() {
                if !candidate.is_empty() {
                    let path = Path::new(candidate);
                    let path = if path.is_absolute() {
                        path.to_owned()
                    } else {
                        workdir.join(path)
                    };
                    if path.is_file() {
                        return Some(path);
                    }
                }
                parts.next();
            }
        }
        None
    })
}
