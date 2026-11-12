//! The text runs the diff painted in the last frame, for hit testing selection endpoints.
//!
//! Every selectable text cell reports its run during prepaint. A frame starts with the first cell prepainted after a
//! paint (the window prepaints every element before it paints any), so the runs always describe exactly the rows on
//! screen.

use gpui_kit::{Bounds, Hitbox, Pixels, Point, SharedString, TextLayout, px};

use super::{SelKey, SelSide};

/// One painted text cell.
pub(super) struct CellRun {
    pub key: SelKey,
    /// The text laid out in `layout` (the display text of the line).
    pub text: SharedString,
    pub layout: TextLayout,
    pub bounds: Bounds<Pixels>,
}

/// The selection geometry of the current (or last finished) frame.
pub(super) struct Frame {
    /// Whether a cell painted since the last prepaint: the next prepaint begins a new frame.
    pub painted: bool,
    pub runs: Vec<CellRun>,
    /// The union of the cells' clip areas (the list viewport): the participant's bounds and hitbox.
    pub area: Option<Bounds<Pixels>>,
    pub hitbox: Option<Hitbox>,
    /// The origin the participant was last registered with: content points are relative to it.
    pub origin: Point<Pixels>,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            painted: true,
            runs: Vec::new(),
            area: None,
            hitbox: None,
            origin: Point::default(),
        }
    }
}

impl Frame {
    /// Starts collecting the runs of a new frame.
    pub fn begin(&mut self) {
        self.painted = false;
        self.runs.clear();
        self.area = None;
        self.hitbox = None;
    }

    /// The run nearest to `point` (window coordinates), restricted to `side` when given and present.
    pub fn run_at(&self, point: Point<Pixels>, side: Option<SelSide>) -> Option<&CellRun> {
        let side = side.filter(|side| self.runs.iter().any(|run| run.key.side == *side));
        self.runs
            .iter()
            .filter(|run| side.is_none_or(|side| run.key.side == side))
            .min_by(|a, b| {
                line_distance(point, a.bounds)
                    .cmp(&line_distance(point, b.bounds))
                    .then_with(|| {
                        distance_to_bounds(point, a.bounds)
                            .total_cmp(&distance_to_bounds(point, b.bounds))
                    })
            })
    }

    /// The endpoint under `point` (window coordinates): the nearest run and the text offset there.
    pub fn key_at(&self, point: Point<Pixels>, side: Option<SelSide>) -> Option<(SelKey, usize)> {
        let run = self.run_at(point, side)?;
        let offset = if point.y < run.bounds.top() {
            0
        } else if point.y >= run.bounds.bottom() {
            run.text.len()
        } else {
            run.layout
                .index_for_position(point)
                .unwrap_or_else(|ix| ix)
                .min(run.text.len())
        };
        let mut offset = offset;
        while !run.text.is_char_boundary(offset) {
            offset -= 1;
        }
        Some((run.key, offset))
    }

    /// The side of the run under `point`, if any run is painted.
    pub fn side_at(&self, point: Point<Pixels>) -> Option<SelSide> {
        self.run_at(point, None).map(|run| run.key.side)
    }
}

/// Prefers the run whose half-open vertical band holds the point, then the vertically nearest.
fn line_distance(point: Point<Pixels>, bounds: Bounds<Pixels>) -> (bool, u32) {
    let outside = point.y < bounds.top() || point.y >= bounds.bottom();
    let distance: f32 = if outside {
        (point.y - bounds.top())
            .abs()
            .min((point.y - bounds.bottom()).abs())
            .into()
    } else {
        0.
    };
    (outside, distance.to_bits())
}

fn distance_to_bounds(point: Point<Pixels>, bounds: Bounds<Pixels>) -> f32 {
    let dx: f32 = (bounds.left() - point.x)
        .max(point.x - bounds.right())
        .max(px(0.))
        .into();
    let dy: f32 = (bounds.top() - point.y)
        .max(point.y - bounds.bottom())
        .max(px(0.))
        .into();
    dx * dx + dy * dy
}
