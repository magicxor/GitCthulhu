//! History operations of the catalogue (spec 21 §2.11; owned by G4 after G0b). Final signatures; "Not
//! implemented yet" until G4 implements them.

use gpui_kit::{Context, Window};

use crate::backend::{ObjectId, RebasePlan, ResetMode};
use crate::tab::RepoTab;

impl RepoTab {
    /// Merges `rev` into the current branch (`merge::merge`). Owner: G4.
    pub fn merge(&mut self, _rev: String, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.not_implemented("Merge", cx);
        false
    }

    /// Rebases the current branch onto `onto_rev` (`rebase::rebase`). Owner: G4.
    pub fn rebase(
        &mut self,
        _onto_rev: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Rebase", cx);
        false
    }

    /// Starts an interactive rebase with the edited plan (`rebase::start_interactive`). Owner: G4.
    pub fn start_interactive_rebase(
        &mut self,
        _plan: RebasePlan,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Interactive rebase", cx);
        false
    }

    /// Cherry-picks a commit (`merge::cherry_pick`). Owner: G4.
    pub fn cherry_pick(
        &mut self,
        _id: ObjectId,
        _mainline: Option<u8>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Cherry-pick", cx);
        false
    }

    /// Reverts a commit (`merge::revert`). Owner: G4.
    pub fn revert(
        &mut self,
        _id: ObjectId,
        _mainline: Option<u8>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Revert", cx);
        false
    }

    /// Resets the current branch to a commit (`merge::reset`). Owner: G4.
    pub fn reset(
        &mut self,
        _id: ObjectId,
        _mode: ResetMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Reset", cx);
        false
    }
}
