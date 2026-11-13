//! File operations of the catalogue (spec 21 §2.11; owned by G5): staging, unstaging, discarding, deleting
//! untracked files and resolving conflicts. Operations that can lose untracked files record a snapshot that
//! captures them and ask first when the snapshot cannot hold them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{Context, Window};

use super::{OpKind, OpSpec, Prepared};
use crate::backend::handle::RepoHandle;
use crate::backend::present::{OpFailure, confirm_unrestorable};
use crate::backend::prompt::Asker;
use crate::backend::{
    ByteSlice as _, DiscardMode, OperationDescriptor, OperationKind, RefreshSet, RelaPath,
    Resolution, Scope, SelectionRequest, SkipReason, SkippedFile, UntrackedCapture, git,
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
        let nested_agreed = NestedAgreed::default();
        let agreed = nested_agreed.clone();
        let spec = OpSpec::new(OpKind::DiscardFile, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::discard_paths(repo, env.perm(), ctx, &paths, mode, agreed.get())
        })
        .prepare(move |handle, asker| {
            confirm_untracked_loss(handle, asker, &title, &untracked, Some(&nested_agreed))
        })
        .record(descriptor)
        .base_refresh(RefreshSet::STATUS | RefreshSet::REPO_STATE);
        self.run_op(spec, window, cx)
    }

    /// Stages everything (`stage::stage_all`). Refresh: STATUS.
    pub fn stage_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let spec = OpSpec::new(OpKind::StageAll, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::stage_all(repo, env.perm(), ctx)
        })
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Unstages everything (`stage::unstage_all`); the backend refuses it while paths are conflicted, so the
    /// status pane disables the button then. Refresh: STATUS.
    pub fn unstage_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let spec = OpSpec::new(OpKind::UnstageAll, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::unstage_all(repo, env.perm(), ctx)
        })
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Stages a directory (`stage::stage_paths([dir])`). Refresh: STATUS.
    pub fn stage_dir(
        &mut self,
        dir: RelaPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::StageDirectory, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::stage_paths(repo, env.perm(), ctx, std::slice::from_ref(&dir))
        })
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Unstages a directory (`stage::unstage_paths([dir])`). Refresh: STATUS.
    pub fn unstage_dir(
        &mut self,
        dir: RelaPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::UnstageDirectory, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::unstage_paths(repo, env.perm(), ctx, std::slice::from_ref(&dir))
        })
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Stages the selected hunks or lines of an unstaged diff (`stage::stage_selection`). Refresh: STATUS (also
    /// after a stale diff, which the runner turns into a diff reload).
    pub fn stage_selection(
        &mut self,
        req: SelectionRequest,
        unit: SelectionUnit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = match unit {
            SelectionUnit::Hunk => OpKind::StageHunk,
            SelectionUnit::Line => OpKind::StageLine,
        };
        let spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::stage_selection(repo, env.perm(), ctx, &req)
        })
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Unstages the selected hunks or lines of a staged diff (`stage::unstage_selection`). Refresh: STATUS.
    pub fn unstage_selection(
        &mut self,
        req: SelectionRequest,
        unit: SelectionUnit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = match unit {
            SelectionUnit::Hunk => OpKind::UnstageHunk,
            SelectionUnit::Line => OpKind::UnstageLine,
        };
        let spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::unstage_selection(repo, env.perm(), ctx, &req)
        })
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Discards the selected hunks or lines of an unstaged diff (`stage::discard_selection`), recorded as a full
    /// snapshot. `untracked` = the file is untracked: the user is asked first when the snapshot cannot hold it.
    /// Refresh: STATUS.
    pub fn discard_selection(
        &mut self,
        req: SelectionRequest,
        unit: SelectionUnit,
        untracked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = match unit {
            SelectionUnit::Hunk => OpKind::DiscardHunk,
            SelectionUnit::Line => OpKind::DiscardLine,
        };
        let path = req.path.clone();
        let title = format!(
            "Discard changes in {}",
            paths_title(std::slice::from_ref(&path))
        );
        let descriptor = OperationDescriptor {
            kind: OperationKind::Discard,
            title: title.clone(),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::Paths(vec![path.clone()]),
        };
        let confirm_paths = if untracked { vec![path] } else { Vec::new() };
        let spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::discard_selection(repo, env.perm(), ctx, &req)
        })
        .prepare(move |handle, asker| {
            confirm_untracked_loss(handle, asker, &title, &confirm_paths, None)
        })
        .record(descriptor)
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Deletes untracked files and directories (`stage::delete_untracked`), recorded as a full snapshot that
    /// captures them. The user is asked first when the snapshot cannot hold some of them. Refresh: STATUS.
    pub fn delete_untracked(
        &mut self,
        paths: Vec<RelaPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Delete {}", paths_title(&paths));
        let descriptor = OperationDescriptor {
            kind: OperationKind::DeleteUntracked,
            title: title.clone(),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::Paths(paths.clone()),
        };
        let confirm_paths = paths.clone();
        let nested_agreed = NestedAgreed::default();
        let agreed = nested_agreed.clone();
        let spec = OpSpec::new(OpKind::DeleteFile, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::delete_untracked(repo, env.perm(), ctx, &paths, agreed.get())
        })
        .prepare(move |handle, asker| {
            confirm_untracked_loss(handle, asker, &title, &confirm_paths, Some(&nested_agreed))
        })
        .record(descriptor)
        .base_refresh(RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Resolves conflicted paths (`stage::resolve_conflict`): take ours or theirs, mark resolved or delete. The
    /// caller warns about remaining conflict markers before `MarkResolved`. Recorded as a full snapshot.
    /// Refresh: STATUS | REPO_STATE.
    pub fn resolve_conflict(
        &mut self,
        paths: Vec<RelaPath>,
        how: Resolution,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let descriptor = OperationDescriptor {
            kind: OperationKind::ResolveConflicts,
            title: format!("Resolve conflict in {}", paths_title(&paths)),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::ResolveConflict, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::stage::resolve_conflict(repo, env.perm(), ctx, &paths, how)
        })
        .record(descriptor)
        .base_refresh(RefreshSet::STATUS | RefreshSet::REPO_STATE);
        self.run_op(spec, window, cx)
    }
}

