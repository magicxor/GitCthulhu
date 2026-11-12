//! Text fields (Gitnuro 07 §10.1, §10.2; 08 §4.6): the outlined field used everywhere, the filled commit-message
//! area and the search field.
//!
//! The kit `Input`/`Textarea` does the editing; these views draw Gitnuro's frame around it (2 dp border whose
//! colour reflects focus, disabled and error state, 38 dp minimum height).

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};

use super::{IconButton, observed};
use crate::icons::AppIcon;
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};

/// Events of the text field views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldEvent {
    /// The text changed.
    Changed,
    /// Enter was pressed (without Shift).
    Enter,
}

/// Which border an [`OutlinedField`] draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderState {
    Focused,
    Unfocused,
    Disabled,
    Error,
}

impl BorderState {
    /// Error wins over disabled, disabled over focus (Gitnuro `outlinedTextFieldColors`).
    pub fn of(focused: bool, disabled: bool, error: bool) -> Self {
        if error {
            BorderState::Error
        } else if disabled {
            BorderState::Disabled
        } else if focused {
            BorderState::Focused
        } else {
            BorderState::Unfocused
        }
    }

    /// The border colour in `tokens`.
    pub fn color(self, tokens: &Tokens) -> Hsla {
        match self {
            BorderState::Focused => tokens.primary_variant,
            BorderState::Unfocused => tokens.field_border(),
            BorderState::Disabled => tokens.field_border_disabled(),
            BorderState::Error => tokens.error,
        }
    }
}

/// Gitnuro `AdjustableOutlinedTextField`: single-line input in a 2 dp frame, at least 38 dp high.
pub struct OutlinedField {
    input: Entity<InputState>,
    id: SharedString,
    disabled: bool,
    error: bool,
    _subscription: Subscription,
}

impl EventEmitter<FieldEvent> for OutlinedField {}

impl OutlinedField {
    /// An empty field. `id` names the frame for tests and must be unique in the frame.
    pub fn new(id: impl Into<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(id.into(), false, window, cx)
    }

    /// A password field (masked input).
    pub fn password(
        id: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(id.into(), true, window, cx)
    }

    fn build(id: SharedString, masked: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let state = InputState::new(window, cx);
            if masked { state.masked(true) } else { state }
        });
        let subscription = cx.subscribe(&input, |_, _, event: &InputEvent, cx| match event {
            InputEvent::Change => cx.emit(FieldEvent::Changed),
            InputEvent::PressEnter { shift: false, .. } => cx.emit(FieldEvent::Enter),
            _ => {}
        });
        Self {
            input,
            id,
            disabled: false,
            error: false,
            _subscription: subscription,
        }
    }

    /// The hint shown while the field is empty (13 sp, `on_background_secondary`).
    pub fn hint(
        self,
        hint: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hint = hint.into();
        self.input
            .update(cx, |input, cx| input.set_placeholder(hint, window, cx));
        self
    }

    /// The current text.
    pub fn text(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    /// Replaces the text.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = SharedString::from(text.to_owned());
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Moves keyboard focus into the field.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Disables or enables editing.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    /// Marks the field as invalid (red border).
    pub fn set_error(&mut self, error: bool, cx: &mut Context<Self>) {
        self.error = error;
        cx.notify();
    }

    /// The border the field draws in its current state.
    pub fn border_state(&self, window: &Window, cx: &App) -> BorderState {
        let focused = self
            .input
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        BorderState::of(focused, self.disabled, self.error)
    }
}

impl Focusable for OutlinedField {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for OutlinedField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let border = self.border_state(window, cx).color(&tokens);
        observed(self.id.clone())
            .w_full()
            .min_h(dp(metrics::FIELD_H))
            .flex()
            .items_center()
            .bg(tokens.background)
            .border(dp(metrics::FIELD_BORDER))
            .border_color(border)
            .rounded(dp(metrics::RADIUS))
            .px(dp(10.))
            .text_body1()
            .text_color(if self.disabled {
                tokens.field_border_disabled()
            } else {
                tokens.on_background
            })
            .child(
                div().flex_1().child(
                    Input::new(&self.input)
                        .appearance(false)
                        .bordered(false)
                        .focus_bordered(false)
                        .disabled(self.disabled),
                ),
            )
    }
}

