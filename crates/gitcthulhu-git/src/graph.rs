//! Commit-graph lane layout (WP-B4, spec §3).
//!
//! [`GraphBuilder`] assigns lanes to commits that arrive in walk order (every child before its parents, as the
//! [`LogWalker`] guarantees). Each pushed commit becomes one immutable row of the [`GraphLayout`]; pagination only
//! appends rows, because the lane state (`lanes`, `waiting`, `free`) persists in the builder between pages.
//!
//! The layout is compact (24 bytes per row plus small shared pools), so a million rows cost about 26 MB, and the
//! per-row cost is O(log W) for the free-lane set plus O(W/64) for the passing-lane snapshot, W being the number of
//! lanes in use.

use std::{collections::BTreeSet, ops::Range};

use gix::ObjectId;
use smallvec::SmallVec;

use crate::{CommitInfo, LogPage, LogWalker, MergeOut, NodeKind};

/// Lanes are numbered `0..MAX_LANES`, so every lane fits the 15 bits a merge-out entry has next to its
/// `joins_existing` flag. A graph that would need more concurrent lanes degrades: the overflowing lines are not laid
/// out (the rows stay consistent, only those lines are missing).
const MAX_LANES: usize = 0x8000;
/// The `joins_existing` flag of a merge-out entry in [`GraphLayout::extra`].
const JOINS_EXISTING: u16 = 0x8000;
/// `RowData::spill_start` of a row that needs no spill words.
const NO_SPILL: u32 = u32::MAX;

/// `RowData::flags` bits.
const LINE_UP: u8 = 1 << 0;
const LINE_DOWN: u8 = 1 << 1;
const LINE_UP_FROM_UNCOMMITTED_ONLY: u8 = 1 << 2;
const UNCOMMITTED_LINK: u8 = 1 << 3;

/// A bitset of lanes (`u64` words), mirroring which lanes wait for a commit.
#[derive(Debug, Default, Clone)]
struct LaneBits(Vec<u64>);

impl LaneBits {
    fn set(&mut self, lane: u16) {
        let word = usize::from(lane) / 64;
        if word >= self.0.len() {
            self.0.resize(word + 1, 0);
        }
        self.0[word] |= 1 << (lane % 64);
    }

    fn clear(&mut self, lane: u16) {
        if let Some(word) = self.0.get_mut(usize::from(lane) / 64) {
            *word &= !(1 << (lane % 64));
        }
    }

    #[cfg(any(test, debug_assertions))]
    fn contains(&self, lane: u16) -> bool {
        self.0
            .get(usize::from(lane) / 64)
            .is_some_and(|word| word & (1 << (lane % 64)) != 0)
    }
}

/// Incremental lane assignment over a walk in date (or topo) order.
///
/// Feed it commits in walk order with [`push()`](Self::push) or [`push_page()`](Self::push_page); every call appends
/// exactly one row per commit to [`layout()`](Self::layout) and never changes earlier rows.
#[derive(Debug, Default)]
pub struct GraphBuilder {
    /// Lane -> the commit it waits for; `None` = free.
    lanes: Vec<Option<ObjectId>>,
    /// Commit -> the lanes waiting for it, ascending. The exact inverse of `lanes`.
    waiting: gix::hashtable::HashMap<ObjectId, SmallVec<[u16; 2]>>,
    /// Free lane indices below `lanes.len()`.
    free: BTreeSet<u16>,
    /// Bitset mirror of `lanes`, for O(W/64) passing-lane snapshots.
    occupied: LaneBits,
    /// Number of occupied lanes.
    open: usize,
    layout: GraphLayout,
    /// Lay out `parents[..1]` only.
    first_parent: bool,
    /// (HEAD, lane) reserved by the uncommitted row until HEAD's row is pushed.
    uncommitted_link: Option<(ObjectId, u16)>,
    /// A merge above HEAD joined the link lane (HEAD is one of its later parents): from the row below that merge on,
    /// the lane also carries the merge's line, so it is no longer reported as the uncommitted link and HEAD's
    /// `line_up` no longer exists only for the uncommitted row.
    uncommitted_link_shared: bool,
    /// Scratch buffers reused for every row, so pushing does not allocate in the steady state.
    parents: SmallVec<[ObjectId; 4]>,
    fork_ins: Vec<u16>,
    merge_outs: Vec<u16>,
}

