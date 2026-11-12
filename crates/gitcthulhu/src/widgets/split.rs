//! The three-pane split of the repository frame and its gutters (Gitnuro 07 §5.3).
//!
//! Widths are in dp and owned by the caller (the repository view keeps them in `UiState`). Dragging a gutter
//! reports clamped widths through `on_change` continuously (fixing Gitnuro's start-only clamp: widths stay within
//! `[150 dp, 0.4 x total]`), and on release the widths are persisted to `UiState` and reported through
//! `on_commit`. The gutters are 8 dp wide, fully transparent, with the horizontal resize cursor.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, InteractiveElement as _, IntoElement,
    MouseButton, MouseUpEvent, ParentElement as _, Render, RenderOnce,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::{BoundsCell, track_bounds};
use crate::ids;
use crate::metrics::{self, BASE_REM, dp};
use crate::settings::state;

/// Widths of the two side panes in dp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitWidths {
    pub first: f32,
    pub third: f32,
}

impl Default for SplitWidths {
    fn default() -> Self {
        Self {
            first: metrics::SIDE_PANE_W,
            third: metrics::RIGHT_PANE_W,
        }
    }
}

/// Clamps a side pane width (dp) to `[PANE_MIN_W, PANE_MAX_FRACTION x total]`. The minimum wins when the
/// container is too narrow for both limits.
pub fn clamp_width(width: f32, total: f32) -> f32 {
    width.clamp(
        metrics::PANE_MIN_W,
        (metrics::PANE_MAX_FRACTION * total).max(metrics::PANE_MIN_W),
    )
}

/// Which side pane a gutter resizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GutterSide {
    /// The gutter between the first and the centre pane (`width += dx`).
    First,
    /// The gutter between the centre and the third pane (`width -= dx`).
    Third,
}

struct GutterDrag {
    side: GutterSide,
}

/// The (invisible) drag image: dragging only moves the gutter.
pub(crate) struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A resize gesture of a gutter, kept across frames (`window.use_keyed_state`) because the gutter element is
/// rebuilt on every frame of the drag.
///
/// GPUI recognises a drag only after the pointer moved a few pixels and reports positions relative to that
/// moment, so the gesture measures from the button press instead: the gutter stays under the pointer.
pub(crate) struct ResizeGesture<T> {
    /// Pointer coordinate (px, along the resize axis) and value at the button press.
    pub(crate) press: Option<(f32, T)>,
    /// The last value reported through `on_change` (what a release commits).
    pub(crate) last: Option<T>,
}

impl<T> Default for ResizeGesture<T> {
    fn default() -> Self {
        Self {
            press: None,
            last: None,
        }
    }
}

impl<T: Copy> ResizeGesture<T> {
    /// Ends the gesture: the value to commit if the press turned into a drag, else `None`.
    pub(crate) fn finish(&mut self, dragged: bool) -> Option<T> {
        let press = self.press.take();
        let last = self.last.take();
        if dragged {
            last.or(press.map(|(_, value)| value))
        } else {
            None
        }
    }
}

type WidthsHandler = Rc<dyn Fn(SplitWidths, &mut Window, &mut App)>;

/// One 8 dp resize gutter. Created by [`ThreePaneSplit`]; public so tests and other layouts can reuse it.
#[derive(IntoElement)]
pub struct SplitGutter {
    side: GutterSide,
    widths: SplitWidths,
    total: BoundsCell,
    on_change: Option<WidthsHandler>,
    on_commit: Option<WidthsHandler>,
}

impl SplitGutter {
    /// A gutter. `total` must hold the bounds of the whole split (see [`track_bounds`]).
    pub fn new(side: GutterSide, widths: SplitWidths, total: BoundsCell) -> Self {
        Self {
            side,
            widths,
            total,
            on_change: None,
            on_commit: None,
        }
    }

    /// Called with clamped widths while dragging.
    pub fn on_change(mut self, f: WidthsHandler) -> Self {
        self.on_change = Some(f);
        self
    }

    /// Called with the final widths when a drag of this gutter is released (not for plain clicks).
    pub fn on_commit(mut self, f: WidthsHandler) -> Self {
        self.on_commit = Some(f);
        self
    }
}

