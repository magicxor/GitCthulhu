//! The restore engine: undo, redo, restore-to and recovery of interrupted restores (spec 17 §3, §4.3).
//!
//! A restore runs in three phases (§3.3):
//!
//! - **A – preflight** ([`preflight`]): checks that write nothing and cost no status walk; refusals and the first
//!   confirmation reasons.
//! - **B – plan** ([`plan`]): capture the pre-restore state P, plan the index (3-way carry of staging) and the
//!   worktree merge base.
//! - **C – write**: journal ([`journal`]), worktree ([`worktree`], transactional), refs ([`refs`], guarded),
//!   stash ([`stash`]), branch config, index ([`index`]), sequencer quit ([`in_progress`]), then the new entry.
//!
//! Nothing is written before the user acknowledged every reason (`NeedsConfirmation` returns early). From the
//! moment the worktree was written, a failure either rolls back (failed ref guard) or leaves the journal, which
//! [`recovery`] finishes or rolls back.

use but_core::sync::RepoExclusive;

use crate::{
    Oplog, OplogError,
    capture::capture,
    fail_point::{self, FailPoint},
    meta::{EntryMeta, FORMAT_VERSION},
    state::{CapturedState, refs_changed},
    storage::{self, Chain},
    types::{
        ConfirmReason, ConfirmToken, Confirmation, EntryId, EntryResult, EntryRole, InProgress,
        InterruptedRestore, NetworkMarker, OperationKind, Recovery, Refusal, RestoreOptions,
        RestoreReport, RestoreTarget, RestoreWarning, Side, SkippedFile, UndoOutcome,
        UntrackedCapture,
    },
};

mod in_progress;
mod index;
mod journal;
mod plan;
mod preflight;
mod recovery;
mod refs;
mod stash;
mod worktree;

use self::{
    index::NewIndex,
    journal::{JOURNAL_VERSION, Journal, Phase},
    plan::{Plan, Planned},
    preflight::{Checked, Preflight},
    worktree::Checkout,
};

/// Which kind of restore runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestoreKind {
    Undo,
    Redo,
    RestoreTo(Side),
}

impl RestoreKind {
    /// The verb of reflog messages.
    fn verb(self) -> &'static str {
        match self {
            RestoreKind::Undo => "undo",
            RestoreKind::Redo => "redo",
            RestoreKind::RestoreTo(_) => "restore",
        }
    }

    /// The kind of the new entry.
    fn operation_kind(self) -> OperationKind {
        match self {
            RestoreKind::Undo => OperationKind::Undo,
            RestoreKind::Redo => OperationKind::Redo,
            RestoreKind::RestoreTo(_) => OperationKind::RestoreTo,
        }
    }

    /// The side of a restore-to.
    fn side(self) -> Option<Side> {
        match self {
            RestoreKind::RestoreTo(side) => Some(side),
            RestoreKind::Undo | RestoreKind::Redo => None,
        }
    }

    /// Reconstruct the kind stored in a journal.
    fn from_journal(kind: OperationKind, side: Option<Side>) -> Option<RestoreKind> {
        match (kind, side) {
            (OperationKind::Undo, _) => Some(RestoreKind::Undo),
            (OperationKind::Redo, _) => Some(RestoreKind::Redo),
            (OperationKind::RestoreTo, Some(side)) => Some(RestoreKind::RestoreTo(side)),
            _ => None,
        }
    }

    /// The title of the new entry.
    fn entry_title(self, title: &str) -> String {
        match self {
            RestoreKind::Undo => format!("Undo {title}"),
            RestoreKind::Redo => format!("Redo {title}"),
            RestoreKind::RestoreTo(Side::Before) => format!("Restore to before {title}"),
            RestoreKind::RestoreTo(Side::After) => format!("Restore to after {title}"),
        }
    }

    /// The role of the new entry, given the pre-restore state.
    fn role(self, reversing: EntryId, pre: &CapturedState) -> EntryRole {
        match self {
            RestoreKind::Undo => EntryRole::Undo {
                reverses: reversing,
                // R5: the before-state has a sequencer state that a redo would not restore.
                redoable: pre.in_progress.is_none(),
            },
            RestoreKind::Redo => EntryRole::Redo {
                reverses: reversing,
            },
            RestoreKind::RestoreTo(side) => EntryRole::RestoreTo {
                target: reversing,
                side,
            },
        }
    }
}

