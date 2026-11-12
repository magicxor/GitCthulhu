//! Text selection of the blame's code column (09 §2.1: "the only selectable column").
//!
//! The column is one participant of gpui-base's window selection. Every painted line is a [`CodeLine`] that reports
//! its laid-out run and paints the selected part; the [`CodeColumn`] around the list registers the participant's
//! geometry once per frame. Because the list is virtualised, the copy text is assembled from the painted
//! projections plus the full text of the lines between the endpoints that are scrolled out (the endpoints carry a
//! stable line key resolved from the pointer position). The byte offset of an endpoint is remembered while its line
//! is painted, so an endpoint that scrolled out still cuts its line where the selection starts or ends.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::{Rc, Weak};

use gpui_kit::base::{
    TextSelectionContentKey, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun,
};
use gpui_kit::{
    AnyElement, App, BorderStyle, Bounds, Corners, Edges, Element, ElementId, GlobalElementId,
    HighlightStyle, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    PaintQuad, Pixels, Point, SharedString, StyledText, UniformListScrollHandle, Window, point, px,
    size, transparent_black,
};

use super::doc::BlameDoc;

/// Shared state of the code column's selection. Clones share everything.
#[derive(Clone)]
pub(crate) struct SelectionBridge(Rc<Inner>);

struct Inner {
    handle: TextSelectionHandle,
    /// The runs painted this frame (hit testing, word selection).
    runs: RefCell<Vec<TextSelectionRun>>,
    /// The selected byte range (in display text) of every painted line that has one, by line.
    selected: RefCell<BTreeMap<u64, Range<usize>>>,
    /// Select all is active (mirrors the handle's local selection).
    all: Cell<bool>,
    /// The document the rows show (copy text of lines that are not painted).
    doc: RefCell<Option<BlameDoc>>,
    /// Row height of the last frame, in pixels (maps content points to line keys).
    row_h: Cell<Pixels>,
    /// Where the selection's endpoints sit inside their lines, as last painted (see [`Endpoint`]).
    endpoints: RefCell<BTreeMap<u64, Endpoint>>,
}

/// The part of an endpoint line that the selection covers, remembered while the line was painted.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Endpoint {
    /// The line holds one endpoint: its byte offset in the display text.
    Offset(usize),
    /// The line holds both endpoints: the selected byte range.
    Range(Range<usize>),
}

impl SelectionBridge {
    /// A bridge with its own selection participant.
    pub(crate) fn new(cx: &mut App) -> Self {
        let handle = TextSelectionHandle::new(String::new(), cx);
        let bridge = Self(Rc::new(Inner {
            handle,
            runs: RefCell::default(),
            selected: RefCell::default(),
            all: Cell::new(false),
            doc: RefCell::default(),
            row_h: Cell::new(px(0.)),
            endpoints: RefCell::default(),
        }));
        let weak = Rc::downgrade(&bridge.0);
        bridge.0.handle.copy_with(
            {
                let weak = weak.clone();
                move |cx| {
                    weak.upgrade()
                        .map(|i| copy_text(&i, cx))
                        .unwrap_or_default()
                }
            },
            cx,
        );
        bridge
            .0
            .handle
            .resolve_content_key_with(move |point, _| line_key(&weak, point), cx);
        bridge
    }

    /// Sets the document shown (for the copy text).
    pub(crate) fn set_doc(&self, doc: Option<BlameDoc>) {
        *self.0.doc.borrow_mut() = doc;
    }

    /// Selects the whole file (the context menu's "Select all").
    pub(crate) fn select_all(&self, cx: &mut App) {
        self.0.handle.set_local_selection(true, cx);
    }

    /// The selected text as copying would produce it.
    pub(crate) fn selected_text(&self, cx: &App) -> String {
        copy_text(&self.0, cx)
    }

    /// Whether any text is selected.
    pub(crate) fn has_selection(&self, cx: &App) -> bool {
        self.0.handle.has_local_selection(cx)
            || !self.0.selected.borrow().is_empty()
            // The whole selection may have scrolled out of the list: its endpoints still differ.
            || self.0.handle.snapshot(cx).is_some_and(|s| {
                s.anchor().content_point() != s.cursor().content_point()
            })
    }
}

