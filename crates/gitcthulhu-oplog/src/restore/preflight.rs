//! Phase A of a restore (spec 17 §3.3 A2-A6): checks that write nothing and cost no status walk.

use std::{collections::BTreeSet, path::PathBuf};

use gix::refs::FullName;

use super::{Expected, Request, RestoreKind};
use crate::{
    Oplog, OplogError,
    capture::capture,
    state::{CapturedState, HeadState, RefValue},
    storage::{self, Chain},
    types::{ConfirmReason, InProgress, PushedSinceRef, Refusal, Scope, UntrackedCapture},
};

/// The result of a passed preflight.
pub(super) struct Preflight {
    /// The current refs, `HEAD`, stash and in-progress state (`Scope::RefsOnly`); the guards of the ref writes.
    pub cur: CapturedState,
    /// The reasons found so far that need the user's confirmation.
    pub reasons: Vec<ConfirmReason>,
    /// Annotated tag objects the target needs that are gone from the object database, with their raw bytes from
    /// `keep-tags/` (re-created before the refs are written).
    pub recreate_tags: Vec<(gix::ObjectId, Vec<u8>)>,
}

/// The outcome of [`check()`].
pub(super) enum Checked {
    /// Go on with phase B (after asking about `reasons`, if any).
    Ready(Box<Preflight>),
    /// Nothing can be done.
    Refused(Refusal),
}

/// Steps A2-A6.
pub(super) fn check(
    oplog: &Oplog,
    repo: &gix::Repository,
    req: &Request,
    chain: &Chain,
) -> Result<Checked, OplogError> {
    // A2: the sequencer state must be the one the undone item left (R5), or none at all (R6).
    let in_progress: Option<InProgress> = repo.state().map(Into::into);
    match (&req.kind, &req.expected) {
        (RestoreKind::Undo | RestoreKind::Redo, Expected::State(expected)) => {
            if in_progress != expected.in_progress {
                return Ok(Checked::Refused(Refusal::InProgressMismatch {
                    expected: expected.in_progress,
                    actual: in_progress,
                }));
            }
        }
        _ => {
            if let Some(kind) = in_progress {
                return Ok(Checked::Refused(Refusal::OperationInProgress { kind }));
            }
        }
    }

    // A3
    let cur = capture(
        repo,
        Scope::RefsOnly,
        &UntrackedCapture::None,
        &oplog.limits(),
    )?;

    // A4
    let mut reasons = Vec::new();
    match (&req.kind, &req.expected) {
        (RestoreKind::RestoreTo(_), _) => reasons.push(ConfirmReason::ArbitraryRestore {
            ref_changes: req.target.compare_refs(&cur),
        }),
        (_, Expected::State(expected)) => {
            let moved = expected.compare_refs(&cur);
            if !moved.is_empty() {
                reasons.push(ConfirmReason::RefsMovedSince(moved));
            }
        }
        (_, Expected::Unknown) => reasons.push(ConfirmReason::AfterStateUnknown),
    }
    if matches!(req.kind, RestoreKind::Undo) {
        let pushed = pushed_since(req, &cur);
        if !pushed.is_empty() {
            reasons.push(ConfirmReason::PushedSince(pushed));
        }
    }

    // A5
    let mut missing = Vec::new();
    let mut recreate_tags = Vec::new();
    for id in needed_objects(&req.target) {
        if repo.has_object(id) {
            continue;
        }
        match kept_tag(repo, chain, id)? {
            Some(data) => recreate_tags.push((id, data)),
            None => missing.push(id),
        }
    }
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        return Ok(Checked::Refused(Refusal::MissingObjects(missing)));
    }

    // A6
    if let Some(refusal) = branch_checked_out_elsewhere(repo, &cur, &req.target)? {
        return Ok(Checked::Refused(refusal));
    }

    Ok(Checked::Ready(Box::new(Preflight {
        cur,
        reasons,
        recreate_tags,
    })))
}

