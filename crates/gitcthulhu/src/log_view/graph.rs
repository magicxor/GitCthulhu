//! The graph decoration of the log list (owned by G4 after G0b; spec 21 §4.2, 08 §2.9, 20 §3.4).
//!
//! ONE `UniformListDecoration` pass per frame paints the lines of every visible row: the clip mask comes from the
//! list's viewport (`bounds.origin - scroll_offset`, never the canvas bounds, which move with the scroll), row tops
//! are computed in f64 (an f32 formula drifts ~2 px near row 1,000,000), and the segments of all rows go into six
//! batched paths (one per palette colour), so tessellation does not grow with the number of rows.
//!
//! Nodes are row elements (`rows.rs`) painted under the decoration, so every segment stops at the node box
//! ("node clearance"): `line_up` ends at `cy - 15`, `line_down` starts at `cy + 15` and fork/merge horizontals start
//! at `x_n ± 15`.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, Bounds, ContentMask, Hsla, IntoElement, PathBuilder, Pixels, Point,
    Styled as _, UniformListDecoration, Window, canvas, point, px, size,
};

use crate::metrics::dpx;
use crate::metrics::log as m;
use crate::tab::model::{GraphCell, LogSnapshot};

/// Lane pitch in dp (08 §2.9): the node of lane `l` is centred at `30 × (l + 1)`.
pub const LANE_W: f32 = 30.;
/// Half the node box in dp: line segments stop this far from the node centre.
pub const NODE_HALF: f32 = 15.;

/// Records the row tops a decoration painted (tests).
pub type PaintRecorder = Rc<RefCell<Vec<(usize, Pixels)>>>;

/// The one-pass graph painter of a log list.
pub struct GraphDecoration {
    pub snapshot: Arc<LogSnapshot>,
    /// 1 while the uncommitted row is hidden (visible index `v` = layout row `v + offset`).
    pub offset: usize,
    pub show_uncommitted: bool,
    /// Width of the graph column.
    pub graph_w: Pixels,
    /// Horizontal graph scroll in pixels.
    pub hscroll: Pixels,
    /// The six lane colours.
    pub lanes: [Hsla; 6],
    /// When set, the painted (layout row, row top) pairs are recorded.
    pub recorder: Option<PaintRecorder>,
}

/// The top of visible row `first + k`: f64 like `uniform_list` itself.
pub fn row_top(
    viewport_top: Pixels,
    item_h: Pixels,
    first: usize,
    scroll_y: Pixels,
    k: usize,
) -> Pixels {
    let base = f64::from(item_h.as_f32()) * first as f64 + f64::from(scroll_y.as_f32());
    viewport_top + px(base as f32) + item_h * k as f32
}

/// The clip mask of the graph: the graph column of the list's viewport. `bounds` and `scroll` are what
/// `UniformListDecoration::compute` receives (the decoration's bounds move with the scroll; the viewport does not).
pub fn graph_mask(
    bounds: Bounds<Pixels>,
    scroll: Point<Pixels>,
    graph_w: Pixels,
) -> Bounds<Pixels> {
    let viewport = Bounds::new(bounds.origin - scroll, bounds.size);
    Bounds::new(viewport.origin, size(graph_w, viewport.size.height))
}

/// One path command of a graph segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    /// Starts a sub-path.
    MoveTo(Point<Pixels>),
    /// A straight line from the current point.
    LineTo(Point<Pixels>),
    /// A quarter ellipse from the current point (`sweep` = clockwise on screen).
    ArcTo {
        radii: Point<Pixels>,
        sweep: bool,
        to: Point<Pixels>,
    },
}

/// Receives the path commands of the rows, by palette slot (`lane % 6`).
pub trait PathSink {
    /// Appends `seg` to the path of palette slot `color`.
    fn push(&mut self, color: usize, seg: Seg);
}

impl PathSink for [PathBuilder; 6] {
    fn push(&mut self, color: usize, seg: Seg) {
        let path = &mut self[color % 6];
        match seg {
            Seg::MoveTo(p) => path.move_to(p),
            Seg::LineTo(p) => path.line_to(p),
            Seg::ArcTo { radii, sweep, to } => path.arc_to(radii, px(0.), false, sweep, to),
        }
    }
}

impl PathSink for Vec<(usize, Seg)> {
    fn push(&mut self, color: usize, seg: Seg) {
        Vec::push(self, (color % 6, seg));
    }
}

/// The geometry of one row, in window pixels.
#[derive(Clone, Copy, Debug)]
pub struct RowGeom {
    /// x of the graph's origin (the viewport's left edge minus the horizontal scroll).
    pub x0: Pixels,
    pub top: Pixels,
    pub h: Pixels,
    /// Lane pitch (30 dp).
    pub pitch: Pixels,
    /// Half the node box (15 dp).
    pub half: Pixels,
    /// The ±2 dp separation of fork and merge horizontals.
    pub offset: Pixels,
}

impl RowGeom {
    /// The geometry of a row at `top` for the window's scale.
    pub fn new(x0: Pixels, top: Pixels, h: Pixels, window: &Window) -> Self {
        Self {
            x0,
            top,
            h,
            pitch: dpx(LANE_W, window),
            half: dpx(NODE_HALF, window),
            offset: dpx(m::FORK_MERGE_OFFSET, window),
        }
    }

