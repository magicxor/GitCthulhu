//! The two input controls the settings pages need beyond the shared widgets: the number field (09 §3.18 `IntInput`:
//! digits only, right-aligned, the error border flashes on invalid input) and the multi-line outlined editor of
//! the custom theme.

use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div,
};

use crate::metrics::{self, dp, settings as sm};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::{BorderState, FieldEvent};
use crate::widgets::observed;

/// Most digits a number field takes (keeps every value inside `u32`).
const MAX_DIGITS: usize = 9;

/// Events of [`NumberField`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumberEvent {
    /// The value changed; `None` while the field is empty.
    Changed(Option<u64>),
}

/// A single-line field that accepts digits only. Anything else that is typed or pasted is removed again and the
/// error border flashes for [`sm::FLASH_MS`].
pub struct NumberField {
    input: Entity<InputState>,
    id: SharedString,
    disabled: bool,
    flashing: bool,
    flash: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<NumberEvent> for NumberField {}

impl NumberField {
    /// An empty field. `id` names the frame for tests and must be unique in the frame.
    pub fn new(id: impl Into<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.changed(window, cx);
                }
            });
        Self {
            input,
            id: id.into(),
            disabled: false,
            flashing: false,
            flash: None,
            _subscription: subscription,
        }
    }

    /// The text of the field.
    pub fn text(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    /// Shows `value` (without flashing).
    pub fn set_value(&mut self, value: u64, window: &mut Window, cx: &mut Context<Self>) {
        let text = SharedString::from(value.to_string());
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Enables or disables editing.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            cx.notify();
        }
    }

    /// Moves keyboard focus into the field.
    #[cfg(test)]
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Rejects the current value: the error border flashes.
    pub fn reject(&mut self, cx: &mut Context<Self>) {
        self.flash_error(cx);
    }

    /// Whether the error border is flashing.
    #[cfg(test)]
    pub fn is_flashing(&self) -> bool {
        self.flashing
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let digits: String = text
            .chars()
            .filter(char::is_ascii_digit)
            .take(MAX_DIGITS)
            .collect();
        if digits != text.as_ref() {
            // Drop the rejected input; the edit re-enters here with the clean text.
            self.input
                .update(cx, |input, cx| input.set_value(digits, window, cx));
            self.flash_error(cx);
            return;
        }
        cx.emit(NumberEvent::Changed(digits.parse().ok()));
    }

    fn flash_error(&mut self, cx: &mut Context<Self>) {
        self.flashing = true;
        cx.notify();
        self.flash = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(sm::FLASH_MS))
                .await;
            this.update(cx, |this, cx| {
                this.flashing = false;
                this.flash = None;
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for NumberField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let focused = self
            .input
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        let border = BorderState::of(focused, self.disabled, self.flashing).color(&tokens);
        observed(self.id.clone())
            .w(dp(sm::NUMBER_FIELD_W))
            .min_h(dp(metrics::FIELD_H))
            .flex()
            .items_center()
            .bg(tokens.background)
            .border(dp(metrics::FIELD_BORDER))
            .border_color(border)
            .rounded(dp(metrics::RADIUS))
            .px(dp(
                crate::widgets::field::FIELD_TEXT_INSET - metrics::FIELD_BORDER
            ))
            .text_body1()
            .text_color(if self.disabled {
                tokens.field_border_disabled()
            } else {
                tokens.on_background
            })
            .child(
                div().flex_1().child(
                    crate::widgets::field::field_input(
                        Input::new(&self.input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .disabled(self.disabled),
                    )
                    .text_right(),
                ),
            )
    }
}

/// A multi-line outlined editor of fixed height (the custom theme JSON).
pub struct ThemeEditor {
    input: Entity<TextareaState>,
    id: SharedString,
    error: bool,
    _subscription: Subscription,
}

impl EventEmitter<FieldEvent> for ThemeEditor {}

impl ThemeEditor {
    /// An empty editor. `id` names the frame for tests and must be unique in the frame.
    pub fn new(id: impl Into<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx));
        let subscription = cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.emit(FieldEvent::Changed);
            }
        });
        Self {
            input,
            id: id.into(),
            error: false,
            _subscription: subscription,
        }
    }

    /// The text of the editor.
    pub fn text(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    /// Replaces the text.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = SharedString::from(text.to_owned());
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Moves keyboard focus into the editor.
    #[cfg(test)]
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Marks the text as unusable (red border).
    pub fn set_error(&mut self, error: bool, cx: &mut Context<Self>) {
        if self.error != error {
            self.error = error;
            cx.notify();
        }
    }

    /// Whether the editor shows the error border.
    #[cfg(test)]
    pub fn has_error(&self) -> bool {
        self.error
    }
}

impl Render for ThemeEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let focused = self
            .input
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        let border = BorderState::of(focused, false, self.error).color(&tokens);
        observed(self.id.clone())
            .w(dp(sm::TEXT_FIELD_W))
            .h(dp(sm::THEME_EDITOR_H))
            .bg(tokens.background)
            .border(dp(metrics::FIELD_BORDER))
            .border_color(border)
            .rounded(dp(metrics::RADIUS))
            // Gitnuro's text field: padding h12 v8 inside a 2 dp border drawn over it, input text body1.
            .px(dp(12. - metrics::FIELD_BORDER))
            .py(dp(8. - metrics::FIELD_BORDER))
            .text_body1()
            .text_color(tokens.on_background)
            .child(
                Textarea::new(&self.input)
                    .appearance(false)
                    .bordered(false)
                    .h_full(),
            )
    }
}
