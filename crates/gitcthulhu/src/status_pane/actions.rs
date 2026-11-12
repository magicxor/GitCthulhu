//! What the status lists and their menus do: stage, unstage, discard, delete, resolve, blame, history and copy.
//!
//! Every function runs from an event handler (click, menu item, key) and starts operations through
//! [`TabCtx::with_tab`], never from `render`. Staging conflicted files warns first when conflict markers remain
//! (21 §2.11, `resolve_conflict`).

use std::path::MAIN_SEPARATOR_STR;
use std::rc::Rc;

use gpui_kit::{App, Window};

use crate::backend::prompt::ConfirmRequest;
use crate::backend::{
    BlameRev, ByteSlice as _, ChangeKind, DiscardMode, RelaPath, Resolution, StatusEntry, bg,
    status,
};
use crate::dialogs::confirm;
use crate::tab::TabCtx;
use crate::tab::ui::{BlameTarget, CloseableView, FileList, RepoUiEvent};

use super::platform;

/// At most this many files are listed in the "markers remain" confirmation.
const MAX_LISTED: usize = 10;

/// The paths a whole-file operation must pass for `entries`: the path and, for renames, the old path too
/// (05 §3.1: a rename stages and unstages both sides). Duplicates are dropped, the order is kept.
pub fn paths_of<'a>(entries: impl IntoIterator<Item = &'a StatusEntry>) -> Vec<RelaPath> {
    let mut out: Vec<RelaPath> = Vec::new();
    for entry in entries {
        for path in std::iter::once(&entry.path).chain(entry.previous_path.as_ref()) {
            if !out.contains(path) {
                out.push(path.clone());
            }
        }
    }
    out
}

/// The conflicted paths among `entries`.
fn conflicted(entries: &[StatusEntry]) -> Vec<RelaPath> {
    entries
        .iter()
        .filter(|e| e.kind == ChangeKind::Conflicted)
        .map(|e| e.path.clone())
        .collect()
}

/// Whether the entry is an untracked (or intent-to-add) file: it is new to the repository, so "discard" is the
/// wrong word for it (08 §4.5 hides it for ADDED).
pub fn is_added(kind: ChangeKind) -> bool {
    matches!(
        kind,
        ChangeKind::Added | ChangeKind::Untracked | ChangeKind::IntentToAdd
    )
}

/// Runs `proceed` after the user accepted that conflicted files with remaining markers are marked resolved. Without
/// conflicted paths, a backend, or markers, `proceed` runs at once.
fn after_marker_check(
    ctx: &TabCtx,
    conflicted: Vec<RelaPath>,
    window: &mut Window,
    cx: &mut App,
    proceed: Rc<dyn Fn(&mut Window, &mut App)>,
) {
    let Some(repo) = ctx
        .backend
        .repo()
        .cloned()
        .filter(|_| !conflicted.is_empty())
    else {
        proceed(window, cx);
        return;
    };
    let task = bg(cx, move || {
        status::files_with_conflict_markers(&repo, &conflicted)
    });
    window
        .spawn(cx, async move |acx| {
            let marked = task.await;
            acx.update(|window, cx| {
                if marked.is_empty() {
                    proceed(window, cx);
                    return;
                }
                let mut request = ConfirmRequest::new(
                    "Conflict markers remain",
                    if marked.len() == 1 {
                        "This file still contains conflict markers. Mark it as resolved anyway?"
                    } else {
                        "These files still contain conflict markers. Mark them as resolved anyway?"
                    },
                );
                request.confirm_label = "Mark as resolved".into();
                request.destructive = true;
                request.details = marked
                    .iter()
                    .take(MAX_LISTED)
                    .map(|p| p.to_str_lossy().into_owned().into())
                    .collect();
                confirm::open_with(
                    window,
                    cx,
                    request,
                    Box::new(move |yes, window, cx| {
                        if yes {
                            proceed(window, cx);
                        }
                    }),
                );
            })
            .ok();
        })
        .detach();
}

/// Stages `entries` of the unstaged list (renames with both paths). Conflicted files are marked resolved after
/// a warning when markers remain.
pub fn stage(ctx: &TabCtx, entries: Vec<StatusEntry>, window: &mut Window, cx: &mut App) {
    if entries.is_empty() {
        return;
    }
    let conflicted = conflicted(&entries);
    let all_conflicted = conflicted.len() == entries.len();
    let paths = paths_of(&entries);
    let run_ctx = ctx.clone();
    let proceed: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, cx| {
        let paths = paths.clone();
        run_ctx.with_tab(window, cx, move |tab, window, cx| {
            if all_conflicted {
                tab.resolve_conflict(paths, Resolution::MarkResolved, window, cx);
            } else {
                tab.stage_paths(paths, window, cx);
            }
        });
    });
    after_marker_check(ctx, conflicted, window, cx, proceed);
}

/// Unstages `entries` of the staged list (renames with both paths).
pub fn unstage(ctx: &TabCtx, entries: Vec<StatusEntry>, window: &mut Window, cx: &mut App) {
    if entries.is_empty() {
        return;
    }
    let paths = paths_of(&entries);
    ctx.with_tab(window, cx, move |tab, window, cx| {
        tab.unstage_paths(paths, window, cx);
    });
}

/// Stages everything; conflicted files among `unstaged` are marked resolved after the marker warning.
pub fn stage_all(ctx: &TabCtx, unstaged: &[StatusEntry], window: &mut Window, cx: &mut App) {
    let run_ctx = ctx.clone();
    let proceed: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, cx| {
        run_ctx.with_tab(window, cx, |tab, window, cx| {
            tab.stage_all(window, cx);
        });
    });
    after_marker_check(ctx, conflicted(unstaged), window, cx, proceed);
}

