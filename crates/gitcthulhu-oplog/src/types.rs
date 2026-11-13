//! Public value types of the oplog API (spec 17 §4.2).

use std::hash::{Hash as _, Hasher as _};

use bstr::BString;
use gix::refs::FullName;

/// The stable identity of an entry within one chain: its sequence number.
///
/// Commit ids are not stable (pruning rewrites commits), sequence numbers are. The first entry of a chain gets the
/// creation time in unix milliseconds, every following entry the previous number plus one.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct EntryId(pub u64);

/// What a snapshot captures (spec 17 §1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    /// HEAD, refs, stash, branch config, index and worktree.
    Full,
    /// Everything except index and worktree; no status walk. For operations that cannot touch index or worktree.
    RefsOnly,
}

/// The kind of a recorded operation. Persisted as a kebab-case string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum OperationKind {
    /// `git commit`.
    Commit,
    /// `git commit --amend`.
    Amend,
    /// `git revert`.
    Revert,
    /// `git cherry-pick`.
    CherryPick,
    /// `git merge`.
    Merge,
    /// `git rebase`.
    Rebase,
    /// `git rebase -i`.
    RebaseInteractive,
    /// `git reset --soft`.
    ResetSoft,
    /// `git reset --mixed`.
    ResetMixed,
    /// `git reset --hard` (also `--keep`).
    ResetHard,
    /// `git pull`.
    Pull,
    /// Check out a local branch.
    Checkout,
    /// Check out a commit (detached HEAD).
    CheckoutCommit,
    /// Check out a remote branch, creating a tracking branch.
    CheckoutRemoteBranch,
    /// Create a branch without checking it out.
    CreateBranch,
    /// Create a branch and check it out.
    CreateBranchAndCheckout,
    /// Delete a local branch.
    DeleteBranch,
    /// Rename a local branch.
    RenameBranch,
    /// Create a tag.
    CreateTag,
    /// Delete a tag.
    DeleteTag,
    /// `git stash push`.
    StashPush,
    /// `git stash apply`.
    StashApply,
    /// `git stash pop`.
    StashPop,
    /// `git stash drop`.
    StashDrop,
    /// `git stash clear`.
    StashClear,
    /// Discard worktree changes (file, hunk, line, all).
    Discard,
    /// Delete untracked files.
    DeleteUntracked,
    /// Resolve conflicts (ours, theirs, mark resolved, delete).
    ResolveConflicts,
    /// `--continue` of an in-progress operation.
    ContinueOperation,
    /// `--skip` of an in-progress operation.
    SkipOperation,
    /// `--abort` of an in-progress operation.
    AbortOperation,
    /// Network marker: push.
    Push,
    /// Network marker: push tags.
    PushTags,
    /// Network marker: delete a remote branch.
    DeleteRemoteBranch,
    /// Network marker: delete a remote tag.
    DeleteRemoteTag,
    /// Internal: an undo.
    Undo,
    /// Internal: a redo.
    Redo,
    /// Internal: a restore to an arbitrary entry.
    RestoreTo,
    /// Internal: a barrier ("history paused").
    Barrier,
    /// Unknown persisted string (written by a newer version).
    #[serde(other)]
    Unknown,
}

impl OperationKind {
    /// The kebab-case persisted string.
    pub fn as_str(self) -> &'static str {
        use OperationKind::*;
        match self {
            Commit => "commit",
            Amend => "amend",
            Revert => "revert",
            CherryPick => "cherry-pick",
            Merge => "merge",
            Rebase => "rebase",
            RebaseInteractive => "rebase-interactive",
            ResetSoft => "reset-soft",
            ResetMixed => "reset-mixed",
            ResetHard => "reset-hard",
            Pull => "pull",
            Checkout => "checkout",
            CheckoutCommit => "checkout-commit",
            CheckoutRemoteBranch => "checkout-remote-branch",
            CreateBranch => "create-branch",
            CreateBranchAndCheckout => "create-branch-and-checkout",
            DeleteBranch => "delete-branch",
            RenameBranch => "rename-branch",
            CreateTag => "create-tag",
            DeleteTag => "delete-tag",
            StashPush => "stash-push",
            StashApply => "stash-apply",
            StashPop => "stash-pop",
            StashDrop => "stash-drop",
            StashClear => "stash-clear",
            Discard => "discard",
            DeleteUntracked => "delete-untracked",
            ResolveConflicts => "resolve-conflicts",
            ContinueOperation => "continue-operation",
            SkipOperation => "skip-operation",
            AbortOperation => "abort-operation",
            Push => "push",
            PushTags => "push-tags",
            DeleteRemoteBranch => "delete-remote-branch",
            DeleteRemoteTag => "delete-remote-tag",
            Undo => "undo",
            Redo => "redo",
            RestoreTo => "restore-to",
            Barrier => "barrier",
            Unknown => "unknown",
        }
    }

    /// `true` for the kinds that are recorded as non-undoable network markers.
    pub fn is_network_marker(self) -> bool {
        matches!(
            self,
            OperationKind::Push
                | OperationKind::PushTags
                | OperationKind::DeleteRemoteBranch
                | OperationKind::DeleteRemoteTag
        )
    }
}

