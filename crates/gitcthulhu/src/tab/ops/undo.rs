//! Undo operations of the catalogue (spec 21 §2.11; owned by X2 after G0b). G0b implements `undo`, `redo` and
//! `restore_to` over `run_restore` (confirmation loop with the guard released before each dialog).

use gpui_kit::{Context, Window};

use super::RestoreKind;
use crate::backend::{OplogRecovery, RestoreTarget};
use crate::tab::RepoTab;

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

    /// Deletes the undo history of this worktree (`oplog_clear`). Owner: X2.
    pub fn clear_undo_history(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.not_implemented("Clear undo history", cx);
        false
    }

    /// Finishes or rolls back an interrupted undo (`oplog_recover`); the generic recovery flow already does this
    /// through `recover_interrupted`. Owner: X2.
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