/// The objects a restore of `target` writes into refs or the worktree: ref targets, a detached `HEAD`, and the
/// trees of the files part.
fn needed_objects(target: &CapturedState) -> BTreeSet<gix::ObjectId> {
    let mut ids: BTreeSet<gix::ObjectId> = target
        .refs
        .values()
        .filter_map(|value| match value {
            RefValue::Direct(id) => Some(*id),
            RefValue::Symbolic(_) => None,
        })
        .collect();
    if let HeadState::Detached(id) = &target.head {
        ids.insert(*id);
    }
    if let Some(files) = &target.files {
        ids.extend([files.head_tree, files.worktree, files.index]);
        ids.extend(files.index_conflicts);
    }
    ids
}

/// The raw bytes of the annotated tag object `id` from the `keep-tags/` of any entry (newest first), if the bytes
/// really hash to `id`.
fn kept_tag(
    repo: &gix::Repository,
    chain: &Chain,
    id: gix::ObjectId,
) -> Result<Option<Vec<u8>>, OplogError> {
    for entry in chain.entries.iter().rev() {
        let Some(data) = storage::kept_tag(repo, entry.commit, id)? else {
            continue;
        };
        let actual = gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Tag, &data)
            .map_err(anyhow::Error::from)?;
        if actual == id {
            return Ok(Some(data));
        }
    }
    Ok(None)
}

/// Markers newer than the undone operation that pushed a local ref which differs between the target and the
/// expected state (the current state when the expected one is unknown).
fn pushed_since(req: &Request, cur: &CapturedState) -> Vec<PushedSinceRef> {
    let other = match &req.expected {
        Expected::State(expected) => expected.as_ref(),
        Expected::Unknown => cur,
    };
    let changed: BTreeSet<&FullName> = req
        .target
        .refs
        .keys()
        .chain(other.refs.keys())
        .filter(|name| req.target.refs.get(*name) != other.refs.get(*name))
        .collect();
    let mut out = Vec::new();
    for (marker_id, marker) in &req.markers_since {
        for update in &marker.updates {
            if let Some(local) = &update.local
                && changed.contains(local)
            {
                out.push(PushedSinceRef {
                    marker: *marker_id,
                    remote: marker.remote.clone(),
                    local: local.clone(),
                    remote_ref: update.remote_ref.clone(),
                });
            }
        }
    }
    out
}

/// Step A6: a local branch the restore creates, deletes or moves, or the branch the target `HEAD` names, must not
/// be checked out in another worktree (Git forbids it, so this cannot be forced).
fn branch_checked_out_elsewhere(
    repo: &gix::Repository,
    cur: &CapturedState,
    target: &CapturedState,
) -> Result<Option<Refusal>, OplogError> {
    let mut branches: BTreeSet<&FullName> = cur
        .refs
        .keys()
        .chain(target.refs.keys())
        .filter(|name| name.as_bstr().starts_with(b"refs/heads/"))
        .filter(|name| cur.refs.get(*name) != target.refs.get(*name))
        .collect();
    if let HeadState::Symbolic(name) | HeadState::Unborn(name) = &target.head
        && name.as_bstr().starts_with(b"refs/heads/")
    {
        branches.insert(name);
    }
    if branches.is_empty() || repo.worktrees().map_err(anyhow::Error::from)?.is_empty() {
        return Ok(None);
    }
    let safe = but_core::branch::SafeDelete::new(repo)?;
    let own = repo.workdir().map(canonical);
    for branch in branches {
        let reference = gix::prelude::ReferenceExt::attach(
            gix::refs::Reference {
                name: branch.clone(),
                target: gix::refs::Target::Object(repo.object_hash().null()),
                peeled: None,
            },
            repo,
        );
        let Some(dirs) = safe.worktree_dirs_with_ref(&reference) else {
            continue;
        };
        let elsewhere: Vec<PathBuf> = dirs
            .iter()
            .filter(|dir| Some(canonical(dir)) != own)
            .cloned()
            .collect();
        if !elsewhere.is_empty() {
            return Ok(Some(Refusal::BranchCheckedOutElsewhere {
                branch: branch.clone(),
                worktrees: elsewhere,
            }));
        }
    }
    Ok(None)
}

fn canonical(path: &std::path::Path) -> PathBuf {
    gix::path::realpath(path).unwrap_or_else(|_| path.to_owned())
}