/// The state the repository is expected to be in (the recorded after-state of what is reversed).
pub(super) enum Expected {
    State(Box<CapturedState>),
    /// The after-capture failed, or a restore-to.
    Unknown,
}

/// One restore request (§3.3 inputs).
pub(super) struct Request {
    pub kind: RestoreKind,
    /// The entry the new entry reverses (undo/redo) or restores (restore-to).
    pub reversing: EntryId,
    /// The user-visible operation title.
    pub title: String,
    pub target: CapturedState,
    pub target_tree: gix::ObjectId,
    pub expected: Expected,
    pub expected_tree: Option<gix::ObjectId>,
    /// Push markers recorded after the undone operation (undo only).
    pub markers_since: Vec<(EntryId, NetworkMarker)>,
    pub ack: Option<ConfirmToken>,
}

/// `true` if there is nothing to confirm, or `ack` acknowledges every reason.
pub(super) fn acknowledged(ack: Option<&ConfirmToken>, reasons: &[ConfirmReason]) -> bool {
    reasons.is_empty() || ack.is_some_and(|token| token.acknowledges_all(reasons))
}

/// Undo the top of the undo stack (§3.3, §4.3).
pub(crate) fn undo(
    oplog: &Oplog,
    repo: &gix::Repository,
    options: RestoreOptions,
    _perm: &mut RepoExclusive,
) -> Result<UndoOutcome, OplogError> {
    journal::refuse_if_present(oplog, repo)?;
    let chain_and_stacks = oplog.chain(repo)?;
    let (chain, stacks) = &*chain_and_stacks;
    let Some(top) = stacks.undo.last().copied() else {
        return Ok(UndoOutcome::Nothing);
    };
    let (target, target_tree) = match load_snapshot(repo, chain, top.reverse, Side::Before)? {
        Ok(loaded) => loaded,
        Err(refusal) => return Ok(UndoOutcome::Refused(refusal)),
    };
    let (expected, expected_tree) = load_expected(repo, chain, top.expected)?;
    let request = Request {
        kind: RestoreKind::Undo,
        reversing: top.reverse,
        title: title_of(chain, top.label),
        target,
        target_tree,
        expected,
        expected_tree,
        markers_since: markers_after(chain, top.label),
        ack: options.confirmation,
    };
    run(oplog, repo, chain, request)
}

/// Redo the top of the redo stack (§3.3, §4.3).
pub(crate) fn redo(
    oplog: &Oplog,
    repo: &gix::Repository,
    options: RestoreOptions,
    _perm: &mut RepoExclusive,
) -> Result<UndoOutcome, OplogError> {
    journal::refuse_if_present(oplog, repo)?;
    let chain_and_stacks = oplog.chain(repo)?;
    let (chain, stacks) = &*chain_and_stacks;
    let Some(top) = stacks.redo.last().copied() else {
        return Ok(UndoOutcome::Nothing);
    };
    let (target, target_tree) = match load_snapshot(repo, chain, top.reverse, Side::Before)? {
        Ok(loaded) => loaded,
        Err(refusal) => return Ok(UndoOutcome::Refused(refusal)),
    };
    let (expected, expected_tree) = load_expected(repo, chain, top.expected)?;
    let request = Request {
        kind: RestoreKind::Redo,
        reversing: top.reverse,
        title: title_of(chain, top.label),
        target,
        target_tree,
        expected,
        expected_tree,
        markers_since: Vec::new(),
        ack: options.confirmation,
    };
    run(oplog, repo, chain, request)
}

/// Restore the before- or after-state of any entry with a snapshot (§3.3).
pub(crate) fn restore_to(
    oplog: &Oplog,
    repo: &gix::Repository,
    target: RestoreTarget,
    options: RestoreOptions,
    _perm: &mut RepoExclusive,
) -> Result<UndoOutcome, OplogError> {
    journal::refuse_if_present(oplog, repo)?;
    let chain_and_stacks = oplog.chain(repo)?;
    let chain = &chain_and_stacks.0;
    let (state, target_tree) = match load_snapshot(repo, chain, target.entry, target.side)? {
        Ok(loaded) => loaded,
        Err(refusal) => return Ok(UndoOutcome::Refused(refusal)),
    };
    let request = Request {
        kind: RestoreKind::RestoreTo(target.side),
        reversing: target.entry,
        title: title_of(chain, target.entry),
        target: state,
        target_tree,
        expected: Expected::Unknown,
        expected_tree: None,
        markers_since: Vec::new(),
        ack: options.confirmation,
    };
    run(oplog, repo, chain, request)
}

