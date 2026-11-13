//! The status file menus: single file, several files, directory (owned by G5 after G0b; 08 §4.5).
//!
//! Gitnuro's items and their conditions are reproduced; deviations: "Discard changes" of a directory works
//! (21 §6 #19), conflicted files get "Resolve using ours / theirs" (the catalogue's `resolve_conflict`), and
//! "Delete file" is offered for untracked files only (the backend refuses to delete tracked paths).

use gpui_kit::App;

use super::{MenuItem, MenuSlot, MenuTarget, extend};
use crate::backend::{ChangeKind, RelaPath, Resolution, StatusEntry};
use crate::icons::AppIcon;
use crate::status_pane::actions;
use crate::tab::ui::FileList;

/// The menu of the file `target.path` in the list the file selection lives in (the unstaged list when nothing
/// is selected); slot `End`. The status lists call [`single_file_menu`], [`multi_file_menu`] and
/// [`directory_menu`] directly because they know the list.
pub fn status_file(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let Some(path) = target.path.clone() else {
        return Vec::new();
    };
    let list = target
        .tab
        .ui
        .read(cx)
        .files
        .list
        .filter(|l| *l != FileList::CommitFiles)
        .unwrap_or(FileList::Unstaged);
    let entry = target
        .tab
        .model
        .read(cx)
        .status
        .value
        .as_ref()
        .and_then(|status| {
            let entries = if list == FileList::Staged {
                &status.staged
            } else {
                &status.unstaged
            };
            entries.iter().find(|e| e.path == path).cloned()
        });
    match entry {
        Some(entry) => single_file_menu(target, list, &entry, cx),
        None => Vec::new(),
    }
}

/// The menu of one file (08 §4.5 "Single-file context menu").
pub fn single_file_menu(
    target: &MenuTarget,
    list: FileList,
    entry: &StatusEntry,
    cx: &App,
) -> Vec<MenuItem> {
    let ctx = target.tab.clone();
    let kind = entry.kind;
    let path = entry.path.clone();
    let mut items = Vec::new();

    if list == FileList::Unstaged && kind == ChangeKind::Conflicted {
        for (id, label, how) in [
            ("status-use-ours", "Resolve using ours", Resolution::Ours),
            (
                "status-use-theirs",
                "Resolve using theirs",
                Resolution::Theirs,
            ),
        ] {
            let (ctx, path) = (ctx.clone(), path.clone());
            items.push(MenuItem::action(id, label, move |window, cx| {
                actions::resolve(&ctx, vec![path.clone()], how, window, cx);
            }));
        }
        items.push(MenuItem::Separator);
    }
    if !actions::is_added(kind) {
        let (ctx, entry) = (ctx.clone(), entry.clone());
        items.push(
            MenuItem::action(
                "status-discard",
                "Discard file changes",
                move |window, cx| {
                    actions::discard(&ctx, list, vec![entry.clone()], window, cx);
                },
            )
            .with_icon(AppIcon::Undo),
        );
    }
    if !actions::is_added(kind) && kind != ChangeKind::Deleted {
        let (blame_ctx, blame_path) = (ctx.clone(), path.clone());
        items.push(
            MenuItem::action("status-blame", "Blame file", move |_, cx| {
                actions::open_blame(&blame_ctx, blame_path.clone(), cx);
            })
            .with_icon(AppIcon::Blame),
        );
        let (history_ctx, history_path) = (ctx.clone(), path.clone());
        items.push(
            MenuItem::action("status-history", "File history", move |_, cx| {
                actions::open_history(&history_ctx, history_path.clone(), cx);
            })
            .with_icon(AppIcon::History),
        );
    }
    if list == FileList::Unstaged && kind == ChangeKind::Untracked {
        let (ctx, path) = (ctx.clone(), path.clone());
        items.push(
            MenuItem::action("status-delete", "Delete file", move |window, cx| {
                actions::delete_untracked(&ctx, vec![path.clone()], window, cx);
            })
            .with_icon(AppIcon::Delete),
        );
    }
    let (reveal_ctx, reveal_path) = (ctx.clone(), path.clone());
    items.push(
        MenuItem::action("status-reveal", "Open file in folder", move |_, cx| {
            actions::reveal(&reveal_ctx, &reveal_path, cx);
        })
        .with_icon(AppIcon::FolderOpen),
    );
    items.push(MenuItem::Separator);
    items.extend(copy_items(&ctx, vec![path], false));
    extend(cx, MenuSlot::End, target, &mut items);
    items
}

