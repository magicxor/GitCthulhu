//! File operations of the catalogue (spec 21 §2.11; owned by G5 after G0b). G0b implements the reference methods
//! `stage_paths`, `unstage_paths` and `discard_paths` (prepare + confirmation + snapshot with untracked paths); the
//! other methods keep their final signatures and show "Not implemented yet" until G5 implements them.

use gpui_kit::{Context, Window};

use super::{OpKind, OpSpec, Prepared};
use crate::backend::present::{OpFailure, confirm_unrestorable};
use crate::backend::{
    ByteSlice as _, DiscardMode, OperationDescriptor, OperationKind, RefreshSet, RelaPath,
    Resolution, Scope, SelectionRequest, UntrackedCapture, git,
};
use crate::tab::RepoTab;

/// Whether a selection operation works on whole hunks or on lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionUnit {
    Hunk,
    Line,
}

/// `'path'` for one path, `{n} files` for several (oplog titles, 17 §4.2).
pub(crate) fn paths_title(paths: &[RelaPath]) -> String {
    match paths {
        [one] => format!("'{}'", one.to_str_lossy()),
        many => format!("{} files", many.len()),
    }
}

impl RepoTab {
    /// ★ Stages whole files and directories (`stage::stage_paths`). Refresh: STATUS.
    pub fn stage_paths(
        &mut self,
        paths: Vec<RelaPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = if paths.len() > 1 {
            OpKind::StageFiles
        } else {
            OpKind::StageFile
        };
        let spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::stage_paths(repo, env.perm(), ctx, &paths)
        })
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// ★ Unstages whole files and directories (`stage::unstage_paths`). Refresh: STATUS.
    pub fn unstage_paths(
        &mut self,
        paths: Vec<RelaPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = if paths.len() > 1 {
            OpKind::UnstageFiles
        } else {
            OpKind::UnstageFile
        };
        let spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::unstage_paths(repo, env.perm(), ctx, &paths)
        })
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// ★ Discards the changes of `paths` (`stage::discard_paths`), recorded as a full snapshot that captures the
    /// listed paths. `untracked` = the untracked subset of `paths`: when the snapshot cannot hold some of them, the
    /// user is asked before anything runs (spec 21 §2.11). Refresh: STATUS | REPO_STATE.
    pub fn discard_paths(
        &mut self,
        paths: Vec<RelaPath>,
        mode: DiscardMode,
        untracked: Vec<RelaPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Discard changes in {}", paths_title(&paths));
        let descriptor = OperationDescriptor {
            kind: OperationKind::Discard,
            title: title.clone(),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::Paths(paths.clone()),
        };
        let spec = OpSpec::new(OpKind::DiscardFile, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::discard_paths(repo, env.perm(), ctx, &paths, mode)
        })
        .prepare(move |handle, asker| {
            let skipped = handle.preview_skipped(&untracked);
            if skipped.is_empty() || asker.confirm(confirm_unrestorable(&title, &skipped)) {
                Ok(Prepared::Continue { untracked: None })
            } else {
                Ok::<_, OpFailure>(Prepared::Cancelled)
            }
        })
        .record(descriptor)
        .base_refresh(RefreshSet::STATUS | RefreshSet::REPO_STATE);
        self.run_op(spec, window, cx)
    }

    /// Stages everything (`stage::stage_all`). Owner: G5.
    pub fn stage_all(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.not_implemented("Stage all", cx);
        false
    }

    /// Unstages everything (`stage::unstage_all`). Owner: G5.
    pub fn unstage_all(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.not_implemented("Unstage all", cx);
        false
    }

    /// Stages a directory (`stage::stage_paths([dir])`). Owner: G5.
    pub fn stage_dir(
        &mut self,
        _dir: RelaPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Stage directory", cx);
        false
    }

    /// Unstages a directory (`stage::unstage_paths([dir])`). Owner: G5.
    pub fn unstage_dir(
        &mut self,
        _dir: RelaPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Unstage directory", cx);
        false
    }

    /// Stages hunks or lines (`stage::stage_selection`). Owner: G5.
    pub fn stage_selection(
        &mut self,
        _req: SelectionRequest,
        _unit: SelectionUnit,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Stage selection", cx);
        false
    }

    /// Unstages hunks or lines (`stage::unstage_selection`). Owner: G5.
    pub fn unstage_selection(
        &mut self,
        _req: SelectionRequest,
        _unit: SelectionUnit,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Unstage selection", cx);
        false
    }

    /// Discards hunks or lines (`stage::discard_selection`); `untracked` = the file is untracked. Owner: G5.
    pub fn discard_selection(
        &mut self,
        _req: SelectionRequest,
        _unit: SelectionUnit,
        _untracked: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Discard selection", cx);
        false
    }

    /// Deletes untracked files (`stage::delete_untracked`). Owner: G5.
    pub fn delete_untracked(
        &mut self,
        _paths: Vec<RelaPath>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Delete file", cx);
        false
    }

    /// Resolves conflicts (`stage::resolve_conflict`). Owner: G5.
    pub fn resolve_conflict(
        &mut self,
        _paths: Vec<RelaPath>,
        _how: Resolution,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Resolve conflict", cx);
        false
    }
}
