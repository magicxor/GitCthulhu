//! Repository state right after a failed command, the input of `error::classify` (§1.9).

use std::path::Path;

use crate::{InProgress, PostState, RelaPath, Repo, Result};

/// `gix::Repository::state()`, one index scan for entries with stage != 0, HEAD's short branch name and the id in
/// `CHERRY_PICK_HEAD` / `REVERT_HEAD` (per-worktree git dir). Cheap: no worktree access.
///
/// Lenient by design: it runs after a git command already failed, so a part that cannot be read (corrupt index,
/// broken HEAD) is logged and left empty rather than replacing the command's own error.
pub(crate) fn read(repo: &Repo) -> Result<PostState> {
    let local = repo.local();
    let in_progress = local.state().map(InProgress::from);
    let conflicted = match local.index_or_empty() {
        Ok(index) => conflicted_paths(&index),
        Err(err) => {
            tracing::warn!(%err, "Could not read the index after a failed git command");
            Vec::new()
        }
    };
    let head_branch = match local.head_name() {
        Ok(name) => name.map(|name| name.shorten().to_string()),
        Err(err) => {
            tracing::warn!(%err, "Could not read HEAD after a failed git command");
            None
        }
    };
    Ok(PostState {
        in_progress,
        conflicted,
        head_branch,
        pick_head: pick_head(repo.git_dir()),
    })
}

/// Every path with an unmerged entry, once, in index order.
fn conflicted_paths(index: &gix::index::State) -> Vec<RelaPath> {
    let mut out: Vec<RelaPath> = Vec::new();
    for entry in index.entries() {
        if entry.stage_raw() == 0 {
            continue;
        }
        let path = entry.path(index);
        // Stages of one path are adjacent in the sorted index.
        if out
            .last()
            .is_none_or(|last| last.as_slice() != path.as_ref() as &[u8])
        {
            out.push(path.to_owned());
        }
    }
    out
}

/// The commit in `CHERRY_PICK_HEAD`, else `REVERT_HEAD`.
fn pick_head(git_dir: &Path) -> Option<gix::ObjectId> {
    ["CHERRY_PICK_HEAD", "REVERT_HEAD"].iter().find_map(|name| {
        let content = std::fs::read(git_dir.join(name)).ok()?;
        let first = content.split(|b| *b == b'\n').next()?;
        gix::ObjectId::from_hex(first.trim_ascii()).ok()
    })
}
