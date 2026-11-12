//! The New tag dialog (owned by G10 after G0b; 09 §3.4).

use gpui_kit::{App, Window};

use crate::backend::ObjectId;

/// Opens the New tag dialog for a tag on `target`.
pub fn open_new_tag(
    _window: &mut Window,
    cx: &mut App,
    tab: crate::tab::TabCtx,
    _target: ObjectId,
) {
    crate::app_view::not_implemented(cx, Some(tab.id), "New tag");
}