/// Describes an operation the GUI is about to run, passed to [`Oplog::prepare()`](crate::Oplog::prepare()).
#[derive(Clone, Debug)]
pub struct OperationDescriptor {
    /// What kind of operation it is.
    pub kind: OperationKind,
    /// English, shown in tooltips and history, e.g. `Commit "Fix login"`, `Merge 'feature' into main`.
    pub title: String,
    /// Optional additional detail for the history UI.
    pub detail: Option<String>,
    /// What the snapshots capture.
    pub scope: Scope,
    /// Which untracked files' contents go into the before/after snapshots (§1.3). Ignored for `RefsOnly`.
    pub untracked: UntrackedCapture,
}

/// Which untracked files' contents an operation's snapshots capture (spec 17 §1.3).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UntrackedCapture {
    /// No untracked contents (all operations that can neither delete nor create untracked files).
    #[default]
    None,
    /// These worktree-relative paths (files or directories); paths that are tracked or absent are ignored.
    #[serde(serialize_with = "crate::meta::bytes::serialize_vec")]
    Paths(Vec<BString>),
    /// Every untracked, non-ignored file (`stash push -u`).
    All,
}

/// The before-state of an operation, returned by [`Oplog::prepare()`](crate::Oplog::prepare()) and consumed by
/// [`Oplog::finish()`](crate::Oplog::finish()).
#[must_use = "call Oplog::finish after the operation, also when it failed"]
pub struct PendingOperation {
    pub(crate) descriptor: OperationDescriptor,
    pub(crate) before: crate::state::CapturedState,
    pub(crate) started_at_ms: i64,
    pub(crate) git_dir: std::path::PathBuf,
}

impl PendingOperation {
    /// The descriptor the operation was prepared with.
    pub fn descriptor(&self) -> &OperationDescriptor {
        &self.descriptor
    }

    /// Requested untracked files that are NOT in the before-snapshot (too large, over budget, unreadable, capture
    /// disabled). Informational: the GUI asked the user before taking the guard (`Oplog::preview_skipped`).
    pub fn skipped_untracked(&self) -> &[SkippedFile] {
        &self.before.skipped_untracked
    }

    /// The sequencer operation that was in progress when the before-state was captured.
    pub fn before_in_progress(&self) -> Option<InProgress> {
        self.before.in_progress
    }
}

/// How the recorded operation ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationResult {
    /// The operation succeeded.
    Succeeded,
    /// The operation failed with `message`.
    Failed {
        /// The error message shown to the user.
        message: String,
    },
    /// The user cancelled the operation.
    Cancelled,
}

/// An untracked file whose contents a snapshot does not hold.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFile {
    /// Worktree-relative, slash-separated path.
    #[serde(serialize_with = "crate::meta::bytes::serialize")]
    pub path: BString,
    /// Size in bytes at capture time (0 if it vanished).
    pub size: u64,
    /// Why it was skipped.
    pub reason: SkipReason,
}

/// Why an untracked file was not captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkipReason {
    /// Larger than `untracked_file_limit`.
    TooLarge,
    /// Did not fit into `untracked_total_limit` after smaller files.
    OverBudget,
    /// Could not be opened for reading (Windows sharing violation, permissions).
    Unreadable,
    /// `OplogOptions::capture_untracked == false`.
    CaptureDisabled,
    /// A directory holding a nested Git repository (a `.git` entry): snapshots never capture one, so deleting it
    /// loses its history for good. Only reported by `Oplog::preview_skipped`.
    NestedRepository,
}

