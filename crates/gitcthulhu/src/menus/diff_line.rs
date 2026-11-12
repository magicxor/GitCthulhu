//! The diff line menu (owned by G7a after G0b; 09 §4.6).

use gpui_kit::App;

use super::{MenuItem, MenuSlot, MenuTarget, extend};

/// The diff line menu (slot `End`).
pub fn diff_line(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::End, target, &mut items);
    items
}
