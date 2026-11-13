//! The confirmation dialog (G10): destructive operations, oplog confirmations, unrestorable files, recovery
//! questions. It has Gitnuro's `IconBasedDialog` structure (icon, title, centred message, optional list of
//! details, Cancel + confirm). A destructive confirm button uses the `error` colour; a non-destructive dialog also
//! accepts with Enter. Escape dismisses without an answer (a blocked background thread then sees "no").

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::SyncSender;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::register::{ACCEPT_CONTEXT, Accept, common};
use crate::backend::prompt::ConfirmRequest;
use crate::icons::{AppIcon, icon};
use crate::metrics::dialogs::CONFIRM_DETAILS_MAX_H;
pub use crate::metrics::dialogs::CONFIRM_W;
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::PrimaryButton;

/// Receives the answer on the UI thread (`true` = confirmed). Not called when the dialog is dismissed with Escape.
pub type OnAnswer = Box<dyn FnOnce(bool, &mut Window, &mut App)>;

/// Element ids of the confirmation dialog.
pub mod ids {
    /// The dialog content.
    pub const CONFIRM_DIALOG: &str = "confirm-dialog";
}

/// Asks `request` for a background thread: the answer goes to `reply` (dropped = no).
pub fn open(window: &mut Window, cx: &mut App, request: ConfirmRequest, reply: SyncSender<bool>) {
    open_with(
        window,
        cx,
        request,
        Box::new(move |yes, _, _| {
            let _ = reply.send(yes);
        }),
    );
}

/// Asks `request` on the UI thread.
pub fn open_with(window: &mut Window, cx: &mut App, request: ConfirmRequest, on_answer: OnAnswer) {
    let view = cx.new(|cx| ConfirmDialog {
        request,
        answer: Rc::new(RefCell::new(Some(on_answer))),
        focus: cx.focus_handle(),
        focused: false,
    });
    common::open_view(window, cx, CONFIRM_W, 260., view);
}

/// The content of an open confirmation.
struct ConfirmDialog {
    request: ConfirmRequest,
    answer: Rc<RefCell<Option<OnAnswer>>>,
    focus: FocusHandle,
    focused: bool,
}

impl ConfirmDialog {
    fn respond(&mut self, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        if let Some(f) = self.answer.borrow_mut().take() {
            f(yes, window, cx);
        }
    }
}

impl Focusable for ConfirmDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused {
            self.focused = true;
            let focus = self.focus.clone();
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| window.focus(&focus, cx));
            });
        }
        let tokens = *cx.tokens();
        let destructive = self.request.destructive;
        let (role, color) = if destructive {
            (AppIcon::Warning, tokens.secondary)
        } else {
            (AppIcon::Info, tokens.on_background)
        };
        let this = cx.weak_entity();
        let cancel = PrimaryButton::new("dialog-cancel", self.request.cancel_label.clone())
            .cancel()
            .on_click({
                let this = this.clone();
                move |window, cx| {
                    this.update(cx, |d, cx| d.respond(false, window, cx)).ok();
                }
            });
        let confirm = common::filled_button(
            "dialog-primary",
            self.request.confirm_label.clone(),
            if destructive {
                tokens.error
            } else {
                tokens.primary
            },
            tokens.on_primary,
            true,
            &tokens,
            move |window, cx| {
                this.update(cx, |d, cx| d.respond(true, window, cx)).ok();
            },
        );
        let details = self
            .request
            .details
            .iter()
            .map(|line| div().text_body2().child(line.clone()));
        div()
            .when(!destructive, |d| d.key_context(ACCEPT_CONTEXT))
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Accept, window, cx| this.respond(true, window, cx)))
            .child(
                crate::widgets::observed(ids::CONFIRM_DIALOG)
                    .w(dp(CONFIRM_W))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .pb(dp(16.))
                            .child(icon(role, metrics::DIALOG_ICON, color)),
                    )
                    .child(
                        div()
                            .pb(dp(8.))
                            .text_body1()
                            .semibold()
                            .child(self.request.title.clone()),
                    )
                    .child(
                        // A flex row would not wrap the text: long messages (the undo confirmations list ref
                        // changes) were clipped on both sides.
                        div()
                            .w_full()
                            .text_center()
                            .text_body2()
                            .child(self.request.message.clone()),
                    )
                    .when(!self.request.details.is_empty(), |col| {
                        col.child(
                            div()
                                .id("confirm-details")
                                .w_full()
                                .mt(dp(16.))
                                .max_h(dp(CONFIRM_DETAILS_MAX_H))
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .text_color(tokens.on_background_secondary)
                                .children(details),
                        )
                    })
                    .child(
                        div()
                            .w_full()
                            .pt(dp(16.))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(dp(8.))
                            .child(cancel)
                            .child(confirm),
                    ),
            )
    }
}
