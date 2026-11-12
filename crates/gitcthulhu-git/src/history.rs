//! File history (WP-B9). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use gix::ObjectId;

use crate::{CancelToken, FileHistoryPage, RelaPath, Repo, Result, internal::not_implemented};

/// An incremental walk over the commits that touched one path.
pub struct FileHistoryWalker {
    _private: (),
}

impl FileHistoryWalker {
    /// `start = None` => HEAD. `follow` = rename following like `git log --follow` (one path at a time).
    pub fn new(
        repo: &Repo,
        path: &RelaPath,
        start: Option<ObjectId>,
        follow: bool,
    ) -> Result<FileHistoryWalker> {
        not_implemented("history::FileHistoryWalker::new")
    }

    /// The next up to `n` entries.
    pub fn next_page(&mut self, n: usize, cancel: &CancelToken) -> Result<FileHistoryPage> {
        not_implemented("history::FileHistoryWalker::next_page")
    }
}
