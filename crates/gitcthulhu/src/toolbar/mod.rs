//! The repository toolbar (owned by G2; 07 §5.1): Open, the centred Pull / Push / Branch / Stash / Pop group
//! (preceded by the D0.3 undo slot) and Terminal, Actions and Settings on the right. The two flex spacers are equal,
//! so the centred group sits between the left and the right group, not in the window centre (07 §5.1.1).

use std::collections::HashMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Global, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, TestSupportExt as _, WeakEntity, Window,
    deferred, div, point, px,
};

use crate::backend::StartPoint;
use crate::icons::AppIcon;
use crate::ids as app_ids;
use crate::menus;
use crate::metrics::{self, dp, dpx};
use crate::settings::SettingsGlobal;
use crate::tab::{TabCtx, TabId};
use crate::terminal::TerminalButtonState;
use crate::theme::{ActiveTokens as _, ThemeGlobal};
use crate::widgets::{BoundsCell, ExtendedMenuButton, MenuButton, observed, track_bounds};

pub mod open_popup;
pub(crate) mod platform;
mod state;

pub use state::ToolbarState;

use open_popup::{OpenPopup, PopupEvent};

/// Element ids of the toolbar.
pub mod ids {
    /// The Open button.
    pub const OPEN: &str = "toolbar-open";
    /// The Pull split button.
    pub const PULL: &str = "toolbar-pull";
    /// The Push split button.
    pub const PUSH: &str = "toolbar-push";
    /// The Branch button.
    pub const BRANCH: &str = "toolbar-branch";
    /// The Stash split button.
    pub const STASH: &str = "toolbar-stash";
    /// The Pop stash button.
    pub const POP_STASH: &str = "toolbar-pop-stash";
    /// The Terminal button.
    pub const TERMINAL: &str = "toolbar-terminal";
    /// The Terminal button's status dot.
    pub const TERMINAL_DOT: &str = "toolbar-terminal-dot";
    /// The Actions button.
    pub const ACTIONS: &str = "toolbar-actions";
    /// The Settings button.
    pub const SETTINGS: &str = "toolbar-settings";
    /// The centred group (Undo ... Pop stash).
    pub const CENTER_GROUP: &str = "toolbar-center-group";
    /// The right group (Terminal, Actions, Settings with their margins).
    pub const RIGHT_GROUP: &str = "toolbar-right-group";
    /// The Open popup (07 §5.2).
    pub const OPEN_POPUP: &str = "open-popup";
    /// The "Open a repository" button of the popup.
    pub const OPEN_POPUP_PICK: &str = "open-popup-pick";
}

/// Width of the gap between the button groups (07 §5.1.1 spacers).
const GROUP_GAP: f32 = metrics::TOOLBAR_GROUP_GAP;
/// Icon top inside a 56 dp button: content (24 icon + 13.3 caption) centred in 56 (07 §5.1.2).
const ICON_TOP: f32 = 9.;
/// Status dot of the Terminal button (21 §5.2): diameter and offset from the icon box's right edge (x) and top.
const DOT_SIZE: f32 = 8.;
/// The dot's left edge inside the 56 dp button: icon right (16 + 24) minus 4.
const DOT_LEFT: f32 = 36.;

/// The toolbars of the open tabs, so that `open_popup::open(&TabCtx, ..)` (the `OpenRepository` action of the
/// window) finds the view that hosts the popup.
#[derive(Default)]
struct Toolbars(HashMap<TabId, WeakEntity<ToolbarView>>);

impl Global for Toolbars {}

/// The toolbar of the tab `id`, if it exists.
pub(crate) fn toolbar_of(id: TabId, cx: &App) -> Option<Entity<ToolbarView>> {
    cx.try_global::<Toolbars>()?.0.get(&id)?.upgrade()
}

/// The toolbar of a repository tab.
pub struct ToolbarView {
    ctx: TabCtx,
    bounds: BoundsCell,
    popup: Option<Entity<OpenPopup>>,
    popup_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl ToolbarView {
    /// The toolbar of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe(&ctx.model, |_, _, cx| cx.notify()),
            cx.observe_global::<SettingsGlobal>(|_, cx| cx.notify()),
            cx.observe_global::<ThemeGlobal>(|_, cx| cx.notify()),
        ];
        let me = cx.entity_id();
        let weak = cx.weak_entity();
        cx.default_global::<Toolbars>().0.insert(ctx.id, weak);
        let id = ctx.id;
        cx.on_release(move |_, cx| {
            let toolbars = cx.default_global::<Toolbars>();
            if toolbars
                .0
                .get(&id)
                .is_some_and(|weak| weak.entity_id() == me)
            {
                toolbars.0.remove(&id);
            }
        })
        .detach();
        Self {
            ctx,
            bounds: Default::default(),
            popup: None,
            popup_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    /// The tab of this toolbar.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// Whether the Open popup is showing.
    pub fn popup_open(&self) -> bool {
        self.popup.is_some()
    }

    /// Shows the Open popup (07 §5.2) and focuses it; a second call keeps the popup that is open.
    pub fn open_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(popup) = &self.popup {
            popup.update(cx, |popup, cx| popup.focus(window, cx));
            return;
        }
        let ctx = self.ctx.clone();
        let popup = cx.new(|cx| OpenPopup::new(ctx, window, cx));
        self.popup_subscription = Some(cx.subscribe(&popup, |this, _, _: &PopupEvent, cx| {
            this.popup = None;
            this.popup_subscription = None;
            cx.notify();
        }));
        popup.update(cx, |popup, cx| popup.focus(window, cx));
        self.popup = Some(popup);
        cx.notify();
    }

