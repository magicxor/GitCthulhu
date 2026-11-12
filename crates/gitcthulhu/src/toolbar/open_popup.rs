//! The toolbar's Open popup (owned by G2; 07 §5.2). Also opened by the `OpenRepository` action in a repository tab.
//!
//! A 600 x 600 dp panel anchored at the bottom-left of the Open button: the "Open a repository" button and the
//! recent repositories (G1's `RecentRepos`). Choosing a repository opens it in place of the current tab. Gitnuro
//! draws the popup's background without its rounded corners; here the background follows the border (deviation 16).

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ParentElement as _, Render,
    Styled as _, Window, div,
};

use super::ids;
use crate::metrics::{self, dp};
use crate::shell::recent_repos::{OnOpen, RecentRepos};
use crate::tab::TabCtx;
use crate::theme::ActiveTokens as _;
use crate::widgets::{ObservedExt as _, PrimaryButton};

/// Width and height of the popup (07 §5.2).
pub const POPUP_W: f32 = 600.;
/// Height of the popup (07 §5.2: `heightIn(max = 600)`, effectively 600 because of the weighted list).
pub const POPUP_H: f32 = 600.;
/// Border width of the popup (07 §5.2).
pub const POPUP_BORDER: f32 = 2.;
/// Padding inside the border (07 §5.2).
pub const POPUP_PAD: f32 = 16.;
/// Space under the "Open a repository" button (07 §5.2).
pub const PICK_BUTTON_GAP: f32 = 16.;
/// Key context of the popup.
pub const POPUP_CONTEXT: &str = "OpenPopup";

gpui_kit::actions!(
    gitcthulhu_open_popup,
    [
        /// Closes the Open popup (Escape).
        ClosePopup
    ]
);

/// Emitted when the popup wants to go away.
pub struct PopupEvent;

/// The popup panel.
pub struct OpenPopup {
    ctx: TabCtx,
    recent: Entity<RecentRepos>,
    focus: FocusHandle,
}

impl EventEmitter<PopupEvent> for OpenPopup {}

impl OpenPopup {
    /// A popup for the tab of `ctx`.
    pub fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let popup = cx.weak_entity();
        let tab = ctx.clone();
        let on_open: OnOpen = Rc::new(move |path, window, cx| {
            popup.update(cx, |popup, cx| popup.dismiss(cx)).ok();
            open_replacing(&tab, path, window, cx);
        });
        let recent = cx.new(|cx| RecentRepos::new(on_open, window, cx));
        Self {
            ctx,
            recent,
            focus: cx.focus_handle(),
        }
    }

    /// The recent repositories list inside the popup.
    pub fn recent(&self) -> &Entity<RecentRepos> {
        &self.recent
    }

    /// Moves the keyboard focus into the popup.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    /// Asks the toolbar to remove the popup.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(PopupEvent);
    }

    fn pick_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(cx);
        let ctx = self.ctx.clone();
        super::platform::pick_directory(
            window,
            cx,
            Box::new(move |path, window, cx| open_replacing(&ctx, path, window, cx)),
        );
    }
}

/// Opens `path` in the slot of the tab of `ctx` (07 §2.6: "Open another repository" replaces the current tab).
fn open_replacing(ctx: &TabCtx, path: PathBuf, window: &mut Window, cx: &mut App) {
    let Some(view) = cx
        .try_global::<crate::app_view::AppViewGlobal>()
        .and_then(|global| global.0.upgrade())
    else {
        return;
    };
    let id = ctx.id;
    view.update(cx, |view, cx| view.replace_repository(id, path, window, cx));
}

impl Focusable for OpenPopup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for OpenPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let this = cx.weak_entity();
        div()
            .key_context(POPUP_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ClosePopup, _, cx| {
                this.dismiss(cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::input::Escape, _, cx| {
                    this.dismiss(cx);
                    cx.stop_propagation();
                }),
            )
            // Presses inside the popup must not reach the dismissing backdrop behind it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .w(dp(POPUP_W))
            .h(dp(POPUP_H))
            .flex()
            .flex_col()
            .p(dp(POPUP_PAD - POPUP_BORDER))
            .border(dp(POPUP_BORDER))
            .border_color(tokens.on_background.alpha(0.2))
            .rounded(dp(metrics::RADIUS_LG))
            .bg(tokens.surface)
            // A flex row keeps the button at its own width at the start, as Gitnuro's (no fillMaxWidth).
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .pb(dp(PICK_BUTTON_GAP))
                    .child(
                        PrimaryButton::new(ids::OPEN_POPUP_PICK, "Open a repository").on_click(
                            move |window, cx| {
                                this.update(cx, |popup, cx| popup.pick_directory(window, cx))
                                    .ok();
                            },
                        ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.recent.clone()))
            .observed(ids::OPEN_POPUP)
    }
}

/// Binds Escape in the popup's context.
pub fn register(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", ClosePopup, Some(POPUP_CONTEXT))]);
}

/// Opens the Open popup of the tab of `ctx` (the toolbar's Open button and the `OpenRepository` action).
/// Deferred: the `OpenRepository` action calls this inside the AppView's update, and the popup's recent list may
/// read the AppView when it is built.
pub fn open(ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    let Some(toolbar) = super::toolbar_of(ctx.id, cx) else {
        tracing::warn!("the Open popup was requested for a tab without a toolbar");
        return;
    };
    window.defer(cx, move |window, cx| {
        toolbar.update(cx, |toolbar, cx| toolbar.open_popup(window, cx));
    });
}
