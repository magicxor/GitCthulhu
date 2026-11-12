//! The confirmation dialog (owned by G10 after G0b): destructive operations, oplog confirmations, unrestorable
//! files, recovery choices. G0b ships a working minimal dialog (IconBasedDialog with Cancel / confirm) because the
//! runner's flows depend on it; G10 implements the exact layout.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::SyncSender;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, IntoElement, ParentElement as _, Styled as _, Window, div};

use super::{DialogFrame, DialogSpec, IconBasedDialog};
use crate::backend::prompt::ConfirmRequest;
use crate::icons::AppIcon;
use crate::metrics;
use crate::theme::TextStyleExt as _;

/// Width of the confirmation dialog's content.
pub const CONFIRM_W: f32 = 420.;

/// Receives the answer on the UI thread (`true` = confirmed). Not called when the dialog is dismissed with Escape.
pub type OnAnswer = Box<dyn FnOnce(bool, &mut Window, &mut App)>;

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
    let answer: Rc<RefCell<Option<OnAnswer>>> = Rc::new(RefCell::new(Some(on_answer)));
    let role = if request.destructive {
        AppIcon::Warning
    } else {
        AppIcon::Info
    };
    DialogFrame::open(
        window,
        cx,
        DialogSpec {
            width: CONFIRM_W + 2. * metrics::DIALOG_PAD,
            est_height: 260.,
        },
        move |_, _| {
            let yes = answer.clone();
            let no = answer.clone();
            let details = request
                .details
                .iter()
                .map(|line| div().text_body2().child(line.clone()));
            IconBasedDialog::new(role, request.title.clone(), request.confirm_label.clone())
                .subtitle(request.message.clone())
                .cancel_label(request.cancel_label.clone())
                .content(div().w_full().flex().flex_col().children(details))
                .on_primary(move |window, cx| {
                    window.close_dialog(cx);
                    if let Some(f) = yes.borrow_mut().take() {
                        f(true, window, cx);
                    }
                })
                .on_cancel(move |window, cx| {
                    if let Some(f) = no.borrow_mut().take() {
                        f(false, window, cx);
                    }
                })
                .into_any_element()
        },
    );
}
