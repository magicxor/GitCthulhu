//! Every public data type of the backend.
//!
//! All types live in this one file so that every module and the GUI compile against them from day one.
//! `Serialize`/`Deserialize` exist only behind the cargo feature `serde`.

use std::{
    collections::HashMap,
    ops::Range,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, atomic::Ordering},
    task::{Context, Poll, Waker},
    time::Duration,
};

use bstr::BString;
use gix::{ObjectId, refs::FullName};
use smallvec::SmallVec;

use crate::{CredentialPrompter, GitError, NoPrompter, PartialBlock};

// ───────────────────────────── basics ─────────────────────────────

/// '/'-separated, worktree-relative path as raw bytes (may be non-UTF-8).
pub type RelaPath = BString;

/// A Git version as reported by `git version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    /// Major version.
    pub major: u16,
    /// Minor version.
    pub minor: u16,
    /// Patch version (0 if the version has only two components).
    pub patch: u16,
}

impl GitVersion {
    /// The oldest Git this crate supports.
    pub const MIN: GitVersion = GitVersion {
        major: 2,
        minor: 34,
        patch: 1,
    };

    /// Parse the output of `git version`, e.g. `git version 2.43.0.windows.1` or `git version 2.34.1`.
    ///
    /// Only the first three numeric components count; platform suffixes are ignored. `None` if the text carries no
    /// `<major>.<minor>` version.
    pub fn parse(git_version_output: &str) -> Option<GitVersion> {
        let text = git_version_output.trim();
        let text = text
            .strip_prefix("git version")
            .unwrap_or(text)
            .trim_start();
        let version_token = text.split_whitespace().next()?;
        let mut numbers = version_token.split('.');
        let major = numbers.next()?.parse().ok()?;
        let minor = numbers.next()?.parse().ok()?;
        // A non-numeric third component ("2.43.windows.1" does not exist in practice, but "2.43.0-rc1" does).
        let patch = numbers
            .next()
            .map(|p| {
                let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .unwrap_or(Some(0))
            .unwrap_or(0);
        Some(GitVersion {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for GitVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A name and an email address, as raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Person {
    /// The name.
    pub name: BString,
    /// The email address.
    pub email: BString,
}

/// A point in time with the author's UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitTime {
    /// Seconds since the Unix epoch.
    pub seconds: i64,
    /// UTC offset in minutes.
    pub offset_minutes: i32,
}

/// Who did something, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// Interned per walker/page.
    pub person: Arc<Person>,
    /// When it happened.
    pub time: GitTime,
}

/// A complete identity (`user.name` and `user.email`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `user.name`.
    pub name: String,
    /// `user.email`.
    pub email: String,
}

/// An identity of which parts may be missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartialIdentity {
    /// `user.name`, if set.
    pub name: Option<String>,
    /// `user.email`, if set.
    pub email: Option<String>,
}

/// The configured identities and the one `git commit` would use.
#[derive(Debug, Clone)]
pub struct IdentityInfo {
    /// `~/.gitconfig` + XDG (Source::User/Git).
    pub global: PartialIdentity,
    /// `.git/config` (+ worktree config).
    pub local: PartialIdentity,
    /// What `git commit` would use: env `GIT_AUTHOR_*` > local > global > system; `None` if incomplete.
    pub effective: Option<Identity>,
}

/// The kind of a tree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A regular file.
    Blob,
    /// An executable file.
    BlobExecutable,
    /// A symbolic link.
    Link,
    /// A submodule (gitlink).
    Submodule,
    /// A directory.
    Tree,
}

impl From<gix::object::tree::EntryKind> for EntryKind {
    fn from(kind: gix::object::tree::EntryKind) -> Self {
        use gix::object::tree::EntryKind as K;
        match kind {
            K::Tree => EntryKind::Tree,
            K::Blob => EntryKind::Blob,
            K::BlobExecutable => EntryKind::BlobExecutable,
            K::Link => EntryKind::Link,
            K::Commit => EntryKind::Submodule,
        }
    }
}

/// One side of a change: an object and what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    /// May be null: worktree content that was not hashed.
    pub id: ObjectId,
    /// What kind of entry it is.
    pub kind: EntryKind,
}

/// Which configuration file a setting belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// The repository's `.git/config`.
    Local,
    /// The user's global configuration.
    Global,
    /// The per-worktree configuration.
    Worktree,
}

// ───────────────────────────── refresh ─────────────────────────────

bitflags::bitflags! {
    /// What the GUI must reload. Model of Gitnuro's DataToRefresh (05 §1.2) plus WORKTREES and UNDO.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct RefreshSet: u16 {
        /// Status lists, uncommitted-changes row, diff of an uncommitted file.
        const STATUS     = 1 << 0;
        /// Log walk and graph.
        const LOG        = 1 << 1;
        /// Local branches, HEAD/current branch, upstream + ahead/behind.
        const BRANCHES   = 1 << 2;
        /// Remotes and remote-tracking branches.
        const REMOTES    = 1 << 3;
        /// Tags.
        const TAGS       = 1 << 4;
        /// Stashes.
        const STASHES    = 1 << 5;
        /// Submodules.
        const SUBMODULES = 1 << 6;
        /// Linked worktrees.
        const WORKTREES  = 1 << 7;
        /// Merging/rebasing/... banner, rebase todo, persisted MERGE_MSG.
        const REPO_STATE = 1 << 8;
        /// Identity, signing config; implies `Repo::reload()`.
        const CONFIG     = 1 << 9;
        /// Oplog undo/redo state (`refs/gitcthulhu/*` changed).
        const UNDO       = 1 << 10;
        /// Everything a history-rewriting operation can touch.
        const OPERATION_STATE = Self::STATUS.bits() | Self::LOG.bits() | Self::BRANCHES.bits() | Self::REPO_STATE.bits()
                              | Self::STASHES.bits() | Self::TAGS.bits() | Self::SUBMODULES.bits() | Self::WORKTREES.bits();
        /// Every category.
        const ALL = 0x07FF;
    }
}

// ───────────────────────────── cancel / progress / op context ─────────────────────────────

#[derive(Default)]
struct CancelInner {
    flag: Arc<AtomicBool>,
    waiters: parking_lot::Mutex<Vec<Waker>>,
}

/// A cloneable cancellation flag with a wake-up notifier.
///
/// `Clone + Send + Sync`: every clone observes the same state.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<CancelInner>);

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CancelToken {
    /// A fresh token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel. Idempotent; wakes everybody waiting in [`Self::cancelled()`].
    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
        let waiters = std::mem::take(&mut *self.0.waiters.lock());
        for waker in waiters {
            waker.wake();
        }
    }

    /// Whether [`Self::cancel()`] was called on this token or any of its clones.
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
    }

    /// The shared flag, for gix' `should_interrupt_owned`.
    pub fn as_atomic(&self) -> Arc<AtomicBool> {
        self.0.flag.clone()
    }

    /// `Err(GitError::Cancelled)` if cancelled.
    pub fn check(&self) -> Result<(), GitError> {
        if self.is_cancelled() {
            Err(GitError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// A future that completes when cancelled (used internally by cli.rs; not part of the stable API surface).
    #[doc(hidden)]
    pub fn cancelled(&self) -> impl std::future::Future<Output = ()> + Send + '_ {
        Cancelled {
            token: self,
            registered: None,
        }
    }
}

/// The future of [`CancelToken::cancelled()`]. It registers its waker with the token and removes it again when
/// dropped, so a long-lived token that outlives many commands does not accumulate wakers.
struct Cancelled<'a> {
    token: &'a CancelToken,
    registered: Option<Waker>,
}

impl std::future::Future for Cancelled<'_> {
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let token = self.token;
        if token.is_cancelled() {
            return Poll::Ready(());
        }
        let mut waiters = token.0.waiters.lock();
        // Re-check under the lock: `cancel()` sets the flag first and then drains the waiters.
        if token.is_cancelled() {
            return Poll::Ready(());
        }
        let current = cx.waker();
        if self
            .registered
            .as_ref()
            .is_none_or(|registered| !registered.will_wake(current))
        {
            if let Some(stale) = self.registered.take() {
                remove_one(&mut waiters, &stale);
            }
            waiters.push(current.clone());
            self.registered = Some(current.clone());
        }
        Poll::Pending
    }
}

