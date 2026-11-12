//! The repository tab strip (07 §2): one 36 dp tab per slot of the window, a "+" button right after the last tab,
//! an 8 dp scrollbar above the tabs while they overflow, middle-click and close-button closing, and drag reordering
//! with live swapping (21 §4.5).
//!
//! The strip reads the tab list of `AppView` in `render` (AppView does not notify it) and changes it only through the
//! tab API (`select_tab`, `close_tab`, `move_tab`, `new_empty_tab`).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, DragMoveEvent, Entity, EntityId, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, WeakEntity, Window, div,
    point, px,
};

use super::{dims, ids};
use crate::app_view::{AppView, TabContent};
use crate::icons::{AppIcon, icon};
use crate::metrics::{self, BASE_REM, dp, dpx};
use crate::tab::TabId;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::scroll::HorizontalThumbBar;
use crate::widgets::{TooltipExt as _, observed, with_layer};

/// The value carried by a tab drag.
#[derive(Clone)]
struct TabDrag {
    id: TabId,
    title: SharedString,
}

/// The view shown under the pointer while a tab is dragged: the tab at 90 % opacity.
struct TabGhost {
    title: SharedString,
}

impl Render for TabGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        div()
            .opacity(dims::TAB_GHOST_OPACITY)
            .min_w(dp(metrics::TAB_MIN_W))
            .max_w(dp(dims::TAB_MAX_W))
            .h(dp(metrics::TAB_STRIP_H))
            .pl(dp(metrics::TAB_TITLE_PAD_START))
            .pr(dp(dims::TAB_CLOSE_W))
            .flex()
            .items_center()
            .bg(tokens.surface)
            .border_b(dp(metrics::TAB_UNDERLINE_H))
            .border_color(tokens.primary_variant)
            .text_body2()
            .medium()
            .text_color(tokens.on_background)
            .whitespace_nowrap()
            .child(self.title.clone())
    }
}

/// What the strip needs to know about a tab to draw it.
struct TabInfo {
    id: TabId,
    title: SharedString,
    tooltip: Option<SharedString>,
    selected: bool,
}

/// The tab index a dragged tab belongs at: the number of other tabs whose midpoint is left of the pointer, which
/// makes the dragged tab swap places once its position crosses the midpoint of a neighbour (07 §2.5).
pub fn drag_target_index(mids: &[f32], dragged: usize, pointer_x: f32) -> usize {
    mids.iter()
        .enumerate()
        .filter(|(ix, mid)| *ix != dragged && **mid < pointer_x)
        .count()
}

/// The scroll offset after an auto-scroll step: the pointer inside `edge` of the viewport edges scrolls by its
/// overshoot. `offset` is zero or negative (content moved left), `max` the scrollable distance.
pub fn autoscroll_offset(
    offset: f32,
    max: f32,
    viewport: (f32, f32),
    edge: f32,
    pointer_x: f32,
) -> f32 {
    let (left, right) = viewport;
    let mut next = offset;
    if pointer_x < left + edge {
        next += left + edge - pointer_x;
    } else if pointer_x > right - edge {
        next -= pointer_x - (right - edge);
    }
    next.clamp(-max.max(0.), 0.)
}

/// The tab strip view of the window.
pub struct TabStripView {
    app: WeakEntity<AppView>,
    scroll: ScrollHandle,
    /// The slot that was selected when the strip last rendered (to scroll it into view and focus a welcome page).
    last_active: Option<TabId>,
    /// The welcome page that was shown when the strip last rendered (to focus a newly shown one).
    last_welcome: Option<EntityId>,
    /// A reorder happened and the tab bounds are stale until the next frame.
    moved_since_render: bool,
}