/// Finish or roll back an interrupted restore (§3.6).
pub(crate) fn recover(
    oplog: &Oplog,
    repo: &gix::Repository,
    how: Recovery,
    _perm: &mut RepoExclusive,
) -> Result<(), OplogError> {
    recovery::recover(oplog, repo, how)
}

/// The interrupted restore described by the journal, if one exists (§3.6).
pub(crate) fn interrupted_restore(
    oplog: &Oplog,
    repo: &gix::Repository,
) -> Result<Option<InterruptedRestore>, OplogError> {
    journal::interrupted(oplog, repo)
}

/// Read the `side` snapshot of entry `id`, with its tree id; a refusal if it cannot be restored.
fn load_snapshot(
    repo: &gix::Repository,
    chain: &Chain,
    id: EntryId,
    side: Side,
) -> Result<Result<(CapturedState, gix::ObjectId), Refusal>, OplogError> {
    let Some(entry) = chain.entry(id.0) else {
        return Ok(Err(Refusal::EntryNotFound(id)));
    };
    if entry.meta.version > FORMAT_VERSION {
        return Ok(Err(Refusal::UnsupportedEntryVersion(id)));
    }
    let tree = match side {
        Side::Before => entry.before,
        Side::After => entry.after,
    };
    let Some(tree) = tree else {
        return Ok(Err(Refusal::EntryHasNoSnapshot(id)));
    };
    let state = CapturedState::read_tree(repo, tree).map_err(|err| OplogError::Corrupt {
        commit: entry.commit,
        reason: format!("{err:#}"),
    })?;
    Ok(Ok((state, tree)))
}

/// The after-state of entry `id`, or `Unknown` if it was not captured (or the entry is gone).
fn load_expected(
    repo: &gix::Repository,
    chain: &Chain,
    id: EntryId,
) -> Result<(Expected, Option<gix::ObjectId>), OplogError> {
    let Some(entry) = chain.entry(id.0) else {
        return Ok((Expected::Unknown, None));
    };
    match entry.after {
        Some(tree) if entry.meta.after_captured => {
            let state =
                CapturedState::read_tree(repo, tree).map_err(|err| OplogError::Corrupt {
                    commit: entry.commit,
                    reason: format!("{err:#}"),
                })?;
            Ok((Expected::State(Box::new(state)), Some(tree)))
        }
        _ => Ok((Expected::Unknown, None)),
    }
}

/// The title of entry `id` (empty if it is gone).
fn title_of(chain: &Chain, id: EntryId) -> String {
    chain
        .entry(id.0)
        .map(|e| e.meta.title.clone())
        .unwrap_or_default()
}

/// The network markers recorded after entry `id`.
fn markers_after(chain: &Chain, id: EntryId) -> Vec<(EntryId, NetworkMarker)> {
    chain
        .entries
        .iter()
        .filter(|e| e.meta.seq > id.0 && matches!(e.meta.role, EntryRole::Marker))
        .filter_map(|e| e.meta.network.clone().map(|n| (e.meta.id(), n)))
        .collect()
}

/// Phases A-C for one request.
fn run(
    oplog: &Oplog,
    repo: &gix::Repository,
    chain: &Chain,
    request: Request,
) -> Result<UndoOutcome, OplogError> {
    let preflight = match preflight::check(oplog, repo, &request, chain)? {
        Checked::Ready(preflight) => *preflight,
        Checked::Refused(refusal) => return Ok(UndoOutcome::Refused(refusal)),
    };
    // A7: no expensive capture before the user agreed.
    if !acknowledged(request.ack.as_ref(), &preflight.reasons) {
        return Ok(UndoOutcome::NeedsConfirmation(Confirmation::new(
            preflight.reasons,
        )));
    }
    let plan = match plan::plan(oplog, repo, &request, &preflight)? {
        Planned::Ready(plan) => *plan,
        Planned::NeedsConfirmation(reasons) => {
            return Ok(UndoOutcome::NeedsConfirmation(Confirmation::new(reasons)));
        }
    };
    execute(oplog, repo, &request, preflight, plan)
}