/// A sequencer operation in progress, mirroring [`gix::state::InProgress`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InProgress {
    /// `git am`.
    ApplyMailbox,
    /// `git am` within a rebase.
    ApplyMailboxRebase,
    /// `git bisect`.
    Bisect,
    /// `git cherry-pick` of one commit.
    CherryPick,
    /// `git cherry-pick` of several commits.
    CherryPickSequence,
    /// `git merge`.
    Merge,
    /// `git rebase`.
    Rebase,
    /// `git rebase -i`.
    RebaseInteractive,
    /// `git revert` of one commit.
    Revert,
    /// `git revert` of several commits.
    RevertSequence,
}

impl From<gix::state::InProgress> for InProgress {
    fn from(value: gix::state::InProgress) -> Self {
        use gix::state::InProgress as G;
        match value {
            G::ApplyMailbox => InProgress::ApplyMailbox,
            G::ApplyMailboxRebase => InProgress::ApplyMailboxRebase,
            G::Bisect => InProgress::Bisect,
            G::CherryPick => InProgress::CherryPick,
            G::CherryPickSequence => InProgress::CherryPickSequence,
            G::Merge => InProgress::Merge,
            G::Rebase => InProgress::Rebase,
            G::RebaseInteractive => InProgress::RebaseInteractive,
            G::Revert => InProgress::Revert,
            G::RevertSequence => InProgress::RevertSequence,
        }
    }
}

impl InProgress {
    /// The family used to decide whether a follow-up step continues an operation (rule R2).
    pub fn family(self) -> InProgressFamily {
        match self {
            InProgress::ApplyMailbox | InProgress::ApplyMailboxRebase => InProgressFamily::Am,
            InProgress::Bisect => InProgressFamily::Bisect,
            InProgress::CherryPick | InProgress::CherryPickSequence => InProgressFamily::CherryPick,
            InProgress::Merge => InProgressFamily::Merge,
            InProgress::Rebase | InProgress::RebaseInteractive => InProgressFamily::Rebase,
            InProgress::Revert | InProgress::RevertSequence => InProgressFamily::Revert,
        }
    }
}

/// Groups of [`InProgress`] kinds that belong to the same sequencer operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InProgressFamily {
    /// `git am`.
    Am,
    /// `git bisect`.
    Bisect,
    /// `git cherry-pick`.
    CherryPick,
    /// `git merge`.
    Merge,
    /// `git rebase`.
    Rebase,
    /// `git revert`.
    Revert,
}

/// The role of an entry in the undo/redo bookkeeping (spec 17 §4.3).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EntryRole {
    /// A user operation.
    Operation {
        /// `false` for barriers caused by unrelated in-progress states or a violated `RefsOnly` invariant.
        undoable: bool,
    },
    /// A follow-up step of the in-progress operation started by `origin` (rule R2).
    Continuation {
        /// The entry that started the in-progress operation.
        origin: EntryId,
        /// `true` if this step aborted the operation (rule R3).
        abort: bool,
    },
    /// An undo of `reverses`.
    Undo {
        /// The entry whose before-state was restored.
        reverses: EntryId,
        /// `false` if the undo ended an in-progress operation (rule R5).
        redoable: bool,
    },
    /// A redo of the undo entry `reverses`.
    Redo {
        /// The undo entry whose before-state was restored.
        reverses: EntryId,
    },
    /// A restore to a side of an arbitrary entry.
    RestoreTo {
        /// The entry whose state was restored.
        target: EntryId,
        /// Which of its states.
        side: Side,
    },
    /// A non-undoable network effect.
    Marker,
    /// History was paused; nothing before it can be undone.
    Barrier {
        /// Why the barrier was recorded.
        reason: String,
    },
}

/// The before- or after-state of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Side {
    /// The state before the operation.
    Before,
    /// The state after the operation.
    After,
}

/// How a recorded operation ended, as persisted.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryResult {
    /// The operation succeeded.
    Succeeded,
    /// The operation failed with the given message.
    Failed(String),
    /// The operation was cancelled.
    Cancelled,
}

impl From<OperationResult> for EntryResult {
    fn from(value: OperationResult) -> Self {
        match value {
            OperationResult::Succeeded => EntryResult::Succeeded,
            OperationResult::Failed { message } => EntryResult::Failed(message),
            OperationResult::Cancelled => EntryResult::Cancelled,
        }
    }
}

