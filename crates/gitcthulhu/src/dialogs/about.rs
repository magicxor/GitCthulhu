//! The About dialog incl. the helper status (owned by G10 after G0b; 09 §3.14).

use gpui_kit::{App, Window};

/// Opens the About dialog.
pub fn open(_window: &mut Window, cx: &mut App) {
    crate::app_view::not_implemented(cx, None, "About");
}