/// Set by `prepare` once the user agreed to lose the listed untracked paths; the backend call passes it as its
/// `allow_nested` flag, so a nested repository the user was not asked about is refused there.
#[derive(Clone, Default)]
pub(crate) struct NestedAgreed(Arc<AtomicBool>);

impl NestedAgreed {
    pub(crate) fn get(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// The `prepare` step of every operation that deletes untracked files (discard, delete, `stash push -u`): asks
/// first when the snapshot would not hold them. `preview_skipped` fails closed: a repository without an oplog,
/// with undo history switched off, or whose probe failed holds nothing, so every path is reported as unrestorable
/// (32 review, T6). The runner refuses to run such an operation when the snapshot itself then fails, and asks again
/// about files the snapshot skips beyond the ones accepted here.
///
/// `nested`: the operation deletes the nested repositories among the paths (discard, delete), which are always asked
/// about, by name, also when the preview fails closed (undo history off, no oplog): they come from the same walk the
/// backend refuses on (`stage::nested_repositories`). The agreement is stored there only if that walk succeeded, so
/// the question named every repository the backend would otherwise refuse. `None` (`stash push -u`, which leaves
/// nested repositories alone, or a single file): they are not reported.
pub(crate) fn confirm_untracked_loss(
    handle: &RepoHandle,
    asker: &Asker,
    title: &str,
    untracked: &[RelaPath],
    nested: Option<&NestedAgreed>,
) -> Result<Prepared, OpFailure> {
    if untracked.is_empty() {
        return Ok(Prepared::Continue { untracked: None });
    }
    let mut skipped = handle.preview_skipped(untracked);
    let mut nested_named = false;
    match nested {
        None => skipped.retain(|s| s.reason != SkipReason::NestedRepository),
        Some(_) => match git::stage::nested_repositories(&handle.repo, untracked) {
            Ok(repositories) => {
                // A path the fail-closed preview lists itself is the repository: named once, as such.
                let trimmed = |path: &RelaPath| RelaPath::from(path.trim_end_with(|c| c == '/'));
                skipped.retain(|s| {
                    s.reason != SkipReason::NestedRepository
                        && !repositories.contains(&trimmed(&s.path))
                });
                skipped.extend(repositories.into_iter().map(|path| SkippedFile {
                    path,
                    size: 0,
                    reason: SkipReason::NestedRepository,
                }));
                nested_named = true;
            }
            // The backend then refuses the repositories itself (`allow_nested` stays off).
            Err(err) => tracing::warn!("cannot look for nested repositories: {err}"),
        },
    }
    if !skipped.is_empty() && !asker.confirm(confirm_unrestorable(title, &skipped)) {
        return Ok(Prepared::Cancelled);
    }
    if let Some(nested) = nested
        && nested_named
        && !skipped.is_empty()
    {
        nested.0.store(true, Ordering::Release);
    }
    Ok(Prepared::DeletesUntracked {
        accepted: skipped.into_iter().map(|s| s.path).collect(),
    })
}
