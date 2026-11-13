//! `RepoModel`: the backend data of a tab, one `Loadable` per refresh category (spec 21 §2.3, §2.6).
//!
//! Views read the model and subscribe to [`RepoModelEvent`]; only the tab's loader writes it.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{Context, EventEmitter, SharedString};
use parking_lot::Mutex;
use smallvec::SmallVec;

use crate::backend::log::LogPager;
use crate::backend::{
    AheadBehind, GitTime, IdentityInfo, InProgress, MergeOut, NodeKind, ObjectId, RefList,
    RefreshSet, RepoState, Status, SubmoduleInfo, UndoRedoState, WorktreeInfo,
};

/// A value that is loaded in the background.
#[derive(Clone, Debug)]
pub struct Loadable<T> {
    /// The last loaded value (kept while a reload runs and after a failed reload).
    pub value: Option<T>,
    /// A load is running.
    pub loading: bool,
    /// The last load failed with this text.
    pub error: Option<SharedString>,
}

impl<T> Default for Loadable<T> {
    fn default() -> Self {
        Self {
            value: None,
            loading: false,
            error: None,
        }
    }
}

impl<T> Loadable<T> {
    /// A loaded value.
    pub fn ready(value: T) -> Self {
        Self {
            value: Some(value),
            loading: false,
            error: None,
        }
    }

    /// Applies a load result: the value on success; on failure the previous value stays and `error` is set.
    pub fn apply<E: std::fmt::Display>(&mut self, result: Result<T, E>) {
        self.loading = false;
        match result {
            Ok(value) => {
                self.value = Some(value);
                self.error = None;
            }
            Err(err) => self.error = Some(SharedString::from(err.to_string())),
        }
    }
}

/// The backend data of one tab.
#[derive(Default)]
pub struct RepoModel {
    /// `RefreshSet::STATUS`.
    pub status: Loadable<Arc<Status>>,
    /// `REPO_STATE`.
    pub state: Loadable<Arc<RepoState>>,
    /// `REPO_STATE` (the persisted commit-box draft).
    pub draft_message: Option<String>,
    /// `BRANCHES | REMOTES | TAGS | STASHES` (one `refs::list` call).
    pub refs: Loadable<Arc<RefList>>,
    /// Loaded on demand, keyed by (local, tracking) commit ids (`RepoTab::request_ahead_behind`).
    pub ahead_behind: Arc<HashMap<(ObjectId, ObjectId), AheadBehind>>,
    /// `LOG`.
    pub log: Loadable<Arc<LogSnapshot>>,
    /// The pager of the current log snapshot; the log view pages with it (a LOG reload replaces it).
    pub log_pager: Option<Arc<Mutex<LogPager>>>,
    /// `SUBMODULES`.
    pub submodules: Loadable<Arc<Vec<SubmoduleInfo>>>,
    /// `WORKTREES`.
    pub worktrees: Loadable<Arc<Vec<WorktreeInfo>>>,
    /// `CONFIG`.
    pub identity: Loadable<Arc<IdentityInfo>>,
    /// `UNDO` (`None` = the oplog is unavailable for this repository).
    pub undo: Option<UndoRedoState>,
    /// Why undo is unavailable (tooltips).
    pub undo_unavailable: Option<SharedString>,
}

impl RepoModel {
    /// 20 §3.5 / 08 §2.8: the uncommitted log row is visible iff the worktree is dirty or a merge, cherry-pick,
    /// revert or rebase is in progress (reverting included: fix of 08 §9.10). Pure; toggling it never reloads the
    /// log.
    pub fn show_uncommitted_row(&self) -> bool {
        let dirty = self
            .status
            .value
            .as_ref()
            .is_some_and(|s| !s.counts.is_clean());
        let state = self.state.value.as_ref().and_then(|s| s.in_progress);
        dirty
            || matches!(
                state,
                Some(
                    InProgress::Merge
                        | InProgress::CherryPick
                        | InProgress::CherryPickSequence
                        | InProgress::Revert
                        | InProgress::RevertSequence
                        | InProgress::Rebase
                        | InProgress::RebaseInteractive
                )
            )
    }