impl Drop for Cancelled<'_> {
    fn drop(&mut self) {
        if let Some(registered) = self.registered.take() {
            remove_one(&mut self.token.0.waiters.lock(), &registered);
        }
    }
}

/// Remove one entry for `waker` (several futures polled by the same task register the same waker once each).
fn remove_one(waiters: &mut Vec<Waker>, waker: &Waker) {
    if let Some(idx) = waiters.iter().position(|w| w.will_wake(waker)) {
        waiters.swap_remove(idx);
    }
}

/// A progress event of a running operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    /// A phase parsed from Git's `--progress` stderr output.
    Phase {
        /// E.g. "Receiving objects".
        phase: String,
        /// The `remote:` prefix was present.
        remote: bool,
        /// Current count.
        current: u64,
        /// Total count, if known.
        total: Option<u64>,
        /// Percentage, if known.
        percent: Option<u8>,
        /// E.g. "1.20 MiB | 2.00 MiB/s".
        throughput: Option<String>,
        /// The phase finished (`, done.`).
        done: bool,
    },
    /// Any other stderr line (hook output, "Already up to date.").
    Message(String),
    /// The sanitised command line, emitted once per spawned command.
    Command(String),
}

/// Receives [`Progress`] events. Called on an executor worker thread: must not block.
pub trait ProgressSink: Send + Sync {
    /// Handle one event.
    fn progress(&self, event: Progress);
}

/// A [`ProgressSink`] that drops everything.
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn progress(&self, _: Progress) {}
}

impl<F: Fn(Progress) + Send + Sync> ProgressSink for F {
    fn progress(&self, event: Progress) {
        self(event)
    }
}

/// Per-call context of a mutating or network operation. Owned and cheap to clone: the executor moves `Arc` clones of
/// the prompter and the progress sink into tasks on its runtime (which require `'static`).
#[derive(Clone)]
pub struct OpContext {
    /// Cancels the operation.
    pub cancel: CancelToken,
    /// Receives progress events.
    pub progress: Arc<dyn ProgressSink>,
    /// `Arc::new(NoPrompter)` => prompts are answered with Cancel.
    pub prompter: Arc<dyn CredentialPrompter>,
    /// Shown in prompt dialogs ("Push to origin", "Fetch all").
    pub label: String,
}

impl OpContext {
    /// `NoProgress`, `NoPrompter`, label "" (tests, background maintenance).
    pub fn silent(cancel: CancelToken) -> OpContext {
        OpContext {
            cancel,
            progress: Arc::new(NoProgress),
            prompter: Arc::new(NoPrompter),
            label: String::new(),
        }
    }
}

/// Result of every mutating operation.
#[derive(Debug, Clone)]
pub struct OpOutcome<T> {
    /// The operation's own result.
    pub value: T,
    /// What the GUI must reload.
    pub refresh: RefreshSet,
    /// Which commands and hooks ran.
    pub log: CommandLog,
    /// Things the user should see although the operation succeeded.
    pub warnings: Vec<OpWarning>,
}

/// Something the user should see although the operation succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpWarning {
    /// A post hook (post-checkout, post-merge, post-commit, post-rewrite, ...) exited non-zero after the operation
    /// completed (§1.11). `output` = stderr + stdout of the git command.
    PostHookFailed {
        /// The hook's name.
        hook: String,
        /// Its exit code, `None` if killed by a signal.
        exit_code: Option<i32>,
        /// stderr + stdout of the git command.
        output: String,
    },
}

/// What happened while an operation ran.
#[derive(Debug, Clone, Default)]
pub struct CommandLog {
    /// The git commands that were run.
    pub commands: Vec<CommandRecord>,
    /// The hooks Git ran.
    pub hooks: Vec<HookRun>,
    /// Lock files deleted after a cancelled command.
    pub removed_stale_locks: Vec<PathBuf>,
}

/// One executed git command.
#[derive(Debug, Clone)]
pub struct CommandRecord {
    /// Sanitised, no secrets.
    pub args: String,
    /// `None` = killed by signal.
    pub exit_code: Option<i32>,
    /// Wall-clock duration.
    pub duration: Duration,
    /// Capped, see cli.
    pub stderr: String,
}

/// One hook Git ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRun {
    /// Hook name, e.g. "pre-commit".
    pub name: String,
    /// Exit code, `None` if killed by a signal.
    pub exit_code: Option<i32>,
    /// Duration in milliseconds.
    pub duration_ms: u64,
}

// ───────────────────────────── repository state ─────────────────────────────

/// What HEAD points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadState {
    /// On a branch that has commits.
    Branch {
        /// The branch's full name.
        name: FullName,
        /// The commit it points to.
        target: ObjectId,
    },
    /// On a branch without commits.
    Unborn {
        /// The branch's full name.
        name: FullName,
    },
    /// Detached.
    Detached {
        /// The commit HEAD points to.
        target: ObjectId,
    },
}

/// A multi-step operation Git is in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InProgress {
    /// `git merge`.
    Merge,
    /// A single `git cherry-pick`.
    CherryPick,
    /// A multi-commit `git cherry-pick`.
    CherryPickSequence,
    /// A single `git revert`.
    Revert,
    /// A multi-commit `git revert`.
    RevertSequence,
    /// `git rebase` (merge or apply backend).
    Rebase,
    /// `git rebase -i`.
    RebaseInteractive,
    /// `git am`.
    ApplyMailbox,
    /// `git rebase` using the apply backend while applying mailbox patches.
    ApplyMailboxRebase,
    /// `git bisect`.
    Bisect,
}

impl From<gix::state::InProgress> for InProgress {
    fn from(state: gix::state::InProgress) -> Self {
        use gix::state::InProgress as S;
        match state {
            S::ApplyMailbox => InProgress::ApplyMailbox,
            S::ApplyMailboxRebase => InProgress::ApplyMailboxRebase,
            S::Bisect => InProgress::Bisect,
            S::CherryPick => InProgress::CherryPick,
            S::CherryPickSequence => InProgress::CherryPickSequence,
            S::Merge => InProgress::Merge,
            S::Rebase => InProgress::Rebase,
            S::RebaseInteractive => InProgress::RebaseInteractive,
            S::Revert => InProgress::Revert,
            S::RevertSequence => InProgress::RevertSequence,
        }
    }
}

/// The state of the repository: HEAD, a running operation and its details.
#[derive(Debug, Clone)]
pub struct RepoState {
    /// HEAD.
    pub head: HeadState,
    /// The operation in progress, if any.
    pub in_progress: Option<InProgress>,
    /// Details of an unfinished merge.
    pub merge: Option<MergeState>,
    /// Details of an unfinished cherry-pick/revert sequence.
    pub sequencer: Option<SequencerState>,
    /// Details of an unfinished rebase.
    pub rebase: Option<RebaseState>,
    /// Details of a running bisect.
    pub bisect: Option<BisectState>,
    /// Number of paths with unmerged index entries.
    pub conflicted_paths: usize,
    /// Static properties of the repository.
    pub flags: RepoFlags,
}

/// Static properties of a repository.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepoFlags {
    /// A bare repository.
    pub bare: bool,
    /// A shallow clone.
    pub shallow: bool,
    /// A linked worktree.
    pub linked_worktree: bool,
    /// Sparse checkout is enabled.
    pub sparse: bool,
    /// The repository has submodules.
    pub has_submodules: bool,
    /// A commit-graph file is present.
    pub commit_graph: bool,
}

/// An unfinished merge.
#[derive(Debug, Clone)]
pub struct MergeState {
    /// `MERGE_HEAD`s.
    pub heads: SmallVec<[ObjectId; 1]>,
    /// The prepared merge message.
    pub message: Option<String>,
    /// `MERGE_MODE` contains `no-ff`.
    pub no_ff: bool,
    /// An autostash is waiting to be re-applied.
    pub autostash: bool,
}

/// Which command owns the sequencer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequencerKind {
    /// `git cherry-pick`.
    CherryPick,
    /// `git revert`.
    Revert,
}

/// An unfinished cherry-pick/revert sequence.
#[derive(Debug, Clone)]
pub struct SequencerState {
    /// Which command runs the sequence.
    pub kind: SequencerKind,
    /// The commit currently being applied.
    pub current: Option<ObjectId>,
    /// The steps still to do.
    pub remaining: Vec<TodoLine>,
    /// The prepared message.
    pub message: Option<String>,
}

/// Which rebase implementation is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseBackend {
    /// `rebase-merge/`.
    Merge,
    /// `rebase-apply/`.
    Apply,
}