/// Derived by replay (§4.3), for display only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryStatus {
    /// On the undo stack; `next` = it is what Undo does now.
    Undoable {
        /// It is what Undo does now.
        next: bool,
    },
    /// Reversed by a later Undo entry; `next` = it is what Redo does now.
    Undone {
        /// It is what Redo does now.
        next: bool,
    },
    /// Was undone, and a later operation cleared the redo stack.
    Superseded,
    /// Cannot be undone (barrier, or recorded inside an unrelated in-progress operation).
    NotUndoable,
    /// Folded into the composite of `origin`.
    PartOf {
        /// The entry that started the composite.
        origin: EntryId,
    },
    /// Undo/redo/restore bookkeeping entries.
    Bookkeeping,
    /// Network marker.
    Marker,
}

/// One entry of the log, as listed by [`Oplog::list()`](crate::Oplog::list()).
#[derive(Clone, Debug)]
pub struct Entry {
    /// The stable identity of the entry.
    pub id: EntryId,
    /// The entry commit (changes when pruning rewrites the chain).
    pub commit: gix::ObjectId,
    /// Its role in the undo/redo bookkeeping.
    pub role: EntryRole,
    /// The operation kind.
    pub kind: OperationKind,
    /// The English title.
    pub title: String,
    /// Optional detail.
    pub detail: Option<String>,
    /// The snapshot scope.
    pub scope: Scope,
    /// When the operation started (unix milliseconds).
    pub started_at_ms: i64,
    /// When the entry was recorded (unix milliseconds).
    pub finished_at_ms: i64,
    /// How the operation ended.
    pub result: EntryResult,
    /// Full ref names (incl. `HEAD`) that differ between before and after.
    pub refs_changed: Vec<FullName>,
    /// The in-progress operation before.
    pub before_in_progress: Option<InProgress>,
    /// The in-progress operation after.
    pub after_in_progress: Option<InProgress>,
    /// `false` if the after-state could not be captured.
    pub after_captured: bool,
    /// Requested untracked files the snapshots do not hold.
    pub skipped_untracked: Vec<SkippedFile>,
    /// `true` if `skipped_untracked` was truncated to 100 entries.
    pub skipped_untracked_truncated: bool,
    /// The network effect of a marker entry.
    pub network: Option<NetworkMarker>,
    /// Derived status for display.
    pub status: EntryStatus,
    /// `false` for entries written by a newer format version (listed, never restored).
    pub restorable: bool,
}

/// A non-undoable network effect (push, remote deletion).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkMarker {
    /// `Push | PushTags | DeleteRemoteBranch | DeleteRemoteTag`.
    pub kind: OperationKind,
    /// E.g. "Push main to origin/main".
    pub title: String,
    /// The remote name.
    pub remote: String,
    /// The ref updates that happened on the remote.
    pub updates: Vec<PushedRef>,
}

/// One ref update of a [`NetworkMarker`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushedRef {
    /// `refs/heads/main`, `refs/tags/v1`; `None` for deletions.
    pub local: Option<FullName>,
    /// The ref name on the remote, e.g. `refs/heads/main`.
    pub remote_ref: FullName,
    /// `None` = deleted on the remote.
    pub new_target: Option<gix::ObjectId>,
    /// `true` for a forced update.
    pub forced: bool,
}

/// What Undo and Redo would do now (cheap, metadata only).
#[derive(Clone, Debug)]
pub struct UndoRedoState {
    /// The top of the undo stack.
    pub undo: Option<StackItemInfo>,
    /// The top of the redo stack.
    pub redo: Option<StackItemInfo>,
    /// Number of undoable items.
    pub undo_depth: usize,
    /// Number of redoable items.
    pub redo_depth: usize,
    /// An interrupted restore must be finished or rolled back first.
    pub interrupted_restore: bool,
}

/// Display information about the top of the undo or redo stack.
#[derive(Clone, Debug)]
pub struct StackItemInfo {
    /// The entry whose recorded states Undo/Redo use.
    pub reverses: EntryId,
    /// The user-visible operation (for a Redo entry or a composite: the original operation).
    pub operation: EntryId,
    /// The kind of the user-visible operation.
    pub kind: OperationKind,
    /// The title of the user-visible operation.
    pub title: String,
    /// When the user-visible operation was recorded.
    pub finished_at_ms: i64,
    /// Some(kind) if undoing it also ends the in-progress `kind` (`git <op> --quit`) and cannot be redone.
    pub ends_in_progress: Option<InProgress>,
}

