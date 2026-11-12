//! Raw pointer input for tests that need modifiers or non-primary buttons (spec 21 §8.1).
//!
//! The kit click helpers dispatch without modifiers and without the middle button; [`click_with`] dispatches the
//! raw mouse events at the centre of an observed element.

use gpui_kit::InputEvent as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    App, ElementId, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Window,
};

/// MouseDown and MouseUp with `mods` and `button` at the centre of `id`, preceded by a pointer move.
pub fn click_with(
    window: &mut Window,
    cx: &mut App,
    id: impl Into<ElementId>,
    mods: Modifiers,
    button: MouseButton,
) {
    window.render_frame(cx);
    let position = window.find(id).bounds().center();
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: mods,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
    window.dispatch_event(
        MouseDownEvent {
            button,
            position,
            modifiers: mods,
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
    window.dispatch_event(
        MouseUpEvent {
            button,
            position,
            modifiers: mods,
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}