impl GraphBuilder {
    /// `first_parent` must equal the walker's `LogOptions::first_parent`: only the first parent of each commit is laid
    /// out (the walk never emits the others, so reserving lanes for them would leak one lane per merge).
    pub fn new(first_parent: bool) -> Self {
        GraphBuilder {
            first_parent,
            ..GraphBuilder::default()
        }
    }

    /// Synthetic row 0 for uncommitted changes. The GUI calls it **always**, before the first commit, exactly once,
    /// whenever the walk's tips include HEAD; whether the row is *visible* is GUI state (§3.5). `head = None` when
    /// HEAD is unborn (row without a line down).
    ///
    /// HEAD's lane (lane 0 on a fresh builder) stays reserved until HEAD's commit is pushed. The rows in between
    /// report it as [`RowView::uncommitted_link_lane`], and HEAD's row reports
    /// [`RowView::line_up_from_uncommitted_only`], so hiding the row needs no relayout (unless a merge joins the
    /// link lane on the way, see there).
    pub fn push_uncommitted(&mut self, head: Option<ObjectId>) {
        debug_assert!(
            self.layout.rows.is_empty() && self.uncommitted_link.is_none(),
            "the uncommitted row is pushed once, before the first commit"
        );
        let lane = self.alloc(0);
        let node_lane = lane.unwrap_or(last_lane());
        let (passing, spill_start, spill_len) = self.snapshot_passing();
        let mut flags = 0;
        match (head, lane) {
            (Some(head), Some(lane)) => {
                self.reserve(head, lane);
                self.uncommitted_link = Some((head, lane));
                self.layout.uncommitted_lane = lane;
                flags |= LINE_DOWN;
            }
            (None, Some(lane)) => {
                self.free.insert(lane);
            }
            (_, None) => {}
        }
        self.emit(
            RowData {
                passing,
                spill_start,
                extra_start: 0,
                node_lane,
                n_fork: 0,
                n_merge: 0,
                kind: NodeKind::Uncommitted,
                flags,
                spill_len,
            },
            0,
        );
    }

    /// Append one row per commit, in walk order. `is_stash` and `shallow_boundary` come from the LogWalker.
    ///
    /// Stashes lay out their first parent only (their index and untracked helper commits are never walked), and a
    /// shallow boundary lays out no parent at all (they are not in the repository).
    pub fn push(&mut self, commit: &CommitInfo, is_stash: bool, shallow_boundary: bool) {
        // Corrupt commits may list a parent twice; a duplicate would join its own freshly reserved lane.
        self.parents.clear();
        for parent in &commit.parents {
            if !self.parents.contains(parent) {
                self.parents.push(*parent);
            }
        }
        let all_parents = self.parents.len();

        // Every line waiting for this commit ends at its node.
        let reserved = self.waiting.remove(&commit.id).unwrap_or_default();
        for &lane in &reserved {
            self.lanes[usize::from(lane)] = None;
            self.occupied.clear(lane);
            self.open -= 1;
        }
        let node_lane = match reserved.first() {
            Some(&lane) => Some(lane),
            None => self.alloc(0),
        };
        // The other lines curve into the node from the top of their lane; their lanes are free again for this row's
        // merge-outs (Gitnuro's order).
        self.fork_ins.clear();
        self.fork_ins.extend(reserved.iter().skip(1).copied());
        self.free.extend(self.fork_ins.iter().copied());

        let mut flags = 0;
        if !reserved.is_empty() {
            flags |= LINE_UP;
        }
        match self.uncommitted_link {
            Some((head, lane)) if head == commit.id => {
                if node_lane == Some(lane) && !self.uncommitted_link_shared {
                    flags |= LINE_UP_FROM_UNCOMMITTED_ONLY;
                }
                self.uncommitted_link = None;
                self.uncommitted_link_shared = false;
            }
            Some(_) if !self.uncommitted_link_shared => flags |= UNCOMMITTED_LINK,
            Some(_) | None => {}
        }

        // Taken before the parents are reserved: the node lane is not occupied at this point.
        let (passing, spill_start, spill_len) = self.snapshot_passing();

        let laid_out = if shallow_boundary {
            0
        } else if is_stash || self.first_parent {
            all_parents.min(1)
        } else {
            all_parents
        };
        let node = node_lane.unwrap_or(last_lane());
        self.merge_outs.clear();
        match node_lane {
            Some(lane) if laid_out > 0 => {
                // The first parent continues straight down.
                self.reserve(self.parents[0], lane);
                flags |= LINE_DOWN;
                for idx in 1..laid_out {
                    let parent = self.parents[idx];
                    if let Some(&existing) =
                        self.waiting.get(&parent).and_then(|lanes| lanes.first())
                    {
                        // Joining the uncommitted→HEAD link: this row still reports the link (its passing segment
                        // belongs to the link alone, the merge-out curve reaches the bottom of the lane), the rows
                        // below do not, because a hidden uncommitted row must not hide the merge's line.
                        if self
                            .uncommitted_link
                            .is_some_and(|(_, link)| link == existing)
                        {
                            self.uncommitted_link_shared = true;
                        }
                        self.merge_outs.push(existing | JOINS_EXISTING);
                    } else if let Some(new_lane) = self.alloc(lane.saturating_add(1)) {
                        // Merges branch out to the right.
                        self.reserve(parent, new_lane);
                        self.merge_outs.push(new_lane);
                    }
                }
            }
            // Root or boundary: the lane ends here.
            Some(lane) => {
                self.free.insert(lane);
            }
            // No lane left at all (more than MAX_LANES concurrent lines): nothing is reserved for the parents.
            None => {}
        }

        let kind = if is_stash {
            NodeKind::Stash
        } else if shallow_boundary {
            NodeKind::ShallowBoundary
        } else if all_parents > 1 {
            NodeKind::Merge
        } else if all_parents == 0 {
            NodeKind::Root
        } else {
            NodeKind::Commit
        };

        // Rows record at most 255 fork-ins and merge-outs each (octopus merges beyond that are drawn partially), but
        // the width accounts for every lane in use.
        let extra_max = self
            .fork_ins
            .iter()
            .chain(&self.merge_outs)
            .map(|lane| lane & !JOINS_EXISTING)
            .max()
            .unwrap_or(0);
        let n_fork = self.fork_ins.len().min(usize::from(u8::MAX));
        let n_merge = self.merge_outs.len().min(usize::from(u8::MAX));
        let extra_start = pool_index(self.layout.extra.len());
        self.layout
            .extra
            .extend_from_slice(&self.fork_ins[..n_fork]);
        self.layout
            .extra
            .extend_from_slice(&self.merge_outs[..n_merge]);
        self.emit(
            RowData {
                passing,
                spill_start,
                extra_start,
                node_lane: node,
                n_fork: n_fork as u8,
                n_merge: n_merge as u8,
                kind,
                flags,
                spill_len,
            },
            extra_max,
        );
    }

