//! The commit-row menu of the log (owned by G4 after G0b; 08 §2.13); calls `menus::extend` at `End`.

use gpui_kit::App;

use super::{MenuItem, MenuSlot, MenuTarget, extend};

/// The commit-row menu (slot `End`).
pub fn commit(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::End, target, &mut items);
    items
}
