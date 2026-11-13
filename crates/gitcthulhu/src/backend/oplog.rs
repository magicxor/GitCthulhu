//! Every call into `gitcthulhu_oplog` (spec 21 §2.4, 17 §4.1). The runner (`tab/ops`) and the undo UI use only
//! these wrappers; errors become texts because `OplogError` is not `Clone`.

use gpui_kit::SharedString;

use super::handle::{RepoHandle, UndoSettings};
use super::{
    EntryId, InterruptedRestore, NetworkMarker, OperationDescriptor, OperationKind,
    OperationResult, Oplog, OplogError, OplogPage, OplogRecovery, PageRequest, PendingOperation,
    PushRefStatus, PushReport, PushedRef, RelaPath, RepoExclusive, RestoreOptions, RestoreTarget,
    SkipReason, SkippedFile, UndoOutcome, UndoRedoState,
};

#[cfg(test)]
thread_local! {
    /// Makes `oplog_prepare` fail on this thread (GPUI tests run operations' background halves on the test thread).
    pub(crate) static FAIL_PREPARE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What `oplog_prepare` produced (one short-lived value per operation, so the large variant is not boxed).
#[allow(clippy::large_enum_variant)]
pub enum PrepareResult {
    /// The before-state is captured; call `oplog_finish` after the operation (also when it failed).
    Recording(PendingOperation),
    /// No oplog for this repository, or recording is off: run the operation unrecorded.
    Disabled,
    /// An interrupted undo must be recovered first; the operation must not run.
    Interrupted,
    /// The capture failed: run the operation unrecorded and warn "{title} can't be undone: {error}".
    Failed(SharedString),
}

/// Which restore to run (re-exported by `tab::ops`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreKind {
    Undo,
    Redo,
    RestoreTo(RestoreTarget),
}

/// An oplog error as the runner presents it (`OplogError` is not `Clone`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OplogErrorInfo {
    /// `OplogError::InterruptedRestore`: run the recovery flow.
    Interrupted,
    /// Anything else, as user-facing text.
    Other(SharedString),
}

/// User-facing text of an oplog error (17 §3.7 wording for locked repositories and files).
pub fn oplog_error_text(err: &OplogError) -> SharedString {
    match err {
        OplogError::RepositoryLocked { .. } => {
            "Another Git process is running in this repository.".into()
        }
        OplogError::FileLocked { path, .. } => match path {
            Some(path) => format!(
                "'{}' is in use by another program. Close it and try Undo again.",
                path.display()
            )
            .into(),
            None => "A file is in use by another program. Close it and try Undo again.".into(),
        },
        OplogError::Unsupported(reason) => {
            format!("Undo is not available for this repository: {reason}").into()
        }
        other => other.to_string().into(),
    }
}

fn info(err: OplogError) -> OplogErrorInfo {
    match err {
        OplogError::InterruptedRestore => OplogErrorInfo::Interrupted,
        other => OplogErrorInfo::Other(oplog_error_text(&other)),
    }
}

impl RepoHandle {
    fn unavailable(&self) -> SharedString {
        let reason = self
            .oplog_unavailable
            .clone()
            .unwrap_or_else(|| "it was not opened".into());
        format!("Undo is not available for this repository: {reason}").into()
    }

    /// `Oplog::state` (no lock, metadata only, cached; 17 §4.1). `Ok(None)` when the oplog is unavailable.
    pub fn undo_state(&self) -> Result<Option<UndoRedoState>, SharedString> {
        let Some(oplog) = &self.oplog else {
            return Ok(None);
        };
        oplog
            .read()
            .state(&self.repo.local())
            .map(Some)
            .map_err(|e| oplog_error_text(&e))
    }