    /// Append the rows of a page (see [`push()`](Self::push)), taking the stash and shallow-boundary flags from
    /// `walker`.
    pub fn push_page(&mut self, page: &LogPage, walker: &LogWalker) {
        let stashes = walker.stash_ids();
        for commit in &page.commits {
            self.push(
                commit,
                stashes.contains(&commit.id),
                walker.is_shallow_boundary(&commit.id),
            );
        }
    }

    /// The rows laid out so far.
    pub fn layout(&self) -> &GraphLayout {
        &self.layout
    }

    /// Lanes still waiting for commits that never came (walk exhausted): drawn to the bottom, then end.
    pub fn open_lanes(&self) -> usize {
        self.open
    }

    /// The smallest free lane `>= min`, else a new lane at the end; `None` if all `MAX_LANES` lanes are in use.
    fn alloc(&mut self, min: u16) -> Option<u16> {
        if let Some(&lane) = self.free.range(min..).next() {
            self.free.remove(&lane);
            return Some(lane);
        }
        if self.lanes.len() >= MAX_LANES {
            return None;
        }
        // `min` is 0 or one above an allocated lane, so the new lane at the end satisfies it.
        let lane = self.lanes.len() as u16;
        debug_assert!(lane >= min, "a new lane at the end is at least {min}");
        self.lanes.push(None);
        Some(lane)
    }

    /// `lane` (allocated, not free) now waits for `commit`.
    fn reserve(&mut self, commit: ObjectId, lane: u16) {
        self.lanes[usize::from(lane)] = Some(commit);
        self.occupied.set(lane);
        self.open += 1;
        let lanes = self.waiting.entry(commit).or_default();
        let pos = lanes.partition_point(|&l| l < lane);
        lanes.insert(pos, lane);
    }

    /// The occupied lanes as (lanes 0..63, spill start, spill length), appending lanes above 63 to the spill pool.
    fn snapshot_passing(&mut self) -> (u64, u32, u16) {
        let words = &self.occupied.0;
        let low = words.first().copied().unwrap_or(0);
        let high = words.get(1..).unwrap_or(&[]);
        let used = high
            .iter()
            .rposition(|&w| w != 0)
            .map_or(0, |last| last + 1);
        if used == 0 {
            return (low, NO_SPILL, 0);
        }
        let start = pool_index(self.layout.spill.len());
        self.layout.spill.extend_from_slice(&high[..used]);
        // `used` <= MAX_LANES / 64, which fits a u16.
        (low, start, used as u16)
    }