/// Unstages everything.
pub fn unstage_all(ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    ctx.with_tab(window, cx, |tab, window, cx| {
        tab.unstage_all(window, cx);
    });
}

/// Stages the directory `dir` and everything below it.
pub fn stage_dir(
    ctx: &TabCtx,
    dir: RelaPath,
    below: &[StatusEntry],
    window: &mut Window,
    cx: &mut App,
) {
    let run_ctx = ctx.clone();
    let proceed: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, cx| {
        let dir = dir.clone();
        run_ctx.with_tab(window, cx, move |tab, window, cx| {
            tab.stage_dir(dir, window, cx);
        });
    });
    after_marker_check(ctx, conflicted(below), window, cx, proceed);
}

/// Unstages the directory `dir` and everything below it.
pub fn unstage_dir(ctx: &TabCtx, dir: RelaPath, window: &mut Window, cx: &mut App) {
    ctx.with_tab(window, cx, move |tab, window, cx| {
        tab.unstage_dir(dir, window, cx);
    });
}

/// What discarding `entries` of `list` passes to the operation: the paths (renames with both), the mode (staged
/// entries are reset to HEAD, unstaged ones to the index) and the untracked subset the snapshot must capture.
pub fn discard_args(
    list: FileList,
    entries: &[StatusEntry],
) -> (Vec<RelaPath>, DiscardMode, Vec<RelaPath>) {
    let mode = match list {
        FileList::Staged => DiscardMode::StagedAndUnstaged,
        _ => DiscardMode::Unstaged,
    };
    let untracked = entries
        .iter()
        .filter(|e| e.kind == ChangeKind::Untracked)
        .map(|e| e.path.clone())
        .collect();
    (paths_of(entries), mode, untracked)
}

/// Discards the changes of `entries` in `list`. Untracked entries are deleted; the operation asks first when the
/// undo snapshot cannot hold them.
pub fn discard(
    ctx: &TabCtx,
    list: FileList,
    entries: Vec<StatusEntry>,
    window: &mut Window,
    cx: &mut App,
) {
    if entries.is_empty() {
        return;
    }
    let (paths, mode, untracked) = discard_args(list, &entries);
    ctx.with_tab(window, cx, move |tab, window, cx| {
        tab.discard_paths(paths, mode, untracked, window, cx);
    });
}

/// Deletes untracked files.
pub fn delete_untracked(ctx: &TabCtx, paths: Vec<RelaPath>, window: &mut Window, cx: &mut App) {
    ctx.with_tab(window, cx, move |tab, window, cx| {
        tab.delete_untracked(paths, window, cx);
    });
}

/// Resolves conflicted `paths` by taking one side.
pub fn resolve(
    ctx: &TabCtx,
    paths: Vec<RelaPath>,
    how: Resolution,
    window: &mut Window,
    cx: &mut App,
) {
    ctx.with_tab(window, cx, move |tab, window, cx| {
        tab.resolve_conflict(paths, how, window, cx);
    });
}

/// Opens the blame view of the worktree file `path` (09 §2.1; the view reacts to `RepoUi::blame`).
pub fn open_blame(ctx: &TabCtx, path: RelaPath, cx: &mut App) {
    ctx.ui.update(cx, |ui, cx| {
        ui.blame = Some(BlameTarget {
            path,
            rev: BlameRev::Worktree,
            minimized: false,
        });
        ui.push_view(CloseableView::Blame, cx);
        cx.emit(RepoUiEvent::CenterChanged);
    });
}

/// Opens the file history of `path` (09 §2.2; the view reacts to `RepoUi::history`).
pub fn open_history(ctx: &TabCtx, path: RelaPath, cx: &mut App) {
    ctx.ui.update(cx, |ui, cx| {
        ui.history = Some(path);
        ui.push_view(CloseableView::FileHistory, cx);
        cx.emit(RepoUiEvent::CenterChanged);
    });
}

/// The text "Copy path" puts on the clipboard: the absolute path of each file, one per line.
pub fn absolute_text(ctx: &TabCtx, paths: &[RelaPath]) -> String {
    let repo = ctx.backend.repo();
    paths
        .iter()
        .map(|path| {
            repo.and_then(|repo| status::absolute_path(repo, path))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_str_lossy().replace('/', MAIN_SEPARATOR_STR))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text "Copy relative path" puts on the clipboard: the repository-relative path of each file, one per line.
pub fn relative_text(paths: &[RelaPath]) -> String {
    paths
        .iter()
        .map(|p| p.to_str_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Copies the absolute paths of `paths` to the clipboard.
pub fn copy_absolute(ctx: &TabCtx, paths: &[RelaPath], cx: &mut App) {
    platform::copy_text(cx, absolute_text(ctx, paths));
}

/// Copies the repository-relative paths of `paths` to the clipboard.
pub fn copy_relative(paths: &[RelaPath], cx: &mut App) {
    platform::copy_text(cx, relative_text(paths));
}

/// Shows `path` in the platform's file manager.
pub fn reveal(ctx: &TabCtx, path: &RelaPath, cx: &mut App) {
    let absolute = ctx
        .backend
        .repo()
        .and_then(|repo| status::absolute_path(repo, path));
    if let Some(absolute) = absolute {
        platform::reveal_path(cx, absolute);
    }
}