    /// `Oplog::preview_skipped` for untracked paths (stat probe, no lock): which of them a snapshot would not
    /// hold. Fails closed (T6): without an oplog, with recording or untracked capture switched off, or when the
    /// probe fails (logged), every path counts as unrestorable, so callers ask before deleting anything.
    pub fn preview_skipped(&self, untracked: &[RelaPath]) -> Vec<SkippedFile> {
        if untracked.is_empty() {
            return Vec::new();
        }
        let all = |reason: SkipReason| {
            untracked
                .iter()
                .map(|path| SkippedFile {
                    path: path.clone(),
                    size: 0,
                    reason,
                })
                .collect()
        };
        let Some(oplog) = &self.oplog else {
            return all(SkipReason::CaptureDisabled);
        };
        let oplog = oplog.read();
        if !oplog.options().enabled {
            return all(SkipReason::CaptureDisabled);
        }
        match oplog.preview_skipped(&self.repo.local(), untracked) {
            Ok(skipped) => skipped,
            Err(err) => {
                tracing::warn!("cannot preview the untracked capture: {err}");
                all(SkipReason::Unreadable)
            }
        }
    }

    /// `Oplog::prepare` under the caller's guard.
    pub fn oplog_prepare(
        &self,
        op: OperationDescriptor,
        perm: &mut RepoExclusive,
    ) -> PrepareResult {
        #[cfg(test)]
        if FAIL_PREPARE.with(std::cell::Cell::get) {
            return PrepareResult::Failed("the snapshot could not be written (test)".into());
        }
        let Some(oplog) = &self.oplog else {
            return PrepareResult::Disabled;
        };
        match oplog.read().prepare(&self.repo.local(), op, perm) {
            Ok(Some(pending)) => PrepareResult::Recording(pending),
            Ok(None) => PrepareResult::Disabled,
            Err(OplogError::InterruptedRestore) => PrepareResult::Interrupted,
            Err(err) => PrepareResult::Failed(oplog_error_text(&err)),
        }
    }

    /// `Oplog::finish`: `Ok(true)` when an entry was written, `Ok(false)` when the oplog dropped it because the
    /// operation changed nothing (the stacks are untouched, so there is nothing to undo); `Err` is the text of the
    /// "{title} can't be undone: {error}" warning (nothing recorded).
    pub fn oplog_finish(
        &self,
        pending: PendingOperation,
        result: OperationResult,
        perm: &mut RepoExclusive,
    ) -> Result<bool, SharedString> {
        let Some(oplog) = &self.oplog else {
            return Err(self.unavailable());
        };
        oplog
            .read()
            .finish(&self.repo.local(), pending, result, perm)
            .map(|entry| entry.is_some())
            .map_err(|e| oplog_error_text(&e))
    }

    /// `Oplog::record_marker` (push family). `Ok` without an oplog: there is nothing to record.
    pub fn oplog_marker(
        &self,
        marker: NetworkMarker,
        perm: &mut RepoExclusive,
    ) -> Result<(), SharedString> {
        let Some(oplog) = &self.oplog else {
            return Ok(());
        };
        oplog
            .read()
            .record_marker(&self.repo.local(), marker, perm)
            .map(drop)
            .map_err(|e| oplog_error_text(&e))
    }

    /// `Oplog::undo` / `redo` / `restore_to` under the caller's guard (never across a dialog).
    pub fn oplog_restore(
        &self,
        kind: &RestoreKind,
        opts: RestoreOptions,
        perm: &mut RepoExclusive,
    ) -> Result<UndoOutcome, OplogErrorInfo> {
        let Some(oplog) = &self.oplog else {
            return Err(OplogErrorInfo::Other(self.unavailable()));
        };
        let oplog = oplog.read();
        let repo = self.repo.local();
        match kind {
            RestoreKind::Undo => oplog.undo(&repo, opts, perm),
            RestoreKind::Redo => oplog.redo(&repo, opts, perm),
            RestoreKind::RestoreTo(target) => oplog.restore_to(&repo, *target, opts, perm),
        }
        .map_err(info)
    }

    /// `Oplog::recover` (finish or roll back an interrupted restore).
    pub fn oplog_recover(
        &self,
        how: OplogRecovery,
        perm: &mut RepoExclusive,
    ) -> Result<(), SharedString> {
        let Some(oplog) = &self.oplog else {
            return Err(self.unavailable());
        };
        oplog
            .read()
            .recover(&self.repo.local(), how, perm)
            .map_err(|e| oplog_error_text(&e))
    }

