//! `AppSwitch` (Gitnuro 07 §10.4): a 48 x 28 pill with a 20 dp thumb; no animation.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, kit_map};

/// A switch. `on_toggle` receives the new value.
#[derive(IntoElement)]
pub struct AppSwitch {
    id: ElementId,
    checked: bool,
    enabled: bool,
    on_toggle: Option<Rc<dyn Fn(bool, &mut Window, &mut App)>>,
}

impl AppSwitch {
    /// A switch. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            id: id.into(),
            checked,
            enabled: true,
            on_toggle: None,
        }
    }

    /// Enables or disables the switch (disabled: track alpha x 0.3, not clickable).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the handler that receives the toggled value.
    pub fn on_toggle(mut self, f: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for AppSwitch {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let checked = self.checked;
        let track = if checked {
            tokens.primary
        } else {
            kit_map::switch_track(&tokens)
        };
        let track = if self.enabled {
            track
        } else {
            track.opacity(0.3)
        };
        let on_toggle = self.on_toggle;
        div()
            .id(self.id)
            .flex_none()
            .w(dp(metrics::SWITCH_W))
            .h(dp(metrics::SWITCH_H))
            .rounded_full()
            .bg(track)
            .py(dp(4.))
            .pr(dp(4.))
            .pl(dp(if checked { 24. } else { 4. }))
            .cursor_pointer()
            .when(self.enabled, |s| {
                s.when_some(on_toggle, |s, f| {
                    s.on_click(move |_, window, cx| f(!checked, window, cx))
                })
            })
            .child(div().size(dp(20.)).rounded_full().bg(tokens.on_secondary))
            .test_support()
    }
}
