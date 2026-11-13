//! `VResizer`: the 8 dp horizontal gutter on the top edge of a bottom panel (the terminal) for vertical resizing.
//!
//! The panel height is owned by the caller. Dragging up grows the panel; heights are clamped to
//! `[min, max]` (dp) continuously and reported through `on_change`, and on release through `on_commit`.

use std::rc::Rc;

use gpui_kit::{
    App, AppContext as _, ElementId, InteractiveElement as _, IntoElement, MouseButton,
    MouseUpEvent, RenderOnce, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window, div,
};

use super::split::{DragGhost, ResizeGesture};
use crate::metrics::{self, BASE_REM, dp};

struct VDrag;

type HeightHandler = Rc<dyn Fn(f32, &mut Window, &mut App)>;

/// A vertical-resize gutter above a panel of `height` dp.
#[derive(IntoElement)]
pub struct VResizer {
    id: ElementId,
    height: f32,
    min: f32,
    max: f32,
    on_change: Option<HeightHandler>,
    on_commit: Option<HeightHandler>,
}

impl VResizer {
    /// A gutter for a panel currently `height` dp high, clamped to `[min, max]` dp. `id` must be unique in the
    /// frame.
    pub fn new(id: impl Into<ElementId>, height: f32, min: f32, max: f32) -> Self {
        Self {
            id: id.into(),
            height,
            min,
            max: max.max(min),
            on_change: None,
            on_commit: None,
        }
    }

    /// Called with the clamped height while dragging.
    pub fn on_change(mut self, f: impl Fn(f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    /// Called with the final height when a drag of this gutter is released (not for plain clicks).
    pub fn on_commit(mut self, f: impl Fn(f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_commit = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for VResizer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (height, min, max) = (self.height, self.min, self.max);
        let on_change = self.on_change;
        let gesture =
            window.use_keyed_state(ElementId::from((self.id.clone(), "resize")), cx, |_, _| {
                ResizeGesture::<f32>::default()
            });
        let finish = {
            let gesture = gesture.clone();
            let on_commit = self.on_commit;
            move |_: &MouseUpEvent, window: &mut Window, cx: &mut App| {
                // The drag is still active while the release is dispatched.
                let dragged = cx.has_active_drag();
                let done = gesture.update(cx, |g, _| g.finish(dragged));
                if let (Some(height), Some(f)) = (done, &on_commit) {
                    f(height, window, cx);
                }
            }
        };
        let press = gesture.clone();
        div()
            .id(self.id)
            .flex_none()
            .w_full()
            .h(dp(metrics::SPLIT_GUTTER))
            .cursor_row_resize()
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let at = event.position.y.as_f32();
                press.update(cx, |g, _| {
                    *g = ResizeGesture {
                        press: Some((at, height)),
                        last: None,
                    }
                });
            })
            .on_drag(VDrag, |_, _, _, cx| cx.new(|_| DragGhost))
            .on_drag_move::<VDrag>(move |event, window, cx| {
                let Some((press_y, start)) = gesture.read(cx).press else {
                    return;
                };
                let scale = window.rem_size().as_f32() / BASE_REM;
                // Dragging up (smaller y) grows the panel below the gutter.
                let moved_dp = (event.event.position.y.as_f32() - press_y) / scale;
                let next = (start - moved_dp).clamp(min, max);
                let previous = gesture.read(cx).last.unwrap_or(start);
                gesture.update(cx, |g, _| g.last = Some(next));
                if next != previous
                    && let Some(f) = &on_change
                {
                    f(next, window, cx);
                }
            })
            .on_mouse_up(MouseButton::Left, finish.clone())
            .on_mouse_up_out(MouseButton::Left, finish)
            .test_support()
    }
}
