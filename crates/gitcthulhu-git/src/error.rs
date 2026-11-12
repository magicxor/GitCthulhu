//! The error model: one [`GitError`] for every public function, a copy-able [`GitErrorKind`] for matching, and the
//! classifier that turns a failed `git` command into the right error.

use std::path::PathBuf;

use crate::{
    GitVersion, InProgress, PushReport, RefreshSet, RelaPath,
    cli::{
        GitOutput,
        trace2::{HookClass, hook_class},
    },
};

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
    /// The operation was cancelled through its `CancelToken`.
    #[error("the operation was cancelled")]
    Cancelled,
    /// Content is larger than the limit the caller allowed (for example `diff::read_blob`'s `max_bytes`). The size is
    /// known without reading the content (object header, file metadata).
    ///
    /// Display limits do not use this error: `DiffOptions::max_file_bytes` (default 50 MiB) and
    /// `DiffOptions::max_lines` (default 1 000 000 lines per side) yield `DiffContent::TooLarge`, and partial
    /// operations refuse sides above those defaults with `PartialNotPossible { reason: PartialBlock::TooLarge }`.
    #[error("the content is too large: {size} bytes, the limit is {limit} bytes")]
    TooLarge {
        /// The size in bytes.
        size: u64,
        /// The limit in bytes.
        limit: u64,
    },
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
    Cancelled,
    TooLarge,
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
            GitError::Cancelled => K::Cancelled,
            GitError::TooLarge { .. } => K::TooLarge,
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

/// How much of stdout the classifier searches (Git prints `CONFLICT (…)`, `nothing to commit` on stdout).
const CLASSIFY_STDOUT_TAIL: usize = 64 * 1024;
/// How much of stdout `CommandFailed::stdout_tail` keeps.
const COMMAND_FAILED_STDOUT_TAIL: usize = 4 * 1024;

