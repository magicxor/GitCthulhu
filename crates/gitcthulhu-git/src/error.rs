//! The error model: one [`GitError`] for every public function, a copy-able [`GitErrorKind`] for matching, and the
//! classifier that turns a failed `git` command into the right error.

use std::path::PathBuf;

use crate::{GitVersion, InProgress, PushReport, RefreshSet, RelaPath, cli::GitOutput};

/// One error type for every public function. It is not `Clone` (it can hold `io::Error`). It is
/// `#[non_exhaustive]`; the GUI matches on [`GitError::kind()`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    // ── environment ──
    /// The `git` executable could not be started.
    #[error("Git was not found. Install Git 2.34 or newer, or set its path in Settings.")]
    GitNotFound {
        /// Where it was looked for.
        searched: String,
    },
    /// The installed Git is older than [`GitVersion::MIN`].
    #[error("Git {found} is too old; GitCthulhu needs Git {required} or newer.")]
    GitTooOld {
        /// The installed version.
        found: GitVersion,
        /// The minimum version.
        required: GitVersion,
    },
    /// The path is not (inside) a Git repository.
    #[error("'{}' is not a Git repository", path.display())]
    NotARepository {
        /// The path that was opened.
        path: PathBuf,
    },
    /// The repository is owned by someone else and not listed in `safe.directory`.
    #[error("Git refuses to work in '{}' because it is owned by someone else (safe.directory)", path.display())]
    UnsafeRepository {
        /// The repository path.
        path: PathBuf,
    },
    /// The operation needs a work tree.
    #[error("this operation is not possible in a bare repository")]
    BareRepository,
    /// The operation needs a commit but HEAD is unborn.
    #[error("the repository has no commits yet")]
    UnbornHead,
    /// The repository or request uses something this version cannot handle.
    #[error("not supported: {what}")]
    Unsupported {
        /// What is not supported.
        what: &'static str,
    },
    /// Stub bodies during development (WP-B0a).
    #[error("not implemented yet: {op}")]
    NotImplemented {
        /// `<module>::<function>`.
        op: &'static str,
    },
    /// The operation was cancelled through its `CancelToken`.
    #[error("the operation was cancelled")]
    Cancelled,
    // ── auth / network ──
    /// The remote rejected the credentials.
    #[error("authentication failed for {url}", url = url.as_deref().unwrap_or("the remote"))]
    AuthFailed {
        /// The remote URL, if known.
        url: Option<String>,
        /// Raw stderr.
        stderr: String,
    },
    /// The user cancelled a credential prompt.
    #[error("authentication was cancelled")]
    AuthCancelled {
        /// The prompt that was refused.
        prompt: String,
    },
    /// The SSH host key could not be verified.
    #[error("the host key of {host} could not be verified", host = host.as_deref().unwrap_or("the remote host"))]
    HostKeyVerificationFailed {
        /// The host, if known.
        host: Option<String>,
        /// The key changed (as opposed to being unknown).
        changed: bool,
        /// Raw stderr.
        stderr: String,
    },
    /// A network failure.
    #[error("network error: {reason}")]
    Network {
        /// The first matching line of stderr.
        reason: String,
        /// Raw stderr.
        stderr: String,
    },
    /// The remote repository does not exist (or is not visible).
    #[error("the remote repository was not found: {url}", url = url.as_deref().unwrap_or("(unknown URL)"))]
    RemoteRepositoryNotFound {
        /// The remote URL, if known.
        url: Option<String>,
        /// Raw stderr.
        stderr: String,
    },
    /// The named remote is not configured.
    #[error("no such remote: {name}")]
    NoSuchRemote {
        /// The remote's name.
        name: String,
    },
    /// A ref does not exist on the remote.
    #[error("the remote ref {name} was not found")]
    RefNotFound {
        /// The ref's name.
        name: String,
        /// Raw stderr.
        stderr: String,
    },
    // The three push rejections carry the full porcelain report: refs that *did* update are in
    // `report.updates` with a non-`Rejected` status (partial success; the GUI records an oplog marker for them).
    /// The push was rejected: the remote has commits we don't have.
    #[error("the push was rejected because the remote branch has commits you don't have")]
    NonFastForward {
        /// The rejected remote refs.
        refs: Vec<String>,
        /// The full push report.
        report: PushReport,
        /// Raw stderr.
        stderr: String,
    },
    /// `--force-with-lease` rejected the push.
    #[error(
        "force-with-lease rejected: the remote branch changed since your last fetch or contains commits you never integrated"
    )]
    LeaseRejected {
        /// The rejected remote refs.
        refs: Vec<String>,
        /// The full push report.
        report: PushReport,
        /// Raw stderr.
        stderr: String,
    },
    /// The remote (hook, branch protection, ...) rejected the push.
    #[error("the remote rejected the push: {reason}")]
    RemoteRejected {
        /// The rejected remote refs.
        refs: Vec<String>,
        /// The remote's reason.
        reason: String,
        /// The full push report.
        report: PushReport,
        /// Raw stderr.
        stderr: String,
    },
    /// The branches diverged and no pull strategy is configured.
    #[error("the branches have diverged; choose merge or rebase for this pull")]
    PullStrategyRequired {
        /// Raw stderr.
        stderr: String,
    },
    /// The current branch has no upstream.
    #[error("the current branch has no upstream branch")]
    NoUpstream {
        /// The current branch, if any.
        branch: Option<String>,
        /// Raw stderr.
        stderr: String,
    },
    // ── working tree / state ──
    /// `operation = None`: conflicts without an in-progress state (`stash apply/pop` never uses this error, see
    /// `StashApplyResult`; `switch -m` / `checkout -m` do).
    #[error("the operation stopped with conflicts")]
    Conflicts {
        /// The operation that is now in progress.
        operation: Option<InProgress>,
        /// The conflicted paths.
        paths: Vec<RelaPath>,
        /// Raw stderr.
        stderr: String,
    },
    /// A cherry-pick or revert would produce an empty commit.
    #[error("the cherry-pick/revert of {commit:?} resulted in no changes")]
    EmptyPick {
        /// The commit, if known.
        commit: Option<gix::ObjectId>,
        /// Raw stderr.
        stderr: String,
    },
    /// Local changes or untracked files would be overwritten.
    #[error("local changes would be overwritten")]
    WouldOverwrite {
        /// What would be overwritten.
        kind: OverwriteKind,
        /// The affected paths.
        paths: Vec<RelaPath>,
        /// Raw stderr.
        stderr: String,
    },
    /// The operation needs a clean worktree or index.
    #[error("you have uncommitted changes")]
    UncommittedChanges {
        /// Raw stderr.
        stderr: String,
    },
    /// Another operation is already in progress.
    #[error("a {kind:?} is already in progress")]
    OperationInProgress {
        /// Which operation.
        kind: InProgress,
        /// Raw stderr.
        stderr: String,
    },
    /// `--continue`/`--abort` without an operation in progress.
    #[error("no {expected:?} is in progress")]
    NoOperationInProgress {
        /// The operation that was expected.
        expected: Option<InProgress>,
    },
    /// Nothing is staged.
    #[error("nothing to commit")]
    NothingToCommit {
        /// Raw stderr.
        stderr: String,
    },
    /// The commit message is empty.
    #[error("the commit message is empty")]
    EmptyMessage,
    /// The histories have no common ancestor.
    #[error("refusing to merge unrelated histories")]
    UnrelatedHistories {
        /// Raw stderr.
        stderr: String,
    },
    /// `--ff-only` and a fast-forward is impossible.
    #[error("not possible to fast-forward")]
    NotFastForwardable {
        /// Raw stderr.
        stderr: String,
    },
    /// The diff a selection was made on no longer matches the file.
    #[error("the diff is out of date for '{path}'; refresh and try again")]
    StaleDiff {
        /// The path.
        path: RelaPath,
    },
    /// The change can only be staged as a whole file.
    #[error("this change cannot be staged partially: {reason}")]
    PartialNotPossible {
        /// The path.
        path: RelaPath,
        /// Why.
        reason: PartialBlock,
    },
    /// The rebase plan no longer matches the repository.
    #[error("the interactive rebase plan is out of date: {detail}")]
    RebasePlanStale {
        /// What differs.
        detail: String,
    },
    /// The stash entry is gone.
    #[error("stash {id} no longer exists")]
    StashNotFound {
        /// The stash commit.
        id: gix::ObjectId,
    },
    // ── refs / names ──
    /// The branch is checked out in another worktree.
    #[error("'{branch}' is checked out in another worktree at '{}'", worktree.display())]
    BranchCheckedOutElsewhere {
        /// The branch.
        branch: String,
        /// The worktree that has it.
        worktree: PathBuf,
    },
    /// A branch, tag or remote of that name exists.
    #[error("{what} '{name}' already exists")]
    AlreadyExists {
        /// "branch", "tag", "remote", ...
        what: &'static str,
        /// The name.
        name: String,
    },
    /// The name is not valid for that kind of ref.
    #[error("'{name}' is not a valid {what} name")]
    InvalidName {
        /// "branch", "tag", "remote", ...
        what: &'static str,
        /// The name.
        name: String,
    },
    /// Deleting a branch that has unmerged commits.
    #[error("branch '{branch}' is not fully merged")]
    NotFullyMerged {
        /// The branch.
        branch: String,
    },
    /// Something does not exist.
    #[error("'{name}' does not exist")]
    NotFound {
        /// "file", "commit", ...
        what: &'static str,
        /// The name.
        name: String,
    },
    // ── commit specifics ──
    /// A pre hook failed.
    #[error("the {hook} hook failed")]
    HookFailed {
        /// The hook's name.
        hook: String,
        /// Its exit code.
        exit_code: Option<i32>,
        /// stderr + stdout of the git command.
        output: String,
    },
    /// Signing failed.
    #[error("signing failed")]
    SigningFailed {
        /// Raw stderr.
        stderr: String,
    },
    /// `user.name` / `user.email` are not set.
    #[error("Git doesn't know who you are (user.name / user.email)")]
    IdentityUnknown {
        /// Raw stderr.
        stderr: String,
    },
    // ── infrastructure ──
    /// A lock file of another Git process exists.
    #[error("another Git process is running ('{}' exists)", path.display())]
    LockExists {
        /// The lock file.
        path: PathBuf,
    },
    /// Any other failed git command.
    #[error("git {args} failed with exit code {exit_code:?}")]
    CommandFailed {
        /// Sanitised arguments.
        args: String,
        /// The exit code, `None` if killed by a signal.
        exit_code: Option<i32>,
        /// Raw stderr.
        stderr: String,
        /// The last 4 KiB of stdout.
        stdout_tail: String,
    },
    /// An I/O error on a path.
    #[error("'{}': {source}", path.display())]
    Io {
        /// The path.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// An error of gix.
    #[error(transparent)]
    Gix(#[from] gix::Error),
    /// Anything else.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// What an operation would overwrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwriteKind {
    /// Modified tracked files.
    LocalChanges,
    /// Untracked files.
    Untracked,
}

/// Why a change can only be staged as a whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialBlock {
    /// Binary content.
    Binary,
    /// A side exceeds the size threshold.
    TooLarge,
    /// A textconv filter produced the text.
    TextConv,
    /// Git LFS pointer.
    Lfs,
    /// Submodule.
    Submodule,
    /// Symbolic link.
    Symlink,
    /// The entry kind changed.
    TypeChange,
    /// Only the mode changed.
    ModeOnly,
    /// A rename.
    Rename,
    /// A conflicted file.
    Conflict,
    /// The diff ignores whitespace.
    WhitespaceIgnored,
    /// The worktree round trip is not lossless.
    UnsafeRoundTrip,
    /// Non-UTF-8 content with mixed line endings.
    NotUtf8LineEndingsMixed,
}

