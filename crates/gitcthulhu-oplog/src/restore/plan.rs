//! Phase B of a restore (spec 17 §3.3 B1-B3): capture the pre-restore state P and plan index and worktree.

use std::collections::BTreeSet;

use bstr::{BString, ByteSlice as _};
use gix::merge::tree::TreatAsUnresolved;

use super::{
    Expected, Request, RestoreKind, acknowledged, index::NewIndex, preflight::Preflight, worktree,
};
use crate::{
    Oplog, OplogError,
    capture::capture,
    state::{CapturedState, FileState},
    types::{ConfirmReason, UntrackedCapture},
};

/// What phase C writes.
pub(super) struct Plan {
    /// The pre-restore state P: the new entry's before-state.
    pub pre: CapturedState,
    /// The untracked contents captured in P and Q (§1.3).
    pub untracked: UntrackedCapture,
    /// The index and worktree plan; `None` for `RefsOnly` targets.
    pub files: Option<FilePlan>,
    /// Every reason of phases A and B (all acknowledged).
    pub reasons: Vec<ConfirmReason>,
}

/// The index and worktree part of a [`Plan`].
pub(super) struct FilePlan {
    /// The index to write in C8.
    pub new_index: NewIndex,
    /// The merge base of the worktree checkout.
    pub base: gix::ObjectId,
    /// `true` if `base` is the forced base (restore-to, unknown expected state).
    pub forced: bool,
    /// `true` if the checkout has nothing to do (`target.worktree == base`).
    pub skip_checkout: bool,
}

/// The outcome of [`plan()`].
pub(super) enum Planned {
    /// Go on with phase C.
    Ready(Box<Plan>),
    /// New reasons need the user's confirmation (all reasons so far).
    NeedsConfirmation(Vec<ConfirmReason>),
}

/// Steps B1-B3.
pub(super) fn plan(
    oplog: &Oplog,
    repo: &gix::Repository,
    req: &Request,
    preflight: &Preflight,
) -> Result<Planned, OplogError> {
    // B1
    let untracked = untracked_union(repo, req)?;
    let mut pre = capture(repo, req.target.scope, &untracked, &oplog.limits())?;
    worktree::capture_left_out(repo, &mut pre, &untracked, &oplog.limits())?;

    // B2
    let mut reasons = preflight.reasons.clone();
    let files = match (&req.target.files, &pre.files) {
        (Some(target), Some(current)) => {
            Some(plan_files(repo, req, &pre, target, current, &mut reasons)?)
        }
        _ => None,
    };

    // B3
    if !acknowledged(req.ack.as_ref(), &reasons) {
        return Ok(Planned::NeedsConfirmation(reasons));
    }
    Ok(Planned::Ready(Box::new(Plan {
        pre,
        untracked,
        files,
        reasons,
    })))
}

/// The untracked contents P and Q capture (§1.3): `untracked(target) ∪ untracked(expected)`, plus the untracked
/// files that exist now at a path the restore changes (`None` if there are none).
///
/// The second part matters for operations that turn tracked files into untracked ones without capturing them
/// (`reset --mixed`, `rm --cached`): their after-state lacks the file, so undo makes it tracked again. Without the
/// file in P, a redo (whose target is P) would see it as "absent in the target" and delete it. The capture skips
/// such a file if it is ignored; [`worktree::capture_left_out()`] adds it afterwards.
pub(super) fn untracked_union(
    repo: &gix::Repository,
    req: &Request,
) -> Result<UntrackedCapture, OplogError> {
    let mut paths: BTreeSet<BString> = req.target.untracked_paths(repo)?.into_iter().collect();
    if let Expected::State(expected) = &req.expected {
        paths.extend(expected.untracked_paths(repo)?);
    }
    if let Some(target) = &req.target.files {
        let other = match &req.expected {
            Expected::State(expected) => expected.files.as_ref().map(|f| f.worktree),
            Expected::Unknown => None,
        };
        let other = match other {
            Some(tree) => tree,
            None => repo
                .head_tree_id_or_empty()
                .map_err(anyhow::Error::from)?
                .detach(),
        };
        paths.extend(untracked_on_disk_among_changes(
            repo,
            other,
            target.worktree,
        )?);
    }
    Ok(if paths.is_empty() {
        UntrackedCapture::None
    } else {
        UntrackedCapture::Paths(paths.into_iter().collect())
    })
}

