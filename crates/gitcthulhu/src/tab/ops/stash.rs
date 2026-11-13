//! Stash operations of the catalogue (spec 21 §2.11, §2.9; owned by G3): stash, apply, pop and drop. G0b wrote the
//! reference method `pop_stash` (prepare reads the stash's untracked paths for the snapshot); apply shares its
//! build. A conflicted or partial apply/pop is a successful run that keeps the stash entry; its one toast is a
//! Warning instead of the success toast (it keeps the undo action of a recorded run).

use gpui_kit::{Context, Window};

use super::{OpKind, OpSpec, Prepared};
use crate::backend::present::OpFailure;
use crate::backend::{
    ByteSlice as _, ChangeKind, ObjectId, OperationDescriptor, OperationKind, RefreshSet, RelaPath,
    Scope, StashApplyOptions, StashApplyResult, StashPushOptions, UntrackedCapture, git, loaders,
};
use crate::notifications::ToastKind;
use crate::tab::RepoTab;

/// The warning toast of a stash apply/pop that did not apply cleanly (the stash entry is kept).
pub(crate) fn stash_result_warning(result: &StashApplyResult) -> Option<String> {
    match result {
        StashApplyResult::Applied => None,
        StashApplyResult::Conflicts { .. } => Some("Stash applied with conflicts".to_owned()),
        StashApplyResult::Partial {
            untracked_not_restored,
            ..
        } => {
            let n = untracked_not_restored.len();
            let first = untracked_not_restored
                .first()
                .map(|p| p.to_str_lossy().into_owned())
                .unwrap_or_default();
            Some(if n == 1 {
                format!(
                    "Stash applied: the untracked file '{first}' was not restored (it already exists)"
                )
            } else {
                format!("Stash applied: {n} untracked files were not restored (they already exist)")
            })
        }
    }
}

impl RepoTab {
    /// The `stash@{n}` index of stash `id` in the loaded refs.
    fn stash_index(&self, id: ObjectId, cx: &Context<Self>) -> Option<usize> {
        self.ctx
            .model
            .read(cx)
            .refs
            .value
            .as_ref()?
            .stashes
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.index)
    }

    /// The untracked files of the loaded status (the candidates of `stash -u`'s snapshot capture).
    fn untracked_paths(&self, cx: &Context<Self>) -> Vec<RelaPath> {
        self.ctx
            .model
            .read(cx)
            .status
            .value
            .as_ref()
            .map(|status| {
                status
                    .unstaged
                    .iter()
                    .filter(|e| e.kind == ChangeKind::Untracked)
                    .map(|e| e.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Apply and pop share everything but the backend call, the oplog kind and the refresh set.
    fn run_stash_apply(
        &mut self,
        id: ObjectId,
        pop: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let index = self.stash_index(id, cx).unwrap_or(0);
        let (op, oplog_kind, verb) = if pop {
            (OpKind::PopStash, OperationKind::StashPop, "Pop")
        } else {
            (OpKind::ApplyStash, OperationKind::StashApply, "Apply")
        };
        let descriptor = OperationDescriptor {
            kind: oplog_kind,
            title: format!("{verb} stash@{{{index}}}"),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let refresh = if pop {
            RefreshSet::STATUS | RefreshSet::STASHES | RefreshSet::LOG | RefreshSet::REPO_STATE
        } else {
            RefreshSet::STATUS | RefreshSet::REPO_STATE
        };
        let spec = OpSpec::new(op, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            let opts = StashApplyOptions::default();
            if pop {
                git::stash::stash_pop(repo, env.perm(), ctx, id, opts)
            } else {
                git::stash::stash_apply(repo, env.perm(), ctx, id, opts)
            }
        })
        .prepare(move |handle, _| {
            let paths = loaders::stash_untracked_paths(&handle.repo, id)
                .map_err(|err| OpFailure::Git(std::sync::Arc::new(err)))?;
            Ok(Prepared::Continue {
                untracked: Some(UntrackedCapture::Paths(paths)),
            })
        })
        .record(descriptor)
        .base_refresh(refresh)
        // A result that did not apply cleanly replaces the success toast with one Warning (which keeps the undo
        // action of a recorded run, 17 §4.5).
        .toast_with(|result| {
            stash_result_warning(result).map(|warning| (ToastKind::Warning, warning.into()))
        });
        self.run_op(spec, window, cx)
    }

    /// ★ `git stash pop` of `id`: a full snapshot that captures the untracked files the stash restores (read in
    /// `prepare`, before the guard). A conflicted or partial pop keeps the entry and shows a Warning toast.
    /// Refresh: STATUS | STASHES | LOG | REPO_STATE.
    pub fn pop_stash(&mut self, id: ObjectId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.run_stash_apply(id, true, window, cx)
    }

    /// `git stash apply` of `id` (without `--index`), recorded like a pop. A conflicted or partial apply keeps the
    /// entry (as every apply does) and shows a Warning toast. Refresh: STATUS | REPO_STATE.
    pub fn apply_stash(
        &mut self,
        id: ObjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.run_stash_apply(id, false, window, cx)
    }

    /// `git stash push` (`include_untracked` = `-u`; Gitnuro always includes them). With untracked files the
    /// snapshot captures `All` of them; when it cannot hold some, the user is asked first. A stash that found
    /// nothing to save shows a Warning toast. Refresh: STATUS | STASHES | LOG.
    pub fn stash(
        &mut self,
        message: Option<String>,
        include_untracked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = "Stash changes".to_owned();
        let untracked = if include_untracked {
            self.untracked_paths(cx)
        } else {
            Vec::new()
        };
        let descriptor = OperationDescriptor {
            kind: OperationKind::StashPush,
            title: title.clone(),
            detail: None,
            scope: Scope::Full,
            untracked: if include_untracked {
                UntrackedCapture::All
            } else {
                UntrackedCapture::None
            },
        };
        let message = message
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty());
        let mut spec = OpSpec::new(OpKind::Stash, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            let opts = StashPushOptions {
                message,
                include_untracked,
                ..StashPushOptions::default()
            };
            git::stash::stash_push(repo, env.perm(), ctx, &opts)
        })
        .record(descriptor)
        .base_refresh(RefreshSet::STATUS | RefreshSet::STASHES | RefreshSet::LOG)
        // The runner's "Changes stashed" toast keeps the undo action of a recorded run; the rare race in which the
        // changes vanished before Git ran (the Stash button is disabled on a clean tree) adds a Warning.
        .then(|tab, saved: &Option<ObjectId>, _, cx| {
            if saved.is_none() {
                crate::app_view::show_toast(
                    cx,
                    Some(tab.ctx.id),
                    ToastKind::Warning,
                    "There were no changes to stash",
                    None,
                );
            }
        });
        if include_untracked {
            spec = spec.prepare(move |handle, asker| {
                super::files::confirm_untracked_loss(handle, asker, &title, &untracked, None)
            });
        }
        self.run_op(spec, window, cx)
    }

    /// `git stash drop` of `id`. Refresh: STASHES | LOG.
    pub fn drop_stash(
        &mut self,
        id: ObjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let index = self.stash_index(id, cx).unwrap_or(0);
        let descriptor = OperationDescriptor {
            kind: OperationKind::StashDrop,
            title: format!("Drop stash@{{{index}}}"),
            detail: None,
            scope: Scope::RefsOnly,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::DropStash, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stash::stash_drop(repo, env.perm(), ctx, id)
        })
        .record(descriptor)
        .base_refresh(RefreshSet::STASHES | RefreshSet::LOG);
        self.run_op(spec, window, cx)
    }
}
