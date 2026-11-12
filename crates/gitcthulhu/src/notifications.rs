//! Toasts (spec 21 §2.13.4, Gitnuro 07 §8): short-lived notifications stacked above the bottom bar of the
//! active tab, optionally with a text action ("Show output", "Undo").

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AsyncApp, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window, div,
};

use crate::icons::{AppIcon, icon};
use crate::ids;
use crate::metrics::{self, dp};
use crate::tab::TabId;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::observed;

/// Gitnuro's three notification types (07 §8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// Success or information: `primary` block with the info icon.
    Positive,
    /// `secondary` block with the warning icon.
    Warning,
    /// `error` block with the error icon.
    Error,
}

/// A text button inside a toast. A toast with an action stays [`ACTION_TOAST_MS`] instead of
/// [`metrics::TOAST_MS`].
#[derive(Clone)]
pub struct ToastAction {
    pub label: SharedString,
    pub run: Rc<dyn Fn(&mut Window, &mut App)>,
}

/// How long a toast with an action stays.
pub const ACTION_TOAST_MS: u64 = 5000;

/// One notification.
#[derive(Clone)]
pub struct Toast {
    /// Unique within the host.
    pub id: u64,
    /// The tab the toast belongs to; `None` toasts show on every tab.
    pub tab: Option<TabId>,
    pub kind: ToastKind,
    pub text: SharedString,
    pub action: Option<ToastAction>,
}

/// The toast layer of the window.
#[derive(Default)]
pub struct ToastHost {
    toasts: Vec<Toast>,
    next_id: u64,
    active_tab: Option<TabId>,
}

impl ToastHost {
    /// An empty host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows a toast tagged with `tab` (`None` = visible on every tab). It disappears after 2 s, or 5 s when it
    /// has an action. Returns the toast id.
    pub fn show(
        &mut self,
        tab: Option<TabId>,
        kind: ToastKind,
        text: impl Into<SharedString>,
        action: Option<ToastAction>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let ms = if action.is_some() {
            ACTION_TOAST_MS
        } else {
            metrics::TOAST_MS
        };
        self.toasts.push(Toast {
            id,
            tab,
            kind,
            text: text.into(),
            action,
        });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor()
                .timer(Duration::from_millis(ms))
                .await;
            this.update(cx, |host, cx| host.dismiss(id, cx)).ok();
        })
        .detach();
        cx.notify();
        id
    }

    /// Removes a toast (no-op when it is already gone).
    pub fn dismiss(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    /// Selects whose toasts are rendered (the active tab; `None` on the welcome page).
    pub fn set_active_tab(&mut self, tab: Option<TabId>, cx: &mut Context<Self>) {
        if self.active_tab != tab {
            self.active_tab = tab;
            cx.notify();
        }
    }

    /// Drops every toast of a closed tab.
    pub fn clear_tab(&mut self, tab: TabId, cx: &mut Context<Self>) {
        self.toasts.retain(|t| t.tab != Some(tab));
        cx.notify();
    }

    /// The toasts rendered now, oldest first (07 §8: oldest at the top).
    pub fn visible(&self) -> impl Iterator<Item = &Toast> {
        self.toasts
            .iter()
            .filter(|t| t.tab.is_none() || t.tab == self.active_tab)
    }

    fn render_toast(&self, toast: &Toast, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let (block, content, role) = match toast.kind {
            ToastKind::Positive => (tokens.primary, tokens.on_primary, AppIcon::Info),
            ToastKind::Warning => (tokens.secondary, tokens.on_secondary, AppIcon::Warning),
            ToastKind::Error => (tokens.error, tokens.on_error, AppIcon::Error),
        };
        let id = toast.id;
        // Outer margin 8, then a 2 dp border around a 32 dp block: 36 dp tall (07 §8). Compose draws the border over
        // its 2 dp padding; a GPUI border takes layout space itself, so there is no extra padding here.
        div()
            .p(dp(metrics::TOAST_MARGIN))
            .child(
                observed(("toast", id as usize))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(dp(8.))
                    .h(dp(metrics::TOAST_H))
                    .border(dp(2.))
                    .border_color(tokens.popup_border())
                    .rounded(dp(metrics::RADIUS_LG))
                    .bg(tokens.background)
                    .overflow_hidden()
                    .child(
                        observed(("toast-block", id as usize))
                            .flex_none()
                            .size(dp(metrics::TOAST_BLOCK))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_l(dp(6.))
                            .bg(block)
                            .child(icon(role, 24., content).p(dp(4.))),
                    )
                    .child(
                        div()
                            .pr(dp(8.))
                            .text_body1()
                            .text_color(tokens.on_background)
                            .child(toast.text.clone()),
                    )
                    .when_some(toast.action.clone(), |row, action| {
                        row.child(self.render_action(id, action, cx))
                    }),
            )
            .into_any_element()
    }

    fn render_action(&self, id: u64, action: ToastAction, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        div()
            .id(("toast-action", id as usize))
            .px(dp(8.))
            .mr(dp(4.))
            .rounded(dp(metrics::RADIUS))
            .text_button()
            .text_color(tokens.primary_variant)
            .cursor_pointer()
            .hover(move |s| s.bg(tokens.hover_layer()))
            .child(action.label.clone())
            .on_click(cx.listener(move |host, _, window, cx| {
                let run = action.run.clone();
                host.dismiss(id, cx);
                run(window, cx);
            }))
            .test_support()
            .into_any_element()
    }
}

