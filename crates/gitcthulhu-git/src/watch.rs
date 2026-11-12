//! Repository watcher (WP-B9). Stub: `start` returns `NotImplemented` until the package merges.
#![allow(unused_variables)]

use crate::{FileStamp, Repo, Result, WatchEvent, internal::not_implemented};

/// Watches a repository and reports what the GUI must reload.
pub struct RepoWatcher {
    _private: (),
}

impl RepoWatcher {
    /// Starts watching `repo` (worktree + git dir + common dir). `on_event` is called on the watcher thread.
    pub fn start(
        repo: &Repo,
        on_event: impl Fn(WatchEvent) + Send + 'static,
    ) -> Result<RepoWatcher> {
        not_implemented("watch::RepoWatcher::start")
    }

    /// While the guard lives, events are accumulated instead of delivered; dropping it schedules delivery after a
    /// grace period.
    pub fn suppress(&self) -> SuppressGuard {
        SuppressGuard { _private: () }
    }

    /// Deliver accumulated events now.
    pub fn flush(&self) {}

    /// The GUI's own `status::refresh_index` wrote the index with this stamp: an `index` change whose current stamp
    /// equals it contributes nothing to the batch (it is not a user change). Replaced by each call.
    pub fn ignore_index_stamp(&self, stamp: FileStamp) {}
}

/// Holds events back while alive.
pub struct SuppressGuard {
    _private: (),
}