/// Pure: classify a failed command. Called by `cli::run_checked` only when `exit_code != 0` and the command did not
/// succeed-with-post-hook-warning (§1.11). `post` is `PostState::default()` for commands without a repository (clone).
///
/// The rules of §1.9 are tried in order and the first match wins. Matching is a case-insensitive substring search
/// over `stderr + "\n" + <last 64 KiB of stdout>`; Git's messages are English because the executor sets
/// `LANGUAGE=en` and `LC_MESSAGES=C`.
///
/// Rule 1 (cancelled) never reaches this function: `cli::run` returns `Err(Cancelled)` itself. Rules 11-13 (push
/// rejections) are evaluated by `network.rs` from the porcelain output before it calls this function.
pub fn classify(op: OpClass, out: &GitOutput, post: &PostState) -> GitError {
    let text = format!(
        "{}\n{}",
        out.stderr,
        lossy_tail(&out.stdout, CLASSIFY_STDOUT_TAIL)
    );
    let m = Matcher::new(&text);
    let stderr = || out.stderr.clone();

    // 2. A credential prompt was refused.
    if let Some(prompt) = &out.prompt_refused {
        return GitError::AuthCancelled {
            prompt: prompt.clone(),
        };
    }
    // 3. A pre hook failed (trace2).
    if let Some(hook) = out
        .hooks
        .iter()
        .find(|hook| hook.exit_code != Some(0) && hook_class(&hook.name) == HookClass::Pre)
    {
        return GitError::HookFailed {
            hook: hook.name.clone(),
            exit_code: hook.exit_code,
            output: combined_output(out),
        };
    }
    // 4. Git 2.36+, and the 2.34.2 backport (Ubuntu's 2.34.1): "unsafe repository ('<p>' is owned by someone else)".
    if let Some(at) = m
        .end_of("detected dubious ownership in repository at '")
        .or_else(|| m.end_of("unsafe repository ('"))
    {
        return GitError::UnsafeRepository {
            path: m.quoted_from(at).unwrap_or_default().into(),
        };
    }
    // 5.
    if let Some(at) = m.end_of("unable to create '")
        && let Some(path) = m.quoted_from(at)
        && m.lower[at + path.len()..].starts_with("': file exists")
    {
        return GitError::LockExists { path: path.into() };
    }
    // 6.
    let changed = m.contains("remote host identification has changed");
    if changed || m.contains("host key verification failed") {
        return GitError::HostKeyVerificationFailed {
            host: host_key_host(&m),
            changed,
            stderr: stderr(),
        };
    }
    // 7.
    if m.contains_any(&[
        "authentication failed for",
        "could not read username",
        "could not read password",
        "invalid username or password",
        "http basic: access denied",
        "permission denied (publickey",
    ]) || m
        .lines()
        .any(|(_, lower)| lower.contains("remote: permission to") && lower.contains("denied"))
    {
        let url = [
            "authentication failed for '",
            "could not read username for '",
            "could not read password for '",
        ]
        .iter()
        .find_map(|pat| m.quoted_from(m.end_of(pat)?))
        .map(str::to_owned);
        return GitError::AuthFailed {
            url,
            stderr: stderr(),
        };
    }
    // 8. and 9. `'<x>' does not appear to be a git repository`: a URL or path => 8, a bare remote name => 9.
    let not_a_repo_target = m
        .end_of("does not appear to be a git repository")
        .and_then(|end| {
            let (line, _) = m.line_at(end - 1);
            let quoted = line.split('\'').nth(1)?;
            Some(quoted.to_owned())
        });
    let looks_like_url = |s: &str| s.contains(['/', '\\', ':', '@']);
    if let Some(url) = m.end_of("repository '").and_then(|at| {
        let url = m.quoted_from(at)?;
        m.lower[at + url.len()..]
            .starts_with("' not found")
            .then(|| url.to_owned())
    }) {
        return GitError::RemoteRepositoryNotFound {
            url: Some(url),
            stderr: stderr(),
        };
    }
    if m.contains("repository not found")
        || not_a_repo_target.as_deref().is_some_and(looks_like_url)
    {
        return GitError::RemoteRepositoryNotFound {
            url: not_a_repo_target.filter(|t| looks_like_url(t)),
            stderr: stderr(),
        };
    }
    if let Some(at) = m.end_of("no such remote") {
        let (line, offset) = m.line_at(at);
        let rest = &line[at - offset..];
        let name = rest
            .split('\'')
            .nth(1)
            .unwrap_or_else(|| rest.trim_start_matches([':', ' ']).trim_end_matches('.'));
        return GitError::NoSuchRemote {
            name: name.to_owned(),
        };
    }
    if let Some(name) = not_a_repo_target {
        return GitError::NoSuchRemote { name };
    }
    // 10.
    const NETWORK: &[&str] = &[
        "could not resolve host",
        "could not resolve hostname",
        "connection timed out",
        "connection refused",
        "failed to connect to",
        "network is unreachable",
        "operation too slow",
        "early eof",
        "the remote end hung up unexpectedly",
    ];
    if let Some((line, _)) = m.lines().find(|(_, lower)| {
        NETWORK.iter().any(|pat| lower.contains(pat))
            || (lower.contains("unable to access")
                && ["ssl", "schannel", "openssl", "certificate"]
                    .iter()
                    .any(|pat| lower.contains(pat)))
    }) {
        return GitError::Network {
            reason: line.trim().to_owned(),
            stderr: stderr(),
        };
    }
    // 14.
    if let Some(at) = m.end_of("couldn't find remote ref ") {
        let (line, offset) = m.line_at(at);
        return GitError::RefNotFound {
            name: line[at - offset..].trim().to_owned(),
            stderr: stderr(),
        };
    }
    if let Some(at) = m.end_of("src refspec ")
        && let Some(len) = m.lower[at..].find(" does not match any")
    {
        return GitError::RefNotFound {
            name: m.text[at..at + len].to_owned(),
            stderr: stderr(),
        };
    }
    // 15.
    if let Some(err) = branch_checked_out_elsewhere(&m) {
        return err;
    }
    // 15a.
    if m.contains("need to specify how to reconcile divergent branches") {
        return GitError::PullStrategyRequired { stderr: stderr() };
    }
    // 15b.
    if m.contains_any(&[
        "there is no tracking information for the current branch",
        "has no upstream branch",
    ]) {
        return GitError::NoUpstream {
            branch: post.head_branch.clone(),
            stderr: stderr(),
        };
    }
    // 15c.
    let picking = matches!(
        post.in_progress,
        Some(
            InProgress::CherryPick
                | InProgress::CherryPickSequence
                | InProgress::Revert
                | InProgress::RevertSequence
        )
    );
    if matches!(op, OpClass::CherryPick | OpClass::Revert)
        && (m.contains("the previous cherry-pick is now empty")
            || (m.contains("nothing to commit") && picking))
    {
        return GitError::EmptyPick {
            commit: post.pick_head,
            stderr: stderr(),
        };
    }
    // 16. and 17.
    for (header, kind) in [
        (
            "your local changes to the following files would be overwritten by",
            OverwriteKind::LocalChanges,
        ),
        (
            "the following untracked working tree files would be overwritten by",
            OverwriteKind::Untracked,
        ),
    ] {
        if let Some(at) = m.end_of(header) {
            return GitError::WouldOverwrite {
                kind,
                paths: m.indented_lines_after(at),
                stderr: stderr(),
            };
        }
    }
    // 18.
    if m.contains_any(&[
        "you have unstaged changes",
        "your index contains uncommitted changes",
        "cannot rebase:",
        "please commit or stash them",
    ]) {
        return GitError::UncommittedChanges { stderr: stderr() };
    }
    // 19.
    if let Some(named) = operation_in_progress_message(&m)
        && let Some(kind) = post.in_progress.or(named)
    {
        return GitError::OperationInProgress {
            kind,
            stderr: stderr(),
        };
    }
    // 20. ("Merging/Cherry-picking/Committing is not possible because you have unmerged files" is how Git refuses
    // to start anything while conflicts are unresolved.)
    if !post.conflicted.is_empty()
        && m.contains_any(&[
            "conflict (",
            "merge conflict in",
            "could not apply",
            "you need to resolve your current index first",
            "is not possible because you have unmerged files",
        ])
    {
        return GitError::Conflicts {
            operation: post.in_progress,
            paths: post.conflicted.clone(),
            stderr: stderr(),
        };
    }
    // 21.
    if m.contains("refusing to merge unrelated histories") {
        return GitError::UnrelatedHistories { stderr: stderr() };
    }
    // 22.
    if m.contains("not possible to fast-forward") {
        return GitError::NotFastForwardable { stderr: stderr() };
    }
    // 23.
    if let Some(err) = already_exists(&m) {
        return err;
    }
    // 24.
    for (pat, what) in [
        ("is not a valid branch name", "branch"),
        ("is not a valid tag name", "tag"),
        ("is not a valid remote name", "remote"),
    ] {
        if let Some(at) = m.end_of(pat) {
            return GitError::InvalidName {
                what,
                name: m.first_quoted_on_line(at).unwrap_or_default().to_owned(),
            };
        }
    }
    // 25.
    if let Some(at) = m.end_of("is not fully merged") {
        return GitError::NotFullyMerged {
            branch: m.first_quoted_on_line(at).unwrap_or_default().to_owned(),
        };
    }
    // 26.
    if m.contains_any(&[
        "gpg failed to sign the data",
        "cannot run gpg",
        "error: couldn't load public key",
        "signing failed",
    ]) || (m.contains("ssh-keygen") && m.contains("failed"))
    {
        return GitError::SigningFailed { stderr: stderr() };
    }
    // 27.
    if m.contains_any(&[
        "author identity unknown",
        "please tell me who you are",
        "empty ident name",
    ]) {
        return GitError::IdentityUnknown { stderr: stderr() };
    }
    // 28.
    if m.contains_any(&[
        "nothing to commit",
        "nothing added to commit",
        "no changes added to commit",
    ]) {
        return GitError::NothingToCommit { stderr: stderr() };
    }
    // 29.
    if m.contains("aborting commit due to empty commit message") {
        return GitError::EmptyMessage;
    }
    // 30.
    if let Some(at) = m.end_of("not a git repository") {
        let (line, offset) = m.line_at(at);
        let rest = &line[at - offset..];
        let path = rest
            .split('\'')
            .nth(1)
            .or_else(|| rest.rsplit_once(": ").map(|(_, path)| path.trim()))
            .unwrap_or_default();
        return GitError::NotARepository { path: path.into() };
    }
    // 31.
    command_failed(out)
}

