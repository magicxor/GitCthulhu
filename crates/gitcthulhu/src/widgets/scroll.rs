//! Scroll containers (Gitnuro 07 §10.6, 08 §0.5): content with an always-visible overlay scrollbar that takes no
//! layout width, plus a minimal horizontal thumb bar for the commit graph.
//!
//! The scrollbar colours come from the kit theme, which `theme::kit_map` fills from `normal_scrollbar` and
//! `hover_scrollbar` and puts in `ScrollbarMode::Always`.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::{Scrollbar, ScrollbarHandle};
use gpui_kit::component::scroll::{Scrollable, ScrollableElement as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Div, ElementId, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, RenderOnce, Stateful, Styled as _, TestSupportExt as _, Window, div,
};

use super::{BoundsCell, track_bounds};
use crate::metrics::dp;
use crate::theme::ActiveTokens as _;

/// A vertically scrolling container with the kit's overlay scrollbar. Add children and size it like a `div`.
/// `id` keeps the scroll position and must be unique among scroll containers of the frame.
pub fn scroll_y(id: impl Into<ElementId>) -> Scrollable<Stateful<Div>> {
    let id = id.into();
    div().id(id.clone()).overflow_y_scrollbar().id(id)
}

/// An overlay vertical scrollbar for a virtualised list (`uniform_list` or `list`). Place it as the last child of
/// the `relative()` container that holds the list; it spans the container and does not take width.
pub fn list_scrollbar<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    handle: &H,
) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .child(Scrollbar::vertical(handle).id(id).viewport_from_layout())
}

/// A thin horizontal thumb bar (the log's graph column scrolls horizontally without a kit scroll container).
///
/// `content_width` and `viewport_width` are in dp; `offset` is the scrolled distance in dp. Clicking the track
/// reports the offset that centres the thumb on the pointer through `on_scroll`.
#[derive(IntoElement)]
pub struct HorizontalThumbBar {
    id: ElementId,
    content_width: f32,
    viewport_width: f32,
    offset: f32,
    on_scroll: Option<Rc<dyn Fn(f32, &mut Window, &mut App)>>,
}

impl HorizontalThumbBar {
    /// A bar for content of `content_width` shown in `viewport_width`, scrolled by `offset` (all dp).
    pub fn new(
        id: impl Into<ElementId>,
        content_width: f32,
        viewport_width: f32,
        offset: f32,
    ) -> Self {
        Self {
            id: id.into(),
            content_width,
            viewport_width,
            offset,
            on_scroll: None,
        }
    }

    /// Sets the handler that receives the new offset in dp.
    pub fn on_scroll(mut self, f: impl Fn(f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_scroll = Some(Rc::new(f));
        self
    }

    /// Thumb geometry as fractions of the track: `(start, length)`, or `None` when nothing scrolls.
    pub fn thumb(content_width: f32, viewport_width: f32, offset: f32) -> Option<(f32, f32)> {
        if content_width <= viewport_width || content_width <= 0. {
            return None;
        }
        let length = (viewport_width / content_width).clamp(0.05, 1.0);
        let max_offset = content_width - viewport_width;
        let start = (offset / max_offset).clamp(0.0, 1.0) * (1.0 - length);
        Some((start, length))
    }
}

impl RenderOnce for HorizontalThumbBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let thumb = Self::thumb(self.content_width, self.viewport_width, self.offset);
        let on_scroll = self.on_scroll;
        let (content, viewport) = (self.content_width, self.viewport_width);
        let bounds: BoundsCell = Rc::new(Cell::new(Default::default()));
        let click_bounds = bounds.clone();
        let _ = window;
        div()
            .id(self.id)
            .relative()
            .w_full()
            .h(dp(8.))
            .when(thumb.is_none(), |d| d.invisible())
            .when_some(thumb, |track, (start, length)| {
                track
                    .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, cx| {
                        let Some(on_scroll) = on_scroll.as_ref() else {
                            return;
                        };
                        // Offset (dp) that puts the thumb centre under the pointer.
                        let track = click_bounds.get();
                        let click = (ev.position.x - track.origin.x).as_f32()
                            / track.size.width.as_f32().max(1.0);
                        let wanted = (click - length / 2.).clamp(0.0, 1.0 - length);
                        let scrollable = (content - viewport).max(0.0);
                        let denom = (1.0 - length).max(f32::EPSILON);
                        on_scroll(wanted / denom * scrollable, window, cx);
                    })
                    .child(track_bounds(&bounds))
                    .child(
                        div()
                            .absolute()
                            .top(dp(2.))
                            .h(dp(4.))
                            .left(gpui_kit::relative(start))
                            .w(gpui_kit::relative(length))
                            .rounded(dp(2.))
                            .bg(tokens.normal_scrollbar),
                    )
            })
            .test_support()
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn no_thumb_when_content_fits() {
        assert_eq!(HorizontalThumbBar::thumb(100., 100., 0.), None);
        assert_eq!(HorizontalThumbBar::thumb(80., 100., 0.), None);
    }

    #[test]
    fn thumb_reaches_the_end_at_max_offset() {
        let (start, length) = HorizontalThumbBar::thumb(400., 100., 300.).expect("thumb");
        assert!((length - 0.25).abs() < 1e-6, "length is viewport/content");
        assert!(
            (start + length - 1.0).abs() < 1e-6,
            "thumb ends at the track end"
        );
        let (start0, _) = HorizontalThumbBar::thumb(400., 100., 0.).expect("thumb");
        assert_eq!(start0, 0.0, "thumb starts at the track start");
    }
}
