//! The Accounts settings page (D0.1; owned by X4 after G0b; spec 21 §5.4).

use gpui_kit::{AnyElement, App, IntoElement, Window, div};

/// The page content. Milestone 1: an empty element.
pub fn page(_window: &mut Window, _cx: &mut App) -> AnyElement {
    div().into_any_element()
}