/// `Err(RepositoryLocked)` if a Git lock file that the restore would need exists: `HEAD.lock` of this worktree,
/// and `index.lock` when the restore writes the index.
fn refuse_leftover_locks(repo: &gix::Repository, writes_index: bool) -> Result<(), OplogError> {
    let mut locks = vec![repo.git_dir().join("HEAD.lock")];
    if writes_index {
        locks.push(repo.index_path().with_extension("lock"));
    }
    match locks.into_iter().find(|lock| lock.is_file()) {
        Some(path) => Err(OplogError::RepositoryLocked { path }),
        None => Ok(()),
    }
}

/// The in-progress state that the restore ends with `git <op> --quit` (C9): the expected state's, if the target
/// has none.
fn quit_kind(expected: &Expected, target: &CapturedState) -> Option<InProgress> {
    match expected {
        Expected::State(expected) if target.in_progress.is_none() => expected.in_progress,
        _ => None,
    }
}

fn hex(id: gix::ObjectId) -> String {
    id.to_hex().to_string()
}

/// Log `err` and return `InterruptedRestore`: the journal stays, the GUI offers to finish or roll back.
fn interrupted(err: OplogError) -> OplogError {
    tracing::error!(%err, "restore interrupted after the worktree was written; the journal is kept for recovery");
    OplogError::InterruptedRestore
}

