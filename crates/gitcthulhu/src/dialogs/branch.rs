//! Branch dialogs (owned by G10 after G0b): New branch (09 §3.2), Rename branch (3.3), Change upstream (3.7),
//! Choose another local name (spec 21 §2.9).

use std::rc::Rc;

use gpui_kit::{App, Window};

use crate::backend::{FullName, StartPoint};

/// Opens the New branch dialog for a branch starting at `start`.
pub fn open_new_branch(
    _window: &mut Window,
    cx: &mut App,
    tab: crate::tab::TabCtx,
    _start: StartPoint,
) {
    crate::app_view::not_implemented(cx, Some(tab.id), "New branch");
}

/// Opens the Rename branch dialog.
pub fn open_rename_branch(
    _window: &mut Window,
    cx: &mut App,
    tab: crate::tab::TabCtx,
    _branch: FullName,
) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Rename branch");
}

/// Opens the Change upstream dialog of `branch`.
pub fn open_change_upstream(
    _window: &mut Window,
    cx: &mut App,
    tab: crate::tab::TabCtx,
    _branch: FullName,
) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Change upstream");
}

/// The "Choose another local name" single-field dialog (checkout of a remote branch whose name exists); calls
/// `on_accept` with the chosen name.
pub fn choose_local_name(
    _window: &mut Window,
    cx: &mut App,
    _suggested: String,
    _on_accept: Rc<dyn Fn(String, &mut Window, &mut App)>,
) {
    crate::app_view::not_implemented(cx, None, "Choose local name");
}
