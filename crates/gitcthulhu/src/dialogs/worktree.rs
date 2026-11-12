//! The Add worktree dialog (D0.7; owned by X6 after G0b; spec 21 §5.7).

use gpui_kit::{App, Window};

/// Opens the Add worktree dialog.
pub fn open_add_worktree(_window: &mut Window, cx: &mut App, tab: crate::tab::TabCtx) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Add worktree");
}
