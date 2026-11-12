//! "Opening repository" (07 §3.1) and the processing overlay's content (07 §3.2) (owned by G1). The overlay
//! container itself (delay, focus, key swallowing) belongs to `tab/repo_view.rs`.

use std::path::Path;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, App, IntoElement, ParentElement as _, Styled as _, Window, div};

use super::{dims, ids};
use crate::metrics::{self, dp};
use crate::tab::BusyState;
use crate::tab::ops::OpKind;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{LinearProgress, TextLink, observed};

/// The subtitle below the title of the processing overlay (07 §3.2, §12.6).
pub fn processing_subtitle(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Pull => "Pulling changes from remote branch to the current",
        _ => "This could take a few seconds",
    }
}

/// The "Opening repository" screen of a tab whose repository is being opened (07 §3.1: 36 sp title, 24 sp path).
pub fn loading_repository(path: &Path, cx: &App) -> AnyElement {
    let tokens = *cx.tokens();
    observed(ids::LOADING)
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(tokens.surface)
        .child(
            observed(ids::LOADING_TITLE)
                .text_h1()
                .text_size(dp(dims::LOADING_TITLE))
                .text_color(tokens.on_background)
                .child("Opening repository"),
        )
        .child(
            observed(ids::LOADING_PATH)
                .text_h2()
                .text_size(dp(dims::LOADING_PATH))
                .text_color(tokens.on_background_secondary)
                .child(path.display().to_string()),
        )
        .into_any_element()
}

/// The processing overlay's content (07 §3.2): title, subtitle, indeterminate progress bar, the newest progress line
/// (deviation 9) and, for cancellable operations, a Cancel text button. Fills the overlay container.
pub fn processing_overlay(
    busy: &BusyState,
    on_cancel: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> AnyElement {
    let tokens = *cx.tokens();
    let has_cancel = on_cancel.is_some();
    observed(ids::PROCESSING)
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .bg(tokens.surface)
        .text_color(tokens.on_background)
        .child(
            div().pb(dp(dims::PROCESSING_TITLE_PAD_BOTTOM)).child(
                observed(ids::PROCESSING_TITLE)
                    .text_h3()
                    .child(busy.kind.processing_title()),
            ),
        )
        .child(
            div().pb(dp(dims::PROCESSING_SUBTITLE_PAD_BOTTOM)).child(
                observed(ids::PROCESSING_SUBTITLE).text_body1().child(
                    busy.detail
                        .clone()
                        .unwrap_or_else(|| processing_subtitle(busy.kind).into()),
                ),
            ),
        )
        .child(
            div().pb(dp(dims::PROCESSING_BAR_PAD_BOTTOM)).child(
                observed(ids::PROCESSING_BAR)
                    .w(dp(metrics::PROCESSING_BAR_W))
                    .child(
                        LinearProgress::new()
                            .color(tokens.secondary)
                            .width(metrics::PROCESSING_BAR_W),
                    ),
            ),
        )
        .when_some(busy.phase.clone(), |col, phase| {
            col.child(
                div()
                    .when(has_cancel, |d| d.pb(dp(dims::PROCESSING_PHASE_PAD_BOTTOM)))
                    .child(
                        observed(ids::PROCESSING_PHASE)
                            .text_body2()
                            .text_color(tokens.on_background_secondary)
                            .child(phase),
                    ),
            )
        })
        .when_some(on_cancel, |col, cancel| {
            col.child(
                TextLink::new(ids::PROCESSING_CANCEL, "Cancel")
                    .on_click(move |window, cx| cancel(window, cx)),
            )
        })
        .into_any_element()
}
