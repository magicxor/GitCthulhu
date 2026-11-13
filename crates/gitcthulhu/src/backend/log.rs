//! Log reads (owned by G4 after G0b; spec 21 §2.4, §4.2): the pager that walks history and lays out the graph,
//! full messages for the log filter, single commits.

use gpui_kit::SharedString;
use smallvec::SmallVec;

use super::{
    ByteSlice as _, CancelToken, CommitInfo, GitError, GraphBuilder, LogOptions, LogWalker,
    ObjectId, Repo, RowView,
};
use crate::tab::model::{GraphCell, LogCommit, LogRow};

/// At most this many ids per `messages` call.
pub const MAX_MESSAGE_IDS: usize = 5_000;

/// A history walk plus its graph layout. `Send`; kept in `Arc<Mutex<..>>` by the model and the log view's paging.
pub struct LogPager {
    walker: LogWalker,
    builder: GraphBuilder,
    /// Layout rows already returned by `next_rows`.
    emitted: usize,
}

impl LogPager {
    /// `LogWalker::new(repo, &LogOptions::default(), cancel)` (all refs incl. HEAD), a graph builder, and ALWAYS the
    /// synthetic uncommitted row 0 (`push_uncommitted(head)`, `head = None` when unborn; 20 §3.5).
    pub fn new(repo: &Repo, cancel: &CancelToken) -> Result<Self, GitError> {
        let walker = LogWalker::new(repo, &LogOptions::default(), cancel)?;
        let mut builder = GraphBuilder::new(walker.first_parent());
        let head = repo.local().head_id().ok().map(|id| id.detach());
        builder.push_uncommitted(head);
        Ok(Self {
            walker,
            builder,
            emitted: 0,
        })
    }

    /// The next `n` commits laid out: the new rows (the first call includes layout row 0) with their strings built
    /// here, the graph's `max_lane` and whether the walk is exhausted.
    pub fn next_rows(
        &mut self,
        n: usize,
        cancel: &CancelToken,
    ) -> Result<(Vec<LogRow>, u16, bool), GitError> {
        let page = self.walker.next_page(n, cancel)?;
        let first_commit_row = self.builder.layout().len();
        self.builder.push_page(&page, &self.walker);
        let layout = self.builder.layout();
        let stashes = self.walker.stash_ids();
        let mut rows = Vec::with_capacity(layout.len() - self.emitted);
        for ix in self.emitted..layout.len() {
            let commit = if ix >= first_commit_row {
                page.commits
                    .get(ix - first_commit_row)
                    .map(|c| log_commit(c, stashes.contains(&c.id)))
            } else {
                None
            };
            rows.push(LogRow {
                commit,
                graph: graph_cell(&layout.row(ix)),
            });
        }
        self.emitted = layout.len();
        Ok((rows, layout.max_lane(), page.exhausted))
    }

    /// Layout rows returned so far.
    pub fn rows_emitted(&self) -> usize {
        self.emitted
    }
}

/// The owned copy of a graph row (20 §2.5).
pub fn graph_cell(row: &RowView<'_>) -> GraphCell {
    GraphCell {
        node_lane: row.node_lane,
        kind: row.kind,
        line_up: row.line_up,
        line_down: row.line_down,
        line_up_from_uncommitted_only: row.line_up_from_uncommitted_only,
        uncommitted_link_lane: row.uncommitted_link_lane,
        passing: row.passing().collect::<SmallVec<_>>(),
        fork_ins: row.fork_ins().collect::<SmallVec<_>>(),
        merge_outs: row.merge_outs().collect::<SmallVec<_>>(),
    }
}

/// The display strings of a commit, built off the UI thread.
pub fn log_commit(c: &CommitInfo, is_stash: bool) -> LogCommit {
    LogCommit {
        id: c.id,
        short_id: SharedString::from(c.id.to_hex_with_len(7).to_string()),
        summary: SharedString::from(c.summary.to_string()),
        author_name: SharedString::from(c.author.person.name.to_str_lossy().into_owned()),
        author_email: SharedString::from(c.author.person.email.to_str_lossy().into_owned()),
        committer_name: SharedString::from(c.committer.person.name.to_str_lossy().into_owned()),
        committer_email: SharedString::from(c.committer.person.email.to_str_lossy().into_owned()),
        committer_time: c.committer.time,
        parent_count: u8::try_from(c.parents.len()).unwrap_or(u8::MAX),
        has_body: c.has_body,
        is_stash,
    }
}

/// Full messages of `ids` (`log::messages`), at most [`MAX_MESSAGE_IDS`] per call.
pub fn messages(
    repo: &Repo,
    ids: &[ObjectId],
    cancel: &CancelToken,
) -> Result<Vec<Box<str>>, GitError> {
    let ids = &ids[..ids.len().min(MAX_MESSAGE_IDS)];
    gitcthulhu_git::log::messages(repo, ids, cancel)
}

/// One commit's summary information (`log::commit_info`).
pub fn commit_info(repo: &Repo, id: ObjectId) -> Result<CommitInfo, GitError> {
    gitcthulhu_git::log::commit_info(repo, id)
}
