//! The New / Edit remote dialog (owned by G10 after G0b; 09 §3.8).

use gpui_kit::{App, Window};

/// Opens the New remote dialog.
pub fn open_new_remote(_window: &mut Window, cx: &mut App, tab: crate::tab::TabCtx) {
    crate::app_view::not_implemented(cx, Some(tab.id), "New remote");
}

/// Opens the Edit remote dialog of remote `name`.
pub fn open_edit_remote(
    _window: &mut Window,
    cx: &mut App,
    tab: crate::tab::TabCtx,
    _name: String,
) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Edit remote");
}
