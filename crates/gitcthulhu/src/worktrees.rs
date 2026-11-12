//! Linked worktrees in the GUI (D0.7; owned by X6; spec 21 §5.7, §2.13.3): the "Worktrees" side-panel section
//! (rows, context menus, the Add worktree dialog, the operations of `tab/ops/worktree.rs`), opening a worktree as
//! its own tab and the "Create worktree…" item of the local-branch menu.
//!
//! The side panel's generic code builds the section once [`section_enabled`] is true and asks [`render_row`] for the
//! rows; the backend (`worktree::*`) and the WORKTREES loader are milestone-1 code.

use std::path::{Path, PathBuf};

use gpui_kit::{AnyElement, App, SharedString, Window};

use crate::app_view::AppViewGlobal;
use crate::backend::present::DialogAction;
use crate::backend::{HeadState, RelaPath, WorktreeInfo};
use crate::icons::AppIcon;
use crate::menus::{self, MenuItem, MenuKind, MenuSlot, MenuTarget};
use crate::tab::TabCtx;

pub(crate) mod metrics;
#[cfg(feature = "qa")]
mod qa;
mod row;
#[cfg(test)]
mod tests;

pub use row::branch_label;

/// Element ids of the section's rows (the header is the side panel's `side:header:worktrees`).
pub mod ids {
    use std::path::Path;

    use gpui_kit::{ElementId, SharedString};

    /// A worktree row, keyed by the worktree's directory.
    pub fn row(path: &Path) -> SharedString {
        format!("side:worktree:{}", path.display()).into()
    }

    /// A named part of a row (`icon`, `text`, `branch`, `lock`, `prunable`).
    pub fn part(row: SharedString, part: &'static str) -> ElementId {
        (ElementId::from(row), part).into()
    }
}

/// Whether the side panel shows the Worktrees section (after Submodules).
pub fn section_enabled(_cx: &App) -> bool {
    true
}

/// A row of the Worktrees section (spec 21 §5.7): folder icon (open folder and bold text for this tab's worktree),
/// the directory name, then the lock and prunable markers and the checked-out branch. Double-click opens the
/// worktree in a new tab; right-click opens its menu.
pub fn render_row(ctx: &TabCtx, info: &WorktreeInfo, selected: bool, cx: &App) -> AnyElement {
    row::render(ctx, info, selected, cx)
}

/// The "Open worktree in new tab" extra button of the `BranchCheckedOutElsewhere` error dialog.
pub fn open_in_new_tab_action(path: PathBuf) -> Option<DialogAction> {
    Some(DialogAction {
        label: "Open worktree in new tab".into(),
        run: Box::new(move |window, cx| open_in_new_tab(window, cx, path)),
    })
}

/// Opens the worktree at `path` in a new, selected tab; when a tab already shows it, that tab is selected instead.
pub fn open_in_new_tab(window: &mut Window, cx: &mut App, path: PathBuf) {
    let Some(view) = cx.try_global::<AppViewGlobal>().and_then(|g| g.0.upgrade()) else {
        return;
    };
    view.update(cx, |app, cx| match tab_index_of(app, &path, cx) {
        Some(ix) => app.select_tab(ix, window, cx),
        None => app.open_repository(path, true, window, cx),
    });
}

/// The index of the tab (open or opening) that shows the worktree at `path`.
pub(crate) fn tab_index_of(app: &crate::app_view::AppView, path: &Path, cx: &App) -> Option<usize> {
    app.tabs()
        .iter()
        .position(|slot| slot.path(cx).is_some_and(|open| same_path(&open, path)))
}

/// Whether a tab other than the one of `current` shows the worktree at `path`.
pub(crate) fn open_in_other_tab(cx: &App, current: &TabCtx, path: &Path) -> bool {
    let Some(view) = cx.try_global::<AppViewGlobal>().and_then(|g| g.0.upgrade()) else {
        return false;
    };
    let app = view.read(cx);
    app.tabs().iter().any(|slot| {
        slot.repo()
            .is_none_or(|tab| tab.read(cx).ctx().id != current.id)
            && slot.path(cx).is_some_and(|open| same_path(&open, path))
    })
}

/// Whether two directory paths name the same directory (canonical forms when both exist).
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The key under which a worktree's directory travels in a [`MenuTarget`] (`path`).
pub(crate) fn path_key(path: &Path) -> RelaPath {
    RelaPath::from(path.to_string_lossy().as_ref())
}

/// The worktree info with the directory `key` in the tab's loaded list.
pub(crate) fn find_info(ctx: &TabCtx, key: &RelaPath, cx: &App) -> Option<WorktreeInfo> {
    let model = ctx.model.read(cx);
    model
        .worktrees
        .value
        .as_ref()?
        .iter()
        .find(|w| path_key(&w.path) == *key)
        .cloned()
}

/// The worktree whose directory is called `name` (QA verbs).
#[cfg(feature = "qa")]
pub(crate) fn find_by_name(ctx: &TabCtx, name: &str, cx: &App) -> Option<WorktreeInfo> {
    ctx.model
        .read(cx)
        .worktrees
        .value
        .as_ref()?
        .iter()
        .find(|w| display_name(&w.path) == name)
        .cloned()
}

/// The directory name of a worktree (its row text).
pub fn display_name(path: &Path) -> SharedString {
    crate::tab::ops::worktree::display_name(path).into()
}

/// The full ref a worktree has checked out (`None` when detached, unreadable or bare).
pub(crate) fn checked_out_branch(info: &WorktreeInfo) -> Option<crate::backend::FullName> {
    match &info.head {
        Some(HeadState::Branch { name, .. } | HeadState::Unborn { name }) if !info.bare => {
            Some(name.clone())
        }
        _ => None,
    }
}

/// "Create worktree…" for a local branch that is neither the current one nor checked out elsewhere.
fn create_worktree_item(target: &MenuTarget, items: &mut Vec<MenuItem>, _cx: &App) {
    let Some(branch) = target.branch.clone() else {
        return;
    };
    if target.is_current || target.checked_out_elsewhere {
        return;
    }
    let tab = target.tab.clone();
    items.push(
        MenuItem::action(
            "branch-create-worktree",
            "Create worktree…",
            move |window, cx| {
                crate::dialogs::worktree::open_add_worktree_for_branch(
                    window,
                    cx,
                    tab.clone(),
                    branch.clone(),
                );
            },
        )
        .with_icon(AppIcon::Worktree),
    );
}

/// Registers the menu extender of the local-branch menu and the QA verbs.
pub fn register(cx: &mut App) {
    menus::register_extender(
        cx,
        MenuKind::LocalBranch,
        MenuSlot::AfterCheckout,
        create_worktree_item,
    );
    #[cfg(feature = "qa")]
    qa::register(cx);
}

/// Lossy display text of a path in a message.
pub(crate) fn shown(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
