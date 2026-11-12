//! Commit log walking and search (WP-B4). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use std::collections::HashSet;

use gix::ObjectId;

use crate::{
    CancelToken, CommitInfo, LogOptions, LogOrder, LogPage, LogTips, Repo, Result, SearchQuery,
    internal::not_implemented,
};

impl Default for LogOptions {
    fn default() -> Self {
        LogOptions {
            tips: LogTips::AllRefs {
                remotes: true,
                tags: true,
                stashes: true,
            },
            order: LogOrder::Date,
            first_parent: false,
            mailmap: true,
        }
    }
}

/// An incremental walk over history, producing pages of commits.
pub struct LogWalker {
    stash_ids: HashSet<ObjectId>,
}

impl LogWalker {
    /// Start a walk. The Topo build (up to seconds without a commit-graph) runs on a detached compute thread (§1.4);
    /// on cancel this returns Err(Cancelled) immediately and the build result is dropped when it finishes.
    pub fn new(repo: &Repo, opts: &LogOptions, cancel: &CancelToken) -> Result<LogWalker> {
        not_implemented("log::LogWalker::new")
    }

    /// `opts.first_parent` (for `GraphBuilder::new`).
    pub fn first_parent(&self) -> bool {
        false
    }

    /// Next up to `n` commits in walk order. Cancel is checked every 256 commits.
    pub fn next_page(&mut self, n: usize, cancel: &CancelToken) -> Result<LogPage> {
        not_implemented("log::LogWalker::next_page")
    }

    /// How many commits were returned so far.
    pub fn loaded(&self) -> usize {
        0
    }

    /// The walk has no more commits.
    pub fn is_exhausted(&self) -> bool {
        true
    }

    /// The stash commits among the walked tips.
    pub fn stash_ids(&self) -> &HashSet<ObjectId> {
        &self.stash_ids
    }

    /// Whether `id` is a commit whose parents are cut off by a shallow clone.
    pub fn is_shallow_boundary(&self, id: &ObjectId) -> bool {
        false
    }
}

/// Walk all tips (or the given ones) and report matching commit ids in walk order, calling `on_match` as they are
/// found. Stops after `limit` matches. Used for "search all history" (the in-memory search over loaded rows is
/// GUI-side, 05 §2.13).
pub fn search(
    repo: &Repo,
    query: &SearchQuery,
    tips: &LogTips,
    limit: usize,
    cancel: &CancelToken,
    on_match: &mut dyn FnMut(&CommitInfo),
) -> Result<usize> {
    not_implemented("log::search")
}

/// Full messages, oldest first, of `range` (`base..head`), for AI PR text (16 A9).
pub fn commit_messages(
    repo: &Repo,
    base: Option<ObjectId>,
    head: ObjectId,
    limit: usize,
) -> Result<Vec<String>> {
    not_implemented("log::commit_messages")
}

/// Message of HEAD (amend prefill, 05 §2.14). `None` for unborn.
pub fn head_message(repo: &Repo) -> Result<Option<String>> {
    not_implemented("log::head_message")
}

/// Full decoded messages of the given commits, same order (Gitnuro's log filter matches the full message of loaded
/// commits, 08 §2.3). The GUI calls it in chunks of <= 5 000 ids for the loaded rows and caches the result. Cancel is
/// checked every 256 commits. Unknown ids => empty string.
pub fn messages(repo: &Repo, ids: &[ObjectId], cancel: &CancelToken) -> Result<Vec<Box<str>>> {
    not_implemented("log::messages")
}

/// Single commit info (jump-to-commit, tooltips).
pub fn commit_info(repo: &Repo, id: ObjectId) -> Result<CommitInfo> {
    not_implemented("log::commit_info")
}
