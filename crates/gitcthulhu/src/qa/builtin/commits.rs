//! Lookups in the loaded model shared by the selection, menu and dialog verbs.

use crate::backend::{FullName, ObjectId};
use crate::tab::model::{LogRow, RepoModel};

/// The log row at visible index `visible` (row 0 is the uncommitted row while it is shown) and its layout index.
pub(super) fn log_row(model: &RepoModel, visible: usize) -> Result<(usize, &LogRow), String> {
    let snapshot = model
        .log
        .value
        .as_ref()
        .ok_or_else(|| "the log is not loaded".to_owned())?;
    let offset = usize::from(!model.show_uncommitted_row());
    let layout = visible + offset;
    snapshot
        .rows
        .get(layout)
        .map(|row| (layout, row))
        .ok_or_else(|| {
            format!(
                "log row {visible} does not exist ({} rows loaded)",
                snapshot.rows.len().saturating_sub(offset)
            )
        })
}

/// The first commit whose hex id starts with `query` (at least 4 hex digits) or, otherwise, whose summary contains
/// `query` (case-insensitive). Searches the loaded rows only.
pub(super) fn find_commit(model: &RepoModel, query: &str) -> Result<(ObjectId, bool), String> {
    let snapshot = model
        .log
        .value
        .as_ref()
        .ok_or_else(|| "the log is not loaded".to_owned())?;
    let hex = query.len() >= 4 && query.chars().all(|c| c.is_ascii_hexdigit());
    let needle = query.to_lowercase();
    snapshot
        .rows
        .iter()
        .filter_map(|row| row.commit.as_ref())
        .find(|c| {
            if hex && c.id.to_string().starts_with(&needle) {
                return true;
            }
            c.summary.to_lowercase().contains(&needle)
        })
        .map(|c| (c.id, c.is_stash))
        .ok_or_else(|| format!("no loaded commit matches '{query}'"))
}

/// The first commit of the log (the newest row).
pub(super) fn first_commit(model: &RepoModel) -> Result<ObjectId, String> {
    let snapshot = model
        .log
        .value
        .as_ref()
        .ok_or_else(|| "the log is not loaded".to_owned())?;
    snapshot
        .rows
        .iter()
        .find_map(|row| row.commit.as_ref().map(|c| c.id))
        .ok_or_else(|| "the log has no commits".to_owned())
}

/// A local branch by short name: its full name and target.
pub(super) fn local_branch(
    model: &RepoModel,
    name: &str,
) -> Result<crate::backend::LocalBranch, String> {
    let refs = model
        .refs
        .value
        .as_ref()
        .ok_or_else(|| "the refs are not loaded".to_owned())?;
    refs.local
        .iter()
        .find(|b| b.name == name)
        .cloned()
        .ok_or_else(|| {
            let known: Vec<&str> = refs.local.iter().map(|b| b.name.as_str()).collect();
            format!("no local branch '{name}' (have: {})", known.join(", "))
        })
}

/// A remote-tracking branch by `remote/name`.
pub(super) fn remote_branch(
    model: &RepoModel,
    name: &str,
) -> Result<crate::backend::RemoteBranch, String> {
    let refs = model
        .refs
        .value
        .as_ref()
        .ok_or_else(|| "the refs are not loaded".to_owned())?;
    refs.remotes
        .iter()
        .flat_map(|r| &r.branches)
        .find(|b| format!("{}/{}", b.remote, b.name) == name)
        .cloned()
        .ok_or_else(|| format!("no remote branch '{name}'"))
}

/// A tag by short name.
pub(super) fn tag(model: &RepoModel, name: &str) -> Result<crate::backend::TagInfo, String> {
    let refs = model
        .refs
        .value
        .as_ref()
        .ok_or_else(|| "the refs are not loaded".to_owned())?;
    refs.tags
        .iter()
        .find(|t| t.name == name)
        .cloned()
        .ok_or_else(|| format!("no tag '{name}'"))
}

/// A stash by its index in the stash list.
pub(super) fn stash(model: &RepoModel, index: usize) -> Result<crate::backend::StashInfo, String> {
    let refs = model
        .refs
        .value
        .as_ref()
        .ok_or_else(|| "the refs are not loaded".to_owned())?;
    refs.stashes
        .iter()
        .find(|s| s.index == index)
        .cloned()
        .ok_or_else(|| {
            format!(
                "no stash with index {index} ({} stashes)",
                refs.stashes.len()
            )
        })
}

/// The tracking ref of a local branch, if it has one.
pub(super) fn tracking(branch: &crate::backend::LocalBranch) -> Option<FullName> {
    branch.upstream.as_ref().and_then(|u| u.tracking.clone())
}