/// Why a rebase stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseStopReason {
    /// A conflict needs resolving.
    Conflict,
    /// An `edit` step.
    Edit,
    /// A `break` step.
    Break,
    /// An `exec` step failed.
    ExecFailed,
    /// The conflict is resolved; `--continue` is needed.
    ResolvedNeedsContinue,
    /// Could not be determined.
    Unknown,
}

/// An unfinished rebase.
#[derive(Debug, Clone)]
pub struct RebaseState {
    /// Which implementation runs it.
    pub backend: RebaseBackend,
    /// Interactive.
    pub interactive: bool,
    /// `None` = rebasing a detached HEAD.
    pub head_name: Option<FullName>,
    /// The commit being rebased onto.
    pub onto: Option<ObjectId>,
    /// HEAD before the rebase started.
    pub orig_head: Option<ObjectId>,
    /// `(msgnum, end)`.
    pub step: Option<(u32, u32)>,
    /// Steps already done.
    pub done: Vec<TodoLine>,
    /// Steps still to do.
    pub todo: Vec<TodoLine>,
    /// The commit the rebase stopped at.
    pub stopped_at: Option<ObjectId>,
    /// `edit` step: the commit HEAD may amend.
    pub amend: Option<ObjectId>,
    /// Why it stopped.
    pub stop_reason: RebaseStopReason,
    /// `rebase-merge/message` (message of the stopped commit).
    pub message: Option<String>,
    /// The rebase was started by this application (editor helper marker present).
    pub started_by_us: bool,
}

/// A running bisect.
#[derive(Debug, Clone)]
pub struct BisectState {
    /// The (bad, good) terms.
    pub terms: (String, String),
    /// The ref bisect started from.
    pub start: Option<String>,
}

/// One line of a rebase/sequencer todo list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoLine {
    /// The command.
    pub action: TodoAction,
    /// The commit it refers to, if any.
    pub commit: Option<ObjectId>,
    /// The abbreviated id as written in the todo.
    pub abbrev: Option<String>,
    /// The rest of the line (summary, label name, command).
    pub rest: String,
    /// The line as written.
    pub raw: String,
}

/// A todo command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoAction {
    /// `pick`.
    Pick,
    /// `reword`.
    Reword,
    /// `edit`.
    Edit,
    /// `squash`.
    Squash,
    /// `fixup` with its flag.
    Fixup(FixupFlag),
    /// `exec`.
    Exec,
    /// `break`.
    Break,
    /// `drop`.
    Drop,
    /// `label`.
    Label,
    /// `reset`.
    Reset,
    /// `merge`.
    Merge,
    /// `update-ref`.
    UpdateRef,
    /// `noop`.
    Noop,
    /// A comment line.
    Comment,
    /// Anything else.
    Unknown(String),
}

/// The message flag of a `fixup` todo command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixupFlag {
    /// Plain `fixup`.
    None,
    /// `fixup -C`.
    UseMessage,
    /// `fixup -c`.
    EditMessage,
}

// ───────────────────────────── status ─────────────────────────────

/// Options of `status::status`.
#[derive(Debug, Clone)]
pub struct StatusOptions {
    /// Default `Files` (Gitnuro lists files).
    pub untracked: UntrackedMode,
    /// Default `IndexAndWorktree { limit: 1000 }`.
    pub renames: RenameMode,
    /// Default `HeadOnly`.
    pub submodules: SubmoduleStatusMode,
    /// Empty = whole repo; matched literally (no globs).
    pub pathspec: Vec<BString>,
}

/// How untracked files are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UntrackedMode {
    /// Every file.
    Files,
    /// Untracked directories as one entry.
    Collapsed,
    /// Not at all.
    None,
}

/// How renames are detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameMode {
    /// No detection.
    Off,
    /// HEAD to index only.
    IndexOnly,
    /// Index and worktree, tracking at most `limit` candidates.
    IndexAndWorktree {
        /// Rename tracking limit.
        limit: usize,
    },
}

/// How submodules are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmoduleStatusMode {
    /// Not at all.
    Ignore,
    /// Commit changes only.
    HeadOnly,
    /// Commit changes and dirtiness.
    HeadAndDirty,
}

/// The result of a status computation.
#[derive(Debug, Clone)]
pub struct Status {
    /// HEAD -> index, sorted by path.
    pub staged: Vec<StatusEntry>,
    /// index -> worktree + untracked + conflicts (Gitnuro places conflicts here), sorted.
    pub unstaged: Vec<StatusEntry>,
    /// Counts per kind.
    pub counts: StatusCounts,
    /// Cheap change detector.
    pub fingerprint: StatusFingerprint,
    /// gix found entries whose stat data is outdated but whose content is unchanged. Status never writes the index;
    /// the GUI may call `status::refresh_index` (§2.5) so the next status need not re-hash them.
    pub index_needs_refresh: bool,
}

/// One changed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// The path.
    pub path: RelaPath,
    /// Renames: where it came from.
    pub previous_path: Option<RelaPath>,
    /// What changed.
    pub kind: ChangeKind,
    /// Staged: HEAD side; unstaged: index side.
    pub old: Option<Side>,
    /// Staged: index side; unstaged: worktree side (id null).
    pub new: Option<Side>,
    /// Details of a conflict.
    pub conflict: Option<ConflictInfo>,
    /// Details of a submodule change.
    pub submodule: Option<SubmoduleChange>,
}

/// What kind of change a path has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    /// Added.
    Added,
    /// Modified.
    Modified,
    /// Deleted.
    Deleted,
    /// Renamed.
    Renamed,
    /// The entry kind changed.
    TypeChanged,
    /// Not tracked.
    Untracked,
    /// Unmerged.
    Conflicted,
    /// `git add -N`.
    IntentToAdd,
}

/// The three sides of a conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictInfo {
    /// What kind of conflict.
    pub kind: ConflictKind,
    /// Stage 1.
    pub base: Option<Side>,
    /// Stage 2.
    pub ours: Option<Side>,
    /// Stage 3.
    pub theirs: Option<Side>,
}

/// What kind of conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides modified it.
    BothModified,
    /// Both sides added it.
    BothAdded,
    /// Both sides deleted it.
    BothDeleted,
    /// Only we added it.
    AddedByUs,
    /// Only they added it.
    AddedByThem,
    /// We deleted it, they modified it.
    DeletedByUs,
    /// They deleted it, we modified it.
    DeletedByThem,
}

/// How a submodule entry changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleChange {
    /// The old commit.
    pub old_commit: Option<ObjectId>,
    /// The new commit.
    pub new_commit: Option<ObjectId>,
    /// Whether the submodule's worktree is dirty, if checked.
    pub dirty: Option<bool>,
}

/// Counts of entries per kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindCounts {
    /// Added.
    pub added: u32,
    /// Modified.
    pub modified: u32,
    /// Deleted.
    pub deleted: u32,
    /// Renamed.
    pub renamed: u32,
    /// Untracked.
    pub untracked: u32,
    /// Conflicted.
    pub conflicted: u32,
}

/// Counts for the staged and unstaged lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusCounts {
    /// Staged entries.
    pub staged: KindCounts,
    /// Unstaged entries.
    pub unstaged: KindCounts,
}

impl StatusCounts {
    /// Nothing staged, nothing unstaged.
    pub fn is_clean(&self) -> bool {
        self.staged == KindCounts::default() && self.unstaged == KindCounts::default()
    }
}

/// Modification time and size of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    /// Size in bytes.
    pub size: u64,
}

/// Identifies the inputs of a status computation cheaply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusFingerprint {
    /// Stamp of the index file.
    pub index: Option<FileStamp>,
    /// HEAD's commit.
    pub head: Option<ObjectId>,
}

// ───────────────────────────── diff ─────────────────────────────

/// What to diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTarget {
    /// index -> worktree. Untracked: empty -> worktree. Conflicted: stage 2 (ours) -> worktree.
    Unstaged {
        /// The path.
        path: RelaPath,
        /// Renames: the old path.
        previous_path: Option<RelaPath>,
    },
    /// `HEAD^{tree}` (empty tree if unborn) -> index.
    Staged {
        /// The path.
        path: RelaPath,
        /// Renames: the old path.
        previous_path: Option<RelaPath>,
    },
    /// `against` -> commit tree.
    Commit {
        /// The commit.
        commit: ObjectId,
        /// What it is compared against.
        against: Against,
        /// The path.
        path: RelaPath,
        /// Renames: the old path.
        previous_path: Option<RelaPath>,
    },
}