impl TabStripView {
    /// A strip that shows (and drives) `app`'s tabs.
    pub fn new(app: WeakEntity<AppView>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            app,
            scroll: ScrollHandle::new(),
            last_active: None,
            last_welcome: None,
            moved_since_render: false,
        }
    }

    /// The window content this strip belongs to.
    pub fn app(&self) -> &WeakEntity<AppView> {
        &self.app
    }

    /// Whether the tabs are wider than the strip, so that the scrollbar is shown.
    pub fn overflows(&self) -> bool {
        self.scroll.max_offset().x > px(0.5)
    }

    fn snapshot(
        &self,
        cx: &gpui_kit::App,
    ) -> (Vec<TabInfo>, Option<Entity<super::welcome::WelcomeView>>) {
        let Some(app) = self.app.upgrade() else {
            return (Vec::new(), None);
        };
        let app = app.read(cx);
        let active = app.active();
        let welcome = app.tabs().get(active).and_then(|slot| match &slot.content {
            TabContent::Welcome(view) => Some(view.clone()),
            _ => None,
        });
        let tabs = app
            .tabs()
            .iter()
            .enumerate()
            .map(|(ix, slot)| TabInfo {
                id: slot.id,
                title: app.tab_title(ix, cx),
                tooltip: app.tab_tooltip(ix, cx),
                selected: ix == active,
            })
            .collect();
        (tabs, welcome)
    }

    fn on_drag_move(
        &mut self,
        drag: &TabDrag,
        event: &DragMoveEvent<TabDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let x = event.event.position.x;
        self.autoscroll(x, window);
        if self.moved_since_render {
            return;
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let order: Vec<TabId> = app.read(cx).tabs().iter().map(|s| s.id).collect();
        let Some(from) = order.iter().position(|id| *id == drag.id) else {
            return;
        };
        let mids: Vec<f32> = (0..order.len())
            .map(|ix| {
                self.scroll
                    .bounds_for_item(ix)
                    .map_or(f32::INFINITY, |b| b.center().x.as_f32())
            })
            .collect();
        let to = drag_target_index(&mids, from, x.as_f32());
        if to != from {
            app.update(cx, |app, cx| app.move_tab(from, to, cx));
            self.moved_since_render = true;
            cx.notify();
        }
    }

    fn autoscroll(&self, pointer_x: Pixels, window: &Window) {
        let bounds = self.scroll.bounds();
        let max = self.scroll.max_offset().x.as_f32();
        if max <= 0. {
            return;
        }
        let offset = self.scroll.offset();
        let next = autoscroll_offset(
            offset.x.as_f32(),
            max,
            (bounds.left().as_f32(), bounds.right().as_f32()),
            dpx(dims::TAB_AUTOSCROLL_EDGE, window).as_f32(),
            pointer_x.as_f32(),
        );
        if (next - offset.x.as_f32()).abs() > f32::EPSILON {
            self.scroll.set_offset(point(px(next), offset.y));
        }
    }

    fn tab(&self, ix: usize, info: &TabInfo, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let tokens = *cx.tokens();
        let (id, selected) = (info.id, info.selected);
        let group = SharedString::from(format!("tab-group-{ix}"));
        let background = if selected {
            tokens.surface
        } else {
            tokens.background
        };
        let (select_app, close_app, drag_app) =
            (self.app.clone(), self.app.clone(), self.app.clone());
        let middle_app = self.app.clone();
        let drag = TabDrag {
            id,
            title: info.title.clone(),
        };
        div()
            .id((ids::TAB, ix))
            .group(group.clone())
            .relative()
            .flex_none()
            .min_w(dp(metrics::TAB_MIN_W))
            .max_w(dp(dims::TAB_MAX_W))
            .h(dp(metrics::TAB_STRIP_H))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .bg(background)
            .cursor_pointer()
            .hover(move |s| s.bg(with_layer(background, tokens.hover_layer())))
            .active(move |s| s.bg(with_layer(background, tokens.press_layer())))
            .on_click(move |_, window, cx| {
                select_app
                    .update(cx, |app, cx| {
                        if let Some(ix) = app.tabs().iter().position(|s| s.id == id) {
                            app.select_tab(ix, window, cx);
                        }
                    })
                    .ok();
            })
            .on_aux_click(move |event, window, cx| {
                if event.is_middle_click() {
                    middle_app
                        .update(cx, |app, cx| app.close_tab(id, window, cx))
                        .ok();
                }
            })
            .on_drag(drag, move |drag, _, window, cx| {
                drag_app
                    .update(cx, |app, cx| {
                        if let Some(ix) = app.tabs().iter().position(|s| s.id == drag.id) {
                            app.select_tab(ix, window, cx);
                        }
                    })
                    .ok();
                let title = drag.title.clone();
                cx.new(|_| TabGhost { title })
            })
            .when_some(info.tooltip.clone(), |tab, path| {
                tab.instant_tooltip(path, None)
            })
            .child(
                // The 720 dp cap is the text's; the box also holds the 16 dp start padding (border-box sizing).
                div()
                    .min_w_0()
                    .max_w(dp(metrics::TAB_TITLE_PAD_START + metrics::TAB_TITLE_MAX_W))
                    .pl(dp(metrics::TAB_TITLE_PAD_START))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_body2()
                    .medium()
                    .text_color(tokens.on_background)
                    .child(info.title.clone()),
            )
            .child(
                div()
                    .id((ids::TAB_CLOSE, ix))
                    .flex_none()
                    .size(dp(dims::TAB_CLOSE_W))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .when(!selected, |close| {
                        close.opacity(0.).group_hover(group, |s| s.opacity(1.))
                    })
                    .hover(move |s| s.bg(tokens.hover_layer()))
                    .active(move |s| s.bg(tokens.press_layer()))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        close_app
                            .update(cx, |app, cx| app.close_tab(id, window, cx))
                            .ok();
                    })
                    .child(icon(
                        AppIcon::Close,
                        metrics::TAB_CLOSE,
                        tokens.on_background,
                    ))
                    .test_support(),
            )
            .when(selected, |tab| {
                tab.child(
                    observed(ids::TAB_UNDERLINE)
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(dp(metrics::TAB_UNDERLINE_H))
                        .bg(tokens.primary_variant),
                )
            })
            .test_support()
            .into_any_element()
    }
}