/// Phase C.
fn execute(
    oplog: &Oplog,
    repo: &gix::Repository,
    req: &Request,
    preflight: Preflight,
    plan: Plan,
) -> Result<UndoOutcome, OplogError> {
    refuse_leftover_locks(repo, plan.files.is_some())?;
    for (id, data) in &preflight.recreate_tags {
        let written = gix::prelude::Write::write_buf(repo, gix::object::Kind::Tag, data)
            .map_err(anyhow::Error::from)?;
        if written != *id {
            return Err(OplogError::Other(anyhow::anyhow!(
                "the kept tag object {id} was re-created as {written}"
            )));
        }
    }
    let pre_tree = plan.pre.write_tree(repo)?;
    let mut journal = Journal {
        version: JOURNAL_VERSION,
        phase: Phase::Worktree,
        kind: req.kind.operation_kind(),
        side: req.kind.side(),
        reversing: req.reversing.0,
        title: req.title.clone(),
        started_at_ms: storage::now_ms(),
        target_tree: hex(req.target_tree),
        pre_restore_tree: hex(pre_tree),
        expected_tree: req.expected_tree.map(hex),
        quit: quit_kind(&req.expected, &req.target),
        new_index: plan.files.as_ref().map(|f| hex(f.new_index.tree)),
        new_index_conflicts: plan
            .files
            .as_ref()
            .and_then(|f| f.new_index.conflicts.map(hex)),
        base: plan.files.as_ref().map(|f| hex(f.base)),
        untracked: plan.untracked.clone(),
    };
    // C1
    journal::write(oplog, repo, &journal)?;
    let backup = journal::index_backup_path(oplog, repo);

    // C2, C3
    let mut checkout_ran = false;
    if let (Some(files), Some(target_files), Some(pre_files)) =
        (&plan.files, &req.target.files, &plan.pre.files)
        && !files.skip_checkout
    {
        let attempt = (|| -> Result<Option<UndoOutcome>, OplogError> {
            if index::has_conflicts(repo)? {
                index::make_conflict_free(repo, &backup, false, pre_files.worktree)?;
            }
            let mut base = files.base;
            let mut forced = files.forced;
            loop {
                match worktree::checkout_guarded(
                    repo,
                    target_files.worktree,
                    base,
                    oplog.options().untracked_file_limit,
                )? {
                    Checkout::Done => return Ok(None),
                    Checkout::InTheWay(paths) if forced => {
                        // Only an untracked file that the snapshots don't hold can be in the way now.
                        return Ok(Some(UndoOutcome::Refused(Refusal::UntrackedInTheWay(
                            paths,
                        ))));
                    }
                    Checkout::InTheWay(paths) => {
                        let reason = ConfirmReason::WorktreeChangesConflict(paths);
                        if req.ack.as_ref().is_some_and(|t| t.acknowledges(&reason)) {
                            base = worktree::force_base(repo, &plan.pre, &req.target)?;
                            forced = true;
                            if base == target_files.worktree {
                                return Ok(None);
                            }
                            continue;
                        }
                        let mut reasons = plan.reasons.clone();
                        reasons.push(reason);
                        return Ok(Some(UndoOutcome::NeedsConfirmation(Confirmation::new(
                            reasons,
                        ))));
                    }
                }
            }
        })();
        let stop = match attempt {
            Ok(None) => None,
            Ok(Some(outcome)) => Some(Ok(outcome)),
            Err(err) => Some(Err(err)),
        };
        if let Some(result) = stop {
            // Nothing was written to the worktree: undo C2 and forget the restore.
            index::restore_backup(repo, &backup)?;
            journal::remove(oplog, repo)?;
            return result;
        }
        checkout_ran = true;
    }

    // ── commit point: from here a failure rolls back (C5) or leaves the journal for recovery ──
    let before_refs = (|| -> Result<(), OplogError> {
        fail_point::hit(repo, FailPoint::AfterWorktree)?;
        // C4
        journal.phase = Phase::Refs;
        journal::write(oplog, repo, &journal)?;
        fail_point::hit(repo, FailPoint::BeforeRefs)
    })();
    before_refs.map_err(interrupted)?;

    // C5
    let message = format!("gitcthulhu: {} {}", req.kind.verb(), req.title);
    if let Err(err) = refs::apply(repo, &preflight.cur, &req.target, &message) {
        return Err(roll_back_after_refs_failed(
            oplog,
            repo,
            req,
            &plan,
            checkout_ran,
            err,
        ));
    }

    let finish = Finish {
        oplog,
        repo,
        kind: req.kind,
        reversing: req.reversing,
        title: &req.title,
        target: &req.target,
        pre: &plan.pre,
        cur: &preflight.cur,
        new_index: plan.files.as_ref().map(|f| f.new_index),
        untracked: &plan.untracked,
        quit: journal.quit,
        started_at_ms: journal.started_at_ms,
    };
    let report = finish.run(&mut journal).map_err(interrupted)?;
    Ok(UndoOutcome::Done(report))
}

/// The ref transaction failed: put the worktree and index back to P and forget the restore. If that fails too,
/// the journal stays and `InterruptedRestore` is returned (§3.3 C5).
fn roll_back_after_refs_failed(
    oplog: &Oplog,
    repo: &gix::Repository,
    req: &Request,
    plan: &Plan,
    checkout_ran: bool,
    err: OplogError,
) -> OplogError {
    let result = (|| -> Result<(), OplogError> {
        if checkout_ran
            && let (Some(pre_files), Some(target_files)) = (&plan.pre.files, &req.target.files)
        {
            match worktree::checkout_guarded(
                repo,
                pre_files.worktree,
                target_files.worktree,
                oplog.options().untracked_file_limit,
            )? {
                Checkout::Done => {}
                Checkout::InTheWay(paths) => {
                    return Err(OplogError::Other(anyhow::anyhow!(
                        "rolling back the worktree is blocked by {paths:?}"
                    )));
                }
            }
            if !index::restore_backup(repo, &journal::index_backup_path(oplog, repo))? {
                index::write_restored(repo, NewIndex::of(pre_files), &pre_files.index_flags)?;
            }
        } else {
            index::restore_backup(repo, &journal::index_backup_path(oplog, repo))?;
        }
        journal::remove(oplog, repo)
    })();
    match result {
        Ok(()) => err,
        Err(rollback) => {
            tracing::error!(%err, %rollback, "rolling back a restore after its ref transaction failed did not work");
            OplogError::InterruptedRestore
        }
    }
}

