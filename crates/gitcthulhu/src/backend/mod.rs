//! The backend seam (spec 21 §2.4).
//!
//! `src/backend/**` is the only place that names `gitcthulhu_git`, `gitcthulhu_oplog`, `gitcthulhu_services`, `gix`
//! and `but_secret` (the unit test `backend_seam_is_respected` enforces it). Everything a region needs from the
//! backend crates is re-exported here; a region that needs a new backend call adds it to its own
//! `backend/<region>.rs`. Every backend call runs on a background thread through [`bg`], never on the UI thread.

use std::sync::Arc;

use gpui_kit::{App, Task};

use crate::settings::{ProxyType, Settings};

pub mod handle;
pub mod loaders;
pub mod oplog;
pub mod present;
pub mod prompt;
pub mod proxy_secret;
pub mod services;

pub mod details;
pub mod diff;
pub mod history;
pub mod log;
pub mod rebase;
pub mod status;
pub mod validate;

// Milestone-2 stubs; their bodies are behind `cfg(feature = "services")`.
pub mod ai;
pub mod forge;
pub mod update;

/// The backend's mutation modules, called by the operation catalogue (`tab/ops/*.rs`) inside `OpSpec::run`
/// closures. Reads go through `loaders` and the region files instead.
pub mod git {
    pub use gitcthulhu_git::{
        branch, commit, config, merge, network, rebase, remote, stage, stash, submodule, tag,
        worktree,
    };
}

pub use gitcthulhu_git::{
    // runtime, repository, credentials, locking
    Against,
    AheadBehind,
    BlameChunk,
    BlameCommit,
    BlameOptions,
    BlameResult,
    BlameRev,
    BlobSide,
    CancelToken,
    ChangeKind,
    CheckoutOptions,
    CloneOptions,
    CommandLog,
    CommandRecord,
    CommitDetails,
    CommitFile,
    CommitFilesOptions,
    CommitInfo,
    CommitOptions,
    CommitOutcome,
    ConfigScope,
    ConflictInfo,
    ConflictKind,
    ContinueOutcome,
    CredentialPrompter,
    CredentialRequest,
    CredentialResponse,
    Decorations,
    DiffAlgorithm,
    DiffContent,
    DiffLine,
    DiffOptions,
    DiffTarget,
    DiscardMode,
    EntryKind,
    EolStyle,
    FastForward,
    FetchOptions,
    FetchReport,
    FetchTarget,
    FileDiff,
    FileHistoryEntry,
    FileHistoryPage,
    FileHistoryWalker,
    FileStamp,
    FileStat,
    ForceMode,
    ForgeRemotes,
    GitError,
    GitErrorKind,
    GitRuntime,
    GitTime,
    GitVersion,
    GraphBuilder,
    GraphLayout,
    HeadState,
    HelperStatus,
    HookRun,
    HostKeyPolicy,
    Hunk,
    HunkHeader,
    Identity,
    IdentityInfo,
    ImageData,
    InProgress,
    KindCounts,
    LineKind,
    LineRef,
    LocalBranch,
    LogOptions,
    LogPage,
    LogTips,
    LogWalker,
    MergeOptions,
    MergeOut,
    MergeOutcome,
    MergeOutcomeOrRebase,
    MergeState,
    NoProgress,
    NoPrompter,
    NodeKind,
    OpContext,
    OpOutcome,
    OpWarning,
    OverwriteKind,
    PartialAvailability,
    PartialBlock,
    PartialIdentity,
    PatchText,
    PatchTextOptions,
    Person,
    PickOptions,
    Progress,
    ProgressSink,
    PromptKind,
    PullMode,
    PullOptions,
    PushOptions,
    PushRefSpec,
    PushRefStatus,
    PushReport,
    PushedRefUpdate,
    RebaseAction,
    RebaseBackend,
    RebaseOptions,
    RebaseOutcome,
    RebasePlan,
    RebaseState,
    RebaseStep,
    RebaseStopReason,
    RefLabel,
    RefList,
    RefListOptions,
    RefreshSet,
    RelaPath,
    RemoteBranch,
    RemoteInfo,
    RemoteRef,
    Repo,
    RepoExclusive,
    RepoExclusiveGuard,
    RepoKind,
    RepoState,
    RepoWatcher,
    ResetMode,
    Resolution,
    RowView,
    RuntimeConfig,
    SearchQuery,
    Selection as LineSelection,
    SelectionRequest,
    Sensitive,
    SequenceOutcome,
    SequencerState,
    Side as EntrySide,
    SignMode,
    Signature,
    SizeUnit,
    StartPoint,
    StashApplyOptions,
    StashApplyResult,
    StashInfo,
    StashPushOptions,
    Status,
    StatusCounts,
    StatusEntry,
    StatusOptions,
    SubmoduleInfo,
    SubmoduleState,
    SubmoduleSummary,
    SuppressGuard,
    TagFetch,
    TagInfo,
    TagOptions,
    TagPush,
    TextDiff,
    TodoAction,
    TodoLine,
    Upstream,
    WatchEvent,
    WhitespaceMode,
    WorktreeAddOptions,
    WorktreeInfo,
    WorktreeTarget,
};
// Name clashes are resolved by alias: git's `InProgress`/`Side` and the GUI's `Selection` keep the short names.
pub use gitcthulhu_oplog::{
    ConfirmReason, ConfirmToken, Confirmation, Entry as OplogEntry, EntryId, EntryResult,
    EntryRole, EntryStatus, GitRunner, InProgress as OplogInProgress, InterruptedRestore,
    NetworkMarker, OperationDescriptor, OperationKind, OperationResult, Oplog, OplogError,
    OplogOptions, Page as OplogPage, PageRequest, PendingOperation, PruneReport, PushedRef,
    PushedSinceRef, Recovery as OplogRecovery, RefDivergence, RefValueInfo, Refusal,
    RestoreOptions, RestoreReport, RestoreTarget, RestoreWarning, RetentionPolicy, Scope,
    Side as SnapshotSide, SkipReason, SkippedFile, StackItemInfo, UndoOutcome, UndoRedoState,
    UnsupportedReason, UntrackedCapture,
};
// gitcthulhu-git does not re-export gix types; the GUI depends on the workspace gix pin for these names only.
pub use bstr::{BString, ByteSlice};
pub use gix::{ObjectId, object::Kind as ObjectKind, refs::FullName};