/// `CommandFailed` for `out`: arguments, exit code, stderr and the last 4 KiB of stdout.
pub(crate) fn command_failed(out: &GitOutput) -> GitError {
    GitError::CommandFailed {
        args: out.record.args.clone(),
        exit_code: out.exit_code,
        stderr: out.stderr.clone(),
        stdout_tail: lossy_tail(&out.stdout, COMMAND_FAILED_STDOUT_TAIL).into_owned(),
    }
}

/// stderr plus (the last 64 KiB of) stdout: the output of hooks, which cannot be separated from Git's own.
pub(crate) fn combined_output(out: &GitOutput) -> String {
    let stdout = lossy_tail(&out.stdout, CLASSIFY_STDOUT_TAIL);
    let mut combined = out.stderr.clone();
    if !combined.is_empty() && !stdout.is_empty() && !combined.ends_with('\n') {
        combined.push('\n');
    }
    combined.push_str(&stdout);
    combined
}

/// The last `max` bytes of `bytes` as lossy UTF-8.
fn lossy_tail(bytes: &[u8], max: usize) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(max)..])
}

/// Case-insensitive search over a text that keeps byte offsets valid for the original (ASCII lowercasing never
/// changes lengths).
struct Matcher<'a> {
    text: &'a str,
    lower: String,
}

