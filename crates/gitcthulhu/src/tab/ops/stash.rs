//! Stash operations of the catalogue (spec 21 §2.11; owned by G3 after G0b). G0b implements the reference method
//! `pop_stash` (prepare reads the stash's untracked paths for the snapshot).

use gpui_kit::{Context, Window};

use super::{OpKind, OpSpec, Prepared};
use crate::backend::present::OpFailure;
use crate::backend::{
    ByteSlice as _, ObjectId, OperationDescriptor, OperationKind, RefreshSet, Scope,
    StashApplyOptions, StashApplyResult, UntrackedCapture, git, loaders,
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

    /// ★ `git stash pop` of `id`: a full snapshot that captures the untracked files the stash restores (read in
    /// `prepare`, before the guard). A conflicted or partial pop keeps the entry and shows a Warning toast.
    /// Refresh: STATUS | STASHES | LOG | REPO_STATE.
    pub fn pop_stash(&mut self, id: ObjectId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let index = self.stash_index(id, cx).unwrap_or(0);
        let descriptor = OperationDescriptor {
            kind: OperationKind::StashPop,
            title: format!("Pop stash@{{{index}}}"),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::PopStash, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stash::stash_pop(repo, env.perm(), ctx, id, StashApplyOptions::default())
        })
        .prepare(move |handle, _| {
            let paths = loaders::stash_untracked_paths(&handle.repo, id)
                .map_err(|err| OpFailure::Git(std::sync::Arc::new(err)))?;
            Ok(Prepared::Continue {
                untracked: Some(UntrackedCapture::Paths(paths)),
            })
        })
        .record(descriptor)
        .base_refresh(
            RefreshSet::STATUS | RefreshSet::STASHES | RefreshSet::LOG | RefreshSet::REPO_STATE,
        )
        .then(|tab, result, _, cx| {
            if let Some(text) = stash_result_warning(result) {
                crate::app_view::show_toast(cx, Some(tab.ctx.id), ToastKind::Warning, text, None);
            }
        });
        self.run_op(spec, window, cx)
    }

    /// `git stash push` (`include_untracked` = `-u`). Owner: G3.
    pub fn stash(
        &mut self,
        _message: Option<String>,
        _include_untracked: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Stash", cx);
        false
    }

    /// `git stash apply` of `id`. Owner: G3.
    pub fn apply_stash(
        &mut self,
        _id: ObjectId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Apply stash", cx);
        false
    }

    /// `git stash drop` of `id`. Owner: G3.
    pub fn drop_stash(
        &mut self,
        _id: ObjectId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Delete stash", cx);
        false
    }
}
