//! Popup menus: the menu model, the single popup layer of the window and the extender registry
//! (spec 21 §2.13.2, Gitnuro 07 §9.3).
//!
//! Region packages build `Vec<MenuItem>` and call [`open_context_menu`] (right button press at the pointer) or
//! [`open_dropdown`] (click, anchored at the bottom-left of the trigger). Menu builders call [`extend`] at every
//! slot of the spec table so later packages can add items without editing the builder.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, BoxShadow, Context, Entity, FocusHandle, Focusable,
    Global, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement as _, Point, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, anchored, deferred, div, point, px,
};

use crate::backend::{FullName, ObjectId, RelaPath};
use crate::icons::{AppIcon, icon};
use crate::ids;
use crate::metrics::{self, dp};
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::observed;

pub mod commit_files;
pub mod diff_line;
pub mod diff_selection;
pub mod files;
pub mod log;
pub mod refs;
pub mod toolbar;
pub mod worktree;

/// The menus that extenders can attach to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuKind {
    LocalBranch,
    RemoteBranch,
    Tag,
    Stash,
    Remote,
    Submodule,
    Commit,
    StatusFile,
    CommitFile,
    DiffLine,
    Worktree,
    WorktreesHeader,
}

/// Positions in a menu where extenders insert their items.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuSlot {
    AfterCheckout,
    AfterRenameGroup,
    AfterPushPullGroup,
    End,
}

/// What a menu was opened for.
#[derive(Clone)]
pub struct MenuTarget {
    pub kind: MenuKind,
    pub tab: TabCtx,
    pub branch: Option<FullName>,
    pub upstream: Option<FullName>,
    pub commit: Option<ObjectId>,
    /// Repository-relative path, byte-preserving.
    pub path: Option<RelaPath>,
    pub is_current: bool,
    pub checked_out_elsewhere: bool,
}

/// One entry of a menu. Gitnuro menus have no submenus.
#[derive(Clone)]
pub enum MenuItem {
    Action {
        id: SharedString,
        icon: Option<AppIcon>,
        label: SharedString,
        enabled: bool,
        on_click: Rc<dyn Fn(&mut Window, &mut App)>,
    },
    Separator,
}

impl MenuItem {
    /// An enabled action without an icon.
    pub fn action(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        MenuItem::Action {
            id: id.into(),
            icon: None,
            label: label.into(),
            enabled: true,
            on_click: Rc::new(on_click),
        }
    }

    /// Sets the icon of an action (no-op on separators).
    pub fn with_icon(mut self, role: AppIcon) -> Self {
        if let MenuItem::Action { icon, .. } = &mut self {
            *icon = Some(role);
        }
        self
    }

    /// Enables or disables an action (no-op on separators).
    pub fn enabled(mut self, value: bool) -> Self {
        if let MenuItem::Action { enabled, .. } = &mut self {
            *enabled = value;
        }
        self
    }
}

/// A function that adds items to a menu at a slot.
pub type MenuExtender = fn(&MenuTarget, &mut Vec<MenuItem>, &App);

/// The registered extenders, in registration order.
#[derive(Default)]
pub struct MenuExtensions {
    entries: Vec<(MenuKind, MenuSlot, MenuExtender)>,
}

impl Global for MenuExtensions {}

/// Registers `f` to add items to menus of `kind` at `slot`.
pub fn register_extender(cx: &mut App, kind: MenuKind, slot: MenuSlot, f: MenuExtender) {
    cx.default_global::<MenuExtensions>()
        .entries
        .push((kind, slot, f));
}

/// Builders call this at each slot: appends the items of the extenders registered for `target.kind` at `slot`
/// (in registration order) followed by a separator, if any item was added. The `End` slot is the last group of
/// its menu, so it gets no trailing separator.
pub fn extend(cx: &App, slot: MenuSlot, target: &MenuTarget, items: &mut Vec<MenuItem>) {
    let Some(extensions) = cx.try_global::<MenuExtensions>() else {
        return;
    };
    let mut added = Vec::new();
    for (kind, at, f) in &extensions.entries {
        if *kind == target.kind && *at == slot {
            f(target, &mut added, cx);
        }
    }
    if !added.is_empty() {
        items.append(&mut added);
        if slot != MenuSlot::End {
            items.push(MenuItem::Separator);
        }
    }
}