impl<'a> Matcher<'a> {
    fn new(text: &'a str) -> Self {
        Matcher {
            text,
            lower: text.to_ascii_lowercase(),
        }
    }

    /// `pat` must be lowercase.
    fn contains(&self, pat: &str) -> bool {
        self.lower.contains(pat)
    }

    fn contains_any(&self, pats: &[&str]) -> bool {
        pats.iter().any(|pat| self.contains(pat))
    }

    /// The offset right after the first occurrence of `pat` (lowercase).
    fn end_of(&self, pat: &str) -> Option<usize> {
        self.lower.find(pat).map(|start| start + pat.len())
    }

    /// The original text from `at` up to the next `'` on the same line.
    fn quoted_from(&self, at: usize) -> Option<&'a str> {
        let rest = &self.text[at..];
        let end = rest.find(['\'', '\n'])?;
        (rest.as_bytes()[end] == b'\'').then(|| &rest[..end])
    }

    /// The original line containing offset `at` and the offset of its start.
    fn line_at(&self, at: usize) -> (&'a str, usize) {
        let start = self.text[..at].rfind('\n').map_or(0, |idx| idx + 1);
        let end = self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |idx| at + idx);
        (&self.text[start..end], start)
    }

    /// The first `'...'` on the line containing `at`.
    fn first_quoted_on_line(&self, at: usize) -> Option<&'a str> {
        let (line, _) = self.line_at(at);
        line.split('\'').nth(1)
    }

    /// Every line as (original, lowercase).
    fn lines(&self) -> impl Iterator<Item = (&'a str, &str)> {
        self.text.split('\n').zip(self.lower.split('\n'))
    }

    /// The tab-indented lines right after the line containing `at` (Git's path lists), unquoted.
    fn indented_lines_after(&self, at: usize) -> Vec<RelaPath> {
        let Some(nl) = self.text[at..].find('\n') else {
            return Vec::new();
        };
        self.text[at + nl + 1..]
            .split('\n')
            .map_while(|line| line.strip_prefix('\t'))
            .map(|path| unquote_c_style(path.trim_end_matches('\r')))
            .collect()
    }
}

