//! Recording: `prepare`/`finish` around an operation, markers and barriers (spec 17 §1.2 notes, §4.1, §4.3).

use but_core::sync::RepoExclusive;

use crate::{
    Oplog, OplogError,
    capture::capture,
    meta::{EntryMeta, FORMAT_VERSION},
    restore,
    stacks::classify,
    state::refs_changed,
    storage::now_ms,
    types::{
        EntryId, EntryResult, EntryRole, NetworkMarker, OperationDescriptor, OperationKind,
        OperationResult, PendingOperation, Scope, UntrackedCapture,
    },
};

/// The `UntrackedCapture` a descriptor effectively uses: `RefsOnly` captures no files at all.
fn effective_untracked(op: &OperationDescriptor) -> UntrackedCapture {
    match op.scope {
        Scope::Full => op.untracked.clone(),
        Scope::RefsOnly => UntrackedCapture::None,
    }
}

/// See [`Oplog::prepare()`].
pub(crate) fn prepare(
    oplog: &Oplog,
    repo: &gix::Repository,
    op: OperationDescriptor,
    _perm: &mut RepoExclusive,
) -> Result<Option<PendingOperation>, OplogError> {
    oplog.check_repo(repo)?;
    if restore::interrupted_restore(oplog, repo)?.is_some() {
        return Err(OplogError::InterruptedRestore);
    }
    if !oplog.options().enabled {
        return Ok(None);
    }
    let started_at_ms = now_ms();
    let before = capture(repo, op.scope, &effective_untracked(&op), &oplog.limits())?;
    Ok(Some(PendingOperation {
        descriptor: op,
        before,
        started_at_ms,
        git_dir: repo.git_dir().to_owned(),
    }))
}

/// See [`Oplog::finish()`].
pub(crate) fn finish(
    oplog: &Oplog,
    repo: &gix::Repository,
    pending: PendingOperation,
    result: OperationResult,
    _perm: &mut RepoExclusive,
) -> Result<Option<EntryId>, OplogError> {
    oplog.check_repo(repo)?;
    if !oplog.is_own_git_dir(&pending.git_dir) {
        return Err(OplogError::WrongRepository);
    }
    let PendingOperation {
        descriptor: op,
        before,
        started_at_ms,
        git_dir: _,
    } = pending;
    let after = match capture(repo, op.scope, &effective_untracked(&op), &oplog.limits()) {
        Ok(after) => Some(after),
        Err(err) => {
            tracing::warn!(?err, title = %op.title, "after-state capture failed; recording the entry without it");
            None
        }
    };
    if after.as_ref().is_some_and(|after| before.equivalent(after)) {
        // A no-op, or a failure without effect: nothing to undo.
        return Ok(None);
    }
    let refs_only_head_moved = op.scope == Scope::RefsOnly
        && after
            .as_ref()
            .is_some_and(|after| before.head_commit(repo) != after.head_commit(repo));
    if refs_only_head_moved {
        tracing::error!(
            title = %op.title,
            kind = op.kind.as_str(),
            "a RefsOnly operation moved HEAD to another commit; recording it as not undoable"
        );
    }
    let changed: Vec<String> = after
        .as_ref()
        .map(|after| {
            refs_changed(&before, after)
                .into_iter()
                .map(|name| name.as_bstr().to_string())
                .collect()
        })
        .unwrap_or_default();
    let after_in_progress = match &after {
        Some(after) => after.in_progress,
        None => repo.state().map(Into::into),
    };
    let finished_at_ms = now_ms();
    let entry = oplog.append_entry(
        repo,
        |chain, stacks, seq| {
            let top = stacks.undo.last().map(|top| {
                let expected_after = chain
                    .entry(top.expected.0)
                    .and_then(|e| e.meta.after_in_progress);
                (*top, expected_after)
            });
            let role = classify(op.kind, refs_only_head_moved, before.in_progress, top);
            EntryMeta {
                version: FORMAT_VERSION,
                seq,
                role,
                kind: op.kind,
                title: op.title.clone(),
                detail: op.detail.clone(),
                scope: op.scope,
                started_at_ms,
                finished_at_ms,
                result: EntryResult::from(result.clone()),
                refs_changed: changed.clone(),
                before_in_progress: before.in_progress,
                after_in_progress,
                after_captured: after.is_some(),
                network: None,
                untracked: op.untracked.clone(),
                worktree_path: oplog.worktree_path().to_owned(),
                app_version: oplog.options().app_version.clone(),
            }
        },
        Some(&before),
        after.as_ref(),
    )?;
    Ok(Some(entry))
}

/// See [`Oplog::record_marker()`].
pub(crate) fn record_marker(
    oplog: &Oplog,
    repo: &gix::Repository,
    marker: NetworkMarker,
    _perm: &mut RepoExclusive,
) -> Result<EntryId, OplogError> {
    oplog.check_repo(repo)?;
    let kind = marker.kind;
    let title = marker.title.clone();
    record_without_snapshots(oplog, repo, kind, title, EntryRole::Marker, Some(marker))
}

/// See [`Oplog::record_barrier()`].
pub(crate) fn record_barrier(
    oplog: &Oplog,
    repo: &gix::Repository,
    kind: OperationKind,
    title: String,
    reason: String,
    _perm: &mut RepoExclusive,
) -> Result<EntryId, OplogError> {
    oplog.check_repo(repo)?;
    record_without_snapshots(
        oplog,
        repo,
        kind,
        title,
        EntryRole::Barrier { reason },
        None,
    )
}

/// Append an entry without snapshots (marker or barrier).
fn record_without_snapshots(
    oplog: &Oplog,
    repo: &gix::Repository,
    kind: OperationKind,
    title: String,
    role: EntryRole,
    network: Option<NetworkMarker>,
) -> Result<EntryId, OplogError> {
    let in_progress = repo.state().map(Into::into);
    let now = now_ms();
    oplog.append_entry(
        repo,
        |_chain, _stacks, seq| EntryMeta {
            version: FORMAT_VERSION,
            seq,
            role: role.clone(),
            kind,
            title: title.clone(),
            detail: None,
            scope: Scope::RefsOnly,
            started_at_ms: now,
            finished_at_ms: now,
            result: EntryResult::Succeeded,
            refs_changed: Vec::new(),
            before_in_progress: in_progress,
            after_in_progress: in_progress,
            after_captured: false,
            network: network.clone(),
            untracked: UntrackedCapture::None,
            worktree_path: oplog.worktree_path().to_owned(),
            app_version: oplog.options().app_version.clone(),
        },
        None,
        None,
    )
}