/// Options of [`Oplog::undo()`](crate::Oplog::undo()), [`redo()`](crate::Oplog::redo()) and
/// [`restore_to()`](crate::Oplog::restore_to()).
#[derive(Clone, Debug, Default)]
pub struct RestoreOptions {
    /// The token of a previous `NeedsConfirmation` the user accepted.
    pub confirmation: Option<ConfirmToken>,
}

/// The state [`Oplog::restore_to()`](crate::Oplog::restore_to()) restores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestoreTarget {
    /// The entry.
    pub entry: EntryId,
    /// Which of its states.
    pub side: Side,
}

/// The outcome of an undo, redo or restore.
#[derive(Clone, Debug)]
pub enum UndoOutcome {
    /// The restore was performed.
    Done(RestoreReport),
    /// Nothing to undo (resp. redo).
    Nothing,
    /// Nothing was changed; show `reasons`, and call again with `RestoreOptions{confirmation: Some(token)}` to go ahead.
    NeedsConfirmation(Confirmation),
    /// Nothing was changed, and this cannot be overridden.
    Refused(Refusal),
}

/// What a performed restore did.
#[derive(Clone, Debug)]
pub struct RestoreReport {
    /// The new Undo / Redo / RestoreTo entry.
    pub entry: EntryId,
    /// The entry that was reversed (or the restore target).
    pub reversed: EntryId,
    /// The user-visible operation title, e.g. `Commit "Fix login"`.
    pub title: String,
    /// Things the user should know about.
    pub warnings: Vec<RestoreWarning>,
}

/// A non-fatal issue of a performed restore.
#[derive(Clone, Debug)]
pub enum RestoreWarning {
    /// `git <op> --quit` failed.
    QuitFailed {
        /// The in-progress operation.
        kind: InProgress,
        /// Git's error output.
        stderr: String,
    },
    /// `git merge --quit` / `git rebase --quit` (or our fallback) saved the operation's autostash as a new stash entry.
    AutostashSaved {
        /// The in-progress operation.
        kind: InProgress,
    },
    /// The state after the restore could not be captured.
    AfterCaptureFailed {
        /// The error message.
        message: String,
    },
    /// Untracked files the snapshot does not hold were left alone.
    SkippedUntrackedLeftAlone(Vec<SkippedFile>),
}

/// A request for confirmation: show `reasons`, then call again with `token`.
#[derive(Clone, Debug)]
pub struct Confirmation {
    /// Pass this in [`RestoreOptions::confirmation`] to acknowledge `reasons`.
    pub token: ConfirmToken,
    /// Why confirmation is needed.
    pub reasons: Vec<ConfirmReason>,
}

impl Confirmation {
    /// Ask for confirmation of `reasons`; the token acknowledges exactly these reasons.
    pub fn new(reasons: Vec<ConfirmReason>) -> Self {
        Confirmation {
            token: ConfirmToken::acknowledging(&reasons),
            reasons,
        }
    }
}

/// Opaque: the digests of every acknowledged reason. A call is acknowledged iff each of its current reasons'
/// digest is contained (so a later, *different* divergence asks again).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmToken {
    digests: Vec<u64>,
}

impl ConfirmToken {
    /// A token that acknowledges all of `reasons`.
    pub(crate) fn acknowledging(reasons: &[ConfirmReason]) -> Self {
        let mut digests: Vec<u64> = reasons.iter().map(ConfirmReason::digest).collect();
        digests.sort_unstable();
        digests.dedup();
        ConfirmToken { digests }
    }

    /// `true` if `reason` was acknowledged by this token.
    pub(crate) fn acknowledges(&self, reason: &ConfirmReason) -> bool {
        self.digests.binary_search(&reason.digest()).is_ok()
    }

    /// `true` if every one of `reasons` was acknowledged by this token.
    pub(crate) fn acknowledges_all(&self, reasons: &[ConfirmReason]) -> bool {
        reasons.iter().all(|r| self.acknowledges(r))
    }
}