    fn popup_layer(&self, window: &Window, popup: Entity<OpenPopup>) -> impl IntoElement + use<> {
        let origin = self.bounds.get().origin;
        // x = left edge of the Open button, y = its bottom (07 §5.2).
        let at = point(
            origin.x + dpx(metrics::TOOLBAR_EDGE, window),
            origin.y + dpx(metrics::TOOLBAR_PAD_V + metrics::MENU_BUTTON, window),
        );
        let size = window.viewport_size();
        let close = |popup: Entity<OpenPopup>| {
            move |_: &gpui_kit::MouseDownEvent, _: &mut Window, cx: &mut App| {
                popup.update(cx, |popup, cx| popup.dismiss(cx));
            }
        };
        let backdrop = div()
            .w(size.width)
            .h(size.height)
            .occlude()
            .on_mouse_down(gpui_kit::MouseButton::Left, close(popup.clone()))
            .on_mouse_down(gpui_kit::MouseButton::Right, close(popup.clone()));
        deferred(
            div()
                .absolute()
                .child(
                    gpui_kit::anchored()
                        .position(point(px(0.), px(0.)))
                        .child(backdrop),
                )
                .child(
                    gpui_kit::anchored()
                        .position(at)
                        .snap_to_window()
                        .child(popup),
                ),
        )
        .with_priority(1)
    }
}

/// The Terminal button (07 §5.1.1 row 11): toggles the embedded terminal panel (21 §5.2) and carries the status dot
/// at the icon's top-right while a shell works, is blocked or failed.
fn terminal_button(ctx: TabCtx, state: TerminalButtonState) -> impl IntoElement {
    let dot = state.status_dot;
    div()
        .relative()
        .flex_none()
        .child(
            MenuButton::new(ids::TERMINAL, AppIcon::Terminal, "Terminal")
                .enabled(state.enabled)
                .tooltip(state.tooltip)
                .hint("Ctrl+`")
                .margins(0., metrics::TOOLBAR_ITEM_GAP)
                .on_click(move |window, cx| crate::terminal::toggle(&ctx, window, cx)),
        )
        .when_some(dot, |wrapper, color| {
            wrapper.child(
                div()
                    .id(ids::TERMINAL_DOT)
                    .absolute()
                    .left(dp(DOT_LEFT))
                    .top(dp(ICON_TOP))
                    .size(dp(DOT_SIZE))
                    .rounded_full()
                    .bg(color)
                    .test_support(),
            )
        })
}