impl std::fmt::Display for PartialBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PartialBlock::Binary => "the file is binary",
            PartialBlock::TooLarge => "the file is too large",
            PartialBlock::TextConv => "the text comes from a textconv filter",
            PartialBlock::Lfs => "the file is stored with Git LFS",
            PartialBlock::Submodule => "it is a submodule",
            PartialBlock::Symlink => "it is a symbolic link",
            PartialBlock::TypeChange => "the file type changed",
            PartialBlock::ModeOnly => "only the file mode changed",
            PartialBlock::Rename => "the file was renamed",
            PartialBlock::Conflict => "the file has conflicts",
            PartialBlock::WhitespaceIgnored => "whitespace changes are hidden",
            PartialBlock::UnsafeRoundTrip => {
                "the change cannot be written back to the file losslessly"
            }
            PartialBlock::NotUtf8LineEndingsMixed => {
                "the file is not UTF-8 and has mixed line endings"
            }
        })
    }
}

/// Copy-able discriminant for exhaustive GUI matching (one variant per [`GitError`] variant, same names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)] // One variant per `GitError` variant; see there.
pub enum GitErrorKind {
    GitNotFound,
    GitTooOld,
    NotARepository,
    UnsafeRepository,
    BareRepository,
    UnbornHead,
    Unsupported,
    NotImplemented,
    Cancelled,
    AuthFailed,
    AuthCancelled,
    HostKeyVerificationFailed,
    Network,
    RemoteRepositoryNotFound,
    NoSuchRemote,
    RefNotFound,
    NonFastForward,
    LeaseRejected,
    RemoteRejected,
    PullStrategyRequired,
    NoUpstream,
    Conflicts,
    EmptyPick,
    WouldOverwrite,
    UncommittedChanges,
    OperationInProgress,
    NoOperationInProgress,
    NothingToCommit,
    EmptyMessage,
    UnrelatedHistories,
    NotFastForwardable,
    StaleDiff,
    PartialNotPossible,
    RebasePlanStale,
    StashNotFound,
    BranchCheckedOutElsewhere,
    AlreadyExists,
    InvalidName,
    NotFullyMerged,
    NotFound,
    HookFailed,
    SigningFailed,
    IdentityUnknown,
    LockExists,
    CommandFailed,
    Io,
    Gix,
    Other,
}

