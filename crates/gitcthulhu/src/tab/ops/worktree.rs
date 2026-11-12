//! Worktree operations of the catalogue (spec 21 §2.11; owned by X6 after G0b). G0b implements the reference
//! method `remove_worktree` (an `after` step that clears the worktree's oplog chain under the guard).

use std::path::PathBuf;

use gpui_kit::{Context, Window};

use super::{OpKind, OpSpec};
use crate::backend::{RefreshSet, WorktreeAddOptions, git};
use crate::tab::RepoTab;

impl RepoTab {
    /// ★ `git worktree remove` (`force` = remove with uncommitted changes); afterwards, still under the guard, the
    /// removed worktree's undo history is cleared. Refresh: WORKTREES | BRANCHES.
    pub fn remove_worktree(
        &mut self,
        path: PathBuf,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::RemoveWorktree, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::worktree::remove(repo, env.perm(), ctx, &path, force)
        })
        .after(|handle, perm, id: &String| {
            handle.clear_linked_worktrees(std::slice::from_ref(id), perm)
        })
        .base_refresh(RefreshSet::WORKTREES | RefreshSet::BRANCHES);
        self.run_op(spec, window, cx)
    }

    /// `git worktree add`. Owner: X6.
    pub fn add_worktree(
        &mut self,
        _opts: WorktreeAddOptions,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Add worktree", cx);
        false
    }

    /// `git worktree prune` (clears the pruned worktrees' undo histories). Owner: X6.
    pub fn prune_worktrees(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.not_implemented("Prune worktrees", cx);
        false
    }

    /// `git worktree lock`. Owner: X6.
    pub fn lock_worktree(
        &mut self,
        _path: PathBuf,
        _reason: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Lock worktree", cx);
        false
    }

    /// `git worktree unlock`. Owner: X6.
    pub fn unlock_worktree(
        &mut self,
        _path: PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Unlock worktree", cx);
        false
    }
}