    /// `Oplog::list` (no lock), newest first.
    pub fn oplog_list(&self, page: PageRequest) -> Result<OplogPage, SharedString> {
        let Some(oplog) = &self.oplog else {
            return Err(self.unavailable());
        };
        oplog
            .read()
            .list(&self.repo.local(), page)
            .map_err(|e| oplog_error_text(&e))
    }

    /// `Oplog::clear`: deletes the whole undo history of this worktree.
    pub fn oplog_clear(&self, perm: &mut RepoExclusive) -> Result<(), SharedString> {
        let Some(oplog) = &self.oplog else {
            return Err(self.unavailable());
        };
        oplog
            .read()
            .clear(&self.repo.local(), perm)
            .map_err(|e| oplog_error_text(&e))
    }

    /// `Oplog::interrupted_restore` (no lock).
    pub fn oplog_interrupted(&self) -> Result<Option<InterruptedRestore>, SharedString> {
        let Some(oplog) = &self.oplog else {
            return Ok(None);
        };
        oplog
            .read()
            .interrupted_restore(&self.repo.local())
            .map_err(|e| oplog_error_text(&e))
    }

    /// `Oplog::record_barrier` with `OperationKind::Barrier` ("Record undo history" turned back on).
    pub fn oplog_barrier(
        &self,
        title: String,
        reason: String,
        perm: &mut RepoExclusive,
    ) -> Result<(), SharedString> {
        let Some(oplog) = &self.oplog else {
            return Ok(());
        };
        oplog
            .read()
            .record_barrier(
                &self.repo.local(),
                OperationKind::Barrier,
                title,
                reason,
                perm,
            )
            .map(drop)
            .map_err(|e| oplog_error_text(&e))
    }

    /// `Oplog::set_options` under the write lock. Background thread only: waits for a running prepare/finish.
    pub fn set_oplog_options(&self, undo: &UndoSettings) {
        if let Some(oplog) = &self.oplog {
            let options = super::handle::oplog_options_for(&self.repo, undo);
            oplog.write().set_options(options);
        }
    }

    /// Whether the oplog records new operations.
    pub fn oplog_enabled(&self) -> bool {
        self.oplog
            .as_ref()
            .is_some_and(|oplog| oplog.read().options().enabled)
    }

    /// `Oplog::clear_linked_worktree` for each id (worktree remove/prune, 20 §2.6). Errors are logged, never shown.
    pub fn clear_linked_worktrees(&self, ids: &[String], perm: &mut RepoExclusive) {
        let repo = self.repo.local();
        for id in ids {
            if let Err(err) = Oplog::clear_linked_worktree(&repo, id, perm) {
                tracing::warn!("cannot clear the undo history of worktree {id}: {err}");
            }
        }
    }

    /// The entry with `id`, if it exists (no lock).
    pub fn oplog_entry(&self, id: EntryId) -> Result<Option<super::OplogEntry>, SharedString> {
        let Some(oplog) = &self.oplog else {
            return Err(self.unavailable());
        };
        oplog
            .read()
            .entry(&self.repo.local(), id)
            .map_err(|e| oplog_error_text(&e))
    }
}

/// The network marker of a push result (17 §4.4 `run_push`): every update whose status is not `Rejected`; `None`
/// when there is none.
pub fn push_marker(
    kind: OperationKind,
    title: String,
    report: &PushReport,
) -> Option<NetworkMarker> {
    let updates: Vec<PushedRef> = report
        .updates
        .iter()
        .filter(|u| u.status != PushRefStatus::Rejected)
        .map(|u| PushedRef {
            local: u.local.clone(),
            remote_ref: u.remote_ref.clone(),
            new_target: if u.status == PushRefStatus::Deleted {
                None
            } else {
                u.new
            },
            forced: u.status == PushRefStatus::Forced,
        })
        .collect();
    if updates.is_empty() {
        return None;
    }
    Some(NetworkMarker {
        kind,
        title,
        remote: report.remote.clone(),
        updates,
    })
}
