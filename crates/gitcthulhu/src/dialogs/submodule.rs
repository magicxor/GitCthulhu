//! The New submodule dialog (owned by G10 after G0b; 09 §3.9).

use gpui_kit::{App, Window};

/// Opens the New submodule dialog.
pub fn open_new_submodule(_window: &mut Window, cx: &mut App, tab: crate::tab::TabCtx) {
    crate::app_view::not_implemented(cx, Some(tab.id), "New submodule");
}
