//! The Create pull request dialog (D0.1; owned by X4 after G0b; spec 21 §5.5).

use gpui_kit::{App, Window};

use crate::backend::FullName;

/// Opens the Create pull request dialog for `branch`.
pub fn open(_window: &mut Window, cx: &mut App, tab: crate::tab::TabCtx, _branch: FullName) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Create pull request");
}
