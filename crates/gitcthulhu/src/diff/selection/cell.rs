//! The selectable text cell: a `StyledText` that reports its run and paints the selection behind its glyphs.

use std::ops::Range;

use gpui_kit::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, StyledText, TextLayout, Window, fill, point, px, size,
};

use super::geometry::CellRun;
use super::{SelKey, SelSide, Shared, finish_frame, insert_hitbox};

/// One row's text cell (see `DiffSelection::text_cell`).
pub(super) struct SelectableCell {
    key: SelKey,
    text: StyledText,
    /// The display text of the line, when the document has one for the key.
    display: Option<SharedString>,
    shared: Shared,
    color: Hsla,
}

impl SelectableCell {
    pub fn new(
        key: SelKey,
        text: StyledText,
        display: Option<SharedString>,
        shared: Shared,
        color: Hsla,
    ) -> Self {
        Self {
            key,
            text,
            display,
            shared,
            color,
        }
    }
}

impl IntoElement for SelectableCell {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for SelectableCell {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        let side = match self.key.side {
            SelSide::Old => 0,
            SelSide::New => 1,
            SelSide::Unified => 2,
        };
        Some(ElementId::NamedInteger(
            "diff-selection-cell".into(),
            (u64::from(self.key.line) << 2) | side,
        ))
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .prepaint(id, inspector_id, bounds, state, window, cx);
        let mut inner = self.shared.borrow_mut();
        if inner.frame.painted {
            inner.frame.begin();
        }
        insert_hitbox(&mut inner.frame, window);
        let layout = self.text.layout().clone();
        // The run's text must be the laid-out text, or offsets would not match glyphs.
        let text = match &self.display {
            Some(display) if display.len() == layout.len() => display.clone(),
            _ => SharedString::from(layout.text()),
        };
        inner.frame.runs.push(CellRun {
            key: self.key,
            text,
            layout,
            bounds,
        });
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut (),
        prepaint_state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if !self.shared.borrow().frame.painted {
            finish_frame(&self.shared, window, cx);
        }
        let layout = self.text.layout().clone();
        let range = {
            let inner = self.shared.borrow();
            let text = match &self.display {
                Some(display) if display.len() == layout.len() => display.clone(),
                _ => SharedString::from(layout.text()),
            };
            inner.painted_selection.run_range(self.key, &text)
        };
        let Some(range) = range else {
            self.text.paint(
                id,
                inspector_id,
                bounds,
                layout_state,
                prepaint_state,
                window,
                cx,
            );
            return;
        };
        // Run backgrounds (intra-line highlights), then the selection, then the glyphs over both.
        if let Err(err) = layout.paint_background(window, cx) {
            tracing::debug!("diff text background: {err}");
        }
        paint_selection(&layout, bounds, range, self.color, window);
        if let Err(err) = layout.paint_foreground(window, cx) {
            tracing::debug!("diff text: {err}");
        }
    }
}

/// Paints the selection of `range` over the rows of a (possibly wrapped) text layout: from the start to the end of
/// the cell on the first row, whole rows in between, the start of the last row up to the end.
fn paint_selection(
    layout: &TextLayout,
    bounds: Bounds<Pixels>,
    range: Range<usize>,
    color: Hsla,
    window: &mut Window,
) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    let line_height = layout.line_height();
    let mut quad = |left: Pixels, top: Pixels, right: Pixels| {
        if right > left {
            window.paint_quad(fill(
                Bounds::new(point(left, top), size(right - left, line_height)),
                color,
            ));
        }
    };
    if (end.y - start.y).abs() < px(0.5) {
        quad(start.x, start.y, end.x);
        return;
    }
    quad(start.x, start.y, bounds.right());
    let mut top = start.y + line_height;
    while top + px(0.5) < end.y {
        quad(bounds.left(), top, bounds.right());
        top += line_height;
    }
    quad(bounds.left(), end.y, end.x);
}
