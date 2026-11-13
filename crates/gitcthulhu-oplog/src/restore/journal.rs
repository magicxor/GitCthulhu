//! The restore journal `<git_dir>/gitcthulhu/oplog/restore.json` (spec 17 §3.4, §3.6).
//!
//! It exists from the moment a restore starts writing (step C1) until its entry is appended (C11), or until a
//! failed restore was undone. While it exists, `prepare`, `undo`, `redo` and `restore_to` refuse with
//! `InterruptedRestore`; `recover` finishes or rolls back the restore it describes. It is written with temp file +
//! rename, so a reader sees either the old or the new content, never half of it.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::Context as _;

use crate::{
    INDEX_BACKUP_FILE, JOURNAL_FILE, Oplog, OplogError,
    types::{EntryId, InProgress, InterruptedRestore, OperationKind, Side, UntrackedCapture},
};

/// The format version written by this build. A journal with a higher version is reported but cannot be
/// completed; rolling back still restores the index backup and removes the journal.
pub(super) const JOURNAL_VERSION: u32 = 1;

/// How far an interrupted restore got (informational: every step of the recovery is idempotent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Phase {
    /// The worktree is being (or was) written (C1-C3).
    Worktree,
    /// Refs, stash and branch config are being written (C4-C7).
    Refs,
    /// The index is being written; the sequencer quit and the entry append follow (C8-C11).
    Index,
}

/// The content of `restore.json`. Object ids are stored as hex strings.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Journal {
    pub version: u32,
    pub phase: Phase,
    /// `Undo`, `Redo` or `RestoreTo`.
    pub kind: OperationKind,
    /// For `RestoreTo`: the restored side of `reversing`.
    pub side: Option<Side>,
    /// The entry that is reversed (undo/redo) or restored (restore-to).
    pub reversing: u64,
    /// The user-visible title of the operation.
    pub title: String,
    pub started_at_ms: i64,
    /// The snapshot tree of the target state.
    pub target_tree: String,
    /// The snapshot tree of the pre-restore state P.
    pub pre_restore_tree: String,
    /// The snapshot tree of the expected state, `None` if unknown.
    pub expected_tree: Option<String>,
    /// The in-progress state the restore ends with `git <op> --quit` (C9), if any.
    pub quit: Option<InProgress>,
    /// The planned index: stage-0 tree and conflict tree.
    pub new_index: Option<String>,
    pub new_index_conflicts: Option<String>,
    /// The merge base of the worktree checkout.
    pub base: Option<String>,
    /// The untracked contents captured in P and Q.
    pub untracked: UntrackedCapture,
}

impl Journal {
    /// The [`InterruptedRestore`] shown to the user.
    pub(super) fn info(&self) -> InterruptedRestore {
        InterruptedRestore {
            started_at_ms: self.started_at_ms,
            title: self.title.clone(),
            kind: self.kind,
            reversing: Some(EntryId(self.reversing)),
        }
    }
}

/// Parse a hex object id stored in the journal.
pub(super) fn oid(hex: &str) -> anyhow::Result<gix::ObjectId> {
    gix::ObjectId::from_hex(hex.as_bytes())
        .with_context(|| format!("the restore journal holds an invalid object id '{hex}'"))
}

/// The journal file of `oplog`'s worktree.
pub(super) fn path(oplog: &Oplog, repo: &gix::Repository) -> PathBuf {
    oplog.journal_dir(repo).join(JOURNAL_FILE)
}

/// The index backup of step C2.
pub(super) fn index_backup_path(oplog: &Oplog, repo: &gix::Repository) -> PathBuf {
    oplog.journal_dir(repo).join(INDEX_BACKUP_FILE)
}

/// What the journal file holds.
pub(super) enum Read {
    /// No journal: no interrupted restore.
    Absent,
    /// A journal this build can act on.
    Valid(Journal),
    /// A journal that cannot be parsed (or was written by a newer version); only a roll back is possible.
    Unreadable(String),
}

/// Read the journal of `oplog`'s worktree.
pub(super) fn read(oplog: &Oplog, repo: &gix::Repository) -> Result<Read, OplogError> {
    let path = path(oplog, repo);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Read::Absent),
        Err(err) => {
            return Err(OplogError::Other(
                anyhow::Error::from(err).context(format!("reading '{}'", path.display())),
            ));
        }
    };
    Ok(match serde_json::from_slice::<Journal>(&data) {
        Ok(journal) if journal.version <= JOURNAL_VERSION => Read::Valid(journal),
        Ok(journal) => Read::Unreadable(format!(
            "the restore journal has format version {}, this build reads up to {JOURNAL_VERSION}",
            journal.version
        )),
        Err(err) => Read::Unreadable(format!("the restore journal is unreadable: {err}")),
    })
}

/// The [`InterruptedRestore`] described by the journal, if one exists. An unreadable journal is still reported,
/// so the GUI offers recovery instead of silently ignoring it.
pub(super) fn interrupted(
    oplog: &Oplog,
    repo: &gix::Repository,
) -> Result<Option<InterruptedRestore>, OplogError> {
    Ok(match read(oplog, repo)? {
        Read::Absent => None,
        Read::Valid(journal) => Some(journal.info()),
        Read::Unreadable(reason) => {
            tracing::warn!(%reason, "found an unreadable restore journal");
            Some(InterruptedRestore {
                started_at_ms: 0,
                title: String::new(),
                kind: OperationKind::Unknown,
                reversing: None,
            })
        }
    })
}

/// `Err(InterruptedRestore)` if a journal exists (step A1).
pub(super) fn refuse_if_present(oplog: &Oplog, repo: &gix::Repository) -> Result<(), OplogError> {
    if path(oplog, repo).exists() {
        Err(OplogError::InterruptedRestore)
    } else {
        Ok(())
    }
}

/// Write `journal` atomically (temp file in the same directory + rename).
pub(super) fn write(
    oplog: &Oplog,
    repo: &gix::Repository,
    journal: &Journal,
) -> Result<(), OplogError> {
    let dir = oplog.journal_dir(repo);
    write_atomically(
        &dir,
        &dir.join(JOURNAL_FILE),
        &serde_json::to_vec_pretty(journal).map_err(anyhow::Error::from)?,
    )
}

/// Write `data` to `path` through a temporary file in `dir` and a rename.
pub(super) fn write_atomically(dir: &Path, path: &Path, data: &[u8]) -> Result<(), OplogError> {
    let context = || format!("writing '{}'", path.display());
    std::fs::create_dir_all(dir)
        .map_err(|err| OplogError::Other(anyhow::Error::from(err).context(context())))?;
    let mut file = tempfile::NamedTempFile::new_in(dir)
        .map_err(|err| OplogError::Other(anyhow::Error::from(err).context(context())))?;
    file.write_all(data)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|err| OplogError::Other(anyhow::Error::from(err).context(context())))?;
    file.persist(path)
        .map_err(|err| OplogError::Other(anyhow::Error::from(err.error).context(context())))?;
    Ok(())
}

/// Delete the journal (missing is fine).
pub(super) fn remove(oplog: &Oplog, repo: &gix::Repository) -> Result<(), OplogError> {
    remove_file(&path(oplog, repo))
}

/// Delete `path` if it exists.
pub(super) fn remove_file(path: &Path) -> Result<(), OplogError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(OplogError::Other(
            anyhow::Error::from(err).context(format!("removing '{}'", path.display())),
        )),
    }
}