impl Render for ToolbarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = crate::settings::get_or_default(cx);
        let st = ToolbarState::read(self.ctx.model.read(cx), &settings);
        let tokens = *cx.tokens();
        let ctx = self.ctx.clone();
        let undo = crate::undo::toolbar_slot(&ctx, window, cx);
        let terminal = crate::terminal::toolbar_button_state(&ctx, cx);
        let newest_stash = self
            .ctx
            .model
            .read(cx)
            .refs
            .value
            .as_ref()
            .and_then(|refs| refs.stashes.first().map(|s| s.id));

        let open = {
            let ctx = ctx.clone();
            MenuButton::new(ids::OPEN, AppIcon::Open, "Open")
                .tooltip("Open a different repository")
                .hint(state::primary_hint(false, false, "O"))
                .margins(metrics::TOOLBAR_EDGE, 0.)
                .on_click(move |window, cx| open_popup::open(&ctx, window, cx))
        };
        let pull = {
            let ctx = ctx.clone();
            ExtendedMenuButton::new(ids::PULL, AppIcon::Download, "Pull")
                .enabled(st.has_remote)
                .tooltip(st.pull_tooltip())
                .hint(state::primary_hint(false, false, "U"))
                .margins(0., metrics::TOOLBAR_ITEM_GAP)
                .dropdown(menus::toolbar::pull_menu(&ctx, cx), true)
                .on_click(move |window, cx| {
                    ctx.with_tab(window, cx, |tab, window, cx| {
                        tab.pull(None, window, cx);
                    })
                })
        };
        let push = {
            let ctx = ctx.clone();
            ExtendedMenuButton::new(ids::PUSH, AppIcon::Upload, "Push")
                .enabled(st.has_remote)
                .tooltip(st.push_tooltip())
                .hint(state::primary_hint(false, false, "P"))
                .dropdown(menus::toolbar::push_menu(&ctx, cx), true)
                .on_click(move |window, cx| {
                    ctx.with_tab(window, cx, |tab, window, cx| {
                        tab.push_current(false, false, window, cx);
                    })
                })
        };
        let branch = {
            let ctx = ctx.clone();
            MenuButton::new(ids::BRANCH, AppIcon::Branch, "Branch")
                .tooltip("Create a new branch")
                .hint(state::primary_hint(false, false, "B"))
                .on_click(move |window, cx| {
                    crate::dialogs::branch::open_new_branch(
                        window,
                        cx,
                        ctx.clone(),
                        StartPoint::Head,
                    )
                })
        };
        let stash = {
            let ctx = ctx.clone();
            ExtendedMenuButton::new(ids::STASH, AppIcon::Stash, "Stash")
                .enabled(st.can_stash)
                .tooltip(st.stash_tooltip())
                .hint(state::primary_hint(false, false, "S"))
                .margins(0., metrics::TOOLBAR_ITEM_GAP)
                .dropdown(menus::toolbar::stash_menu(&ctx, cx), true)
                .on_click(move |window, cx| {
                    ctx.with_tab(window, cx, |tab, window, cx| {
                        tab.stash(None, true, window, cx);
                    })
                })
        };
        let pop = {
            let ctx = ctx.clone();
            MenuButton::new(ids::POP_STASH, AppIcon::ApplyStash, "Pop stash")
                .enabled(st.has_stash)
                .tooltip(st.pop_tooltip())
                .hint(state::primary_hint(false, true, "S"))
                .on_click(move |window, cx| {
                    if let Some(id) = newest_stash {
                        ctx.with_tab(window, cx, |tab, window, cx| {
                            tab.pop_stash(id, window, cx);
                        })
                    }
                })
        };
        let terminal_button = terminal_button(ctx.clone(), terminal);
        let actions = {
            let ctx = ctx.clone();
            MenuButton::new(ids::ACTIONS, AppIcon::Actions, "Actions")
                .tooltip("Additional actions")
                .margins(0., metrics::TOOLBAR_ITEM_GAP)
                .on_click(move |window, cx| {
                    crate::dialogs::quick_actions::open(window, cx, ctx.clone())
                })
        };
        let settings_button = MenuButton::new(ids::SETTINGS, AppIcon::Settings, "Settings")
            .tooltip("GitCthulhu's settings")
            .hint(state::primary_hint(true, false, "S"))
            .on_click(|window, cx| crate::dialogs::settings::open(window, cx, None));

        let popup = self
            .popup
            .clone()
            .map(|popup| self.popup_layer(window, popup));

        observed(app_ids::TOOLBAR)
            .relative()
            .flex_none()
            .w_full()
            .h(dp(metrics::TOOLBAR_H))
            .py(dp(metrics::TOOLBAR_PAD_V))
            .bg(tokens.surface)
            .flex()
            .flex_row()
            .items_center()
            .child(track_bounds(&self.bounds))
            .child(open)
            .child(div().flex_1())
            .child(
                observed(ids::CENTER_GROUP)
                    .flex()
                    .flex_row()
                    .flex_none()
                    .child(undo)
                    .child(div().flex_none().w(dp(GROUP_GAP)))
                    .child(pull)
                    .child(push)
                    .child(div().flex_none().w(dp(GROUP_GAP)))
                    .child(branch)
                    .child(div().flex_none().w(dp(GROUP_GAP)))
                    .child(stash)
                    .child(pop),
            )
            .child(div().flex_1())
            .child(
                observed(ids::RIGHT_GROUP)
                    .flex()
                    .flex_row()
                    .flex_none()
                    .child(terminal_button)
                    .child(actions)
                    .child(
                        div()
                            .flex_none()
                            .pr(dp(metrics::TOOLBAR_EDGE))
                            .child(settings_button),
                    ),
            )
            .children(popup)
    }
}

/// Registers the toolbar's key bindings and actions (G2): the Quick actions dialog and the Open popup.
pub fn register(cx: &mut App) {
    crate::dialogs::quick_actions::register(cx);
    open_popup::register(cx);
}

#[cfg(test)]
mod tests;
