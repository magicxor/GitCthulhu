//! Branch operations of the catalogue (spec 21 §2.11; owned by G3 after G0b). Every method keeps its final
//! signature and shows "Not implemented yet" until G3 implements it after the reference patterns in `files.rs`,
//! `commit.rs`, `stash.rs` and `network.rs`.

use gpui_kit::{Context, Window};

use crate::backend::{FullName, ObjectId, StartPoint};
use crate::tab::RepoTab;

impl RepoTab {
    /// Checks out a local branch (`branch::checkout_branch`). Owner: G3.
    pub fn checkout_branch(
        &mut self,
        _full: FullName,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Checkout branch", cx);
        false
    }

    /// Checks out a remote branch as a tracking branch (`branch::checkout_remote_branch`). Owner: G3.
    pub fn checkout_remote_branch(
        &mut self,
        _remote_ref: FullName,
        _local_name: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Checkout remote branch", cx);
        false
    }

    /// Checks out a commit, detached (`branch::checkout_detached`). Owner: G3.
    pub fn checkout_commit(
        &mut self,
        _id: ObjectId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Checkout commit", cx);
        false
    }

    /// Checks out a tag's commit, detached (`branch::checkout_detached`). Owner: G3.
    pub fn checkout_tag(
        &mut self,
        _full: FullName,
        _target: ObjectId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Checkout tag", cx);
        false
    }

    /// Creates a branch, optionally checking it out (`branch::create_branch`). Owner: G3.
    pub fn create_branch(
        &mut self,
        _name: String,
        _start: StartPoint,
        _checkout: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Create branch", cx);
        false
    }

    /// Renames a local branch (`branch::rename_branch`). Owner: G3.
    pub fn rename_branch(
        &mut self,
        _full: FullName,
        _new_name: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Rename branch", cx);
        false
    }

    /// Deletes a local branch (`branch::delete_branch`). Owner: G3.
    pub fn delete_branch(
        &mut self,
        _full: FullName,
        _force: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Delete branch", cx);
        false
    }

    /// Sets or unsets a branch's upstream (`branch::set_upstream`). Owner: G3.
    pub fn set_upstream(
        &mut self,
        _full: FullName,
        _upstream: Option<FullName>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Change upstream", cx);
        false
    }
}