    /// Replaces the status and notifies `STATUS` subscribers.
    pub fn set_status(&mut self, status: Arc<Status>, cx: &mut Context<Self>) {
        self.status = Loadable::ready(status);
        cx.emit(RepoModelEvent(RefreshSet::STATUS));
        cx.notify();
    }

    /// Replaces the repository state and notifies `REPO_STATE` subscribers.
    pub fn set_state(&mut self, state: Arc<RepoState>, cx: &mut Context<Self>) {
        self.state = Loadable::ready(state);
        cx.emit(RepoModelEvent(RefreshSet::REPO_STATE));
        cx.notify();
    }

    /// Replaces the refs and notifies the ref categories' subscribers.
    pub fn set_refs(&mut self, refs: Arc<RefList>, cx: &mut Context<Self>) {
        self.refs = Loadable::ready(refs);
        cx.emit(RepoModelEvent(REFS));
        cx.notify();
    }

    /// Replaces the log snapshot and notifies `LOG` subscribers. Returns the previous snapshot, so a caller can drop
    /// a large one off the UI thread.
    pub fn set_log(
        &mut self,
        log: Arc<LogSnapshot>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<LogSnapshot>> {
        let previous = std::mem::replace(&mut self.log, Loadable::ready(log)).value;
        cx.emit(RepoModelEvent(RefreshSet::LOG));
        cx.notify();
        previous
    }

    /// The local branches with a tracking ref: their (local, tracking) commit pairs.
    pub fn tracking_pairs(&self) -> Vec<(ObjectId, ObjectId)> {
        let Some(refs) = &self.refs.value else {
            return Vec::new();
        };
        refs.local
            .iter()
            .filter_map(|b| {
                let tracking = b.upstream.as_ref()?.tracking.as_ref()?;
                let target = refs
                    .remotes
                    .iter()
                    .flat_map(|r| &r.branches)
                    .find(|rb| &rb.full == tracking)?
                    .target;
                Some((b.target, target))
            })
            .collect()
    }
}

/// The refresh categories that share the one `refs::list` load.
pub const REFS: RefreshSet = RefreshSet::BRANCHES
    .union(RefreshSet::REMOTES)
    .union(RefreshSet::TAGS)
    .union(RefreshSet::STASHES);

/// The categories whose data changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepoModelEvent(pub RefreshSet);

impl EventEmitter<RepoModelEvent> for RepoModel {}

/// Rows per chunk of [`ChunkedRows`].
pub const LOG_CHUNK: usize = 500;

/// Immutable log rows shared with the UI. Layout row 0 is ALWAYS the synthetic uncommitted row (20 §3.5); whether
/// it is visible is `RepoModel::show_uncommitted_row()`.
#[derive(Clone)]
pub struct LogSnapshot {
    pub rows: ChunkedRows<LogRow>,
    /// `GraphLayout::max_lane()`; never shrinks (20 §3.4).
    pub max_lane: u16,
    pub exhausted: bool,
    /// A new pager means a new generation; pages of an older generation are dropped.
    pub generation: u64,
}

/// Fixed chunks of [`LOG_CHUNK`] rows (the last may be shorter): `get(ix) = chunks[ix / 500][ix % 500]`. Appending
/// clones only the outer `Vec` of `Arc`s plus the last partial chunk.
#[derive(Clone)]
pub struct ChunkedRows<T> {
    chunks: Arc<Vec<Arc<Vec<T>>>>,
    len: usize,
}

impl<T: Clone> Default for ChunkedRows<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> ChunkedRows<T> {
    /// No rows.
    pub fn new() -> Self {
        Self {
            chunks: Arc::new(Vec::new()),
            len: 0,
        }
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.len
    }

    /// No rows.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The row at `ix`.
    pub fn get(&self, ix: usize) -> Option<&T> {
        if ix >= self.len {
            return None;
        }
        self.chunks.get(ix / LOG_CHUNK)?.get(ix % LOG_CHUNK)
    }