impl GitError {
    /// The copy-able discriminant of this error.
    pub fn kind(&self) -> GitErrorKind {
        use GitErrorKind as K;
        match self {
            GitError::GitNotFound { .. } => K::GitNotFound,
            GitError::GitTooOld { .. } => K::GitTooOld,
            GitError::NotARepository { .. } => K::NotARepository,
            GitError::UnsafeRepository { .. } => K::UnsafeRepository,
            GitError::BareRepository => K::BareRepository,
            GitError::UnbornHead => K::UnbornHead,
            GitError::Unsupported { .. } => K::Unsupported,
            GitError::NotImplemented { .. } => K::NotImplemented,
            GitError::Cancelled => K::Cancelled,
            GitError::AuthFailed { .. } => K::AuthFailed,
            GitError::AuthCancelled { .. } => K::AuthCancelled,
            GitError::HostKeyVerificationFailed { .. } => K::HostKeyVerificationFailed,
            GitError::Network { .. } => K::Network,
            GitError::RemoteRepositoryNotFound { .. } => K::RemoteRepositoryNotFound,
            GitError::NoSuchRemote { .. } => K::NoSuchRemote,
            GitError::RefNotFound { .. } => K::RefNotFound,
            GitError::NonFastForward { .. } => K::NonFastForward,
            GitError::LeaseRejected { .. } => K::LeaseRejected,
            GitError::RemoteRejected { .. } => K::RemoteRejected,
            GitError::PullStrategyRequired { .. } => K::PullStrategyRequired,
            GitError::NoUpstream { .. } => K::NoUpstream,
            GitError::Conflicts { .. } => K::Conflicts,
            GitError::EmptyPick { .. } => K::EmptyPick,
            GitError::WouldOverwrite { .. } => K::WouldOverwrite,
            GitError::UncommittedChanges { .. } => K::UncommittedChanges,
            GitError::OperationInProgress { .. } => K::OperationInProgress,
            GitError::NoOperationInProgress { .. } => K::NoOperationInProgress,
            GitError::NothingToCommit { .. } => K::NothingToCommit,
            GitError::EmptyMessage => K::EmptyMessage,
            GitError::UnrelatedHistories { .. } => K::UnrelatedHistories,
            GitError::NotFastForwardable { .. } => K::NotFastForwardable,
            GitError::StaleDiff { .. } => K::StaleDiff,
            GitError::PartialNotPossible { .. } => K::PartialNotPossible,
            GitError::RebasePlanStale { .. } => K::RebasePlanStale,
            GitError::StashNotFound { .. } => K::StashNotFound,
            GitError::BranchCheckedOutElsewhere { .. } => K::BranchCheckedOutElsewhere,
            GitError::AlreadyExists { .. } => K::AlreadyExists,
            GitError::InvalidName { .. } => K::InvalidName,
            GitError::NotFullyMerged { .. } => K::NotFullyMerged,
            GitError::NotFound { .. } => K::NotFound,
            GitError::HookFailed { .. } => K::HookFailed,
            GitError::SigningFailed { .. } => K::SigningFailed,
            GitError::IdentityUnknown { .. } => K::IdentityUnknown,
            GitError::LockExists { .. } => K::LockExists,
            GitError::CommandFailed { .. } => K::CommandFailed,
            GitError::Io { .. } => K::Io,
            GitError::Gix(_) => K::Gix,
            GitError::Other(_) => K::Other,
        }
    }

