//! The graph decoration of the log list (owned by G4 after G0b; spec 21 §4.2, 08 §2.9).
//!
//! G0b ships the reference structure every frame must keep: ONE `UniformListDecoration` pass per frame, the clip
//! mask taken from the list's viewport (`bounds.origin - scroll_offset`, never the canvas bounds, which move with the
//! scroll), row tops computed in f64 (an f32 formula drifts ~2 px near row 1,000,000), and six batched paths (one per
//! palette colour). It paints vertical node-lane segments (stopping 15 dp short of the node centre), passing lanes
//! and straight fork/merge connectors; G4 replaces the connectors with the 08 §2.9 arcs and adds the node elements.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, Bounds, ContentMask, Hsla, IntoElement, PathBuilder, Pixels, Point,
    Styled as _, UniformListDecoration, Window, canvas, point, px, size,
};

use crate::metrics::dpx;
use crate::tab::model::LogSnapshot;

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
        let viewport = Bounds::new(bounds.origin - scroll, bounds.size);
        let snapshot = self.snapshot.clone();
        let (offset, show, graph_w, hscroll, lanes) = (
            self.offset,
            self.show_uncommitted,
            self.graph_w,
            self.hscroll,
            self.lanes,
        );
        let recorder = self.recorder.clone();
        let (pitch, half, stroke) = (dpx(LANE_W, window), dpx(NODE_HALF, window), dpx(2., window));
        canvas(
            |_, _, _| (),
            move |_canvas_bounds, _, window, _cx| {
                let mask = Bounds::new(viewport.origin, size(graph_w, viewport.size.height));
                window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                    let mut paths: [PathBuilder; 6] =
                        std::array::from_fn(|_| PathBuilder::stroke(stroke));
                    let lane_x =
                        |lane: u16| viewport.origin.x + pitch * (f32::from(lane) + 1.) - hscroll;
                    for (k, v) in visible.clone().enumerate() {
                        let top = row_top(viewport.origin.y, h, visible.start, scroll.y, k);
                        let ix = v + offset;
                        let Some(row) = snapshot.rows.get(ix) else {
                            break;
                        };
                        if let Some(recorder) = &recorder {
                            recorder.borrow_mut().push((ix, top));
                        }
                        let g = &row.graph;
                        let cy = top + h / 2.;
                        let bottom = top + h;
                        let x = lane_x(g.node_lane);
                        let path = &mut paths[usize::from(g.node_lane) % 6];
                        let line_up = g.line_up && (show || !g.line_up_from_uncommitted_only);
                        if line_up {
                            path.move_to(point(x, top));
                            path.line_to(point(x, cy - half));
                        }
                        if g.line_down {
                            path.move_to(point(x, cy + half));
                            path.line_to(point(x, bottom));
                        }
                        for &lane in &g.passing {
                            if !show && g.uncommitted_link_lane == Some(lane) {
                                continue;
                            }
                            let lx = lane_x(lane);
                            let p = &mut paths[usize::from(lane) % 6];
                            p.move_to(point(lx, top));
                            p.line_to(point(lx, bottom));
                        }
                        for &lane in &g.fork_ins {
                            let lx = lane_x(lane);
                            let p = &mut paths[usize::from(lane) % 6];
                            p.move_to(point(lx, top));
                            p.line_to(point(if lx > x { x + half } else { x - half }, cy));
                        }
                        for out in &g.merge_outs {
                            let lx = lane_x(out.lane);
                            let p = &mut paths[usize::from(out.lane) % 6];
                            p.move_to(point(if lx > x { x + half } else { x - half }, cy));
                            p.line_to(point(lx, bottom));
                        }
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