impl Render for ToastHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toasts: Vec<Toast> = self.visible().cloned().collect();
        let cards: Vec<AnyElement> = toasts.iter().map(|t| self.render_toast(t, cx)).collect();
        observed(ids::TOAST_LAYER)
            .absolute()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .pb(dp(metrics::TOAST_BOTTOM))
            .children(cards)
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use std::cell::Cell;
    use std::time::Duration;

    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

    use super::*;
    use crate::test_support::geometry::{
        assert_height, assert_rect, bounds, init_visual, window_options,
    };

    fn mount(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ToastHost>) {
        init_visual(cx);
        cx.update(|cx| {
            gpui_kit::open_window(window_options(1280., 720.), cx, |_, cx| {
                cx.new(|_| ToastHost::new())
            })
            .expect("open the test window")
        })
    }

    fn count(cx: &mut TestAppContext, host: &Entity<ToastHost>) -> usize {
        cx.update(|cx| host.read(cx).visible().count())
    }

    #[gpui_kit::test]
    fn toast_expires_after_2s(cx: &mut TestAppContext) {
        let (handle, host) = mount(cx);
        cx.update(|cx| {
            host.update(cx, |h, cx| {
                h.show(None, ToastKind::Positive, "Pushed", None, cx);
            });
        });
        assert_eq!(count(cx, &host), 1, "the toast is visible right away");
        cx.executor().advance_clock(Duration::from_millis(1900));
        cx.run_until_parked();
        assert_eq!(count(cx, &host), 1, "still visible before 2 s");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_height(window, ("toast", 0usize), metrics::TOAST_H);
            // The coloured 32 dp block sits right inside the 2 dp border (07 §8: 2 + 32 + 2 = 36).
            let card = bounds(window, ("toast", 0usize));
            assert_rect(
                window,
                ("toast-block", 0usize),
                card.origin.x.as_f32() + 2.,
                card.origin.y.as_f32() + 2.,
                metrics::TOAST_BLOCK,
                metrics::TOAST_BLOCK,
            );
        })
        .expect("window");
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert_eq!(count(cx, &host), 0, "gone after 2 s");
    }

    #[gpui_kit::test]
    fn toast_with_action_stays_5s_and_runs(cx: &mut TestAppContext) {
        let (handle, host) = mount(cx);
        let ran = Rc::new(Cell::new(0));
        let counter = ran.clone();
        cx.update(|cx| {
            host.update(cx, |h, cx| {
                h.show(
                    None,
                    ToastKind::Warning,
                    "Hook failed",
                    Some(ToastAction {
                        label: "Show output".into(),
                        run: Rc::new(move |_, _| counter.set(counter.get() + 1)),
                    }),
                    cx,
                );
            });
        });
        cx.executor().advance_clock(Duration::from_millis(2500));
        cx.run_until_parked();
        assert_eq!(count(cx, &host), 1, "an action toast outlives 2 s");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(("toast-action", 0usize), cx);
        })
        .expect("window");
        assert_eq!(ran.get(), 1, "the action ran once");
        assert_eq!(
            count(cx, &host),
            0,
            "running the action dismisses the toast"
        );
    }

    #[gpui_kit::test]
    fn action_toast_expires_after_5s(cx: &mut TestAppContext) {
        let (_, host) = mount(cx);
        cx.update(|cx| {
            host.update(cx, |h, cx| {
                h.show(
                    None,
                    ToastKind::Positive,
                    "Done",
                    Some(ToastAction {
                        label: "Undo".into(),
                        run: Rc::new(|_, _| {}),
                    }),
                    cx,
                );
            });
        });
        cx.executor().advance_clock(Duration::from_millis(5100));
        cx.run_until_parked();
        assert_eq!(count(cx, &host), 0, "gone after 5 s");
    }

    #[gpui_kit::test]
    fn toasts_follow_the_active_tab(cx: &mut TestAppContext) {
        let (_, host) = mount(cx);
        cx.update(|cx| {
            host.update(cx, |h, cx| {
                h.show(Some(TabId(1)), ToastKind::Positive, "one", None, cx);
                h.show(Some(TabId(2)), ToastKind::Error, "two", None, cx);
                h.show(None, ToastKind::Warning, "all", None, cx);
                h.set_active_tab(Some(TabId(2)), cx);
            });
            let texts: Vec<String> = host
                .read(cx)
                .visible()
                .map(|t| t.text.to_string())
                .collect();
            assert_eq!(
                texts,
                ["two", "all"],
                "only the active tab's and untagged toasts show, oldest first"
            );
        });
    }

    #[gpui_kit::test]
    fn toast_stack_is_bottom_centred_above_the_bar(cx: &mut TestAppContext) {
        let (handle, host) = mount(cx);
        cx.update(|cx| {
            host.update(cx, |h, cx| {
                h.show(None, ToastKind::Positive, "first", None, cx);
                h.show(None, ToastKind::Error, "second", None, cx);
            });
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let first = bounds(window, ("toast", 0usize));
            let second = bounds(window, ("toast", 1usize));
            assert!(
                first.origin.y < second.origin.y,
                "oldest toast is at the top"
            );
            let bottom = second.origin.y.as_f32() + second.size.height.as_f32();
            assert!(
                (bottom - (720. - metrics::TOAST_BOTTOM - metrics::TOAST_MARGIN)).abs() <= 0.5,
                "the lowest card ends 56 dp above the bottom (48 padding + 8 margin), got {bottom}"
            );
            let centre = second.origin.x.as_f32() + second.size.width.as_f32() / 2.;
            assert!(
                (centre - 640.).abs() <= 0.5,
                "toasts are horizontally centred, got {centre}"
            );
        })
        .expect("window");
    }
}
