//! The Quick actions dialog (owned by G2 after G0b; 09 §3.13, 07 §5.4).

use gpui_kit::{App, Window};

/// Opens the Quick actions dialog.
pub fn open(_window: &mut Window, cx: &mut App, tab: crate::tab::TabCtx) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Quick actions");
}
