//! The embedded-terminal settings rows (D0.4; owned by X1 after G0b; spec 21 §5.4); they replace the Terminal
//! page body when `terminal::settings_available` is true.

use gpui_kit::{AnyElement, App, IntoElement, Window, div};

/// The page content. Milestone 1: an empty element.
pub fn page(_window: &mut Window, _cx: &mut App) -> AnyElement {
    div().into_any_element()
}