impl Render for TabStripView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.moved_since_render = false;
        let tokens = *cx.tokens();
        let (tabs, welcome) = self.snapshot(cx);
        let active = tabs.iter().position(|t| t.selected);
        let active_id = active.map(|ix| tabs[ix].id);
        if self.last_active != active_id {
            self.last_active = active_id;
            if let Some(ix) = active {
                self.scroll.scroll_to_item(ix);
            }
        }
        // A welcome page gets the focus when it appears: on selection, and also when a slot falls back to a new
        // welcome page (a failed open keeps the tab id).
        let welcome_id = welcome.as_ref().map(|w| w.entity_id());
        if self.last_welcome != welcome_id {
            self.last_welcome = welcome_id;
            if let Some(welcome) = welcome {
                window.defer(cx, move |window, cx| {
                    welcome.update(cx, |page, cx| page.focus_page(window, cx));
                });
            }
        }
        let scale = window.rem_size().as_f32() / BASE_REM;
        let scrollbar = self.overflows().then(|| {
            let viewport = self.scroll.bounds().size.width.as_f32() / scale;
            let content = viewport + self.scroll.max_offset().x.as_f32() / scale;
            let offset = -self.scroll.offset().x.as_f32() / scale;
            let handle = self.scroll.clone();
            div()
                .id("tab-strip-scrollbar-tip")
                .w_full()
                .delayed_tooltip("\"Shift + Mouse wheel\" to scroll")
                .child(
                    HorizontalThumbBar::new(ids::TAB_SCROLLBAR, content, viewport, offset)
                        .on_scroll(move |offset, window, _| {
                            let current = handle.offset();
                            handle.set_offset(point(px(-offset * scale), current.y));
                            window.refresh();
                        }),
                )
        });
        let cells: Vec<_> = tabs
            .iter()
            .enumerate()
            .map(|(ix, info)| self.tab(ix, info, cx))
            .collect();
        let app = self.app.clone();
        observed(crate::ids::TAB_STRIP)
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .bg(tokens.background)
            .children(scrollbar)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .h(dp(metrics::TAB_STRIP_H))
                    .on_drag_move(cx.listener(
                        |this, event: &DragMoveEvent<TabDrag>, window, cx| {
                            let drag = event.drag(cx).clone();
                            this.on_drag_move(&drag, event, window, cx);
                        },
                    ))
                    .child(
                        div()
                            .id(ids::TAB_SCROLL)
                            .flex()
                            .flex_row()
                            .flex_shrink(1.)
                            .min_w_0()
                            .h_full()
                            .overflow_x_scroll()
                            .track_scroll(&self.scroll)
                            .children(cells),
                    )
                    .child(
                        div()
                            .id(ids::TAB_ADD)
                            .flex_none()
                            .size(dp(metrics::TAB_ADD))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .cursor_pointer()
                            .hover(move |s| s.bg(tokens.hover_layer()))
                            .active(move |s| s.bg(tokens.press_layer()))
                            .on_click(move |_, window, cx| {
                                app.update(cx, |app, cx| app.new_empty_tab(window, cx)).ok();
                            })
                            .child(icon(
                                AppIcon::Add,
                                metrics::TOOLBAR_ICON,
                                tokens.primary_variant,
                            ))
                            .test_support(),
                    ),
            )
    }
}