/// The paths that differ between the trees `a` and `b`, are not in the current index, and exist in the worktree:
/// untracked files the restore may touch. One tree diff, one index lookup and one `lstat` per changed path.
fn untracked_on_disk_among_changes(
    repo: &gix::Repository,
    a: gix::ObjectId,
    b: gix::ObjectId,
) -> Result<Vec<BString>, OplogError> {
    if a == b {
        return Ok(Vec::new());
    }
    let Some(workdir) = repo.workdir() else {
        return Ok(Vec::new());
    };
    let (a, b) = (
        repo.find_tree(a).map_err(anyhow::Error::from)?,
        repo.find_tree(b).map_err(anyhow::Error::from)?,
    );
    let changes = repo
        .diff_tree_to_tree(&a, &b, gix::diff::Options::default().with_rewrites(None))
        .map_err(anyhow::Error::from)?;
    let index = repo.index_or_empty().map_err(anyhow::Error::from)?;
    let mut out = Vec::new();
    for change in changes {
        if change.entry_mode().is_tree() {
            continue;
        }
        let location = change.location();
        if index.entry_range(location).is_some() {
            continue;
        }
        let Ok(rela) = gix::path::from_bstr(location) else {
            continue;
        };
        if std::fs::symlink_metadata(workdir.join(rela)).is_ok() {
            out.push(location.to_owned());
        }
    }
    Ok(out)
}

/// The index plan and the worktree base (§3.3 B2). If staging done since the operation conflicts with the target,
/// `StagedChangesConflict` is pushed to `reasons` and the target's index is planned: B3 asks unless the user
/// acknowledged it already, and an acknowledged conflict takes the target's index.
fn plan_files(
    repo: &gix::Repository,
    req: &Request,
    pre: &CapturedState,
    target: &FileState,
    current: &FileState,
    reasons: &mut Vec<ConfirmReason>,
) -> Result<FilePlan, OplogError> {
    let expected = match &req.expected {
        Expected::State(expected) => Some(expected.as_ref()),
        Expected::Unknown => None,
    };
    let expected_files = expected.and_then(|e| e.files.as_ref());
    let unknown = matches!(req.kind, RestoreKind::RestoreTo(_)) || expected_files.is_none();
    let head_moved = expected
        .is_some_and(|e| pre.head != e.head || pre.head_commit(repo) != e.head_commit(repo));
    // The carry base (§2.7): what index and worktree were when the undone item left them, or, after an external
    // HEAD move, the current HEAD tree (`git checkout -m` semantics).
    let carry_base = match expected_files {
        Some(files) if !unknown && !head_moved => files.clone(),
        Some(_) if !unknown => FileState::clean(current.head_tree),
        _ => current.clone(),
    };

    let target_index = NewIndex::of(target);
    let new_index = if unknown
        || (current.index == carry_base.index
            && current.index_conflicts == carry_base.index_conflicts)
    {
        target_index
    } else if current.index_conflicts != carry_base.index_conflicts {
        let paths =
            conflict_paths_differing(repo, carry_base.index_conflicts, current.index_conflicts)?;
        reasons.push(ConfirmReason::StagedChangesConflict(paths));
        target_index
    } else {
        let mut outcome = repo
            .merge_trees(
                carry_base.index,
                current.index,
                target.index,
                but_core::RepositoryExt::default_merge_labels(repo),
                repo.tree_merge_options()
                    .map_err(anyhow::Error::from)?
                    .with_rewrites(None),
            )
            .map_err(anyhow::Error::from)?;
        let unresolved = TreatAsUnresolved::git();
        if outcome.has_unresolved_conflicts(unresolved) {
            let mut paths: Vec<BString> = outcome
                .conflicts
                .iter()
                .filter(|c| c.is_unresolved(unresolved))
                .map(|c| c.ours.location().to_owned())
                .collect();
            paths.sort();
            paths.dedup();
            reasons.push(ConfirmReason::StagedChangesConflict(paths));
            target_index
        } else {
            NewIndex {
                tree: outcome.tree.write().map_err(anyhow::Error::from)?.detach(),
                conflicts: target.index_conflicts,
            }
        }
    };

    let base = if unknown {
        worktree::force_base(repo, pre, &req.target)?
    } else {
        carry_base.worktree
    };
    Ok(FilePlan {
        new_index,
        base,
        forced: unknown,
        skip_checkout: target.worktree == base,
    })
}

/// The paths whose conflict stages differ between the but-core conflict trees `a` and `b`.
fn conflict_paths_differing(
    repo: &gix::Repository,
    a: Option<gix::ObjectId>,
    b: Option<gix::ObjectId>,
) -> Result<Vec<BString>, OplogError> {
    let entries =
        |tree: Option<gix::ObjectId>| -> Result<BTreeSet<(BString, gix::ObjectId)>, OplogError> {
            let Some(tree) = tree else {
                return Ok(BTreeSet::new());
            };
            let mut recorder = gix::traverse::tree::Recorder::default();
            repo.find_tree(tree)
                .map_err(anyhow::Error::from)?
                .traverse()
                .breadthfirst(&mut recorder)
                .map_err(anyhow::Error::from)?;
            Ok(recorder
                .records
                .into_iter()
                .filter(|r| !r.mode.is_tree())
                .map(|r| (r.filepath, r.oid))
                .collect())
        };
    let (a, b) = (entries(a)?, entries(b)?);
    let mut paths: Vec<BString> = a
        .symmetric_difference(&b)
        .filter_map(|(path, _)| {
            path.rsplit_once_str("/")
                .map(|(path, _stage)| BString::from(path))
        })
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}