    /// Append `row`, growing the monotonic `max_lane` by the row's lanes.
    fn emit(&mut self, row: RowData, extra_max: u16) {
        let row_max = row
            .node_lane
            .max(highest_lane(row.passing, self.layout.spill_of(&row)))
            .max(extra_max);
        self.layout.max_lane = self.layout.max_lane.max(row_max);
        self.layout.rows.push(row);
        #[cfg(debug_assertions)]
        if let Err(violation) = self.check_invariants() {
            panic!("graph layout invariant violated: {violation}");
        }
    }

    /// Invariant 1 of §3.3: `occupied` mirrors `lanes`, `waiting` is the exact inverse of `lanes`, `free` and the
    /// occupied lanes are disjoint and together cover every lane, and `open` counts the occupied lanes.
    #[cfg(any(test, debug_assertions))]
    fn check_invariants(&self) -> Result<(), String> {
        let mut open = 0;
        for (idx, waits_for) in self.lanes.iter().enumerate() {
            let lane = idx as u16;
            match waits_for {
                Some(commit) => {
                    open += 1;
                    if !self.occupied.contains(lane) {
                        return Err(format!(
                            "lane {lane} waits for {commit} but is not marked occupied"
                        ));
                    }
                    if self.free.contains(&lane) {
                        return Err(format!(
                            "lane {lane} waits for {commit} but is listed as free"
                        ));
                    }
                    if !self
                        .waiting
                        .get(commit)
                        .is_some_and(|lanes| lanes.contains(&lane))
                    {
                        return Err(format!(
                            "lane {lane} waits for {commit} but `waiting` does not list it"
                        ));
                    }
                }
                None => {
                    if self.occupied.contains(lane) {
                        return Err(format!("free lane {lane} is marked occupied"));
                    }
                    if !self.free.contains(&lane) {
                        return Err(format!("unused lane {lane} is missing from the free set"));
                    }
                }
            }
        }
        if let Some(&beyond) = self.free.range(self.lanes.len() as u16..).next() {
            return Err(format!(
                "free lane {beyond} is beyond the {} allocated lanes",
                self.lanes.len()
            ));
        }
        let in_waiting: usize = self.waiting.values().map(|lanes| lanes.len()).sum();
        if in_waiting != open || self.open != open {
            return Err(format!(
                "{open} lanes are occupied, but `waiting` lists {in_waiting} and the counter says {}",
                self.open
            ));
        }
        for (commit, lanes) in &self.waiting {
            if lanes.is_empty() || !lanes.windows(2).all(|pair| pair[0] < pair[1]) {
                return Err(format!(
                    "the lanes waiting for {commit} are not strictly ascending: {lanes:?}"
                ));
            }
        }
        Ok(())
    }
}

/// The last lane, used as the node lane of a row for which no lane could be allocated.
fn last_lane() -> u16 {
    (MAX_LANES - 1) as u16
}

/// An index into a layout pool. The pools hold a few entries per row, far below `u32::MAX` for any history Git can
/// store in practice.
fn pool_index(len: usize) -> u32 {
    debug_assert!(
        len < NO_SPILL as usize,
        "layout pools stay below u32::MAX entries"
    );
    len as u32
}

/// The highest lane set in a passing snapshot, or 0.
fn highest_lane(low: u64, spill: &[u64]) -> u16 {
    if let Some((idx, word)) = spill.iter().enumerate().rev().find(|(_, w)| **w != 0) {
        return ((idx + 1) * 64 + 63 - word.leading_zeros() as usize) as u16;
    }
    if low == 0 {
        0
    } else {
        (63 - low.leading_zeros()) as u16
    }
}

/// One row, 24 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowData {
    /// Lanes 0..63 with a full-height vertical line (node lane excluded).
    passing: u64,
    /// `NO_SPILL`, else an index into `GraphLayout::spill` (`spill_len` words, lanes from 64 up).
    spill_start: u32,
    /// Index into `GraphLayout::extra`: `n_fork` fork-in lanes, then `n_merge` merge-out entries.
    extra_start: u32,
    node_lane: u16,
    n_fork: u8,
    n_merge: u8,
    kind: NodeKind,
    /// `LINE_UP`, `LINE_DOWN`, `LINE_UP_FROM_UNCOMMITTED_ONLY`, `UNCOMMITTED_LINK`.
    flags: u8,
    /// Spill words of this row. A `u16` (instead of the spec's `u8` + padding byte) so rows with lanes above 16383
    /// keep their passing lines; the row stays 24 bytes.
    spill_len: u16,
}