/// The line keys of the selection's anchor and cursor.
fn endpoint_lines(inner: &Inner, cx: &App) -> Option<(u64, u64)> {
    let snapshot = inner.handle.snapshot(cx)?;
    Some((
        snapshot.anchor().content_key()?.value(),
        snapshot.cursor().content_key()?.value(),
    ))
}

/// Remembers where the painted `line` holds an endpoint of the selection (`range` is what is selected on it).
fn remember_endpoint(inner: &Inner, line: u64, range: &Range<usize>, cx: &App) {
    let Some((anchor, cursor)) = endpoint_lines(inner, cx) else {
        return;
    };
    if line != anchor && line != cursor {
        return;
    }
    let (first, last) = (anchor.min(cursor), anchor.max(cursor));
    let endpoint = if first == last {
        Endpoint::Range(range.clone())
    } else if line == first {
        // The selection runs from here to the end of the line.
        Endpoint::Offset(range.start)
    } else {
        // The selection runs from the start of the line to here.
        Endpoint::Offset(range.end)
    };
    inner.endpoints.borrow_mut().insert(line, endpoint);
}

fn line_key(inner: &Weak<Inner>, point: Point<Pixels>) -> Option<TextSelectionContentKey> {
    let inner = inner.upgrade()?;
    let row_h = inner.row_h.get();
    if row_h <= px(0.) {
        return None;
    }
    let line = (point.y / row_h).floor().max(0.) as u64;
    Some(TextSelectionContentKey::new(line))
}

fn slice_of(doc: &BlameDoc, line: u64, range: &Range<usize>) -> String {
    let source = doc.line_text(line as usize);
    let shown = doc.display(line as usize);
    if range.start == 0 && range.end >= shown.len() && shown.len() >= source.len() {
        return source.to_owned();
    }
    shown
        .get(range.clone())
        .map(str::to_owned)
        .unwrap_or_default()
}

fn copy_text(inner: &Inner, cx: &App) -> String {
    let doc = inner.doc.borrow();
    let Some(doc) = doc.as_ref() else {
        return String::new();
    };
    if inner.handle.has_local_selection(cx) {
        return doc.full_text();
    }
    let selected = inner.selected.borrow();
    let keys = inner.handle.snapshot(cx).and_then(|s| {
        Some((
            s.anchor().content_key()?.value(),
            s.cursor().content_key()?.value(),
        ))
    });
    let (lo, hi) = match keys {
        Some((a, b)) => (a.min(b), a.max(b)),
        None => match (selected.keys().next(), selected.keys().next_back()) {
            (Some(&lo), Some(&hi)) => (lo, hi),
            _ => return String::new(),
        },
    };
    let endpoints = inner.endpoints.borrow();
    let last = doc.line_count().saturating_sub(1) as u64;
    let mut out = Vec::new();
    for line in lo..=hi.min(last) {
        if let Some(range) = selected.get(&line) {
            out.push(slice_of(doc, line, range));
            continue;
        }
        // Not painted: scrolled out. An endpoint line is cut at the offset remembered from when it was painted,
        // every line between the endpoints is whole; a lone endpoint line without memory adds nothing.
        let shown_len = doc.display(line as usize).len();
        match (endpoints.get(&line), lo < hi) {
            (Some(Endpoint::Range(range)), _) if lo == hi => {
                out.push(slice_of(doc, line, range));
            }
            (Some(Endpoint::Offset(at)), true) if line == lo => {
                out.push(slice_of(doc, line, &(*at.min(&shown_len)..shown_len)));
            }
            (Some(Endpoint::Offset(at)), true) if line == hi => {
                out.push(slice_of(doc, line, &(0..*at.min(&shown_len))));
            }
            (_, true) => out.push(doc.line_text(line as usize).to_owned()),
            _ => {}
        }
    }
    out.join("\n")
}

