//! "Opening repository" (07 §3.1) and the processing overlay's content (07 §3.2) (owned by G1 after G0b). The
//! overlay container itself (delay, focus, key swallowing) belongs to `tab/repo_view.rs`.

use std::path::Path;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, App, IntoElement, ParentElement as _, Styled as _, Window, div};

use super::ids;
use crate::metrics::{self, dp};
use crate::tab::BusyState;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{LinearProgress, TextLink, observed};

/// The "Opening repository" screen of a tab whose repository is being opened.
pub fn loading_repository(path: &Path, cx: &App) -> AnyElement {
    let tokens = *cx.tokens();
    observed(ids::LOADING)
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(tokens.surface)
        .child(div().text_h1().child("Opening repository"))
        .child(
            div()
                .text_h3()
                .text_color(tokens.on_background_secondary)
                .child(path.display().to_string()),
        )
        .into_any_element()
}

/// The processing overlay's content: title, subtitle, progress bar, phase line and (for cancellable operations) a
/// Cancel button. Fills the overlay container.
pub fn processing_overlay(
    busy: &BusyState,
    on_cancel: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> AnyElement {
    let tokens = *cx.tokens();
    observed(ids::PROCESSING)
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(tokens.surface)
        .child(
            div()
                .text_h3()
                .pb(dp(8.))
                .child(busy.kind.processing_title()),
        )
        .child(
            div()
                .text_body1()
                .pb(dp(32.))
                .child("This could take a few seconds"),
        )
        .child(
            div().pb(dp(16.)).child(
                LinearProgress::new()
                    .color(tokens.secondary)
                    .width(metrics::PROCESSING_BAR_W),
            ),
        )
        .when_some(busy.phase.clone(), |col, phase| {
            col.child(div().text_body2().pb(dp(16.)).child(phase))
        })
        .when_some(on_cancel, |col, cancel| {
            col.child(
                TextLink::new(ids::PROCESSING_CANCEL, "Cancel")
                    .on_click(move |window, cx| cancel(window, cx)),
            )
        })
        .into_any_element()
}
