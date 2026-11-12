//! The generic choice dialog (owned by G10 after G0b): 2-3 buttons plus Cancel and an optional checkbox (pull
//! strategy, mainline parent, empty pick, interrupted undo). G0b ships a working minimal dialog without the checkbox
//! (the runner's oplog recovery uses it); G10 adds the checkbox and the exact layout.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div};

use super::{DialogFrame, DialogSpec};
use crate::metrics::{self, dp};
use crate::theme::TextStyleExt as _;
use crate::widgets::PrimaryButton;

/// Width of the choice dialog's content.
pub const CHOICE_W: f32 = 420.;

/// What the dialog asks.
#[derive(Clone, Debug, PartialEq)]
pub struct ChoiceSpec {
    pub title: SharedString,
    pub message: SharedString,
    /// The choices, left to right (a Cancel button is added before them).
    pub options: Vec<SharedString>,
    /// An optional checkbox ("Remember for this repository").
    pub checkbox: Option<SharedString>,
}

/// The answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChoiceResult {
    /// The index of the chosen option; `None` = Cancel.
    pub option: Option<usize>,
    /// The checkbox state.
    pub checked: bool,
}

/// Receives the answer (not called when the dialog is dismissed with Escape).
pub type OnChoice = Box<dyn FnOnce(ChoiceResult, &mut Window, &mut App)>;

/// Opens the dialog.
pub fn open(window: &mut Window, cx: &mut App, spec: ChoiceSpec, on_choice: OnChoice) {
    let answer: Rc<RefCell<Option<OnChoice>>> = Rc::new(RefCell::new(Some(on_choice)));
    DialogFrame::open(
        window,
        cx,
        DialogSpec {
            width: CHOICE_W + 2. * metrics::DIALOG_PAD,
            est_height: 200.,
        },
        move |_, _| {
            let choose = |option: Option<usize>| {
                let answer = answer.clone();
                move |window: &mut Window, cx: &mut App| {
                    window.close_dialog(cx);
                    if let Some(f) = answer.borrow_mut().take() {
                        f(
                            ChoiceResult {
                                option,
                                checked: false,
                            },
                            window,
                            cx,
                        );
                    }
                }
            };
            let buttons = spec.options.iter().enumerate().map(|(ix, label)| {
                PrimaryButton::new(("choice-option", ix), label.clone()).on_click(choose(Some(ix)))
            });
            div()
                .w(dp(CHOICE_W))
                .flex()
                .flex_col()
                .child(div().text_body1().semibold().child(spec.title.clone()))
                .child(div().pt(dp(16.)).text_body2().child(spec.message.clone()))
                .child(
                    div()
                        .pt(dp(24.))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(dp(8.))
                        .child(
                            PrimaryButton::new("choice-cancel", "Cancel")
                                .cancel()
                                .on_click(choose(None)),
                        )
                        .children(buttons),
                )
                .into_any_element()
        },
    );
}