/// One painted code line: syntax-coloured text that reports its run to the selection and paints the selected part.
pub(crate) struct CodeLine {
    line: u64,
    text: SharedString,
    styled: StyledText,
    bridge: SelectionBridge,
    selection_color: Hsla,
}

impl CodeLine {
    /// Line `line` (0-based) with the display `text` and its highlight runs (non-overlapping, ascending).
    pub(crate) fn new(
        line: usize,
        text: SharedString,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        bridge: SelectionBridge,
        selection_color: Hsla,
    ) -> Self {
        let styled = StyledText::new(text.clone()).with_highlights(highlights);
        Self {
            line: line as u64,
            text,
            styled,
            bridge,
            selection_color,
        }
    }

    fn paint_selection(&self, range: Range<usize>, window: &mut Window) {
        let layout = self.styled.layout();
        let (Some(start), Some(end)) = (
            layout.position_for_index(range.start),
            layout.position_for_index(range.end),
        ) else {
            return;
        };
        let bounds = Bounds::from_corners(start, point(end.x, end.y + layout.line_height()));
        window.paint_quad(PaintQuad {
            bounds,
            background: self.selection_color.into(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            border_color: transparent_black(),
            border_style: BorderStyle::default(),
        });
    }
}

impl IntoElement for CodeLine {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for CodeLine {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.styled
            .request_layout(global_id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let inner = &self.bridge.0;
        let run = TextSelectionRun::new(self.text.clone(), self.styled.layout().clone(), bounds)
            .with_document_order(self.line);
        let range = if inner.all.get() {
            Some(0..self.text.len())
        } else {
            inner
                .handle
                .update_runs(std::slice::from_ref(&run), cx)
                .ranges()
                .first()
                .cloned()
                .flatten()
        };
        if let Some(range) = range.filter(|r| !r.is_empty()) {
            self.paint_selection(range.clone(), window);
            if !inner.all.get() {
                remember_endpoint(inner, self.line, &range, cx);
            }
            inner.selected.borrow_mut().insert(self.line, range);
        }
        inner.runs.borrow_mut().push(run);
        self.styled.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
    }
}

/// Wraps the list and registers the code area (right of `code_left`) as the selection participant.
pub(crate) struct CodeColumn {
    child: AnyElement,
    bridge: SelectionBridge,
    code_left: Pixels,
    row_h: Pixels,
    scroll: UniformListScrollHandle,
}

impl CodeColumn {
    /// `child` is the list; the code starts `code_left` right of its left edge; rows are `row_h` high.
    pub(crate) fn new(
        child: impl IntoElement,
        bridge: SelectionBridge,
        code_left: Pixels,
        row_h: Pixels,
        scroll: UniformListScrollHandle,
    ) -> Self {
        Self {
            child: child.into_any_element(),
            bridge,
            code_left,
            row_h,
            scroll,
        }
    }
}

impl IntoElement for CodeColumn {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for CodeColumn {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.child.prepaint(window, cx);
        let left = self.code_left.min(bounds.size.width);
        let code = Bounds::new(
            point(bounds.origin.x + left, bounds.origin.y),
            size(bounds.size.width - left, bounds.size.height),
        );
        let hitbox = window.insert_hitbox(code, HitboxBehavior::Normal);
        let scroll_offset = self.scroll.0.borrow().base_handle.offset();
        self.bridge.0.row_h.set(self.row_h);
        let registration = TextSelectionRegistration::new(hitbox.clone(), code)
            .with_scroll_offset(point(px(0.), scroll_offset.y))
            .with_text_bounds(vec![code])
            .with_rendered_element(&self.bridge.0.handle, window, cx);
        self.bridge.0.handle.register(registration, window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let inner = &self.bridge.0;
        inner.runs.borrow_mut().clear();
        inner.selected.borrow_mut().clear();
        inner.all.set(inner.handle.has_local_selection(cx));
        if inner.handle.snapshot(cx).is_none() {
            inner.endpoints.borrow_mut().clear();
        }
        self.child.paint(window, cx);
        let runs = std::mem::take(&mut *inner.runs.borrow_mut());
        inner.handle.update_runs(&runs, cx);
    }
}
