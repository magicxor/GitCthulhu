//! Branch, remote-branch, tag, stash, remote and submodule menus (owned by G3 after G0b; 07 §6.6, 08 §2.13),
//! reused by the log's chips (G4). Builders call `menus::extend` at every slot of spec 21 §2.13.2; the placeholders
//! return only the extenders' items.

use gpui_kit::App;

use super::{MenuItem, MenuSlot, MenuTarget, extend};

/// The local-branch menu (slots `AfterCheckout`, `AfterRenameGroup`).
pub fn local_branch(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::AfterCheckout, target, &mut items);
    extend(cx, MenuSlot::AfterRenameGroup, target, &mut items);
    items
}

/// The remote-branch menu (slot `AfterPushPullGroup`).
pub fn remote_branch(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::AfterPushPullGroup, target, &mut items);
    items
}

/// The tag menu (slot `End`).
pub fn tag(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::End, target, &mut items);
    items
}

/// The stash menu (slot `End`).
pub fn stash(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::End, target, &mut items);
    items
}

/// The remote menu (slot `End`).
pub fn remote(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::End, target, &mut items);
    items
}

/// The submodule menu (slot `End`).
pub fn submodule(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    extend(cx, MenuSlot::End, target, &mut items);
    items
}