/// Undo Git's C-style quoting of a path (`"a\tb"`, octal escapes for bytes); unquoted paths are returned as-is.
fn unquote_c_style(path: &str) -> RelaPath {
    let Some(inner) = path
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return path.into();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        let b = bytes[idx];
        idx += 1;
        if b != b'\\' || idx == bytes.len() {
            out.push(b);
            continue;
        }
        let escaped = bytes[idx];
        idx += 1;
        match escaped {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'0'..=b'3'
                if idx + 1 < bytes.len()
                    && bytes[idx].is_ascii_digit()
                    && bytes[idx + 1].is_ascii_digit() =>
            {
                let value =
                    (escaped - b'0') * 64 + (bytes[idx] - b'0') * 8 + (bytes[idx + 1] - b'0');
                out.push(value);
                idx += 2;
            }
            other => out.push(other),
        }
    }
    out.into()
}

/// The host named by OpenSSH's host key messages.
fn host_key_host(m: &Matcher<'_>) -> Option<String> {
    for (start, end) in [
        ("host key for ", " has changed"),
        ("host key is known for ", " and "),
    ] {
        if let Some(at) = m.end_of(start)
            && let Some(len) = m.lower[at..].find(end)
        {
            return Some(m.text[at..at + len].to_owned());
        }
    }
    None
}

/// Rule 15: the branch is checked out in (used by) another worktree.
fn branch_checked_out_elsewhere(m: &Matcher<'_>) -> Option<GitError> {
    const PATTERNS: &[&str] = &[
        "is already checked out at '",
        "is already used by worktree at '",
        "used by worktree at '",
        "checked out at '",
    ];
    let at = PATTERNS.iter().find_map(|pat| m.end_of(pat))?;
    let worktree = m.quoted_from(at)?;
    Some(GitError::BranchCheckedOutElsewhere {
        branch: m.first_quoted_on_line(at).unwrap_or_default().to_owned(),
        worktree: worktree.into(),
    })
}

/// Rule 19: `Some(kind named by the message)` if the message says an operation is in progress; the inner `None`
/// means the message did not name one Git operation.
fn operation_in_progress_message(m: &Matcher<'_>) -> Option<Option<InProgress>> {
    let named = |rest: &str| -> Option<InProgress> {
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix("an ")
            .or_else(|| rest.strip_prefix("a "))
            .unwrap_or(rest);
        [
            ("cherry-pick", InProgress::CherryPick),
            ("revert", InProgress::Revert),
            ("rebase", InProgress::Rebase),
            ("merge", InProgress::Merge),
            ("am ", InProgress::ApplyMailbox),
            ("bisect", InProgress::Bisect),
        ]
        .into_iter()
        .find_map(|(word, kind)| rest.starts_with(word).then_some(kind))
    };
    for pat in ["you have not concluded your ", "you are in the middle of "] {
        if let Some(at) = m.end_of(pat) {
            return Some(named(&m.lower[at..]));
        }
    }
    for (pat, kind) in [
        ("cherry-pick is already in progress", InProgress::CherryPick),
        ("revert is already in progress", InProgress::Revert),
        (
            "it seems that there is already a rebase-merge directory",
            InProgress::Rebase,
        ),
        ("rebase-apply directory", InProgress::Rebase),
    ] {
        if m.contains(pat) {
            return Some(Some(kind));
        }
    }
    None
}