/// What a commit is compared against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Against {
    /// The first parent.
    FirstParent,
    /// The n-th parent (0-based).
    Parent(u8),
    /// Another commit.
    Commit(ObjectId),
    /// The empty tree.
    Empty,
}

/// Options of a file diff.
#[derive(Debug, Clone)]
pub struct DiffOptions {
    /// Default 3.
    pub context_lines: u32,
    /// One hunk with the whole file (Gitnuro "display full file").
    pub full_file: bool,
    /// Default None; anything else blocks partial staging.
    pub whitespace: WhitespaceMode,
    /// Default true (§2.4 diff.rs).
    pub inline_highlights: bool,
    /// Default 50 MiB (Gitnuro); either side above => TooLarge.
    pub max_file_bytes: u64,
    /// Default 1_000_000 lines per side => TooLarge.
    pub max_lines: u32,
    /// Default 20 MiB per side for Image previews.
    pub max_image_bytes: u64,
    /// `None` = `diff.algorithm` config (gix default Myers).
    pub algorithm: Option<DiffAlgorithm>,
}

/// How whitespace differences are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhitespaceMode {
    /// Whitespace matters.
    None,
    /// Ignore whitespace at end of line.
    IgnoreAtEol,
    /// Ignore changes in the amount of whitespace.
    IgnoreChange,
    /// Ignore all whitespace.
    IgnoreAll,
}

/// The diff algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffAlgorithm {
    /// Myers.
    Myers,
    /// Myers with the minimal-diff heuristic.
    MyersMinimal,
    /// Histogram.
    Histogram,
}

/// The diff of one file.
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// What was diffed.
    pub target: DiffTarget,
    /// The path.
    pub path: RelaPath,
    /// Renames: the old path.
    pub previous_path: Option<RelaPath>,
    /// The kind of change.
    pub kind: ChangeKind,
    /// Old side.
    pub old: Option<BlobSide>,
    /// New side.
    pub new: Option<BlobSide>,
    /// The content comparison.
    pub content: DiffContent,
}

/// One side of a file diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobSide {
    /// Real blob id. For worktree sides it is the hash of the git-form content computed during the diff
    /// (so stage/discard can detect staleness). Null only for TooLarge worktree files (not hashed).
    pub id: ObjectId,
    /// What kind of entry it is.
    pub kind: EntryKind,
    /// Git-form size in bytes.
    pub size: u64,
    /// This side is the worktree file.
    pub from_worktree: bool,
}

/// What a file diff contains.
#[derive(Debug, Clone)]
pub enum DiffContent {
    /// A text diff.
    Text(Arc<TextDiff>),
    /// Binary content.
    Binary,
    /// A side exceeds the configured threshold.
    TooLarge {
        /// The threshold in bytes.
        threshold: u64,
    },
    /// Image previews.
    Image {
        /// Old image.
        old: Option<ImageData>,
        /// New image.
        new: Option<ImageData>,
    },
    /// A submodule pointer change.
    Submodule {
        /// Old commit.
        old_commit: Option<ObjectId>,
        /// New commit.
        new_commit: Option<ObjectId>,
        /// Whether the submodule worktree is dirty.
        dirty: Option<bool>,
        /// The commits in between.
        summary: Option<SubmoduleSummary>,
    },
    /// The entry kind changed.
    TypeChange {
        /// Old kind.
        old: EntryKind,
        /// New kind.
        new: EntryKind,
    },
    /// Content identical, executable bit changed.
    ModeOnly {
        /// Old kind.
        old: EntryKind,
        /// New kind.
        new: EntryKind,
    },
    /// Added or deleted empty file.
    Empty,
}

/// Encoded image bytes with their MIME type.
#[derive(Debug, Clone)]
pub struct ImageData {
    /// The MIME type.
    pub mime: &'static str,
    /// The encoded image.
    pub bytes: Arc<[u8]>,
}

/// The commits that make up a submodule pointer change.
#[derive(Debug, Clone)]
pub struct SubmoduleSummary {
    /// `(id, summary)` per commit.
    pub commits: Vec<(ObjectId, String)>,
    /// More commits exist than are listed.
    pub truncated: bool,
    /// The pointer moved backwards.
    pub rewound: bool,
}

/// A text diff with all line texts in one arena.
#[derive(Debug, Clone)]
pub struct TextDiff {
    /// The hunks.
    pub hunks: Vec<Hunk>,
    /// All lines of all hunks.
    pub lines: Vec<DiffLine>,
    /// Arena: all line texts concatenated (decoded, no EOL).
    pub(crate) text: String,
    /// Arena: intra-line highlight ranges (byte offsets within the line text).
    pub(crate) spans: Vec<Range<u32>>,
    /// Number of added lines.
    pub added: u32,
    /// Number of removed lines.
    pub removed: u32,
    /// "UTF-8", "windows-1252", ... (chardetng over the first 64 KiB).
    pub encoding: &'static str,
    /// Line endings of the old side.
    pub old_eol: EolStyle,
    /// Line endings of the new side.
    pub new_eol: EolStyle,
    /// The old side has no newline at the end.
    pub old_missing_final_newline: bool,
    /// The new side has no newline at the end.
    pub new_missing_final_newline: bool,
    /// Binary-to-text conversion was used.
    pub textconv: bool,
    /// Whether hunks and lines can be staged on their own.
    pub partial: PartialAvailability,
}

impl TextDiff {
    /// The text of `line`, without its terminator.
    ///
    /// Panics if `line` does not belong to this diff.
    pub fn line_text(&self, line: &DiffLine) -> &str {
        &self.text[line.text.start as usize..line.text.end as usize]
    }

    /// The intra-line highlight ranges of `line`, as byte offsets into [`Self::line_text()`].
    ///
    /// Panics if `line` does not belong to this diff.
    pub fn inline_spans(&self, line: &DiffLine) -> &[Range<u32>] {
        &self.spans[line.spans.start as usize..line.spans.end as usize]
    }

    /// The lines of `hunk`.
    ///
    /// Panics if `hunk` does not belong to this diff.
    pub fn hunk_lines(&self, hunk: &Hunk) -> &[DiffLine] {
        &self.lines[hunk.lines.start as usize..hunk.lines.end as usize]
    }
}

/// How the line endings of a file look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EolStyle {
    /// `\n` only.
    Lf,
    /// `\r\n` only.
    CrLf,
    /// Both.
    Mixed,
    /// No line ending at all.
    None,
}

/// Whether a diff can be staged in parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialAvailability {
    /// Hunks and lines can be staged.
    Allowed,
    /// Only the whole file, for this reason.
    WholeFileOnly(PartialBlock),
}

/// The header of a hunk (`@@ -a,b +c,d @@`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HunkHeader {
    /// First old line (1-based; an empty side has the line before, like Git).
    pub old_start: u32,
    /// Number of old lines.
    pub old_lines: u32,
    /// First new line.
    pub new_start: u32,
    /// Number of new lines.
    pub new_lines: u32,
}

/// A hunk of a [`TextDiff`].
#[derive(Debug, Clone)]
pub struct Hunk {
    /// Its header.
    pub header: HunkHeader,
    /// Index range into [`TextDiff::lines`].
    pub lines: Range<u32>,
}

/// One line of a [`TextDiff`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffLine {
    /// Context, added or removed.
    pub kind: LineKind,
    /// 1-based, `Some` for Context/Removed.
    pub old_lineno: Option<u32>,
    /// 1-based, `Some` for Context/Added.
    pub new_lineno: Option<u32>,
    /// Terminator of this line in its source image.
    pub eol: Eol,
    /// Range into `TextDiff::text`.
    pub(crate) text: ArenaRange,
    /// Range into `TextDiff::spans`.
    pub(crate) spans: ArenaRange,
}

/// A `Copy` range into one of the arenas of a [`TextDiff`] (`std::ops::Range` is not `Copy`, but [`DiffLine`] is).
/// Crate-private like the arenas themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArenaRange {
    /// First index.
    pub(crate) start: u32,
    /// One past the last index.
    pub(crate) end: u32,
}

/// The role of a line in a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Unchanged.
    Context,
    /// Only in the new side.
    Added,
    /// Only in the old side.
    Removed,
}

