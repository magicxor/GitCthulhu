//! The Reset dialog (owned by G10 after G0b; 09 §3.6).

use gpui_kit::{App, Window};

use crate::backend::ObjectId;

/// Opens the Reset dialog for a reset to `commit`.
pub fn open(_window: &mut Window, cx: &mut App, tab: crate::tab::TabCtx, _commit: ObjectId) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Reset");
}