/// The gix repository type (for region backend files that read through `Repo::local`).
pub type GixRepo = gix::Repository;

/// State of the process-wide Git runtime (`git version` check at startup).
#[derive(Clone, Debug)]
pub enum GitStatus {
    /// `start_runtime` is still running.
    Starting,
    /// Git was found and is new enough.
    Ready(Arc<GitRuntime>),
    /// Git is missing or too old; the welcome page shows a banner (spec 21 §2.9).
    Failed(Arc<GitError>),
}

impl GitStatus {
    /// The runtime, when ready.
    pub fn runtime(&self) -> Option<&Arc<GitRuntime>> {
        match self {
            GitStatus::Ready(rt) => Some(rt),
            _ => None,
        }
    }
}

/// The `RuntimeConfig` the settings ask for: Git path, host-key policy and the network `-c` pairs (proxy, TLS).
/// The helper programs default to `gitcthulhu-askpass` / `gitcthulhu-editor` next to the executable.
pub fn runtime_config(settings: &Settings) -> RuntimeConfig {
    let mut network_config = Vec::new();
    if settings.use_proxy && !settings.proxy_host.trim().is_empty() {
        let scheme = match settings.proxy_type {
            ProxyType::Http => "http",
            ProxyType::Socks => "socks5",
        };
        let user = if settings.proxy_auth && !settings.proxy_user.is_empty() {
            format!("{}@", settings.proxy_user)
        } else {
            String::new()
        };
        let port = if settings.proxy_port == 0 {
            String::new()
        } else {
            format!(":{}", settings.proxy_port)
        };
        network_config.push((
            "http.proxy".to_owned(),
            format!("{scheme}://{user}{}{port}", settings.proxy_host.trim()),
        ));
    }
    if !settings.verify_ssl {
        network_config.push(("http.sslVerify".to_owned(), "false".to_owned()));
    }
    RuntimeConfig {
        git_path: settings.git_path.clone(),
        host_key_policy: if settings.ssh_ask_new_hosts {
            HostKeyPolicy::Ask
        } else {
            HostKeyPolicy::AcceptNew
        },
        network_config,
        ..RuntimeConfig::default()
    }
}

/// Starts the Git runtime from the settings (blocking: runs `git version`; call it inside [`bg`]).
///
/// Errors: `GitNotFound`, `GitTooOld`.
pub fn start_runtime(settings: &Settings) -> Result<Arc<GitRuntime>, GitError> {
    let mut config = runtime_config(settings);
    proxy_secret::apply(&mut config, settings);
    GitRuntime::new(config)
}

/// Runs a blocking backend call on GPUI's background executor (on Windows the OS thread pool, so blocking git
/// calls do not starve the UI). Every backend call goes through here; never call the backend on the UI thread.
pub fn bg<T: Send + 'static>(cx: &App, f: impl FnOnce() -> T + Send + 'static) -> Task<T> {
    cx.background_executor().spawn(async move { f() })
}

#[cfg(test)]
mod tests;
