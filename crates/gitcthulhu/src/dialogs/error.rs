//! The error dialog (G10; Gitnuro 09 §3.17, hook output D0.6): the title with an error icon, the message, the hook
//! output (expanded by default) or the details (behind a toggle: double-click the title or "Show details"), a copy
//! button that copies the real text, and Retry / extra / OK buttons. Warning mode (a failed post hook) shows the
//! Warning icon and a single OK.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div,
};

use super::register::common;
use crate::backend::present::{DialogAction, ErrorDialogData, RetryAction};
use crate::icons::{AppIcon, icon};
use crate::metrics::dialogs::{
    ERROR_BUTTONS_TOP, ERROR_COPY_PAD, ERROR_DETAILS_H, ERROR_DETAILS_TOP, ERROR_ICON,
    ERROR_MESSAGE_TOP,
};
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::scroll::scroll_y;
use crate::widgets::{IconButton, PrimaryButton, TextLink, observed};

pub use crate::metrics::dialogs::ERROR_W;

/// Element ids of the error dialog.
pub mod ids {
    /// The dialog content.
    pub const ERROR_DIALOG: &str = "error-dialog";
    /// The hook output area.
    pub const ERROR_HOOK_OUTPUT: &str = "error-hook-output";
    /// The details area of an error without hook output.
    pub const ERROR_DETAILS: &str = "error-details";
    /// The OK button.
    pub const ERROR_OK: &str = "error-ok";
    /// The retry button.
    pub const ERROR_RETRY: &str = "error-retry";
    /// The extra action button.
    pub const ERROR_EXTRA: &str = "error-extra";
    /// The copy button of the details area.
    pub const ERROR_COPY: &str = "error-copy";
    /// The title row (double-click toggles the details).
    pub const ERROR_TITLE: &str = "error-title";
    /// The "Show details" / "Hide details" link.
    pub const ERROR_TOGGLE: &str = "error-toggle";
}

type OnClose = Box<dyn FnOnce(&mut Window, &mut App)>;

/// The content of an open error dialog.
pub struct ErrorDialog {
    data: ErrorDialogData,
    retry: Option<RetryAction>,
    extra: Option<DialogAction>,
    on_close: Rc<RefCell<Option<OnClose>>>,
    /// The details area is showing (hook output: from the start).
    details_open: bool,
}

impl ErrorDialog {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        if let Some(on_close) = self.on_close.borrow_mut().take() {
            on_close(window, cx);
        }
    }

    /// The text of the details area: the hook output, else Git's details.
    fn area_text(&self) -> Option<SharedString> {
        match (&self.data.hook, &self.data.details) {
            (Some(hook), _) => Some(hook.output.clone()),
            (None, Some(details)) => Some(details.clone()),
            (None, None) => None,
        }
    }

    fn toggle_details(&mut self, cx: &mut Context<Self>) {
        if self.data.hook.is_none() && self.data.details.is_some() {
            self.details_open = !self.details_open;
            cx.notify();
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
        let toggle = (self.data.hook.is_none() && self.data.details.is_some()).then(|| {
            let this = this.clone();
            TextLink::new(
                ids::ERROR_TOGGLE,
                if self.details_open {
                    "Hide details"
                } else {
                    "Show details"
                },
            )
            .on_click(move |_, cx| {
                this.update(cx, |d, cx| d.toggle_details(cx)).ok();
            })
        });
        let area = self.area_text().filter(|_| self.details_open).map(|text| {
            let this_copy = text.clone();
            let id = if self.data.hook.is_some() {
                ids::ERROR_HOOK_OUTPUT
            } else {
                ids::ERROR_DETAILS
            };
            observed(id)
                .relative()
                .mt(dp(ERROR_DETAILS_TOP))
                .w_full()
                .max_h(dp(ERROR_DETAILS_H))
                .min_h(dp(metrics::FIELD_H * 2.))
                .flex()
                .flex_col()
                .rounded(dp(metrics::RADIUS))
                .overflow_hidden()
                .bg(tokens.secondary_surface)
                .child(
                    scroll_y("error-details-scroll")
                        .size_full()
                        .max_h(dp(ERROR_DETAILS_H))
                        .p(dp(metrics::DIALOG_PAD))
                        .pb(dp(metrics::DIALOG_PAD + metrics::MENU_ICON_SLOT * 2.))
                        .text_mono13()
                        .text_color(tokens.on_background)
                        .child(text),
                )
                .child(
                    div()
                        .absolute()
                        .right(dp(ERROR_COPY_PAD))
                        .bottom(dp(ERROR_COPY_PAD))
                        .child(
                            IconButton::new(ids::ERROR_COPY, AppIcon::Copy)
                                .icon_size(ERROR_ICON)
                                .tooltip("Copy error")
                                .on_click(move |_, cx| common::copy_text(cx, &this_copy)),
                        ),
                )
                .into_any_element()
        });
        let title_row = div()
            .id(ids::ERROR_TITLE)
            .flex()
            .flex_row()
            .justify_between()
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    this.toggle_details(cx);
                }
            }))
            .child(div().text_h4().semibold().child(self.data.title.clone()))
            .child(icon(role, ERROR_ICON, color));
        observed(ids::ERROR_DIALOG)
            .w(dp(ERROR_W))
            .flex()
            .flex_col()
            .child(title_row)
            .child(
                div()
                    .pt(dp(ERROR_MESSAGE_TOP))
                    .text_body2()
                    .child(self.data.message.clone()),
            )
            .children(area)
            .child(
                div()
                    .pt(dp(ERROR_BUTTONS_TOP))
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(div().flex_1().children(toggle))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(dp(8.))
                            .children(extra)
                            .children(retry)
                            .child(ok),
                    ),
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
    let (retry, extra) = if data.warning {
        // Warning mode has a single OK.
        data.retry.take();
        data.extra.take();
        (None, None)
    } else {
        (data.retry.take(), data.extra.take())
    };
    let est_height = if data.hook.is_some() { 640. } else { 240. };
    let details_open = data.hook.is_some();
    let view = cx.new(|_| ErrorDialog {
        data,
        retry,
        extra,
        on_close: Rc::new(RefCell::new(Some(on_close))),
        details_open,
    });
    common::open_view(window, cx, ERROR_W, est_height, view);
}