/// A line terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    /// `\n`.
    Lf,
    /// `\r\n`.
    CrLf,
    /// None (last line without newline).
    None,
}

/// A file touched by a commit.
#[derive(Debug, Clone)]
pub struct CommitFile {
    /// The path.
    pub path: RelaPath,
    /// Renames: the old path.
    pub previous_path: Option<RelaPath>,
    /// The kind of change.
    pub kind: ChangeKind,
    /// Old side.
    pub old: Option<Side>,
    /// New side.
    pub new: Option<Side>,
    /// Ready for `diff::file_diff`.
    pub target: DiffTarget,
    /// When `CommitFilesOptions::line_stats`.
    pub lines_added: Option<u32>,
    /// When `CommitFilesOptions::line_stats`.
    pub lines_removed: Option<u32>,
}

/// Options of `diff::commit_files`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommitFilesOptions {
    /// Compute added/removed line counts.
    pub line_stats: bool,
    /// Append untracked-commit files of a stash.
    pub stash: bool,
}

/// A commit with its message and changed files.
#[derive(Debug, Clone)]
pub struct CommitDetails {
    /// Summary information.
    pub info: CommitInfo,
    /// The full decoded message.
    pub message: String,
    /// The `encoding` header, if any.
    pub encoding: Option<String>,
    /// A `gpgsig` header is present.
    pub signed: bool,
    /// The changed files.
    pub files: Arc<Vec<CommitFile>>,
}

/// Diff text for AI prompts (16 A7), budgeted.
#[derive(Debug, Clone)]
pub struct PatchTextOptions {
    /// Default 16_000.
    pub budget_chars: usize,
    /// Default 500.
    pub per_file_min_chars: usize,
    /// Default: lock files, `*.min.js`, `*.map`.
    pub skip_globs: Vec<String>,
}

/// Budgeted patch text.
#[derive(Debug, Clone)]
pub struct PatchText {
    /// Per-file statistics.
    pub files: Vec<FileStat>,
    /// The patch text.
    pub text: String,
    /// How many files were cut to fit the budget.
    pub truncated_files: usize,
}

/// Statistics of one file in a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    /// The path.
    pub path: RelaPath,
    /// The kind of change.
    pub kind: ChangeKind,
    /// Added lines.
    pub added: u32,
    /// Removed lines.
    pub removed: u32,
    /// Binary file.
    pub binary: bool,
}

// ───────────────────────────── staging ─────────────────────────────

/// A request to stage, unstage or discard part of a file.
#[derive(Debug, Clone)]
pub struct SelectionRequest {
    /// The path.
    pub path: RelaPath,
    /// Renames: the old path.
    pub previous_path: Option<RelaPath>,
    /// `old.id` of the FileDiff the selection was made on (stale check, §4.4).
    pub base_old: Option<ObjectId>,
    /// `new.id` of the FileDiff the selection was made on (stale check, §4.4).
    pub base_new: Option<ObjectId>,
    /// What was selected.
    pub selection: Selection,
}

/// A selection within a diff.
#[derive(Debug, Clone)]
pub enum Selection {
    /// Whole hunks.
    Hunks(Vec<HunkHeader>),
    /// Single lines.
    Lines(Vec<LineRef>),
}

/// A reference to a changed line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineRef {
    /// An added line, by its new line number.
    Added {
        /// 1-based line number in the new side.
        new_lineno: u32,
    },
    /// A removed line, by its old line number.
    Removed {
        /// 1-based line number in the old side.
        old_lineno: u32,
    },
}

/// What a discard throws away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardMode {
    /// Index -> worktree changes only.
    Unstaged,
    /// Staged and unstaged changes.
    StagedAndUnstaged,
}

/// How a conflicted path is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Take our side.
    Ours,
    /// Take their side.
    Theirs,
    /// Keep the worktree file as is and mark it resolved.
    MarkResolved,
    /// Delete the file.
    Delete,
}

// ───────────────────────────── log / graph ─────────────────────────────

/// Summary information of a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// The commit's id.
    pub id: ObjectId,
    /// Its parents.
    pub parents: SmallVec<[ObjectId; 2]>,
    /// The author.
    pub author: Signature,
    /// The committer.
    pub committer: Signature,
    /// First line, decoded (commit encoding / lossy UTF-8).
    pub summary: Box<str>,
    /// The message has more than a summary.
    pub has_body: bool,
}

/// Options of `LogWalker::new`.
#[derive(Debug, Clone)]
pub struct LogOptions {
    /// Where the walk starts.
    pub tips: LogTips,
    /// The sort order.
    pub order: LogOrder,
    /// Follow only first parents.
    pub first_parent: bool,
    /// Apply the mailmap.
    pub mailmap: bool,
}

/// The starting points of a walk.
#[derive(Debug, Clone)]
pub enum LogTips {
    /// HEAD and all branches, optionally remotes, tags and stashes.
    AllRefs {
        /// Include remote-tracking branches.
        remotes: bool,
        /// Include tags.
        tags: bool,
        /// Include stashes.
        stashes: bool,
    },
    /// Exactly these commits.
    Explicit(Vec<ObjectId>),
}

/// The order of a log walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogOrder {
    /// `--date-order`.
    Date,
    /// `--topo-order`.
    Topo,
}

/// A page of a log walk.
#[derive(Debug, Clone)]
pub struct LogPage {
    /// The commits, in walk order.
    pub commits: Vec<CommitInfo>,
    /// The walk has no more commits.
    pub exhausted: bool,
}

/// A search over history.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// The text to look for.
    pub text: String,
    /// Search the message.
    pub in_message: bool,
    /// Search the author.
    pub in_author: bool,
    /// Search the committer.
    pub in_committer: bool,
    /// Match a hex prefix of the id.
    pub in_hash: bool,
    /// Case-sensitive matching.
    pub case_sensitive: bool,
}

/// The kind of a node in the commit graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeKind {
    /// An ordinary commit.
    Commit,
    /// A merge commit.
    Merge,
    /// A commit without parents.
    Root,
    /// A stash commit.
    Stash,
    /// The synthetic "uncommitted changes" row.
    Uncommitted,
    /// A commit whose parents are cut off by a shallow clone.
    ShallowBoundary,
}

/// A lane a merge commit draws a line to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeOut {
    /// The lane.
    pub lane: u16,
    /// The line joins a lane that already exists.
    pub joins_existing: bool,
}

// ───────────────────────────── refs ─────────────────────────────

/// Which ref namespaces `refs::list` returns.
#[derive(Debug, Clone, Copy, Default)]
pub struct RefListOptions {
    /// Remote-tracking branches.
    pub remotes: bool,
    /// Tags.
    pub tags: bool,
    /// Stashes.
    pub stashes: bool,
}

/// All refs of a repository.
#[derive(Debug, Clone)]
pub struct RefList {
    /// HEAD.
    pub head: HeadState,
    /// Sorted by short name.
    pub local: Vec<LocalBranch>,
    /// Sorted by name; branches sorted.
    pub remotes: Vec<RemoteInfo>,
    /// Sorted by name (version-aware: v1.10 after v1.9).
    pub tags: Vec<TagInfo>,
    /// `stash@{0}` first.
    pub stashes: Vec<StashInfo>,
    /// Labels per commit.
    pub decorations: Decorations,
}

/// A local branch.
#[derive(Debug, Clone)]
pub struct LocalBranch {
    /// Short, e.g. "feature/x".
    pub name: String,
    /// The full ref name.
    pub full: FullName,
    /// The commit it points to.
    pub target: ObjectId,
    /// It is the current branch.
    pub is_head: bool,
    /// Its upstream.
    pub upstream: Option<Upstream>,
    /// `None` until `refs::ahead_behind` ran.
    pub ahead_behind: Option<AheadBehind>,
    /// Another worktree that has it checked out.
    pub checked_out_in: Option<PathBuf>,
}

/// The upstream configuration of a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// The remote's name.
    pub remote: String,
    /// `refs/heads/x` on the remote.
    pub merge: FullName,
    /// `refs/remotes/<r>/x`.
    pub tracking: Option<FullName>,
    /// The tracking ref no longer exists.
    pub gone: bool,
}

/// Commits ahead of and behind an upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AheadBehind {
    /// Commits only on our side.
    pub ahead: u32,
    /// Commits only on the other side.
    pub behind: u32,
    /// The walk was capped ("999+").
    pub capped: bool,
}