    /// A copy with `rows` appended, filling the last partial chunk first (copy-on-write of that one chunk only).
    pub fn appended(&self, rows: Vec<T>) -> Self {
        let mut chunks: Vec<Arc<Vec<T>>> = (*self.chunks).clone();
        let mut rows = rows.into_iter().peekable();
        if let Some(last) = chunks.last_mut()
            && last.len() < LOG_CHUNK
            && rows.peek().is_some()
        {
            let mut filled: Vec<T> = (**last).clone();
            filled.extend(rows.by_ref().take(LOG_CHUNK - filled.len()));
            *last = Arc::new(filled);
        }
        while rows.peek().is_some() {
            let chunk: Vec<T> = rows.by_ref().take(LOG_CHUNK).collect();
            chunks.push(Arc::new(chunk));
        }
        let len = chunks.iter().map(|c| c.len()).sum();
        Self {
            chunks: Arc::new(chunks),
            len,
        }
    }

    /// The index of the first row matching `pred`.
    pub fn position(&self, pred: impl Fn(&T) -> bool) -> Option<usize> {
        self.chunks.iter().flat_map(|c| c.iter()).position(pred)
    }

    /// Iterates over all rows in order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.chunks.iter().flat_map(|c| c.iter())
    }
}

/// One layout row of the log.
#[derive(Clone, Debug)]
pub struct LogRow {
    /// `None` only for layout row 0 (uncommitted changes).
    pub commit: Option<LogCommit>,
    pub graph: GraphCell,
}

/// A commit as the log shows it; strings are built in the background from `CommitInfo`.
#[derive(Clone, Debug)]
pub struct LogCommit {
    pub id: ObjectId,
    /// 7 hex digits.
    pub short_id: SharedString,
    pub summary: SharedString,
    pub author_name: SharedString,
    pub author_email: SharedString,
    /// Log filter (08 §2.3).
    pub committer_name: SharedString,
    pub committer_email: SharedString,
    /// Dates use the committer time (08 §0.5).
    pub committer_time: GitTime,
    pub parent_count: u8,
    pub has_body: bool,
    pub is_stash: bool,
}

/// Owned copy of `gitcthulhu_git::RowView` (20 §2.5), complete.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphCell {
    pub node_lane: u16,
    pub kind: NodeKind,
    pub line_up: bool,
    pub line_down: bool,
    /// Only on HEAD's row: `line_up` exists solely because of the uncommitted row. Row hidden => draw
    /// `line_up = false`.
    pub line_up_from_uncommitted_only: bool,
    /// The uncommitted->HEAD link lane passing through this row. Row hidden => do not draw this passing lane.
    pub uncommitted_link_lane: Option<u16>,
    pub passing: SmallVec<[u16; 4]>,
    pub fork_ins: SmallVec<[u16; 2]>,
    pub merge_outs: SmallVec<[MergeOut; 2]>,
}

impl GraphCell {
    /// A cell with a node on `lane` and nothing else.
    pub fn node(lane: u16, kind: NodeKind) -> Self {
        Self {
            node_lane: lane,
            kind,
            line_up: false,
            line_down: false,
            line_up_from_uncommitted_only: false,
            uncommitted_link_lane: None,
            passing: SmallVec::new(),
            fork_ins: SmallVec::new(),
            merge_outs: SmallVec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn chunked_rows_fill_the_last_chunk_first() {
        let rows = ChunkedRows::new().appended((0..1200).collect());
        assert_eq!(rows.len(), 1200, "all rows are there");
        assert_eq!(rows.chunks.len(), 3, "500 + 500 + 200");
        let more = rows.appended((1200..1500).collect());
        assert_eq!(more.chunks.len(), 3, "the partial chunk is filled first");
        assert_eq!(more.get(1499), Some(&1499), "the last row is addressable");
        assert_eq!(more.get(1500), None, "past the end");
        assert!(
            Arc::ptr_eq(&rows.chunks[0], &more.chunks[0]),
            "full chunks are shared, not copied"
        );
        assert_eq!(rows.len(), 1200, "the original is unchanged");
        assert_eq!(
            more.position(|v| *v == 777),
            Some(777),
            "position scans in order"
        );
    }
}