    /// Raw stderr if this error came from a git child (for the "Details" expander).
    pub fn stderr(&self) -> Option<&str> {
        match self {
            GitError::AuthFailed { stderr, .. }
            | GitError::HostKeyVerificationFailed { stderr, .. }
            | GitError::Network { stderr, .. }
            | GitError::RemoteRepositoryNotFound { stderr, .. }
            | GitError::RefNotFound { stderr, .. }
            | GitError::NonFastForward { stderr, .. }
            | GitError::LeaseRejected { stderr, .. }
            | GitError::RemoteRejected { stderr, .. }
            | GitError::PullStrategyRequired { stderr }
            | GitError::NoUpstream { stderr, .. }
            | GitError::Conflicts { stderr, .. }
            | GitError::EmptyPick { stderr, .. }
            | GitError::WouldOverwrite { stderr, .. }
            | GitError::UncommittedChanges { stderr }
            | GitError::OperationInProgress { stderr, .. }
            | GitError::NothingToCommit { stderr }
            | GitError::UnrelatedHistories { stderr }
            | GitError::NotFastForwardable { stderr }
            | GitError::SigningFailed { stderr }
            | GitError::IdentityUnknown { stderr }
            | GitError::CommandFailed { stderr, .. } => Some(stderr),
            GitError::HookFailed { output, .. } => Some(output),
            _ => None,
        }
    }