/// A remote with its branches.
#[derive(Debug, Clone)]
pub struct RemoteInfo {
    /// The remote's name.
    pub name: String,
    /// The fetch URL.
    pub fetch_url: Option<String>,
    /// The push URL.
    pub push_url: Option<String>,
    /// The remote's default branch.
    pub head_branch: Option<String>,
    /// Its remote-tracking branches.
    pub branches: Vec<RemoteBranch>,
}

/// A remote-tracking branch.
#[derive(Debug, Clone)]
pub struct RemoteBranch {
    /// The remote's name.
    pub remote: String,
    /// "main".
    pub name: String,
    /// The full ref name.
    pub full: FullName,
    /// The commit it points to.
    pub target: ObjectId,
}

/// A tag.
#[derive(Debug, Clone)]
pub struct TagInfo {
    /// The short name.
    pub name: String,
    /// The full ref name.
    pub full: FullName,
    /// The peeled commit.
    pub target: ObjectId,
    /// The tag object, for annotated tags.
    pub tag_object: Option<ObjectId>,
    /// First line of the tag message.
    pub message_summary: Option<String>,
    /// When the tag was created.
    pub tagger_time: Option<GitTime>,
}

/// A stash entry.
#[derive(Debug, Clone)]
pub struct StashInfo {
    /// Position in the stash list.
    pub index: usize,
    /// The stash commit.
    pub id: ObjectId,
    /// The stash message.
    pub message: String,
    /// The branch it was made on.
    pub branch: Option<String>,
    /// When it was made.
    pub time: GitTime,
    /// The commit it is based on.
    pub base: ObjectId,
    /// The index commit (parent 2).
    pub index_commit: Option<ObjectId>,
    /// The untracked-files commit (parent 3).
    pub untracked_commit: Option<ObjectId>,
}

/// A label attached to a commit in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefLabel {
    /// HEAD.
    Head,
    /// A local branch.
    LocalBranch(String),
    /// A remote-tracking branch.
    RemoteBranch {
        /// The remote's name.
        remote: String,
        /// The branch's name.
        name: String,
    },
    /// A tag.
    Tag(String),
    /// A stash, by index.
    Stash(usize),
}

/// Labels per commit.
#[derive(Debug, Clone, Default)]
pub struct Decorations(pub HashMap<ObjectId, SmallVec<[RefLabel; 2]>>);

// ───────────────────────────── remotes / forge inputs ─────────────────────────────

/// A remote's name and URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    /// The remote's name.
    pub name: String,
    /// Its URL.
    pub url: String,
}

/// Inputs for the forge facade (15 §6.1); resolved by `remote::forge_remotes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRemotes {
    /// The remote pull requests target.
    pub base: Option<RemoteRef>,
    /// The remote branches are pushed to.
    pub push: Option<RemoteRef>,
}

// ───────────────────────────── commit / branch / tag / stash options ─────────────────────────────

/// Options of `commit::commit`.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    /// Empty + amend => keep old message (`--no-edit`).
    pub message: String,
    /// `--amend`.
    pub amend: bool,
    /// `--allow-empty`.
    pub allow_empty: bool,
    /// `--author`.
    pub author: Option<Identity>,
    /// `-c user.name/-c user.email` (author AND committer), for one commit.
    pub identity_override: Option<Identity>,
    /// `--signoff`.
    pub signoff: bool,
    /// `--trailer "<k>: <v>"`.
    pub trailers: Vec<(String, String)>,
    /// Whether to sign.
    pub sign: SignMode,
    /// `--no-verify` (explicit user choice only).
    pub skip_hooks: bool,
}

/// Whether to sign a commit or tag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignMode {
    /// Follow `commit.gpgsign` / `tag.gpgsign`.
    #[default]
    FromConfig,
    /// Always sign.
    Sign,
    /// Never sign.
    DontSign,
}

/// The result of a commit.
#[derive(Debug, Clone)]
pub struct CommitOutcome {
    /// The new commit.
    pub id: ObjectId,
    /// Its summary.
    pub summary: String,
}

/// Options of the checkout operations.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckoutOptions {
    /// `-m`.
    pub carry_changes: bool,
    /// `--discard-changes`.
    pub force: bool,
}

/// Where a new branch starts.
#[derive(Debug, Clone)]
pub enum StartPoint {
    /// HEAD.
    Head,
    /// A commit.
    Commit(ObjectId),
    /// A ref.
    Ref(FullName),
}

/// Options of `tag::create_tag`.
#[derive(Debug, Clone, Default)]
pub struct TagOptions {
    /// `Some` => annotated.
    pub message: Option<String>,
    /// Whether to sign.
    pub sign: SignMode,
    /// `--force`.
    pub force: bool,
}

/// Options of `stash::stash_push`.
#[derive(Debug, Clone, Default)]
pub struct StashPushOptions {
    /// The stash message.
    pub message: Option<String>,
    /// GUI default true.
    pub include_untracked: bool,
    /// `--keep-index`.
    pub keep_index: bool,
    /// `--staged`, git >= 2.35.
    pub staged_only: bool,
    /// Restrict to these paths.
    pub paths: Vec<RelaPath>,
}

/// Options of stash apply/pop.
#[derive(Debug, Clone, Copy, Default)]
pub struct StashApplyOptions {
    /// `--index`.
    pub restore_index: bool,
}

// ───────────────────────────── merge / rebase / cherry-pick / revert / reset ─────────────────────────────

/// Options of `merge::merge`.
#[derive(Debug, Clone)]
pub struct MergeOptions {
    /// Ref name or sha.
    pub rev: String,
    /// Fast-forward behaviour.
    pub ff: FastForward,
    /// `--squash`.
    pub squash: bool,
    /// The merge message.
    pub message: Option<String>,
    /// `--autostash`.
    pub autostash: bool,
    /// `--no-verify`.
    pub skip_hooks: bool,
}

/// Fast-forward behaviour of a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastForward {
    /// Fast-forward when possible.
    Auto,
    /// `--no-ff`.
    NoFf,
    /// `--ff-only`.
    FfOnly,
}

/// The result of a merge.
#[derive(Debug, Clone)]
pub enum MergeOutcome {
    /// Nothing to merge.
    UpToDate,
    /// HEAD moved forward.
    FastForwarded {
        /// The new HEAD.
        new_head: ObjectId,
    },
    /// A merge commit was created.
    Merged {
        /// The new HEAD.
        new_head: ObjectId,
    },
    /// `--squash`: changes staged, no commit.
    Squashed,
    /// The merge succeeded but re-applying the autostash conflicted.
    AutostashConflict {
        /// The new HEAD.
        new_head: ObjectId,
    },
}

/// Options of cherry-pick and revert.
#[derive(Debug, Clone, Copy, Default)]
pub struct PickOptions {
    /// `-m <parent>`.
    pub mainline: Option<u8>,
    /// `--no-commit`.
    pub no_commit: bool,
}

/// The result of a cherry-pick or revert sequence. Conflicts are `Err(Conflicts)`.
#[derive(Debug, Clone)]
pub enum SequenceOutcome {
    /// All commits were applied.
    Completed {
        /// The new HEAD.
        new_head: ObjectId,
    },
}

/// The mode of `merge::reset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// `--soft`.
    Soft,
    /// `--mixed`.
    Mixed,
    /// `--hard`.
    Hard,
    /// `--keep`.
    Keep,
}

/// The result of `merge::continue_operation`.
#[derive(Debug, Clone)]
pub enum ContinueOutcome {
    /// The operation finished.
    Completed,
    /// A rebase continued.
    Rebase(RebaseOutcome),
}

/// The result of a pull.
#[derive(Debug, Clone)]
pub enum MergeOutcomeOrRebase {
    /// The pull merged.
    Merge(MergeOutcome),
    /// The pull rebased.
    Rebase(RebaseOutcome),
}

/// The result of stash apply/pop.
#[derive(Debug, Clone)]
pub enum StashApplyResult {
    /// Applied cleanly.
    Applied,
    /// Tracked changes applied with conflicts; the stash entry is kept (also by pop).
    Conflicts {
        /// The conflicted paths.
        paths: Vec<RelaPath>,
    },
    /// Git applied the tracked part (possibly with conflicts) but refused to restore untracked files of the stash that
    /// already exist (`<p> already exists, no checkout`, exit 128). The stash entry is kept (also by pop).
    Partial {
        /// The conflicted paths.
        conflicts: Vec<RelaPath>,
        /// Untracked files that were not restored.
        untracked_not_restored: Vec<RelaPath>,
    },
}