/// Window of time in which a second popup request is ignored: nested wrappers all see the same right-click and
/// only the innermost one may open a menu (07 §9.3).
const POPUP_DEBOUNCE: Duration = Duration::from_millis(20);

struct OpenMenu {
    items: Vec<MenuItem>,
    at: Point<gpui_kit::Pixels>,
    show_icons: bool,
    /// `at` is the bottom-left corner of the menu instead of its top-left one (it grows upwards).
    above: bool,
}

/// The single popup layer of the window.
pub struct MenuHost {
    open: Option<OpenMenu>,
    focus: FocusHandle,
    opened_at: Option<Instant>,
    /// The element that had the keyboard focus when the menu opened: Escape gives it back (a menu opened with
    /// the keyboard must not strand the keyboard).
    previous_focus: Option<FocusHandle>,
}

/// The window's menu host entity.
pub struct MenuHostGlobal(pub Entity<MenuHost>);

impl Global for MenuHostGlobal {}

impl MenuHost {
    /// Creates the host and registers it as the window's [`MenuHostGlobal`]. Called once by the app view.
    pub fn install(cx: &mut App) -> Entity<MenuHost> {
        let host = cx.new(|cx| MenuHost {
            open: None,
            focus: cx.focus_handle(),
            opened_at: None,
            previous_focus: None,
        });
        cx.set_global(MenuHostGlobal(host.clone()));
        host
    }

    /// Whether a menu is showing.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Opens a menu with its top-left corner at `at` (the pointer for context menus, the bottom-left of the
    /// trigger for dropdowns). Ignored when another menu opened less than 20 ms ago or `items` is empty.
    pub fn open(
        &mut self,
        at: Point<gpui_kit::Pixels>,
        items: Vec<MenuItem>,
        show_icons: bool,
        above: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        if self
            .opened_at
            .is_some_and(|t| t.elapsed() < POPUP_DEBOUNCE && self.open.is_some())
        {
            return;
        }
        self.opened_at = Some(Instant::now());
        self.open = Some(OpenMenu {
            items,
            at,
            show_icons,
            above,
        });
        // A menu that replaces an open one keeps the focus to give back; otherwise it is whatever has the focus now
        // (nothing: nothing is given back, not the element of an earlier menu).
        self.previous_focus = match window.focused(cx) {
            Some(handle) if handle == self.focus => self.previous_focus.take(),
            other => other,
        };
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Closes the menu.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            cx.notify();
        }
    }

    fn render_item(&self, item: &MenuItem, show_icons: bool, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        match item {
            MenuItem::Separator => div()
                .px(dp(metrics::MENU_SEPARATOR_PAD_H))
                .py(dp(metrics::MENU_SEPARATOR_PAD_V))
                .w_full()
                .child(
                    div()
                        .w_full()
                        .h(dp(metrics::MENU_SEPARATOR_H))
                        .bg(tokens.popup_border()),
                )
                .into_any_element(),
            MenuItem::Action {
                id,
                icon: role,
                label,
                enabled,
                on_click,
            } => {
                let enabled = *enabled;
                let on_click = on_click.clone();
                let text_color = if enabled {
                    tokens.on_background
                } else {
                    tokens.disabled_content()
                };
                div()
                    .id(SharedString::from(format!("menu-item-{id}")))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .py(dp(metrics::MENU_ITEM_PAD))
                    .px(dp(if show_icons {
                        metrics::MENU_ITEM_PAD
                    } else {
                        12.
                    }))
                    .when(show_icons, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .size(dp(metrics::MENU_ICON_SLOT))
                                .mr(dp(metrics::MENU_ICON_GAP))
                                .when_some(*role, |slot, role| {
                                    slot.child(icon(role, metrics::ICON_SM, tokens.menu_icon()))
                                }),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .text_body2()
                            .text_color(text_color)
                            .child(label.clone()),
                    )
                    .when(enabled, |row| {
                        row.hover(move |s| s.bg(tokens.hover_layer()))
                            .active(move |s| s.bg(tokens.press_layer()))
                            .on_click(cx.listener(move |host, _, window, cx| {
                                host.close(cx);
                                on_click(window, cx);
                            }))
                    })
                    .test_support()
                    .into_any_element()
            }
        }
    }
}

