//! Worktree row and Worktrees header menus (D0.7; owned by X6; spec 21 §5.7).
//!
//! Row: Open in new tab, Open in file manager, Copy path, then Lock… / Unlock and Remove for linked worktrees.
//! Header: Add worktree… and Prune worktrees. Both call `menus::extend` at `End` so later packages can add items.

use std::path::PathBuf;

use gpui_kit::{App, Window};

use super::{MenuItem, MenuKind, MenuSlot, MenuTarget, extend};
use crate::backend::WorktreeInfo;
use crate::backend::present::ErrorDialogData;
use crate::backend::prompt::ConfirmRequest;
use crate::commit_changes::platform::{RevealTarget, open_target};
use crate::dialogs::{SingleFieldConfig, SingleTextFieldDialog};
use crate::icons::AppIcon;
use crate::tab::TabCtx;
use crate::worktrees;

/// The menu target of a worktree row (`path` carries the worktree's directory, see `worktrees::path_key`).
pub fn row_target(tab: &TabCtx, info: &WorktreeInfo) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::Worktree,
        tab: tab.clone(),
        branch: worktrees::checked_out_branch(info),
        upstream: None,
        commit: None,
        path: Some(worktrees::path_key(&info.path)),
        is_current: info.is_current,
        checked_out_elsewhere: false,
    }
}

/// The menu target of the Worktrees header.
pub fn header_target(tab: &TabCtx) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::WorktreesHeader,
        tab: tab.clone(),
        branch: None,
        upstream: None,
        commit: None,
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    }
}

/// The worktree row menu (slot `End`). Empty when the worktree vanished from the list.
pub fn worktree_row(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    let Some(info) = target
        .path
        .as_ref()
        .and_then(|key| worktrees::find_info(&target.tab, key, cx))
    else {
        return items;
    };
    let tab = &target.tab;
    let present = !info.prunable && !info.bare;

    let path = info.path.clone();
    items.push(
        MenuItem::action("worktree-open", "Open in new tab", move |window, cx| {
            worktrees::open_in_new_tab(window, cx, path.clone());
        })
        .with_icon(AppIcon::Open)
        .enabled(present && !info.is_current),
    );
    let path = info.path.clone();
    items.push(
        MenuItem::action("worktree-reveal", "Open in file manager", move |_, cx| {
            open_target(cx, &RevealTarget::Folder(path.clone()));
        })
        .with_icon(AppIcon::FolderOpen)
        .enabled(present),
    );
    let (copy_tab, text) = (tab.clone(), worktrees::shown(&info.path));
    items.push(
        MenuItem::action("worktree-copy-path", "Copy path", move |_, cx| {
            super::refs::copy_with_toast(cx, &copy_tab, &text);
        })
        .with_icon(AppIcon::Copy),
    );

    if info.id.is_some() {
        items.push(MenuItem::Separator);
        let path = info.path.clone();
        let lock_tab = tab.clone();
        if info.locked.is_some() {
            items.push(
                MenuItem::action("worktree-unlock", "Unlock", move |window, cx| {
                    let path = path.clone();
                    lock_tab.with_tab(window, cx, move |t, w, c| {
                        t.unlock_worktree(path, w, c);
                    });
                })
                .with_icon(AppIcon::Lock),
            );
        } else {
            items.push(
                MenuItem::action("worktree-lock", "Lock…", move |window, cx| {
                    open_lock_dialog(window, cx, lock_tab.clone(), path.clone());
                })
                .with_icon(AppIcon::Lock),
            );
        }
        if !info.is_current {
            let (remove_tab, info) = (tab.clone(), info);
            items.push(
                MenuItem::action("worktree-remove", "Remove worktree", move |window, cx| {
                    remove_worktree(window, cx, &remove_tab, &info);
                })
                .with_icon(AppIcon::Delete),
            );
        }
    }
    extend(cx, MenuSlot::End, target, &mut items);
    if matches!(items.last(), Some(MenuItem::Separator)) {
        items.pop();
    }
    items
}

/// The Worktrees header menu (slot `End`): Add worktree… and Prune worktrees.
pub fn worktrees_header(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let tab = &target.tab;
    let add_tab = tab.clone();
    let prune_tab = tab.clone();
    let mut items = vec![
        MenuItem::action("worktrees-add", "Add worktree…", move |window, cx| {
            crate::dialogs::worktree::open_add_worktree(window, cx, add_tab.clone());
        })
        .with_icon(AppIcon::Add),
        MenuItem::action("worktrees-prune", "Prune worktrees", move |window, cx| {
            prune_tab.with_tab(window, cx, |t, w, c| {
                t.prune_worktrees(w, c);
            });
        })
        .with_icon(AppIcon::Refresh),
    ];
    extend(cx, MenuSlot::End, target, &mut items);
    if matches!(items.last(), Some(MenuItem::Separator)) {
        items.pop();
    }
    items
}

/// Asks for an optional reason, then locks the worktree.
fn open_lock_dialog(window: &mut Window, cx: &mut App, tab: TabCtx, path: PathBuf) {
    let mut config = SingleFieldConfig::new(
        AppIcon::Lock,
        "Lock worktree",
        "Lock",
        move |reason, window, cx| {
            let reason = Some(reason.trim().to_owned()).filter(|r| !r.is_empty());
            let path = path.clone();
            tab.with_tab(window, cx, move |t, w, c| {
                t.lock_worktree(path, reason, w, c);
            });
        },
    );
    config.subtitle = Some("A locked worktree is never pruned or removed by accident".into());
    config.hint = Some("Reason (optional)".into());
    config.is_valid = std::rc::Rc::new(|_| true);
    SingleTextFieldDialog::open(window, cx, config);
}

/// "Remove worktree": refuses a worktree that another tab shows, otherwise confirms and removes it. A locked
/// worktree needs `force` (Git asks for the flag twice); the confirmation says so.
fn remove_worktree(window: &mut Window, cx: &mut App, tab: &TabCtx, info: &WorktreeInfo) {
    let name = worktrees::display_name(&info.path);
    if worktrees::open_in_other_tab(cx, tab, &info.path) {
        crate::app_view::show_error(
            cx,
            ErrorDialogData::new(
                "Removing worktree failed",
                format!(
                    "The worktree '{name}' is open in a tab. Close the tab of this worktree first."
                ),
            ),
        );
        return;
    }
    let locked = info.locked.clone();
    let message = match &locked {
        Some(reason) if !reason.is_empty() => format!(
            "The worktree '{name}' is locked ({reason}). Removing it deletes its directory, including any uncommitted changes."
        ),
        Some(_) => format!(
            "The worktree '{name}' is locked. Removing it deletes its directory, including any uncommitted changes."
        ),
        None => {
            format!("Remove the worktree '{name}'? Its directory is deleted; its branch is kept.")
        }
    };
    let mut request = ConfirmRequest::new("Remove worktree", message);
    request.details = vec![worktrees::shown(&info.path).into()];
    request.confirm_label = "Remove".into();
    request.destructive = true;
    let (tab, path) = (tab.clone(), info.path.clone());
    crate::dialogs::confirm::open_with(
        window,
        cx,
        request,
        Box::new(move |yes, window, cx| {
            if yes {
                tab.with_tab(window, cx, move |t, w, c| {
                    t.remove_worktree(path, locked.is_some(), w, c);
                });
            }
        }),
    );
}
