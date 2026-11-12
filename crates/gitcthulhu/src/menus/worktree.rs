//! Worktree row and Worktrees header menus (D0.7; owned by X6 after G0b; spec 21 §5.7).

use gpui_kit::App;

use super::{MenuItem, MenuTarget};

/// The worktree row menu. Milestone 1: empty.
pub fn worktree_row(_target: &MenuTarget, _cx: &App) -> Vec<MenuItem> {
    Vec::new()
}

/// The Worktrees header menu. Milestone 1: empty.
pub fn worktrees_header(_target: &MenuTarget, _cx: &App) -> Vec<MenuItem> {
    Vec::new()
}
