//! Undo operations of the catalogue (spec 21 §2.11, 17 §4.4; owned by X2): `undo`, `redo` and `restore_to` over
//! `run_restore` (a confirmation loop that releases the repository guard before each dialog), clearing the undo
//! history and finishing or rolling back an interrupted restore.

use std::sync::Arc;

use gpui_kit::{Context, SharedString, Window};
use parking_lot::Mutex;

use super::{OpKind, OpSpec, Prepared, RestoreKind};
use crate::backend::prompt::ConfirmRequest;
use crate::backend::{OpOutcome, OplogRecovery, RefreshSet, RestoreTarget};
use crate::notifications::ToastKind;
use crate::tab::RepoTab;

/// The question before the undo history is deleted.
pub fn clear_history_request() -> ConfirmRequest {
    let mut request = ConfirmRequest::new(
        "Clear the undo history?",
        "Undo and Redo will not be able to restore anything that was recorded so far, including the copies \
         of deleted untracked files. Your files, branches and commits stay as they are.",
    );
    request.confirm_label = "Clear history".into();
    request.destructive = true;
    request
}

impl RepoTab {
    /// ★ Undoes the newest undoable operation.
    pub fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.run_restore(RestoreKind::Undo, window, cx)
    }

    /// ★ Redoes the newest undone operation.
    pub fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.run_restore(RestoreKind::Redo, window, cx)
    }

    /// ★ Restores the repository to the before- or after-state of an entry (always confirmed).
    pub fn restore_to(
        &mut self,
        target: RestoreTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.run_restore(RestoreKind::RestoreTo(target), window, cx)
    }

    /// Deletes the undo history of this worktree (`oplog_clear`) after asking. Nothing else changes: the files,
    /// branches and commits stay. Refresh: UNDO.
    pub fn clear_undo_history(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // `run` has no handle: the deletion happens in `after` (under the guard), which reports a failure through
        // this slot and the completion toast shows it.
        let failure: Arc<Mutex<Option<SharedString>>> = Arc::new(Mutex::new(None));
        let in_after = failure.clone();
        let spec = OpSpec::new(OpKind::ClearUndoHistory, |_| {
            Ok(OpOutcome {
                value: (),
                refresh: RefreshSet::UNDO,
                log: Default::default(),
                warnings: Vec::new(),
            })
        })
        .prepare(|_, asker| {
            Ok(if asker.confirm(clear_history_request()) {
                Prepared::Continue { untracked: None }
            } else {
                Prepared::Cancelled
            })
        })
        .after(move |handle, perm, _| {
            if let Err(text) = handle.oplog_clear(perm) {
                *in_after.lock() = Some(text);
            }
        })
        .toast_with(move |_| {
            failure.lock().take().map(|text| {
                (
                    ToastKind::Error,
                    SharedString::from(format!("Clearing the undo history failed: {text}")),
                )
            })
        })
        .base_refresh(RefreshSet::UNDO);
        self.run_op(spec, window, cx)
    }

    /// Finishes or rolls back an interrupted undo (`oplog_recover`); the generic recovery flow already does this
    /// through `recover_interrupted`.
    pub fn recover_undo(
        &mut self,
        how: OplogRecovery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.recover_interrupted(how, window, cx);
        true
    }
}

#[cfg(test)]
mod tests;
