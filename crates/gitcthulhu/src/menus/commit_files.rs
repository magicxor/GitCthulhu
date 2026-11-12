//! The committed-file menu (owned by G6 after G0b; 09 §4.8).

use gpui_kit::App;

use super::{MenuItem, MenuSlot, MenuTarget, extend};

/// The committed-file menu (slot `End`).
pub fn commit_file(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::End, target, &mut items);
    items
}
