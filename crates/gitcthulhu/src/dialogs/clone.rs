//! The Clone dialog (owned by G10 after G0b; 09 §3.1): URL, directory (default from Settings), progress and Cancel;
//! it calls `AppView::clone_repository`.

use gpui_kit::{App, Window};

/// Opens the Clone dialog.
pub fn open(_window: &mut Window, cx: &mut App) {
    crate::app_view::not_implemented(cx, None, "Clone");
}
