//! Implement the file-monitoring agent that informs about changes in interesting locations.
#![deny(unsafe_code)]
#![allow(clippy::doc_markdown, clippy::missing_errors_doc)]

mod watch_plan;

mod events;
pub use events::{Checkout, InternalEvent, LinkedWorktree};

mod file_monitor;
pub use file_monitor::{
    FETCH_HEAD, FileMonitorHandle, HEAD, HEAD_ACTIVITY, INDEX, LINKED_WORKTREE_GITDIR,
    LINKED_WORKTREES_DIR, MODULES_DIR, REFS_DIR, STASH_ACTIVITY, WatchMode, spawn,
};
