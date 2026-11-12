//! The status file menus: single file, several files, directory (owned by G5 after G0b; 08 §4.5).

use gpui_kit::App;

use super::{MenuItem, MenuSlot, MenuTarget, extend};

/// The menu of one or more status files (slot `End`).
pub fn status_file(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::End, target, &mut items);
    items
}
