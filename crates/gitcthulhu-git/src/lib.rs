//! The plain Git backend of GitCthulhu.
//!
//! UI-agnostic and blocking: every public function must be called from a background thread. Reads run in-process
//! with `gix`; mutations run the `git` executable (so hooks, filters and signing behave natively). See
//! `GitCthulhu.plan/20-backend-architecture.md`.
// `unsafe` is allowed only in src/cli/kill.rs (Job Object / killpg) and src/helper_main/askpass/windows*.rs (pipe
// client), which use #[allow(unsafe_code)].
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod types;
pub use types::*;
pub mod error;
pub use error::{GitError, GitErrorKind, OpClass, OverwriteKind, PartialBlock, PostState, Result};
pub mod repo;
pub use repo::{GitRuntime, HelperStatus, Repo, RepoKind, RuntimeConfig};
pub mod cli;
pub mod credentials;
pub use credentials::{
    CredentialPrompter, CredentialRequest, CredentialResponse, NoPrompter, PromptKind, Sensitive,
};
pub mod blame;
pub mod branch;
pub mod commit;
pub mod config;
pub mod diff;
pub mod graph;
pub mod helper_main;
pub mod history;
mod internal;
pub mod log;
pub mod merge;
pub mod network;
pub mod rebase;
pub mod refs;
pub mod remote;
pub mod stage;
pub mod stash;
pub mod state;
pub mod status;
pub mod submodule;
pub mod tag;
pub mod watch;
pub mod worktree;
pub use graph::{GraphBuilder, GraphLayout, RowView};
pub use history::FileHistoryWalker;
pub use log::LogWalker;
pub use watch::{RepoWatcher, SuppressGuard};

pub use but_core::sync::{RepoExclusive, RepoExclusiveGuard, RepoShared, RepoSharedGuard};
