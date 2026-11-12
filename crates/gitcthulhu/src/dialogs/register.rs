//! Dialog-level key bindings (owned by G10) and the home of the helpers the dialogs share (`common`).
//!
//! Escape is handled by the kit's dialog host (`DialogFrame`), Enter in a text field by the field itself
//! (`FieldEvent::Enter`). The one action bound here, [`Accept`], lets a dialog without a text field accept with
//! Enter: a view puts [`ACCEPT_CONTEXT`] on its root, tracks its own focus handle and handles the action.

use gpui_kit::{App, KeyBinding};

pub(crate) mod common;
#[cfg(test)]
mod tests;

gpui_kit::actions!(gitcthulhu_dialogs, [Accept]);

/// Key context of dialogs that accept with Enter (confirmations that are not destructive).
pub const ACCEPT_CONTEXT: &str = "DialogAccept";

/// Registers the dialogs' key bindings.
pub fn register(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", Accept, Some(ACCEPT_CONTEXT))]);
}
