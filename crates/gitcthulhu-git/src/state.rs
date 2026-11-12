//! Repository state detection (WP-B1). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use crate::{Repo, RepoState, Result, TodoLine, internal::not_implemented};

/// HEAD, the operation in progress and its details.
pub fn repo_state(repo: &Repo) -> Result<RepoState> {
    not_implemented("state::repo_state")
}

/// Parse one line of a rebase/sequencer todo list; pure; `None` for blank lines.
///
/// Stub: always `None`.
pub fn parse_todo_line(line: &str) -> Option<TodoLine> {
    None
}

/// Cheap: scans the index for stage != 0.
pub fn has_conflicts(repo: &Repo) -> Result<bool> {
    not_implemented("state::has_conflicts")
}
