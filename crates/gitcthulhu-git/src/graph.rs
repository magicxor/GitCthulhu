//! Commit-graph lane layout (WP-B4). Stub: the builder lays out nothing and the layout stays empty.
#![allow(unused_variables)]

use std::{marker::PhantomData, ops::Range};

use gix::ObjectId;

use crate::{CommitInfo, LogPage, LogWalker, MergeOut, NodeKind};

/// Incremental lane assignment over a walk in date order.
#[derive(Debug, Default)]
pub struct GraphBuilder {
    layout: GraphLayout,
}

impl GraphBuilder {
    /// `first_parent` must equal the walker's `LogOptions::first_parent`: only the first parent of each commit is laid
    /// out (the walk never emits the others, so reserving lanes for them would leak one lane per merge).
    pub fn new(first_parent: bool) -> Self {
        GraphBuilder::default()
    }

    /// Synthetic row 0 for uncommitted changes. The GUI calls it **always**, before the first commit, exactly once,
    /// whenever the walk's tips include HEAD; whether the row is *visible* is GUI state (§3.5). `head = None` when
    /// HEAD is unborn (row without a line down).
    pub fn push_uncommitted(&mut self, head: Option<ObjectId>) {}

    /// Append one row per commit, in walk order. `is_stash` and `shallow_boundary` come from the LogWalker.
    pub fn push(&mut self, commit: &CommitInfo, is_stash: bool, shallow_boundary: bool) {}

    /// Append the rows of a page.
    pub fn push_page(&mut self, page: &LogPage, walker: &LogWalker) {}

    /// The rows laid out so far.
    pub fn layout(&self) -> &GraphLayout {
        &self.layout
    }

    /// Lanes still waiting for commits that never came (walk exhausted): drawn to the bottom, then end.
    pub fn open_lanes(&self) -> usize {
        0
    }
}

/// The rows laid out so far; rows are immutable once emitted.
#[derive(Debug, Default)]
pub struct GraphLayout {
    max_lane: u16,
}

impl GraphLayout {
    /// Number of rows.
    pub fn len(&self) -> usize {
        0
    }

    /// No rows yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The row at `index`. Panics if `index >= len()`.
    pub fn row(&self, index: usize) -> RowView<'_> {
        panic!(
            "row index {index} is out of range: the layout has {} rows",
            self.len()
        )
    }

    /// Over ALL rows so far; never shrinks.
    pub fn max_lane(&self) -> u16 {
        self.max_lane
    }

    /// For auto-width of the visible window.
    pub fn max_lane_in(&self, rows: Range<usize>) -> u16 {
        0
    }
}

/// What the GUI paints for one row.
#[derive(Debug, Clone, Copy)]
pub struct RowView<'a> {
    /// The lane of the node.
    pub node_lane: u16,
    /// The node's kind.
    pub kind: NodeKind,
    /// A line enters the node from above.
    pub line_up: bool,
    /// A line leaves the node downwards.
    pub line_down: bool,
    /// Only on HEAD's row: the `line_up` segment exists solely because the uncommitted row reserved HEAD's lane
    /// (no other child of HEAD enters on the node lane). Hidden uncommitted row => draw `line_up = false`.
    pub line_up_from_uncommitted_only: bool,
    /// The uncommitted-to-HEAD link lane passes through this row (rows between row 0 and HEAD's row), and it is this
    /// lane. Hidden uncommitted row => do not draw this passing lane.
    pub uncommitted_link_lane: Option<u16>,
    pools: PhantomData<&'a ()>,
}

impl<'a> RowView<'a> {
    /// Lanes with a full-height vertical line through this row, ascending.
    pub fn passing(&self) -> impl Iterator<Item = u16> + 'a {
        std::iter::empty()
    }

    /// Lanes that fork into the node from above, ascending.
    pub fn fork_ins(&self) -> impl Iterator<Item = u16> + 'a {
        std::iter::empty()
    }

    /// Lanes a merge node draws lines to.
    pub fn merge_outs(&self) -> impl Iterator<Item = MergeOut> + 'a {
        std::iter::empty()
    }

    /// GUI: `palette[color % palette.len()]`.
    pub fn color_of(lane: u16) -> u16 {
        lane
    }
}