/// What `diff::export_patch` exports.
#[derive(Debug, Clone)]
pub enum ExportPatch {
    /// Unstaged changes of these paths.
    Unstaged(Vec<RelaPath>),
    /// Staged changes of these paths.
    Staged(Vec<RelaPath>),
    /// A commit.
    Commit(ObjectId),
}

/// How unknown SSH host keys are treated.
///
/// Will be replaced by a re-export of `gitbutler_git::askpass_harness::HostKeyPolicy` once WP-B0c has introduced
/// the harness module; the variants are the same.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// `StrictHostKeyChecking=accept-new`: trust unknown hosts on first use, refuse changed keys.
    #[default]
    AcceptNew,
    /// `StrictHostKeyChecking=ask`: confirm every unknown host through the credential prompter.
    Ask,
}

/// Options of `rebase::rebase` and `rebase::start_interactive`.
#[derive(Debug, Clone, Default)]
pub struct RebaseOptions {
    /// `--onto`.
    pub onto: Option<ObjectId>,
    /// `--autostash`.
    pub autostash: bool,
    /// `--rebase-merges`.
    pub rebase_merges: bool,
    /// `--no-verify`.
    pub skip_hooks: bool,
}

/// The result of a rebase step.
#[derive(Debug, Clone)]
pub enum RebaseOutcome {
    /// The rebase finished.
    Completed {
        /// The new HEAD.
        new_head: ObjectId,
    },
    /// The rebase stopped.
    Stopped {
        /// Why.
        reason: RebaseStopReason,
        /// The commit it stopped at.
        at: Option<ObjectId>,
        /// The conflicted paths.
        conflicts: Vec<RelaPath>,
    },
}

/// An interactive rebase plan, edited in the GUI before anything starts.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RebasePlan {
    /// HEAD when planned (stale check).
    pub head: ObjectId,
    /// The branch HEAD was on.
    pub head_ref: Option<FullName>,
    /// `None` => `--root`.
    pub base: Option<ObjectId>,
    /// Oldest first (todo order).
    pub steps: Vec<RebaseStep>,
    /// Merge commits between base and HEAD that `rebase -i` flattens.
    pub merges_dropped: usize,
}

/// One step of a [`RebasePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RebaseStep {
    /// What to do with the commit.
    pub action: RebaseAction,
    /// The commit.
    pub commit: ObjectId,
    /// Its summary.
    pub summary: String,
    /// Original full message.
    pub message: String,
    /// Reword / squash result.
    pub new_message: Option<String>,
}

/// An action of an interactive rebase step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RebaseAction {
    /// Keep.
    Pick,
    /// Keep, change the message.
    Reword,
    /// Stop to amend.
    Edit,
    /// Meld into the previous commit, combining messages.
    Squash,
    /// Meld into the previous commit, discarding the message.
    Fixup,
    /// Remove.
    Drop,
}

// ───────────────────────────── network ─────────────────────────────

/// Options of `network::clone`.
#[derive(Debug, Clone)]
pub struct CloneOptions {
    /// The URL to clone.
    pub url: String,
    /// The target directory.
    pub directory: PathBuf,
    /// `--branch`.
    pub branch: Option<String>,
    /// `--depth`.
    pub depth: Option<u32>,
    /// Gitnuro default true.
    pub recurse_submodules: bool,
    /// `--bare`.
    pub bare: bool,
}

/// What to fetch.
#[derive(Debug, Clone)]
pub enum FetchTarget {
    /// One remote.
    Remote(String),
    /// All remotes.
    All,
}

/// Options of `network::fetch`.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// What to fetch.
    pub target: FetchTarget,
    /// `None` = `fetch.prune` config; GUI passes `Some(true)` for Gitnuro parity.
    pub prune: Option<bool>,
    /// Tag handling.
    pub tags: TagFetch,
    /// Explicit refspecs.
    pub refspecs: Vec<String>,
}

/// Tag handling of a fetch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TagFetch {
    /// Git's default.
    #[default]
    Default,
    /// `--tags`.
    All,
    /// `--no-tags`.
    None,
}

/// The result of a fetch.
#[derive(Debug, Clone)]
pub struct FetchReport {
    /// Per remote: success or the error text.
    pub per_remote: Vec<(String, Result<(), String>)>,
    /// How many refs were updated.
    pub updated_refs: usize,
}

/// Options of `network::pull`.
#[derive(Debug, Clone)]
pub struct PullOptions {
    /// `None` = the branch's remote.
    pub remote: Option<String>,
    /// `None` = the branch's upstream.
    pub branch: Option<String>,
    /// How to integrate.
    pub mode: PullMode,
    /// `--autostash`.
    pub autostash: bool,
}

/// How a pull integrates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullMode {
    /// `pull.rebase` / `pull.ff` / `branch.<b>.rebase`.
    FromConfig,
    /// `--no-rebase`.
    Merge,
    /// `--rebase`.
    Rebase,
    /// `--ff-only`.
    FfOnly,
}

/// Options of `network::push`.
#[derive(Debug, Clone)]
pub struct PushOptions {
    /// The remote.
    pub remote: String,
    /// What to push.
    pub refspecs: Vec<PushRefSpec>,
    /// Forcing.
    pub force: ForceMode,
    /// Tag handling.
    pub tags: TagPush,
    /// `-u`.
    pub set_upstream: bool,
    /// `-o`.
    pub push_options: Vec<String>,
    /// `--no-verify`.
    pub skip_hooks: bool,
}

/// One refspec of a push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRefSpec {
    /// `None` = delete.
    pub src: Option<FullName>,
    /// The destination ref.
    pub dst: FullName,
    /// Lease value; `None` with `WithLease` = use the tracking ref.
    pub expected_remote: Option<ObjectId>,
}

/// How a push is forced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceMode {
    /// No forcing.
    No,
    /// `--force-with-lease --force-if-includes`.
    WithLease,
    /// `--force`.
    Force,
}

/// Tag handling of a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagPush {
    /// No tags.
    None,
    /// `--follow-tags`.
    Follow,
    /// `--tags`.
    All,
}

/// The result of a push.
#[derive(Debug, Clone)]
pub struct PushReport {
    /// The remote.
    pub remote: String,
    /// Its URL.
    pub url: Option<String>,
    /// One entry per pushed ref.
    pub updates: Vec<PushedRefUpdate>,
}

/// What happened to one pushed ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedRefUpdate {
    /// The local ref.
    pub local: Option<FullName>,
    /// The remote ref.
    pub remote_ref: FullName,
    /// Its old value.
    pub old: Option<ObjectId>,
    /// Its new value.
    pub new: Option<ObjectId>,
    /// The outcome.
    pub status: PushRefStatus,
    /// Git's summary text.
    pub summary: String,
}

/// The outcome of one pushed ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushRefStatus {
    /// Fast-forwarded.
    FastForward,
    /// Forced.
    Forced,
    /// Deleted.
    Deleted,
    /// Created.
    New,
    /// Nothing to do.
    UpToDate,
    /// Rejected.
    Rejected,
}

// ───────────────────────────── submodules / worktrees ─────────────────────────────

/// A submodule.
#[derive(Debug, Clone)]
pub struct SubmoduleInfo {
    /// Its name.
    pub name: String,
    /// Its path.
    pub path: RelaPath,
    /// Its URL.
    pub url: Option<String>,
    /// Its configured branch.
    pub branch: Option<String>,
    /// Its state.
    pub state: SubmoduleState,
    /// The commit recorded in the index.
    pub index_commit: Option<ObjectId>,
    /// The commit checked out.
    pub head_commit: Option<ObjectId>,
    /// Whether its worktree is dirty, if checked.
    pub dirty: Option<bool>,
}

/// The state of a submodule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmoduleState {
    /// Not in `.git/config`.
    Uninitialized,
    /// Configured, not checked out.
    Initialized,
    /// Checked out.
    CheckedOut,
    /// Its path does not exist although it is in the index.
    Missing,
}

/// A worktree of a repository.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// `None` = main.
    pub id: Option<String>,
    /// The worktree's directory.
    pub path: PathBuf,
    /// Its git directory.
    pub git_dir: PathBuf,
    /// `None` if unreadable.
    pub head: Option<HeadState>,
    /// It is the worktree of the open repository.
    pub is_current: bool,
    /// `Some("")` = locked without reason.
    pub locked: Option<String>,
    /// Its directory is gone and `git worktree prune` would remove it.
    pub prunable: bool,
    /// A bare main repository.
    pub bare: bool,
}

