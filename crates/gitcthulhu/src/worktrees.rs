//! Linked worktrees in the GUI (D0.7; owned by X6 after G0b; spec 21 §5.7, §2.13.3). Milestone 1: no side-panel
//! section, no "Open worktree in new tab" action; the backend and the WORKTREES loader are complete.

use std::path::PathBuf;

use gpui_kit::{AnyElement, App, IntoElement, Styled as _, div};

use crate::backend::WorktreeInfo;
use crate::backend::present::DialogAction;
use crate::tab::TabCtx;

/// Whether the side panel shows the Worktrees section. Milestone 1: `false`.
pub fn section_enabled(_cx: &App) -> bool {
    false
}

/// A row of the Worktrees section. Milestone 1: an empty row.
pub fn render_row(_ctx: &TabCtx, _info: &WorktreeInfo, _selected: bool, _cx: &App) -> AnyElement {
    div().w_full().into_any_element()
}

/// The "Open worktree in new tab" extra button of the `BranchCheckedOutElsewhere` error dialog. Milestone 1: `None`.
pub fn open_in_new_tab_action(_path: PathBuf) -> Option<DialogAction> {
    None
}

/// Registers the worktree menu extenders and actions (X6).
pub fn register(_cx: &mut App) {}