/// Why an undo, redo or restore needs the user's confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmReason {
    /// Refs moved since the operation (outside the app).
    RefsMovedSince(Vec<RefDivergence>),
    /// Staging done since the operation conflicts with the restore.
    StagedChangesConflict(Vec<BString>),
    /// Worktree edits done since the operation conflict with the restore.
    WorktreeChangesConflict(Vec<BString>),
    /// Refs the operation changed were pushed since.
    PushedSince(Vec<PushedSinceRef>),
    /// The after-state of the operation is unknown (its capture failed).
    AfterStateUnknown,
    /// A restore to an arbitrary entry; lists the ref changes it makes.
    ArbitraryRestore {
        /// The ref changes the restore makes.
        ref_changes: Vec<RefDivergence>,
    },
}

impl ConfirmReason {
    /// A digest of the full content of this reason. Stable within one process, which is all a token needs.
    pub(crate) fn digest(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{self:?}").hash(&mut hasher);
        hasher.finish()
    }
}

/// A ref whose current value differs from the expected one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefDivergence {
    /// Full ref name, or `HEAD`.
    pub name: FullName,
    /// The expected value, `None` = should not exist.
    pub expected: Option<RefValueInfo>,
    /// The actual value, `None` = does not exist.
    pub actual: Option<RefValueInfo>,
}

/// The value of a ref or of `HEAD`, for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefValueInfo {
    /// Points to an object.
    Object(gix::ObjectId),
    /// Points to another ref.
    Symbolic(FullName),
    /// `HEAD` points to a branch that does not exist yet.
    Unborn(FullName),
}

/// A ref the undone operation changed that was pushed since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushedSinceRef {
    /// The marker entry of the push.
    pub marker: EntryId,
    /// The remote name.
    pub remote: String,
    /// The local ref.
    pub local: FullName,
    /// The ref on the remote.
    pub remote_ref: FullName,
}

/// Why an undo, redo or restore was refused (cannot be overridden).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A sequencer operation is in progress.
    OperationInProgress {
        /// The in-progress operation.
        kind: InProgress,
    },
    /// The in-progress state differs from the one the undone item left.
    InProgressMismatch {
        /// The state the undone item left.
        expected: Option<InProgress>,
        /// The current state.
        actual: Option<InProgress>,
    },
    /// The restore would move a branch checked out in another worktree.
    BranchCheckedOutElsewhere {
        /// The branch.
        branch: FullName,
        /// The worktrees that have it checked out.
        worktrees: Vec<std::path::PathBuf>,
    },
    /// Objects the target state needs are gone.
    MissingObjects(Vec<gix::ObjectId>),
    /// Untracked files that the snapshot does not contain (not requested, too large, unreadable) sit where the target
    /// writes tracked files. The user must move them away; nothing was changed.
    UntrackedInTheWay(Vec<BString>),
    /// No entry with this id exists.
    EntryNotFound(EntryId),
    /// The entry has no snapshot (marker or barrier).
    EntryHasNoSnapshot(EntryId),
    /// The entry was written by a newer format version.
    UnsupportedEntryVersion(EntryId),
}

/// A page request of [`Oplog::list()`](crate::Oplog::list()).
#[derive(Clone, Copy, Debug)]
pub struct PageRequest {
    /// Exclusive cursor: only entries older than this one. `None` = start with the newest.
    pub before: Option<EntryId>,
    /// Maximum number of entries, clamped to `1..=200`.
    pub limit: usize,
    /// Include Undo/Redo bookkeeping entries.
    pub include_bookkeeping: bool,
}

/// A page of entries, newest first.
#[derive(Clone, Debug)]
pub struct Page {
    /// The entries, newest first.
    pub entries: Vec<Entry>,
    /// The cursor for the next page, if there are more entries.
    pub next: Option<EntryId>,
}

/// An undo/redo that was interrupted and must be finished or rolled back.
#[derive(Clone, Debug)]
pub struct InterruptedRestore {
    /// When it started (unix milliseconds).
    pub started_at_ms: i64,
    /// The title of the restored operation.
    pub title: String,
    /// `Undo`, `Redo` or `RestoreTo`.
    pub kind: OperationKind,
    /// The entry that was being reversed.
    pub reversing: Option<EntryId>,
}

/// How to resolve an interrupted restore.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Finish the restore.
    Complete,
    /// Put back the pre-restore state.
    RollBack,
}

/// What a prune did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PruneReport {
    /// Number of removed entries.
    pub removed: usize,
    /// Number of kept entries.
    pub kept: usize,
}