/// What a new worktree checks out.
#[derive(Debug, Clone)]
pub enum WorktreeTarget {
    /// An existing branch.
    ExistingBranch(FullName),
    /// A new branch.
    NewBranch {
        /// The branch's name.
        name: String,
        /// Where it starts.
        start: StartPoint,
    },
    /// A commit, detached.
    Detached(ObjectId),
}

/// Options of `worktree::add`.
#[derive(Debug, Clone)]
pub struct WorktreeAddOptions {
    /// The directory of the new worktree.
    pub path: PathBuf,
    /// What it checks out.
    pub target: WorktreeTarget,
    /// Lock it with this reason.
    pub lock_reason: Option<String>,
}

// ───────────────────────────── blame / history ─────────────────────────────

/// What to blame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlameRev {
    /// The worktree file.
    Worktree,
    /// The file at a commit.
    Commit(ObjectId),
}

/// Options of `blame::blame`.
#[derive(Debug, Clone, Default)]
pub struct BlameOptions {
    /// Default true.
    pub follow_renames: bool,
    /// Ignore commits before this time.
    pub since: Option<GitTime>,
    /// Restrict to these lines.
    pub line_range: Option<Range<u32>>,
}

/// The result of a blame.
#[derive(Debug, Clone)]
pub struct BlameResult {
    /// The blamed path.
    pub path: RelaPath,
    /// What was blamed.
    pub rev: BlameRev,
    /// The commits referenced by the chunks.
    pub commits: Vec<BlameCommit>,
    /// Consecutive lines from the same commit, in file order.
    pub chunks: Vec<BlameChunk>,
    /// Line i => byte range in `text`.
    pub lines: Vec<Range<u32>>,
    /// Decoded file content (no EOLs).
    pub text: String,
}

/// A commit referenced by a blame.
#[derive(Debug, Clone)]
pub struct BlameCommit {
    /// The commit.
    pub id: ObjectId,
    /// Its author.
    pub author: Signature,
    /// Its summary.
    pub summary: String,
    /// The path the lines had in that commit.
    pub source_path: RelaPath,
}

/// A run of lines attributed to one commit.
#[derive(Debug, Clone, Copy)]
pub struct BlameChunk {
    /// 0-based.
    pub start_line: u32,
    /// Number of lines.
    pub len: u32,
    /// Index into `BlameResult::commits`; `None` = not committed yet.
    pub commit: Option<u32>,
    /// First line in the source file of that commit.
    pub source_start_line: u32,
}

/// One commit in a file's history.
#[derive(Debug, Clone)]
pub struct FileHistoryEntry {
    /// The commit.
    pub commit: CommitInfo,
    /// The file's path in that commit.
    pub path: RelaPath,
    /// Renames: the path before.
    pub previous_path: Option<RelaPath>,
    /// What the commit did to the file.
    pub kind: ChangeKind,
}

/// A page of a file history.
#[derive(Debug, Clone)]
pub struct FileHistoryPage {
    /// The entries.
    pub entries: Vec<FileHistoryEntry>,
    /// The walk has no more commits.
    pub exhausted: bool,
}

// ───────────────────────────── config ─────────────────────────────

/// The effective signing configuration.
#[derive(Debug, Clone)]
pub struct SigningConfig {
    /// `commit.gpgsign`.
    pub commit_sign: bool,
    /// `tag.gpgsign`.
    pub tag_sign: bool,
    /// `gpg.format`.
    pub format: SigningFormat,
    /// `user.signingkey`.
    pub key: Option<String>,
    /// `gpg.program` (or the format-specific program).
    pub program: Option<String>,
}

/// The signature format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningFormat {
    /// OpenPGP.
    OpenPgp,
    /// SSH.
    Ssh,
    /// X.509.
    X509,
}

// ───────────────────────────── watch ─────────────────────────────

/// An event of the repository watcher.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// Something changed.
    Changed {
        /// What to reload.
        refresh: RefreshSet,
        /// The change happened while one of our own operations ran.
        during_own_operation: bool,
        /// A few of the changed paths, for diagnostics.
        sample_paths: Vec<PathBuf>,
    },
    /// The watcher failed.
    Error {
        /// What went wrong.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_version_parse_handles_platform_suffixes() {
        let v = |s: &str| GitVersion::parse(s);
        assert_eq!(
            v("git version 2.34.1\n"),
            Some(GitVersion {
                major: 2,
                minor: 34,
                patch: 1
            }),
            "plain Linux version"
        );
        assert_eq!(
            v("git version 2.43.0.windows.1"),
            Some(GitVersion {
                major: 2,
                minor: 43,
                patch: 0
            }),
            "Git for Windows suffix is ignored"
        );
        assert_eq!(
            v("git version 2.39.3 (Apple Git-146)"),
            Some(GitVersion {
                major: 2,
                minor: 39,
                patch: 3
            }),
            "Apple suffix is ignored"
        );
        assert_eq!(
            v("git version 2.50"),
            Some(GitVersion {
                major: 2,
                minor: 50,
                patch: 0
            }),
            "missing patch counts as 0"
        );
        assert_eq!(
            v("git version 2.51.0-rc1"),
            Some(GitVersion {
                major: 2,
                minor: 51,
                patch: 0
            }),
            "release candidate suffix is ignored"
        );
        assert_eq!(v("not git"), None, "garbage is rejected");
        assert_eq!(v(""), None, "empty output is rejected");
    }

    #[test]
    fn git_version_orders_and_displays() {
        assert!(
            GitVersion::parse("git version 2.30.0").expect("valid") < GitVersion::MIN,
            "2.30.0 is below the minimum"
        );
        assert_eq!(
            GitVersion::MIN.to_string(),
            "2.34.1",
            "display is major.minor.patch"
        );
    }

    #[test]
    fn cancel_token_is_shared_between_clones() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(token.check().is_ok(), "fresh token is not cancelled");
        clone.cancel();
        clone.cancel();
        assert!(token.is_cancelled(), "cancelling a clone cancels the token");
        assert!(
            matches!(token.check(), Err(GitError::Cancelled)),
            "check reports Cancelled"
        );
        assert!(
            token.as_atomic().load(Ordering::SeqCst),
            "the atomic handle shares the state"
        );
    }

    #[test]
    fn cancelled_future_wakes_after_cancel() {
        let token = CancelToken::new();
        let waiter = {
            let token = token.clone();
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("runtime")
                    .block_on(token.cancelled());
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        token.cancel();
        waiter.join().expect("the future completes once cancelled");
    }

    #[test]
    fn dropped_cancelled_futures_do_not_accumulate_wakers() {
        let token = CancelToken::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        for _ in 0..10 {
            // One command per iteration: wait for cancellation or completion, whichever comes first.
            runtime.block_on(async {
                tokio::select! {
                    _ = token.cancelled() => panic!("nobody cancelled"),
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                }
            });
        }
        assert_eq!(
            token.0.waiters.lock().len(),
            0,
            "every finished wait removed its waker from the long-lived token"
        );
        let pending = {
            let token = token.clone();
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("runtime")
                    .block_on(token.cancelled());
            })
        };
        while token.0.waiters.lock().is_empty() {
            std::thread::yield_now();
        }
        token.cancel();
        pending
            .join()
            .expect("a waiter registered after earlier ones were dropped is still woken");
    }

    #[test]
    fn refresh_set_all_covers_every_flag() {
        assert_eq!(
            RefreshSet::ALL,
            RefreshSet::all(),
            "ALL has exactly the bits of all declared flags"
        );
        assert!(
            RefreshSet::OPERATION_STATE.contains(RefreshSet::STATUS | RefreshSet::LOG),
            "operation state includes status and log"
        );
        assert!(
            !RefreshSet::OPERATION_STATE
                .intersects(RefreshSet::UNDO | RefreshSet::CONFIG | RefreshSet::REMOTES),
            "operation state does not claim config, remotes or undo"
        );
    }

    #[test]
    fn status_counts_is_clean() {
        let mut counts = StatusCounts::default();
        assert!(counts.is_clean(), "default counts are clean");
        counts.unstaged.untracked = 1;
        assert!(!counts.is_clean(), "an untracked file makes it dirty");
    }
}