    /// Refresh categories the GUI must reload even though the operation failed.
    ///
    /// - `Conflicts`, `EmptyPick`, `OperationInProgress` and `CommandFailed` may have left an operation half-way:
    ///   [`RefreshSet::OPERATION_STATE`] (`OperationInProgress`: only `REPO_STATE`, nothing was changed).
    /// - `HookFailed`: `STATUS` (a pre-commit hook may have reformatted files).
    /// - A push rejection with any non-rejected update: `REMOTES | BRANCHES | LOG`.
    /// - `StashNotFound`: `STASHES`.
    /// - Everything else (including `Network`): empty.
    pub fn refresh_on_failure(&self) -> RefreshSet {
        use crate::PushRefStatus;
        let partial_push = |report: &PushReport| {
            if report
                .updates
                .iter()
                .any(|u| u.status != PushRefStatus::Rejected)
            {
                RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::LOG
            } else {
                RefreshSet::empty()
            }
        };
        match self {
            GitError::Conflicts { .. }
            | GitError::EmptyPick { .. }
            | GitError::CommandFailed { .. } => RefreshSet::OPERATION_STATE,
            GitError::OperationInProgress { .. } => RefreshSet::REPO_STATE,
            GitError::HookFailed { .. } => RefreshSet::STATUS,
            GitError::StashNotFound { .. } => RefreshSet::STASHES,
            GitError::NonFastForward { report, .. }
            | GitError::LeaseRejected { report, .. }
            | GitError::RemoteRejected { report, .. } => partial_push(report),
            _ => RefreshSet::empty(),
        }
    }

    /// True for failures a retry may fix without user action (`Network`, `LockExists`).
    pub fn is_transient(&self) -> bool {
        matches!(self, GitError::Network { .. } | GitError::LockExists { .. })
    }

    /// An I/O error on `path`.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        GitError::Io {
            path: path.into(),
            source,
        }
    }
}

/// The result type of every public function.
pub type Result<T, E = GitError> = std::result::Result<T, E>;

/// Which operation a git command belongs to; selects the operation-specific rules of the classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // The names are the git commands they belong to.
pub enum OpClass {
    Generic,
    Commit,
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Stash,
    Checkout,
    Fetch,
    Pull,
    Push,
    Clone,
    Submodule,
    Worktree,
    Tag,
    Branch,
    Remote,
    Config,
    Reset,
    Restore,
}