/// The menu of several selected files (08 §4.5 "Multi-file context menu").
pub fn multi_file_menu(
    target: &MenuTarget,
    list: FileList,
    entries: Vec<StatusEntry>,
    cx: &App,
) -> Vec<MenuItem> {
    let ctx = target.tab.clone();
    let count = entries.len();
    let mut items = Vec::new();
    let (stage_ctx, stage_entries) = (ctx.clone(), entries.clone());
    if list == FileList::Staged {
        items.push(
            MenuItem::action(
                "status-unstage-many",
                format!("Unstage {count} files"),
                move |window, cx| actions::unstage(&stage_ctx, stage_entries.clone(), window, cx),
            )
            .with_icon(AppIcon::RemoveDone),
        );
    } else {
        items.push(
            MenuItem::action(
                "status-stage-many",
                format!("Stage {count} files"),
                move |window, cx| actions::stage(&stage_ctx, stage_entries.clone(), window, cx),
            )
            .with_icon(AppIcon::Done),
        );
    }
    let (discard_ctx, discard_entries) = (ctx.clone(), entries.clone());
    items.push(
        MenuItem::action(
            "status-discard-many",
            format!("Discard {count} files"),
            move |window, cx| {
                actions::discard(&discard_ctx, list, discard_entries.clone(), window, cx)
            },
        )
        .with_icon(AppIcon::Undo),
    );
    items.push(MenuItem::Separator);
    let paths: Vec<RelaPath> = entries.iter().map(|e| e.path.clone()).collect();
    items.extend(copy_items(&ctx, paths, true));
    extend(cx, MenuSlot::End, target, &mut items);
    items
}

/// The menu of a directory row in tree mode (08 §4.5 "Directory context menu"). `below` = the entries of the
/// list at or below `dir`.
pub fn directory_menu(
    target: &MenuTarget,
    list: FileList,
    dir: RelaPath,
    below: Vec<StatusEntry>,
    cx: &App,
) -> Vec<MenuItem> {
    let ctx = target.tab.clone();
    let mut items = Vec::new();
    if list == FileList::Staged {
        items.push(
            MenuItem::action(
                "status-unstage-dir",
                "Unstage changes in the directory",
                move |window, cx| actions::unstage_dir(&ctx, dir.clone(), window, cx),
            )
            .with_icon(AppIcon::RemoveDone),
        );
    } else {
        let (stage_ctx, stage_dir, stage_below) = (ctx.clone(), dir, below.clone());
        items.push(
            MenuItem::action(
                "status-stage-dir",
                "Stage changes in the directory",
                move |window, cx| {
                    actions::stage_dir(&stage_ctx, stage_dir.clone(), &stage_below, window, cx)
                },
            )
            .with_icon(AppIcon::Done),
        );
        items.push(MenuItem::Separator);
        items.push(
            MenuItem::action(
                "status-discard-dir",
                "Discard changes",
                move |window, cx| {
                    actions::discard(&ctx, FileList::Unstaged, below.clone(), window, cx)
                },
            )
            .with_icon(AppIcon::Undo),
        );
    }
    extend(cx, MenuSlot::End, target, &mut items);
    items
}

/// "Copy path" and "Copy relative path" (plural forms for a multi-selection).
fn copy_items(ctx: &crate::tab::TabCtx, paths: Vec<RelaPath>, plural: bool) -> Vec<MenuItem> {
    let (absolute_ctx, absolute_paths) = (ctx.clone(), paths.clone());
    vec![
        MenuItem::action(
            "status-copy-path",
            if plural { "Copy paths" } else { "Copy path" },
            move |_, cx| actions::copy_absolute(&absolute_ctx, &absolute_paths, cx),
        )
        .with_icon(AppIcon::Copy),
        MenuItem::action(
            "status-copy-relative",
            if plural {
                "Copy relative paths"
            } else {
                "Copy relative path"
            },
            move |_, cx| actions::copy_relative(&paths, cx),
        )
        .with_icon(AppIcon::CopyAll),
    ]
}