/// Steps C6-C11, shared by a normal restore (after its ref transaction) and by `recover(Complete)`.
struct Finish<'a> {
    oplog: &'a Oplog,
    repo: &'a gix::Repository,
    kind: RestoreKind,
    reversing: EntryId,
    title: &'a str,
    target: &'a CapturedState,
    /// The pre-restore state P (the new entry's before-state).
    pre: &'a CapturedState,
    /// The current values guarding the stash update.
    cur: &'a CapturedState,
    new_index: Option<NewIndex>,
    untracked: &'a UntrackedCapture,
    quit: Option<InProgress>,
    started_at_ms: i64,
}

impl Finish<'_> {
    fn run(&self, journal: &mut Journal) -> Result<RestoreReport, OplogError> {
        let repo = self.repo;
        fail_point::hit(repo, FailPoint::AfterRefs)?;
        // C6
        stash::restore(repo, self.cur, self.target)?;
        // C7
        refs::restore_branch_config(repo, self.pre, self.target)?;
        // C8
        journal.phase = Phase::Index;
        journal::write(self.oplog, repo, journal)?;
        if let (Some(new_index), Some(target_files)) = (self.new_index, &self.target.files) {
            index::write_restored(repo, new_index, &target_files.index_flags)?;
            journal::remove_file(&journal::index_backup_path(self.oplog, repo))?;
        }
        fail_point::hit(repo, FailPoint::AfterIndex)?;
        // C9
        let mut warnings = Vec::new();
        if let Some(kind) = self.quit
            && repo.state().is_some()
        {
            warnings.extend(in_progress::quit(self.oplog, repo, kind));
        }
        // C10
        let limits = self.oplog.limits();
        let after =
            match capture(repo, self.target.scope, self.untracked, &limits).and_then(|mut after| {
                worktree::capture_left_out(repo, &mut after, self.untracked, &limits)?;
                Ok(after)
            }) {
                Ok(after) => Some(after),
                Err(err) => {
                    tracing::warn!(%err, "capturing the state after a restore failed");
                    warnings.push(RestoreWarning::AfterCaptureFailed {
                        message: err.to_string(),
                    });
                    None
                }
            };
        let skipped = skipped_left_alone(self.target, self.pre);
        if !skipped.is_empty() {
            warnings.push(RestoreWarning::SkippedUntrackedLeftAlone(skipped));
        }
        // C11
        let entry = self.append(after.as_ref())?;
        journal::remove(self.oplog, repo)?;
        Ok(RestoreReport {
            entry,
            reversed: self.reversing,
            title: self.title.to_owned(),
            warnings,
        })
    }

    fn append(&self, after: Option<&CapturedState>) -> Result<EntryId, OplogError> {
        let (oplog, repo) = (self.oplog, self.repo);
        let changed: Vec<String> = after
            .map(|after| {
                refs_changed(self.pre, after)
                    .into_iter()
                    .map(|name| name.as_bstr().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let after_in_progress = match after {
            Some(after) => after.in_progress,
            None => repo.state().map(Into::into),
        };
        let role = self.kind.role(self.reversing, self.pre);
        let title = self.kind.entry_title(self.title);
        let finished_at_ms = storage::now_ms();
        oplog.append_entry(
            repo,
            |_chain, _stacks, seq| EntryMeta {
                version: FORMAT_VERSION,
                seq,
                role: role.clone(),
                kind: self.kind.operation_kind(),
                title: title.clone(),
                detail: None,
                scope: self.target.scope,
                started_at_ms: self.started_at_ms,
                finished_at_ms,
                result: EntryResult::Succeeded,
                refs_changed: changed.clone(),
                before_in_progress: self.pre.in_progress,
                after_in_progress,
                after_captured: after.is_some(),
                network: None,
                untracked: self.untracked.clone(),
                worktree_path: oplog.worktree_path().to_owned(),
                app_version: oplog.options().app_version.clone(),
            },
            Some(self.pre),
            after,
        )
    }
}

/// Untracked files that neither the target nor the pre-restore snapshot holds (too large, unreadable, capture
/// disabled), so the restore left them as they were. Sorted by path.
fn skipped_left_alone(target: &CapturedState, pre: &CapturedState) -> Vec<SkippedFile> {
    let mut out: Vec<SkippedFile> = target
        .skipped_untracked
        .iter()
        .chain(&pre.skipped_untracked)
        .cloned()
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}