const _: () = assert!(std::mem::size_of::<RowData>() == 24);

/// The rows laid out so far; rows are immutable once emitted.
///
/// Row `i` corresponds to commit `i - 1` of the walk when the uncommitted row was pushed, else to commit `i`.
/// Two layouts compare equal when they are byte-for-byte the same.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GraphLayout {
    rows: Vec<RowData>,
    /// Fork-in lanes, then merge-out lanes per row; merge-out high bit (`0x8000`) = joins an existing line.
    extra: Vec<u16>,
    /// Passing-lane words beyond lane 63, for rows that need them.
    spill: Vec<u64>,
    /// Monotonic over all rows.
    max_lane: u16,
    /// The lane the uncommitted row reserved for HEAD (lane 0 when it was pushed first, as specified).
    uncommitted_lane: u16,
}

impl GraphLayout {
    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// No rows yet.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The row at `index`. Panics if `index >= len()`.
    pub fn row(&self, index: usize) -> RowView<'_> {
        assert!(
            index < self.rows.len(),
            "row index {index} is out of range: the layout has {} rows",
            self.rows.len()
        );
        let row = &self.rows[index];
        let start = row.extra_start as usize;
        let n_fork = usize::from(row.n_fork);
        let extra = &self.extra[start..start + n_fork + usize::from(row.n_merge)];
        let (fork_ins, merge_outs) = extra.split_at(n_fork);
        RowView {
            node_lane: row.node_lane,
            kind: row.kind,
            line_up: row.flags & LINE_UP != 0,
            line_down: row.flags & LINE_DOWN != 0,
            line_up_from_uncommitted_only: row.flags & LINE_UP_FROM_UNCOMMITTED_ONLY != 0,
            uncommitted_link_lane: (row.flags & UNCOMMITTED_LINK != 0)
                .then_some(self.uncommitted_lane),
            passing_low: row.passing,
            spill: self.spill_of(row),
            fork_ins,
            merge_outs,
        }
    }

    /// The highest lane used by any row so far; never shrinks.
    pub fn max_lane(&self) -> u16 {
        self.max_lane
    }

    /// The highest lane used by the rows in `rows` (clamped to the layout), for auto-width of the visible window.
    pub fn max_lane_in(&self, rows: Range<usize>) -> u16 {
        let end = rows.end.min(self.rows.len());
        let start = rows.start.min(end);
        self.rows[start..end]
            .iter()
            .map(|row| {
                let start = row.extra_start as usize;
                let extra =
                    &self.extra[start..start + usize::from(row.n_fork) + usize::from(row.n_merge)];
                let extra_max = extra
                    .iter()
                    .map(|lane| lane & !JOINS_EXISTING)
                    .max()
                    .unwrap_or(0);
                row.node_lane
                    .max(highest_lane(row.passing, self.spill_of(row)))
                    .max(extra_max)
            })
            .max()
            .unwrap_or(0)
    }

    fn spill_of(&self, row: &RowData) -> &[u64] {
        if row.spill_start == NO_SPILL {
            return &[];
        }
        let start = row.spill_start as usize;
        &self.spill[start..start + usize::from(row.spill_len)]
    }
}

/// What the GUI paints for one row (drawing contract: §3.4).
#[derive(Debug, Clone, Copy)]
pub struct RowView<'a> {
    /// The lane of the node.
    pub node_lane: u16,
    /// The node's kind.
    pub kind: NodeKind,
    /// A line enters the node from above, on the node lane.
    pub line_up: bool,
    /// A line leaves the node downwards, on the node lane.
    pub line_down: bool,
    /// Only on HEAD's row: the `line_up` segment exists solely because the uncommitted row reserved HEAD's lane
    /// (no other child of HEAD enters on the node lane). Hidden uncommitted row => draw `line_up = false`.
    ///
    /// False when a merge above HEAD joined the link lane (HEAD is a later parent of that merge): the lane then
    /// carries the merge's line into HEAD.
    pub line_up_from_uncommitted_only: bool,
    /// The uncommitted-to-HEAD link lane passes through this row (rows between row 0 and HEAD's row), and it is this
    /// lane. Hidden uncommitted row => do not draw this passing lane.
    ///
    /// When a merge above HEAD joins the link lane, the merge's row is the last one that reports it (its merge-out
    /// curve still reaches the bottom of the lane); the rows below draw the lane as an ordinary passing line.
    pub uncommitted_link_lane: Option<u16>,
    passing_low: u64,
    spill: &'a [u64],
    fork_ins: &'a [u16],
    merge_outs: &'a [u16],
}