/// Repository state right after a failed command, computed by `internal::post_state::read(repo)` (WP-B0b):
/// `gix::Repository::state()`, one index scan for entries with stage != 0, HEAD's short branch name, and the id in
/// `CHERRY_PICK_HEAD` / `REVERT_HEAD` (per-worktree git dir). Cheap (no worktree access).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostState {
    /// The operation that is in progress.
    pub in_progress: Option<InProgress>,
    /// Paths with unmerged index entries.
    pub conflicted: Vec<RelaPath>,
    /// HEAD's short branch name.
    pub head_branch: Option<String>,
    /// The id in `CHERRY_PICK_HEAD` / `REVERT_HEAD`.
    pub pick_head: Option<gix::ObjectId>,
}

/// Pure: classify a failed command. Called by `cli::run_checked` only when `exit_code != 0` and the command did not
/// succeed-with-post-hook-warning (§1.11). `post` is `PostState::default()` for commands without a repository (clone).
///
/// Stub (WP-B0b implements the table of §1.9): every failure is a `CommandFailed` carrying the arguments, exit code,
/// stderr and the last 4 KiB of stdout.
pub fn classify(op: OpClass, out: &GitOutput, post: &PostState) -> GitError {
    let _ = (op, post);
    const TAIL: usize = 4 * 1024;
    let start = out.stdout.len().saturating_sub(TAIL);
    GitError::CommandFailed {
        args: out.record.args.clone(),
        exit_code: out.exit_code,
        stderr: out.stderr.clone(),
        stdout_tail: String::from_utf8_lossy(&out.stdout[start..]).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PushRefStatus, PushedRefUpdate};

    fn report(status: PushRefStatus) -> PushReport {
        PushReport {
            remote: "origin".into(),
            url: None,
            updates: vec![PushedRefUpdate {
                local: None,
                remote_ref: "refs/heads/x".try_into().expect("valid ref"),
                old: None,
                new: None,
                status,
                summary: String::new(),
            }],
        }
    }

    #[test]
    fn push_rejection_refreshes_only_when_something_was_updated() {
        let rejected = GitError::NonFastForward {
            refs: vec![],
            report: report(PushRefStatus::Rejected),
            stderr: String::new(),
        };
        assert_eq!(
            rejected.refresh_on_failure(),
            RefreshSet::empty(),
            "nothing changed when every ref was rejected"
        );
        let partial = GitError::LeaseRejected {
            refs: vec![],
            report: report(PushRefStatus::FastForward),
            stderr: String::new(),
        };
        assert_eq!(
            partial.refresh_on_failure(),
            RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::LOG,
            "a partially successful push updated remote-tracking refs"
        );
    }

    #[test]
    fn kind_stderr_and_transience() {
        let err = GitError::Network {
            reason: "x".into(),
            stderr: "boom".into(),
        };
        assert_eq!(
            err.kind(),
            GitErrorKind::Network,
            "kind mirrors the variant"
        );
        assert_eq!(err.stderr(), Some("boom"), "stderr is exposed");
        assert!(err.is_transient(), "network failures are transient");
        assert_eq!(
            err.refresh_on_failure(),
            RefreshSet::empty(),
            "network failures refresh nothing"
        );
        assert!(
            !GitError::Cancelled.is_transient(),
            "cancellation is not transient"
        );
        assert_eq!(GitError::Cancelled.stderr(), None, "no child, no stderr");
        let hook = GitError::HookFailed {
            hook: "pre-commit".into(),
            exit_code: Some(1),
            output: "lint failed".into(),
        };
        assert_eq!(
            hook.stderr(),
            Some("lint failed"),
            "hook output is the details"
        );
        assert_eq!(
            hook.refresh_on_failure(),
            RefreshSet::STATUS,
            "hooks may rewrite files"
        );
    }

    #[test]
    fn stub_classifier_reports_command_failed_with_stdout_tail() {
        let out = GitOutput {
            exit_code: Some(1),
            stdout: vec![b'a'; 5000],
            stderr: "fatal: x".into(),
            hooks: vec![],
            record: crate::CommandRecord {
                args: "status".into(),
                exit_code: Some(1),
                duration: std::time::Duration::ZERO,
                stderr: String::new(),
            },
            prompt_refused: None,
            removed_stale_locks: vec![],
            warnings: vec![],
        };
        match classify(OpClass::Generic, &out, &PostState::default()) {
            GitError::CommandFailed {
                args,
                exit_code,
                stdout_tail,
                ..
            } => {
                assert_eq!(args, "status", "arguments are carried over");
                assert_eq!(exit_code, Some(1), "exit code is carried over");
                assert_eq!(stdout_tail.len(), 4096, "the tail is capped at 4 KiB");
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }
}
