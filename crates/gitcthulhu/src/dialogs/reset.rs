//! The Reset dialog (G10; 09 §3.6): three radio cards (Soft, Mixed, Hard) and "Reset branch".

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
};

use super::IconBasedDialog;
use super::register::common;
use crate::backend::{ObjectId, ResetMode};
use crate::icons::AppIcon;
use crate::metrics::dialogs::{RESET_GAP, RESET_W};
use crate::metrics::dp;
use crate::tab::TabCtx;
use crate::widgets::{RadioRow, observed};

/// Element ids of the reset dialog (the cards are `("reset-mode", index)` in the order of [`MODES`]).
pub mod ids {
    /// The dialog content.
    pub const RESET_DIALOG: &str = "reset-dialog";
    /// Prefix of the three radio cards: `(RESET_MODE, index)`.
    pub const RESET_MODE: &str = "reset-mode";
}

/// The cards in display order: mode, title, subtitle (09 §3.6).
pub const MODES: [(ResetMode, &str, &str); 3] = [
    (
        ResetMode::Soft,
        "Soft reset",
        "Keep the changes in the index (staged)",
    ),
    (
        ResetMode::Mixed,
        "Mixed reset",
        "Keep the changes (unstaged)",
    ),
    (ResetMode::Hard, "Hard", "Discard all the changes"),
];

/// Index of the preselected card (Mixed).
const DEFAULT_MODE: usize = 1;

/// Receives the commit and the chosen mode after the dialog closed.
pub(crate) type OnReset = Rc<dyn Fn(ObjectId, ResetMode, &mut Window, &mut App)>;

/// The content of an open reset dialog.
struct ResetDialog {
    on_reset: OnReset,
    commit: ObjectId,
    selected: usize,
}

impl Render for ResetDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        let cards = MODES.iter().enumerate().map(|(ix, (_, title, subtitle))| {
            let this = this.clone();
            RadioRow::new((ids::RESET_MODE, ix), *title, self.selected == ix)
                .subtitle(*subtitle)
                .width(RESET_W)
                .on_select(move |_, cx| {
                    this.update(cx, |d, cx| {
                        d.selected = ix;
                        cx.notify();
                    })
                    .ok();
                })
        });
        let on_primary = this.clone();
        IconBasedDialog::new(AppIcon::Undo, "Reset current branch", "Reset branch")
            .subtitle(
                "Reset the changes of your current branch to a \nprevious or different commit",
            )
            .content(
                observed(ids::RESET_DIALOG)
                    .w(dp(RESET_W))
                    .flex()
                    .flex_col()
                    .gap(dp(RESET_GAP))
                    .children(cards),
            )
            .on_primary(move |window, cx| {
                on_primary
                    .update(cx, |d, cx| {
                        window.close_dialog(cx);
                        (d.on_reset)(d.commit, MODES[d.selected].0, window, cx);
                    })
                    .ok();
            })
    }
}

/// Opens the Reset dialog for a reset to `commit`.
pub fn open(window: &mut Window, cx: &mut App, tab: TabCtx, commit: ObjectId) {
    open_with(
        window,
        cx,
        commit,
        Rc::new(move |commit, mode, window, cx| {
            tab.with_tab(window, cx, move |t, window, cx| {
                t.reset(commit, mode, window, cx);
            });
        }),
    );
}

/// Opens the dialog with a custom reset callback.
pub(crate) fn open_with(window: &mut Window, cx: &mut App, commit: ObjectId, on_reset: OnReset) {
    let view = cx.new(|_| ResetDialog {
        on_reset,
        commit,
        selected: DEFAULT_MODE,
    });
    common::open_view(window, cx, RESET_W, 420., view);
}

/// Mixed is the default; Soft is first (used by tests).
pub fn default_mode() -> ResetMode {
    MODES[DEFAULT_MODE].0
}
