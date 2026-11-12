//! The Stash with message dialog (owned by G10 after G0b; 09 §3.5).

use gpui_kit::{App, Window};

/// Opens the Stash with message dialog.
pub fn open_stash_with_message(_window: &mut Window, cx: &mut App, tab: crate::tab::TabCtx) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Stash with message");
}
