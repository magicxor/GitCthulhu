//! The error dialog (owned by G10 after G0b; 09 §3.17, D0.6): title with error icon, message, hook output
//! (expanded by default), details, Retry / extra / OK buttons, warning mode with a single OK. G0b ships a working
//! minimal dialog so the error queue and the hook flows run end to end; G10 implements the exact layout.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
    div,
};

use super::{DialogFrame, DialogSpec};
use crate::backend::present::{DialogAction, ErrorDialogData, RetryAction};
use crate::icons::{AppIcon, icon};
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{PrimaryButton, observed};

/// Width of the error dialog's content column (09 §3.17).
pub const ERROR_W: f32 = 580.;

/// Element ids of the error dialog.
pub mod ids {
    /// The dialog content.
    pub const ERROR_DIALOG: &str = "error-dialog";
    /// The hook output area.
    pub const ERROR_HOOK_OUTPUT: &str = "error-hook-output";
    /// The OK button.
    pub const ERROR_OK: &str = "error-ok";
    /// The retry button.
    pub const ERROR_RETRY: &str = "error-retry";
    /// The extra action button.
    pub const ERROR_EXTRA: &str = "error-extra";
}

type OnClose = Box<dyn FnOnce(&mut Window, &mut App)>;

/// The content of an open error dialog.
pub struct ErrorDialog {
    data: ErrorDialogData,
    retry: Option<RetryAction>,
    extra: Option<DialogAction>,
    on_close: Rc<RefCell<Option<OnClose>>>,
}

impl ErrorDialog {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        if let Some(on_close) = self.on_close.borrow_mut().take() {
            on_close(window, cx);
        }
    }
}

impl Render for ErrorDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let (role, color) = if self.data.warning {
            (AppIcon::Warning, tokens.secondary)
        } else {
            (AppIcon::Error, tokens.error)
        };
        let this = cx.weak_entity();
        let ok = PrimaryButton::new(ids::ERROR_OK, "OK").on_click({
            let this = this.clone();
            move |window, cx| {
                this.update(cx, |d, cx| d.close(window, cx)).ok();
            }
        });
        let retry = self.retry.as_ref().map(|r| {
            let this = this.clone();
            PrimaryButton::new(ids::ERROR_RETRY, r.label.clone()).on_click(move |window, cx| {
                let retry = this
                    .update(cx, |d, cx| {
                        let retry = d.retry.take();
                        d.close(window, cx);
                        retry
                    })
                    .ok()
                    .flatten();
                if let Some(retry) = retry {
                    let run = retry.run;
                    retry.tab.update(cx, |tab, cx| run(tab, window, cx)).ok();
                }
            })
        });
        let extra = self.extra.as_ref().map(|e| {
            let this = this.clone();
            PrimaryButton::new(ids::ERROR_EXTRA, e.label.clone()).on_click(move |window, cx| {
                let extra = this
                    .update(cx, |d, cx| {
                        let extra = d.extra.take();
                        d.close(window, cx);
                        extra
                    })
                    .ok()
                    .flatten();
                if let Some(extra) = extra {
                    (extra.run)(window, cx);
                }
            })
        });
        observed(ids::ERROR_DIALOG)
            .w(dp(ERROR_W))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(div().text_body1().semibold().child(self.data.title.clone()))
                    .child(icon(role, 24., color)),
            )
            .child(
                div()
                    .pt(dp(16.))
                    .text_body2()
                    .child(self.data.message.clone()),
            )
            .when_some(self.data.hook.clone(), |col, hook| {
                col.child(
                    observed(ids::ERROR_HOOK_OUTPUT)
                        .mt(dp(24.))
                        .p(dp(metrics::DIALOG_PAD))
                        .max_h(dp(400.))
                        .overflow_hidden()
                        .rounded(dp(metrics::RADIUS))
                        .bg(tokens.secondary_surface)
                        .text_mono13()
                        .child(hook.output),
                )
            })
            .when_some(self.data.details.clone(), |col, details| {
                col.child(
                    div()
                        .mt(dp(16.))
                        .max_h(dp(200.))
                        .overflow_hidden()
                        .text_mono13()
                        .text_color(tokens.on_background_secondary)
                        .child(details),
                )
            })
            .child(
                div()
                    .pt(dp(32.))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(dp(8.))
                    .children(extra)
                    .children(retry)
                    .child(ok),
            )
    }
}

/// Opens the error dialog for `data`; `on_close` runs when it is closed through one of its buttons.
pub fn open(
    window: &mut Window,
    cx: &mut App,
    mut data: ErrorDialogData,
    on_close: Box<dyn FnOnce(&mut Window, &mut App)>,
) {
    let retry = data.retry.take();
    let extra = data.extra.take();
    let view = cx.new(|_| ErrorDialog {
        data,
        retry,
        extra,
        on_close: Rc::new(RefCell::new(Some(on_close))),
    });
    DialogFrame::open(
        window,
        cx,
        DialogSpec {
            width: ERROR_W + 2. * metrics::DIALOG_PAD,
            est_height: 240.,
        },
        move |_, _| view.clone().into_any_element(),
    );
}
