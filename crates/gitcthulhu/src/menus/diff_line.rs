//! The diff line menu (owned by G7a; 09 §4.6): "Discard line" on a changed line of an unstaged, modified file.

use std::rc::Rc;

use gpui_kit::{App, Window};

use super::{MenuItem, MenuSlot, MenuTarget, extend};
use crate::icons::AppIcon;

/// Id of the "Discard line" item (rendered as `menu-item-diff-discard-line`).
pub const DISCARD_LINE: &str = "diff-discard-line";

/// The diff line menu (slot `End`) without line actions: only extender items.
pub fn diff_line(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    diff_line_with(target, None, cx)
}

/// The diff line menu: "Discard line" when `discard` is given (the caller checked 09 §4.6's conditions: no text
/// selected, changed line, unstaged, modified), then the extender items of slot `End`.
pub fn diff_line_with(
    target: &MenuTarget,
    discard: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if let Some(discard) = discard {
        items.push(MenuItem::Action {
            id: DISCARD_LINE.into(),
            icon: Some(AppIcon::Undo),
            label: "Discard line".into(),
            enabled: true,
            on_click: discard,
        });
    }
    extend(cx, MenuSlot::End, target, &mut items);
    items
}