impl RenderOnce for SplitGutter {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let side = self.side;
        let widths = self.widths;
        let total = self.total.clone();
        let on_change = self.on_change.clone();
        let id = match side {
            GutterSide::First => ids::GUTTER_FIRST,
            GutterSide::Third => ids::GUTTER_THIRD,
        };
        let gesture = window.use_keyed_state(
            ElementId::from((ElementId::from(id), "resize")),
            cx,
            |_, _| ResizeGesture::<SplitWidths>::default(),
        );
        let finish = {
            let gesture = gesture.clone();
            let on_commit = self.on_commit.clone();
            move |_: &MouseUpEvent, window: &mut Window, cx: &mut App| {
                // The drag is still active while the release is dispatched.
                let dragged = cx.has_active_drag();
                let done = gesture.update(cx, |g, _| g.finish(dragged));
                if let (Some(widths), Some(f)) = (done, &on_commit) {
                    f(widths, window, cx);
                }
            }
        };
        let press = gesture.clone();
        div()
            .id(id)
            .flex_none()
            .w(dp(metrics::SPLIT_GUTTER))
            .h_full()
            .cursor_col_resize()
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let at = event.position.x.as_f32();
                press.update(cx, |g, _| {
                    *g = ResizeGesture {
                        press: Some((at, widths)),
                        last: None,
                    }
                });
            })
            .on_drag(GutterDrag { side }, |_, _, _, cx| cx.new(|_| DragGhost))
            .on_drag_move::<GutterDrag>(move |event, window, cx| {
                if event.drag(cx).side != side {
                    return;
                }
                let Some((press_x, start)) = gesture.read(cx).press else {
                    return;
                };
                let scale = window.rem_size().as_f32() / BASE_REM;
                let total_dp = total.get().size.width.as_f32() / scale;
                let moved_dp = (event.event.position.x.as_f32() - press_x) / scale;
                let next = match side {
                    GutterSide::First => SplitWidths {
                        first: clamp_width(start.first + moved_dp, total_dp),
                        ..start
                    },
                    GutterSide::Third => SplitWidths {
                        third: clamp_width(start.third - moved_dp, total_dp),
                        ..start
                    },
                };
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

/// The repository frame's split: optional first pane, flexible centre, third pane.
#[derive(IntoElement)]
pub struct ThreePaneSplit {
    first: Option<AnyElement>,
    center: AnyElement,
    third: AnyElement,
    widths: SplitWidths,
    on_change: Option<WidthsHandler>,
    on_commit: Option<WidthsHandler>,
}

impl ThreePaneSplit {
    /// A split with the given panes and widths. Pass `first = None` to hide the first pane and its gutter (the
    /// interactive rebase hides the side panel).
    pub fn new(
        first: Option<AnyElement>,
        center: impl IntoElement,
        third: impl IntoElement,
        widths: SplitWidths,
    ) -> Self {
        Self {
            first,
            center: center.into_any_element(),
            third: third.into_any_element(),
            widths,
            on_change: None,
            on_commit: None,
        }
    }

    /// Called with clamped widths while a gutter is dragged.
    pub fn on_change(mut self, f: impl Fn(SplitWidths, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    /// Called with the final widths when a drag ends (after they were persisted to `UiState`).
    pub fn on_commit(mut self, f: impl Fn(SplitWidths, &mut Window, &mut App) + 'static) -> Self {
        self.on_commit = Some(Rc::new(f));
        self
    }
}

/// Persists widths to `UiState` unless they are already stored.
fn persist(widths: SplitWidths, cx: &mut App) {
    let unchanged = cx.try_global::<state::UiStateGlobal>().is_some_and(|s| {
        s.0.first_pane_width == widths.first && s.0.third_pane_width == widths.third
    });
    if !unchanged {
        state::update(cx, |s| {
            s.first_pane_width = widths.first;
            s.third_pane_width = widths.third;
        });
    }
}

impl RenderOnce for ThreePaneSplit {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let total: BoundsCell = Rc::new(Cell::new(Default::default()));
        let widths = self.widths;
        let user_commit = self.on_commit;
        let commit: WidthsHandler = Rc::new(move |w, window, cx| {
            persist(w, cx);
            if let Some(f) = &user_commit {
                f(w, window, cx);
            }
        });
        let gutter = |side: GutterSide| {
            let mut g = SplitGutter::new(side, widths, total.clone()).on_commit(commit.clone());
            if let Some(f) = &self.on_change {
                g = g.on_change(f.clone());
            }
            g
        };
        let has_first = self.first.is_some();
        let first_gutter = has_first.then(|| gutter(GutterSide::First));
        let third_gutter = gutter(GutterSide::Third);
        div()
            .id(ids::SPLIT)
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .child(track_bounds(&total))
            .children(self.first.map(|first| {
                div()
                    .id(ids::PANE_FIRST)
                    .flex_none()
                    .w(dp(widths.first))
                    .h_full()
                    .child(first)
                    .test_support()
            }))
            .children(first_gutter)
            .child(
                div()
                    .id(ids::PANE_CENTER)
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.center)
                    .test_support(),
            )
            .child(third_gutter)
            .child(
                div()
                    .id(ids::PANE_THIRD)
                    .flex_none()
                    .w(dp(widths.third))
                    .h_full()
                    .child(self.third)
                    .test_support(),
            )
            .test_support()
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn clamp_keeps_widths_between_150_and_forty_percent() {
        assert_eq!(clamp_width(100., 1280.), 150., "below the minimum");
        assert_eq!(clamp_width(300., 1280.), 300., "inside the range");
        assert_eq!(clamp_width(900., 1280.), 512., "above 40 % of the total");
        assert_eq!(
            clamp_width(900., 300.),
            150.,
            "the minimum wins in a narrow window"
        );
    }

    #[test]
    fn gesture_commits_only_after_a_drag() {
        let mut click = ResizeGesture {
            press: Some((10., 1.0_f32)),
            last: None,
        };
        assert_eq!(click.finish(false), None, "a plain click commits nothing");
        assert!(click.press.is_none(), "and ends the gesture");

        let mut drag = ResizeGesture {
            press: Some((10., 1.0_f32)),
            last: Some(2.0),
        };
        assert_eq!(
            drag.finish(true),
            Some(2.0),
            "a drag commits the last reported value"
        );

        let mut foreign = ResizeGesture::<f32>::default();
        assert_eq!(
            foreign.finish(true),
            None,
            "a drag that did not start on this gutter commits nothing"
        );
    }
}