/// Rule 23: a branch, tag, remote or clone target of that name exists.
fn already_exists(m: &Matcher<'_>) -> Option<GitError> {
    for (line, lower) in m.lines() {
        if !lower.contains("already exists") {
            continue;
        }
        let quoted = || line.split('\'').nth(1).map(str::to_owned);
        let (what, name) = if lower.contains("a branch named '") {
            ("branch", quoted()?)
        } else if lower.contains("tag '") {
            ("tag", quoted()?)
        } else if lower.contains("already exists and is not an empty directory") {
            ("directory", quoted()?)
        } else if let Some(idx) = lower.find("remote ") {
            let rest = &line[idx + "remote ".len()..];
            let name = rest.split_whitespace().next()?;
            ("remote", name.trim_matches('\'').to_owned())
        } else {
            continue;
        };
        return Some(GitError::AlreadyExists { what, name });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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

    fn output(stderr: &str, stdout: &str) -> GitOutput {
        GitOutput {
            exit_code: Some(1),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.into(),
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
        }
    }

    fn hook(name: &str, exit_code: i32) -> crate::HookRun {
        crate::HookRun {
            name: name.into(),
            exit_code: Some(exit_code),
            duration_ms: 1,
        }
    }

    fn generic(stderr: &str) -> GitError {
        classify(OpClass::Generic, &output(stderr, ""), &PostState::default())
    }

    #[test]
    fn pre_push_hook_failure_wins_over_push_failure_text() {
        let mut out = output(
            "lint failed\nerror: failed to push some refs to 'origin'\n",
            "",
        );
        out.hooks = vec![hook("pre-push", 1)];
        match classify(OpClass::Push, &out, &PostState::default()) {
            GitError::HookFailed {
                hook,
                exit_code,
                output,
            } => {
                assert_eq!(hook, "pre-push", "the failing hook is named");
                assert_eq!(exit_code, Some(1), "with its exit code");
                assert!(output.contains("lint failed"), "and the output: {output}");
            }
            other => panic!("rule 3 comes before any text rule: {other:?}"),
        }

        out.hooks = vec![hook("post-checkout", 1)];
        assert_ne!(
            classify(OpClass::Push, &out, &PostState::default()).kind(),
            GitErrorKind::HookFailed,
            "a failing post hook is not a HookFailed error"
        );
    }

    #[test]
    fn conflict_text_on_stdout_only_is_found() {
        let post = PostState {
            in_progress: Some(InProgress::Merge),
            conflicted: vec!["f.txt".into()],
            head_branch: Some("main".into()),
            pick_head: None,
        };
        let out = output(
            "",
            "Auto-merging f.txt\nCONFLICT (content): Merge conflict in f.txt\nAutomatic merge failed; fix conflicts and then commit the result.\n",
        );
        match classify(OpClass::Merge, &out, &post) {
            GitError::Conflicts {
                operation, paths, ..
            } => {
                assert_eq!(operation, Some(InProgress::Merge), "from the post state");
                assert_eq!(paths, ["f.txt"], "conflicted paths from the index");
            }
            other => panic!("expected conflicts: {other:?}"),
        }
        assert_eq!(
            classify(OpClass::Merge, &out, &PostState::default()).kind(),
            GitErrorKind::CommandFailed,
            "conflict texts without unmerged entries are not Conflicts"
        );
    }

    #[test]
    fn branch_delete_of_checked_out_branch_on_both_versions() {
        for stderr in [
            "error: Cannot delete branch 'main' checked out at '/tmp/repo'\n",
            "error: cannot delete branch 'main' used by worktree at '/tmp/repo'\n",
        ] {
            match generic(stderr) {
                GitError::BranchCheckedOutElsewhere { branch, worktree } => {
                    assert_eq!(branch, "main", "branch from {stderr:?}");
                    assert_eq!(worktree, Path::new("/tmp/repo"), "worktree from {stderr:?}");
                }
                other => panic!("{stderr:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn extracted_fields() {
        match generic(
            "fatal: Unable to create '/r/.git/index.lock': File exists.\n\nAnother git process seems to be running",
        ) {
            GitError::LockExists { path } => {
                assert_eq!(
                    path,
                    Path::new("/r/.git/index.lock"),
                    "the lock path keeps its case"
                )
            }
            other => panic!("{other:?}"),
        }
        match generic(
            "error: Your local changes to the following files would be overwritten by checkout:\n\ta.txt\n\t\"t\\303\\266\\tb.txt\"\nPlease commit your changes or stash them before you switch branches.\nAborting\n",
        ) {
            GitError::WouldOverwrite { kind, paths, .. } => {
                assert_eq!(kind, OverwriteKind::LocalChanges, "local changes");
                assert_eq!(
                    paths,
                    [RelaPath::from("a.txt"), RelaPath::from("tö\tb.txt")],
                    "quoted paths are unquoted to their bytes"
                );
            }
            other => panic!("{other:?}"),
        }
        match generic(
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled\n",
        ) {
            GitError::AuthFailed { url, .. } => {
                assert_eq!(
                    url.as_deref(),
                    Some("https://github.com"),
                    "url from the message"
                )
            }
            other => panic!("{other:?}"),
        }
        match generic(
            "fatal: 'origin' does not appear to be a git repository\nfatal: Could not read from remote repository.\n",
        ) {
            GitError::NoSuchRemote { name } => {
                assert_eq!(name, "origin", "a bare name is a remote")
            }
            other => panic!("{other:?}"),
        }
        match generic("fatal: '/no/such/path' does not appear to be a git repository\n") {
            GitError::RemoteRepositoryNotFound { url, .. } => {
                assert_eq!(
                    url.as_deref(),
                    Some("/no/such/path"),
                    "a path is a repository URL"
                )
            }
            other => panic!("{other:?}"),
        }
        match generic(
            "No ED25519 host key is known for github.com and you have requested strict checking.\nHost key verification failed.\n",
        ) {
            GitError::HostKeyVerificationFailed { host, changed, .. } => {
                assert_eq!(host.as_deref(), Some("github.com"), "host from the message");
                assert!(!changed, "an unknown key did not change");
            }
            other => panic!("{other:?}"),
        }
        match generic("fatal: a branch named 'main' already exists\n") {
            GitError::AlreadyExists { what, name } => {
                assert_eq!((what, name.as_str()), ("branch", "main"), "branch and name")
            }
            other => panic!("{other:?}"),
        }
        match generic("error: remote origin already exists.\n") {
            GitError::AlreadyExists { what, name } => {
                assert_eq!(
                    (what, name.as_str()),
                    ("remote", "origin"),
                    "remote and name"
                )
            }
            other => panic!("{other:?}"),
        }
        match generic(
            "error: src refspec nosuch does not match any\nerror: failed to push some refs to 'x'\n",
        ) {
            GitError::RefNotFound { name, .. } => assert_eq!(name, "nosuch", "refspec name"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn operation_in_progress_kind_falls_back_to_message() {
        match generic("fatal: You have not concluded your cherry-pick (CHERRY_PICK_HEAD exists).\n")
        {
            GitError::OperationInProgress { kind, .. } => {
                assert_eq!(kind, InProgress::CherryPick, "named by the message")
            }
            other => panic!("{other:?}"),
        }
        match generic(
            "error: cherry-pick is already in progress
hint: try \"git cherry-pick (--continue | --abort | --quit)\"
fatal: cherry-pick failed
",
        ) {
            GitError::OperationInProgress { kind, .. } => {
                assert_eq!(
                    kind,
                    InProgress::CherryPick,
                    "a running cherry-pick sequence"
                )
            }
            other => panic!("{other:?}"),
        }
        let post = PostState {
            in_progress: Some(InProgress::RebaseInteractive),
            ..PostState::default()
        };
        match classify(
            OpClass::Merge,
            &output("You are in the middle of a rebase -- cannot amend.\n", ""),
            &post,
        ) {
            GitError::OperationInProgress { kind, .. } => assert_eq!(
                kind,
                InProgress::RebaseInteractive,
                "the post state is more precise than the message"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn refused_prompt_is_auth_cancelled() {
        let mut out = output("fatal: Authentication failed for 'https://x'\n", "");
        out.prompt_refused = Some("Password for 'https://x': ".into());
        assert_eq!(
            classify(OpClass::Fetch, &out, &PostState::default()).kind(),
            GitErrorKind::AuthCancelled,
            "rule 2 comes before rule 7"
        );
    }

    #[test]
    fn unquote_handles_escapes() {
        assert_eq!(unquote_c_style("plain"), "plain", "unquoted stays");
        assert_eq!(
            unquote_c_style(r#""a\"b\\c\n""#),
            "a\"b\\c\n",
            "quote, backslash and newline escapes"
        );
        assert_eq!(
            unquote_c_style(r#""\377""#),
            RelaPath::from(vec![0xff]),
            "octal escapes are raw bytes"
        );
    }

    #[test]
    fn unmatched_failure_reports_command_failed_with_stdout_tail() {
        let out = output("fatal: x", &"a".repeat(5000));
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
