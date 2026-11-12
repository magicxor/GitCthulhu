//! The Sign-off dialog (owned by G10 after G0b; 09 §3.12).

use gpui_kit::{App, Window};

/// Opens the Sign-off dialog.
pub fn open(_window: &mut Window, cx: &mut App, tab: crate::tab::TabCtx) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Sign-off");
}
