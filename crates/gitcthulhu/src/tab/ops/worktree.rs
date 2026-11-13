//! Worktree operations of the catalogue (spec 21 §2.11, §2.9, §5.7; owned by X6): add, remove, prune, lock and
//! unlock. Removing and pruning clear the undo history (oplog chain) of every worktree that disappeared, still under
//! the repository guard.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::{Context, Window};

use super::{Handled, OpKind, OpSpec};
use crate::app_view::show_toast;
use crate::backend::present::ErrorDialogData;
use crate::backend::prompt::ConfirmRequest;
use crate::backend::{GitErrorKind, RefreshSet, WorktreeAddOptions, WorktreeInfo, git};
use crate::notifications::{ToastAction, ToastKind};
use crate::tab::RepoTab;

/// What a worktree change can touch: the list itself and the "checked out in" marks of the branches.
const WORKTREE_REFRESH: RefreshSet = RefreshSet::WORKTREES.union(RefreshSet::BRANCHES);

/// The directory name of a worktree, for titles and messages.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl RepoTab {
    /// ★ `git worktree remove` (`force` = remove with uncommitted changes); afterwards, still under the guard, the
    /// removed worktree's undo history is cleared. A worktree with uncommitted changes asks "Remove it anyway?" and
    /// retries with `force`; the worktree of this very tab (`CurrentWorktree`) or the main one (`MainWorktree`) is explained instead, by the
    /// kind of the error and not by the (possibly stale) tab model (21 §2.9).
    /// Refresh: WORKTREES | BRANCHES.
    pub fn remove_worktree(
        &mut self,
        path: PathBuf,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (retry_path, shown) = (path.clone(), display_name(&path));
        let spec = OpSpec::new(OpKind::RemoveWorktree, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::worktree::remove(repo, env.perm(), ctx, &path, force)
        })
        .after(|handle, perm, id: &String| {
            handle.clear_linked_worktrees(std::slice::from_ref(id), perm)
        })
        .toast(format!("Worktree '{shown}' removed"))
        .base_refresh(WORKTREE_REFRESH)
        .on_error(move |tab, failure, window, cx| match failure.git_kind() {
            Some(GitErrorKind::UncommittedChanges) if !force => {
                let mut request = ConfirmRequest::new(
                    "Remove worktree",
                    "The worktree has uncommitted changes. Remove it anyway?",
                );
                request.details = vec![retry_path.display().to_string().into()];
                request.confirm_label = "Remove".into();
                request.destructive = true;
                let ctx = tab.ctx.clone();
                crate::dialogs::confirm::open_with(
                    window,
                    cx,
                    request,
                    Box::new(move |yes, window, cx| {
                        if yes {
                            let path = retry_path.clone();
                            ctx.with_tab(window, cx, move |tab, window, cx| {
                                tab.remove_worktree(path, true, window, cx);
                            });
                        }
                    }),
                );
                Handled::Yes
            }
            Some(kind @ (GitErrorKind::MainWorktree | GitErrorKind::CurrentWorktree)) => {
                let text = if kind == GitErrorKind::CurrentWorktree {
                    "Close the tab of this worktree first."
                } else {
                    "The main worktree cannot be removed."
                };
                let data = ErrorDialogData::new(OpKind::RemoveWorktree.error_title(), text);
                crate::app_view::show_error(cx, data);
                Handled::Yes
            }
            _ => Handled::No,
        });
        self.run_op(spec, window, cx)
    }

    /// `git worktree add` (from an existing branch, with a new branch or detached at a commit). Afterwards a toast
    /// offers to open the new worktree in its own tab. Hooks (post-checkout) run natively; a failing hook shows the
    /// standard hook dialog. Refresh: WORKTREES | BRANCHES.
    pub fn add_worktree(
        &mut self,
        opts: WorktreeAddOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::AddWorktree, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::worktree::add(repo, env.perm(), ctx, &opts)
        })
        .toast("")
        .base_refresh(WORKTREE_REFRESH)
        .then(|tab, info: &WorktreeInfo, _, cx| {
            let path = info.path.clone();
            let action = ToastAction {
                label: "Open in new tab".into(),
                run: Rc::new(move |window, cx| {
                    crate::worktrees::open_in_new_tab(window, cx, path.clone());
                }),
            };
            show_toast(
                cx,
                Some(tab.ctx.id),
                ToastKind::Positive,
                format!("Worktree '{}' added", display_name(&info.path)),
                Some(action),
            );
        });
        self.run_op(spec, window, cx)
    }

    /// `git worktree prune` (clears the pruned worktrees' undo histories). Refresh: WORKTREES | BRANCHES.
    pub fn prune_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let spec = OpSpec::new(OpKind::PruneWorktrees, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::worktree::prune(repo, env.perm(), ctx)
        })
        .after(|handle, perm, ids: &Vec<String>| handle.clear_linked_worktrees(ids, perm))
        .toast_with(|ids: &Vec<String>| {
            let text = match ids.len() {
                0 => "No worktrees to prune".to_owned(),
                1 => "Pruned 1 worktree".to_owned(),
                n => format!("Pruned {n} worktrees"),
            };
            Some((ToastKind::Positive, text.into()))
        })
        .base_refresh(WORKTREE_REFRESH);
        self.run_op(spec, window, cx)
    }

    /// `git worktree lock` (`reason` is optional). Refresh: WORKTREES.
    pub fn lock_worktree(
        &mut self,
        path: PathBuf,
        reason: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::LockWorktree, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::worktree::lock(repo, env.perm(), ctx, &path, reason.as_deref())
        })
        .base_refresh(RefreshSet::WORKTREES);
        self.run_op(spec, window, cx)
    }

    /// `git worktree unlock`. Refresh: WORKTREES.
    pub fn unlock_worktree(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::UnlockWorktree, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::worktree::unlock(repo, env.perm(), ctx, &path)
        })
        .base_refresh(RefreshSet::WORKTREES);
        self.run_op(spec, window, cx)
    }
}
