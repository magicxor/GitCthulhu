//! The generic choice dialog (G10): a title, a message, an optional checkbox ("Remember for this repository"),
//! Cancel and two or three option buttons (pull strategy, mainline parent, empty pick, interrupted undo).

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, div,
};

use super::register::common;
use crate::metrics::dp;
use crate::theme::TextStyleExt as _;
use crate::widgets::{CheckboxText, PrimaryButton, observed};

pub use crate::metrics::dialogs::CHOICE_W;

/// Element ids of the choice dialog (options are `("choice-option", index)`).
pub mod ids {
    /// The dialog content.
    pub const CHOICE_DIALOG: &str = "choice-dialog";
    /// The optional checkbox.
    pub const CHOICE_CHECKBOX: &str = "choice-checkbox";
    /// The Cancel button.
    pub const CHOICE_CANCEL: &str = "choice-cancel";
    /// Prefix of the option buttons: `(CHOICE_OPTION, index)`.
    pub const CHOICE_OPTION: &str = "choice-option";
}

/// What the dialog asks.
#[derive(Clone, Debug, PartialEq)]
pub struct ChoiceSpec {
    /// Bold title.
    pub title: SharedString,
    /// Explanation under the title.
    pub message: SharedString,
    /// The choices, left to right (a Cancel button is added before them).
    pub options: Vec<SharedString>,
    /// An optional checkbox ("Remember for this repository"), unchecked at first.
    pub checkbox: Option<SharedString>,
}

/// The answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChoiceResult {
    /// The index of the chosen option; `None` = Cancel.
    pub option: Option<usize>,
    /// The checkbox state (`false` when the dialog has no checkbox).
    pub checked: bool,
}

/// Receives the answer (not called when the dialog is dismissed with Escape).
pub type OnChoice = Box<dyn FnOnce(ChoiceResult, &mut Window, &mut App)>;

/// Opens the dialog.
pub fn open(window: &mut Window, cx: &mut App, spec: ChoiceSpec, on_choice: OnChoice) {
    let view = cx.new(|_| ChoiceDialog {
        spec,
        checked: false,
        answer: Rc::new(RefCell::new(Some(on_choice))),
    });
    common::open_view(window, cx, CHOICE_W, 200., view);
}

/// The content of an open choice dialog.
struct ChoiceDialog {
    spec: ChoiceSpec,
    checked: bool,
    answer: Rc<RefCell<Option<OnChoice>>>,
}

impl ChoiceDialog {
    fn respond(&mut self, option: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        let result = ChoiceResult {
            option,
            checked: self.spec.checkbox.is_some() && self.checked,
        };
        if let Some(f) = self.answer.borrow_mut().take() {
            f(result, window, cx);
        }
    }
}

impl Render for ChoiceDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        let options = self.spec.options.iter().enumerate().map(|(ix, label)| {
            let this = this.clone();
            PrimaryButton::new((ids::CHOICE_OPTION, ix), label.clone()).on_click(
                move |window, cx| {
                    this.update(cx, |d, cx| d.respond(Some(ix), window, cx))
                        .ok();
                },
            )
        });
        let cancel = PrimaryButton::new(ids::CHOICE_CANCEL, "Cancel")
            .cancel()
            .on_click({
                let this = this.clone();
                move |window, cx| {
                    this.update(cx, |d, cx| d.respond(None, window, cx)).ok();
                }
            });
        let checkbox = self.spec.checkbox.clone().map(|label| {
            let this = this.clone();
            div().pt(dp(8.)).child(
                CheckboxText::new(ids::CHOICE_CHECKBOX, self.checked, label).on_toggle(
                    move |checked, _, cx| {
                        this.update(cx, |d, cx| {
                            d.checked = checked;
                            cx.notify();
                        })
                        .ok();
                    },
                ),
            )
        });
        observed(ids::CHOICE_DIALOG)
            .w(dp(CHOICE_W))
            .flex()
            .flex_col()
            .child(div().text_body1().semibold().child(self.spec.title.clone()))
            .child(
                div()
                    .pt(dp(16.))
                    .text_body2()
                    .child(self.spec.message.clone()),
            )
            .when_some(checkbox, |col, checkbox| col.child(checkbox))
            .child(
                div()
                    .pt(dp(24.))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(dp(8.))
                    .child(cancel)
                    .children(options),
            )
    }
}