impl Focusable for MenuHost {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MenuHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = &self.open else {
            return div().into_any_element();
        };
        let tokens = *cx.tokens();
        let items: Vec<AnyElement> = open
            .items
            .iter()
            .map(|item| self.render_item(item, open.show_icons, cx))
            .collect();
        let (at, above) = (open.at, open.above);
        let menu = observed(ids::MENU)
            .flex()
            .flex_col()
            .min_w(dp(metrics::MENU_MIN_W))
            .rounded(dp(metrics::RADIUS))
            .bg(tokens.background)
            .overflow_hidden()
            .shadow(vec![BoxShadow {
                color: tokens.menu_shadow(),
                offset: point(px(0.), px(2.)),
                blur_radius: px(5.),
                spread_radius: px(1.),
                inset: false,
            }])
            .when(!tokens.is_light, |m| {
                m.border(dp(2.)).border_color(tokens.popup_border())
            })
            .children(items);
        let popup = div()
            .track_focus(&self.focus)
            // Presses inside the menu must not reach the dismissing backdrop behind it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|host, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    host.close(cx);
                    if let Some(previous) = host.previous_focus.take() {
                        window.focus(&previous, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(menu);
        let layer = div()
            .absolute()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|host, _: &MouseDownEvent, _, cx| host.close(cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|host, _: &MouseDownEvent, _, cx| host.close(cx)),
            )
            .child(
                anchored()
                    .position(at)
                    .when(above, |a| a.anchor(Anchor::BottomLeft))
                    .snap_to_window()
                    .child(popup),
            )
            .id(ids::MENU_LAYER)
            .test_support();
        deferred(layer).with_priority(1).into_any_element()
    }
}

/// Opens a context menu at the pointer: call from a right-button press handler.
pub fn open_context_menu(
    window: &mut Window,
    cx: &mut App,
    at: Point<gpui_kit::Pixels>,
    items: Vec<MenuItem>,
) {
    open_with(window, cx, at, items, true, false);
}

/// Opens a context menu that grows upwards: its bottom-left corner is `anchor` (for a trigger at the bottom of the
/// window that the menu must not cover, `anchor` is the trigger's top-left corner).
pub fn open_menu_above(
    window: &mut Window,
    cx: &mut App,
    anchor: Point<gpui_kit::Pixels>,
    items: Vec<MenuItem>,
) {
    open_with(window, cx, anchor, items, true, true);
}

/// Opens a dropdown whose top-left corner is `anchor` (the bottom-left of the trigger element). `show_icons`
/// reserves the icon column.
pub fn open_dropdown(
    window: &mut Window,
    cx: &mut App,
    anchor: Point<gpui_kit::Pixels>,
    items: Vec<MenuItem>,
    show_icons: bool,
) {
    open_with(window, cx, anchor, items, show_icons, false);
}

fn open_with(
    window: &mut Window,
    cx: &mut App,
    at: Point<gpui_kit::Pixels>,
    items: Vec<MenuItem>,
    show_icons: bool,
    above: bool,
) {
    let Some(host) = cx.try_global::<MenuHostGlobal>().map(|g| g.0.clone()) else {
        tracing::warn!("a menu was requested before the menu host was installed");
        return;
    };
    host.update(cx, |host, cx| {
        host.open(at, items, show_icons, above, window, cx)
    });
}

/// Closes the open menu, if any.
pub fn close_menu(cx: &mut App) {
    if let Some(host) = cx.try_global::<MenuHostGlobal>().map(|g| g.0.clone()) {
        host.update(cx, |host, cx| host.close(cx));
    }
}

#[cfg(test)]
mod tests;