    /// The centre x of `lane`.
    pub fn lane_x(&self, lane: u16) -> Pixels {
        self.x0 + self.pitch * (f32::from(lane) + 1.)
    }
}

/// The path commands of one row (08 §2.9, 20 §3.4) with node clearance. While the uncommitted row is hidden
/// (`show_uncommitted == false`) HEAD's `line_up` is dropped when it exists only for the uncommitted row and the
/// uncommitted link lane is not drawn (20 §3.5).
pub fn emit_row(g: &GraphCell, show_uncommitted: bool, geom: &RowGeom, sink: &mut impl PathSink) {
    let (top, h, half) = (geom.top, geom.h, geom.half);
    let cy = top + h / 2.;
    let bottom = top + h;
    let xn = geom.lane_x(g.node_lane);
    let own = usize::from(g.node_lane);
    // Quarter ellipse: rx = 7.5 dp, ry = (15 dp - d) / 2 (08 §2.9).
    let d = if !g.fork_ins.is_empty() && !g.merge_outs.is_empty() {
        geom.offset
    } else {
        px(0.)
    };
    let rx = half / 2.;
    let ry = (half - d) / 2.;

    if g.line_up && (show_uncommitted || !g.line_up_from_uncommitted_only) {
        sink.push(own, Seg::MoveTo(point(xn, top)));
        sink.push(own, Seg::LineTo(point(xn, cy - half)));
    }
    for &lane in &g.fork_ins {
        let xp = geom.lane_x(lane);
        let color = usize::from(lane);
        if lane == g.node_lane {
            sink.push(color, Seg::MoveTo(point(xn, top)));
            sink.push(color, Seg::LineTo(point(xn, cy - half)));
            continue;
        }
        let dir = if xp > xn { 1. } else { -1. };
        let y = cy - d;
        sink.push(color, Seg::MoveTo(point(xn + half * dir, y)));
        sink.push(color, Seg::LineTo(point(xp - rx * dir, y)));
        sink.push(
            color,
            Seg::ArcTo {
                radii: point(rx, ry),
                // Moving right and bending up turns counter-clockwise on screen.
                sweep: dir < 0.,
                to: point(xp, y - ry),
            },
        );
        sink.push(color, Seg::LineTo(point(xp, top)));
    }
    for out in &g.merge_outs {
        let xp = geom.lane_x(out.lane);
        let color = usize::from(out.lane);
        if out.lane == g.node_lane {
            sink.push(color, Seg::MoveTo(point(xn, cy + half)));
            sink.push(color, Seg::LineTo(point(xn, bottom)));
            continue;
        }
        let dir = if xp > xn { 1. } else { -1. };
        let y = cy + d;
        sink.push(color, Seg::MoveTo(point(xn + half * dir, y)));
        sink.push(color, Seg::LineTo(point(xp - rx * dir, y)));
        sink.push(
            color,
            Seg::ArcTo {
                radii: point(rx, ry),
                // Moving right and bending down turns clockwise on screen.
                sweep: dir > 0.,
                to: point(xp, y + ry),
            },
        );
        sink.push(color, Seg::LineTo(point(xp, bottom)));
    }
    if g.line_down {
        sink.push(own, Seg::MoveTo(point(xn, cy + half)));
        sink.push(own, Seg::LineTo(point(xn, bottom)));
    }
    for &lane in &g.passing {
        if !show_uncommitted && g.uncommitted_link_lane == Some(lane) {
            continue;
        }
        let x = geom.lane_x(lane);
        let color = usize::from(lane);
        sink.push(color, Seg::MoveTo(point(x, top)));
        sink.push(color, Seg::LineTo(point(x, bottom)));
    }
}

impl UniformListDecoration for GraphDecoration {
    fn compute(
        &self,
        visible: Range<usize>,
        bounds: Bounds<Pixels>,
        scroll: Point<Pixels>,
        h: Pixels,
        _count: usize,
        window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        let mask = graph_mask(bounds, scroll, self.graph_w);
        let viewport_top = bounds.origin.y - scroll.y;
        let snapshot = self.snapshot.clone();
        let (offset, show, hscroll, lanes) =
            (self.offset, self.show_uncommitted, self.hscroll, self.lanes);
        let recorder = self.recorder.clone();
        let template = RowGeom::new(mask.origin.x - hscroll, px(0.), h, window);
        let stroke = dpx(m::STROKE, window);
        canvas(
            |_, _, _| (),
            move |_canvas_bounds, _, window, _cx| {
                window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                    let mut paths: [PathBuilder; 6] =
                        std::array::from_fn(|_| PathBuilder::stroke(stroke));
                    for (k, v) in visible.clone().enumerate() {
                        let top = row_top(viewport_top, h, visible.start, scroll.y, k);
                        let ix = v + offset;
                        let Some(row) = snapshot.rows.get(ix) else {
                            break;
                        };
                        if let Some(recorder) = &recorder {
                            recorder.borrow_mut().push((ix, top));
                        }
                        let geom = RowGeom { top, ..template };
                        emit_row(&row.graph, show, &geom, &mut paths);
                    }
                    for (c, p) in paths.into_iter().enumerate() {
                        if let Ok(p) = p.build() {
                            window.paint_path(p, lanes[c]);
                        }
                    }
                });
            },
        )
        .size_full()
        .into_any_element()
    }
}