/// Material filled text area with a label (the commit message box, 08 §4.6).
pub struct FilledTextArea {
    input: Entity<TextareaState>,
    id: SharedString,
    label: SharedString,
    disabled: bool,
    _subscription: Subscription,
}

impl EventEmitter<FieldEvent> for FilledTextArea {}

impl FilledTextArea {
    /// An empty area with `label` (body 2, `primary_variant`). `id` must be unique in the frame.
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx));
        let subscription = cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.emit(FieldEvent::Changed);
            }
        });
        Self {
            input,
            id: id.into(),
            label: label.into(),
            disabled: false,
            _subscription: subscription,
        }
    }

    /// Changes the label.
    pub fn set_label(&mut self, label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.label = label.into();
        cx.notify();
    }

    /// The current text.
    pub fn text(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    /// Replaces the text.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = SharedString::from(text.to_owned());
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Enables or disables editing.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    /// Moves keyboard focus into the area.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }
}

impl Focusable for FilledTextArea {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for FilledTextArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let focused = self
            .input
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        // Material filled field: `on_background` at 4 % over the pane, an indicator line below.
        let fill = tokens.background.blend(tokens.on_background.alpha(0.04));
        let indicator = if focused {
            tokens.primary_variant
        } else {
            tokens.on_background.alpha(0.42)
        };
        observed(self.id.clone())
            .size_full()
            .flex()
            .flex_col()
            .bg(fill)
            .rounded_t(dp(metrics::RADIUS))
            .border_b(dp(if focused { 2. } else { 1. }))
            .border_color(indicator)
            .px(dp(16.))
            .pt(dp(8.))
            .child(
                div()
                    .flex_none()
                    .text_body2()
                    .text_color(tokens.primary_variant)
                    .child(self.label.clone()),
            )
            .child(
                div().flex_1().min_h_0().text_body1().child(
                    Textarea::new(&self.input)
                        .appearance(false)
                        .bordered(false)
                        .disabled(self.disabled)
                        .h_full(),
                ),
            )
    }
}

/// Gitnuro `SearchTextField`: a field on a `background` strip with 4 dp padding and a trailing clear button.
pub struct SearchField {
    field: Entity<OutlinedField>,
    id: SharedString,
    _subscription: Subscription,
}

/// Events of [`SearchField`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEvent {
    /// The query changed.
    Changed,
    /// The close button was pressed.
    Close,
}

impl EventEmitter<SearchEvent> for SearchField {}

impl SearchField {
    /// A search field with `hint` (for example "Search files by name or path"). `id` must be unique in the frame;
    /// the inner field is `"<id>-field"` and the close button `"<id>-close"`.
    pub fn new(
        id: impl Into<SharedString>,
        hint: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let id: SharedString = id.into();
        let hint = hint.into();
        let field_id = SharedString::from(format!("{id}-field"));
        let field = cx.new(|cx| OutlinedField::new(field_id, window, cx).hint(hint, window, cx));
        let subscription = cx.subscribe(&field, |_, _, event: &FieldEvent, cx| {
            if matches!(event, FieldEvent::Changed) {
                cx.emit(SearchEvent::Changed);
            }
        });
        Self {
            field,
            id,
            _subscription: subscription,
        }
    }

    /// The query text.
    pub fn query(&self, cx: &App) -> SharedString {
        self.field.read(cx).text(cx)
    }

    /// Moves keyboard focus into the field.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.field.update(cx, |f, cx| f.focus(window, cx));
    }
}

impl Focusable for SearchField {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.read(cx).focus_handle(cx)
    }
}

impl Render for SearchField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let close_id = SharedString::from(format!("{}-close", self.id));
        let this = cx.entity().downgrade();
        observed(self.id.clone())
            .w_full()
            .p(dp(4.))
            .bg(tokens.background)
            .flex()
            .flex_row()
            .items_center()
            .child(div().flex_1().child(self.field.clone()))
            .child(
                div().flex_none().ml(dp(4.)).mr(dp(8.)).child(
                    IconButton::new(close_id, AppIcon::Close)
                        .icon_size(metrics::ICON_SM)
                        .on_click(move |_, cx| {
                            this.update(cx, |_, cx| cx.emit(SearchEvent::Close)).ok();
                        }),
                ),
            )
    }
}