impl<'a> RowView<'a> {
    /// Lanes with a full-height vertical line through this row, ascending. Never contains the node lane.
    pub fn passing(&self) -> impl Iterator<Item = u16> + 'a {
        LaneIter {
            word: self.passing_low,
            base: 0,
            rest: self.spill.iter(),
        }
    }

    /// Lanes whose line ends in this node with a curve from the top of the lane, ascending.
    pub fn fork_ins(&self) -> impl Iterator<Item = u16> + 'a {
        self.fork_ins.iter().copied()
    }

    /// Lanes a merge node draws lines to: a curve from the node to the bottom of the lane. `joins_existing` lanes
    /// already pass through this row.
    pub fn merge_outs(&self) -> impl Iterator<Item = MergeOut> + 'a {
        self.merge_outs.iter().map(|&entry| MergeOut {
            lane: entry & !JOINS_EXISTING,
            joins_existing: entry & JOINS_EXISTING != 0,
        })
    }

    /// The colour index of `lane`. GUI: `palette[color % palette.len()]`.
    pub fn color_of(lane: u16) -> u16 {
        lane
    }
}

/// Iterates the set bits of a passing snapshot in ascending order.
struct LaneIter<'a> {
    word: u64,
    base: usize,
    rest: std::slice::Iter<'a, u64>,
}

impl Iterator for LaneIter<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        loop {
            if self.word != 0 {
                let bit = self.word.trailing_zeros() as usize;
                self.word &= self.word - 1;
                return Some((self.base + bit) as u16);
            }
            self.word = *self.rest.next()?;
            self.base += 64;
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use smallvec::SmallVec;

    use super::*;
    use crate::{GitTime, Person, Signature};

    fn id(n: usize) -> ObjectId {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(&(n as u64 + 1).to_be_bytes());
        ObjectId::from_bytes_or_panic(&bytes)
    }

    fn commit(n: usize, parents: &[usize]) -> CommitInfo {
        let signature = Signature {
            person: std::sync::Arc::new(Person {
                name: "a".into(),
                email: "a@example.com".into(),
            }),
            time: GitTime {
                seconds: 0,
                offset_minutes: 0,
            },
        };
        CommitInfo {
            id: id(n),
            parents: parents.iter().map(|&p| id(p)).collect::<SmallVec<_>>(),
            author: signature.clone(),
            committer: signature,
            summary: "c".into(),
            has_body: false,
        }
    }

    /// Commits `0..n` in walk order: commit `i` picks its parents among `i+1..n` (so children come first), with
    /// occasional duplicates and parents outside the walk.
    fn random_dag() -> impl Strategy<Value = Vec<Vec<usize>>> {
        (1usize..120).prop_flat_map(|n| {
            proptest::collection::vec(proptest::collection::vec(0usize..1000, 0..5), n).prop_map(
                move |raw| {
                    raw.into_iter()
                        .enumerate()
                        .map(|(i, picks)| {
                            picks
                                .into_iter()
                                .map(|p| {
                                    if p % 17 == 0 {
                                        n + p
                                    } else {
                                        i + 1 + p % (n - i).max(1)
                                    }
                                })
                                .collect()
                        })
                        .collect()
                },
            )
        })
    }

    proptest! {
        /// Invariant 1 holds after every push, also with the uncommitted row and in first-parent mode.
        #[test]
        fn internal_state_stays_consistent(dag in random_dag(), first_parent in any::<bool>(), with_head in any::<bool>()) {
            let mut builder = GraphBuilder::new(first_parent);
            if with_head {
                builder.push_uncommitted(Some(id(0)));
            }
            for (n, parents) in dag.iter().enumerate() {
                builder.push(&commit(n, parents), n % 11 == 3, n % 13 == 5);
                prop_assert_eq!(builder.check_invariants(), Ok(()));
            }
        }
    }

    #[test]
    fn rows_are_24_bytes() {
        assert_eq!(
            std::mem::size_of::<RowData>(),
            24,
            "the 1M-row memory budget assumes 24-byte rows"
        );
    }
}
