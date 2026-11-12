//! Compile-only: takes a function pointer of every public function of the backend, so a change of any signature
//! that the GUI or a sibling package relies on breaks this test first.
#![allow(clippy::type_complexity)]

use std::{path::Path, sync::Arc, time::Duration};

use bstr::BString;
use gitcthulhu_git::{
    cli, config, credentials, diff, error, network, rebase, refs, remote, stage, stash, state,
    status, submodule, tag, watch, worktree, *,
};
use gix::{ObjectId, refs::FullName};

#[test]
fn public_functions_have_the_documented_signatures() {
    // repo / runtime
    let _ = GitRuntime::new as fn(RuntimeConfig) -> Result<Arc<GitRuntime>>;
    let _ = GitRuntime::version as fn(&GitRuntime) -> GitVersion;
    let _ = GitRuntime::git_path as fn(&GitRuntime) -> &Path;
    let _ = GitRuntime::has_askpass as fn(&GitRuntime) -> bool;
    let _ = GitRuntime::has_editor_helper as fn(&GitRuntime) -> bool;
    let _ = GitRuntime::helper_status as fn(&GitRuntime) -> HelperStatus;
    let _ = Repo::open as fn(&Arc<GitRuntime>, &Path) -> Result<Repo>;
    let _ = Repo::init as fn(&Arc<GitRuntime>, &Path, Option<&str>) -> Result<Repo>;
    let _ = Repo::workdir as fn(&Repo) -> Option<&Path>;
    let _ = Repo::git_dir as fn(&Repo) -> &Path;
    let _ = Repo::common_dir as fn(&Repo) -> &Path;
    let _ = Repo::kind as fn(&Repo) -> &RepoKind;
    let _ = Repo::runtime as fn(&Repo) -> &Arc<GitRuntime>;
    let _ = Repo::local as fn(&Repo) -> gix::Repository;
    let _ = Repo::reload as fn(&Repo) -> Result<()>;
    let _ = Repo::lock_exclusive as fn(&Repo) -> RepoExclusiveGuard;
    let _ = Repo::try_lock_exclusive as fn(&Repo, Duration) -> Option<RepoExclusiveGuard>;
    let _ = Repo::lock_shared as fn(&Repo) -> RepoSharedGuard;

    // types with behaviour
    let _ = GitVersion::parse as fn(&str) -> Option<GitVersion>;
    let _ = CancelToken::new as fn() -> CancelToken;
    let _ = CancelToken::cancel as fn(&CancelToken);
    let _ = CancelToken::is_cancelled as fn(&CancelToken) -> bool;
    let _ = CancelToken::check as fn(&CancelToken) -> Result<()>;
    let _ = CancelToken::as_atomic as fn(&CancelToken) -> Arc<std::sync::atomic::AtomicBool>;
    let _ = OpContext::silent as fn(CancelToken) -> OpContext;
    let _ = StatusCounts::is_clean as fn(&StatusCounts) -> bool;
    let _ = TextDiff::line_text as for<'a, 'b> fn(&'a TextDiff, &'b DiffLine) -> &'a str;
    let _ = TextDiff::inline_spans
        as for<'a, 'b> fn(&'a TextDiff, &'b DiffLine) -> &'a [std::ops::Range<u32>];
    let _ = TextDiff::hunk_lines as for<'a, 'b> fn(&'a TextDiff, &'b Hunk) -> &'a [DiffLine];
    let _ = <StatusOptions as Default>::default as fn() -> StatusOptions;
    let _ = <DiffOptions as Default>::default as fn() -> DiffOptions;
    let _ = <LogOptions as Default>::default as fn() -> LogOptions;
    let _: RefListOptions = RefListOptions::ALL;

    // errors
    let _ = GitError::kind as fn(&GitError) -> GitErrorKind;
    let _ = GitError::stderr as fn(&GitError) -> Option<&str>;
    let _ = GitError::refresh_on_failure as fn(&GitError) -> RefreshSet;
    let _ = GitError::is_transient as fn(&GitError) -> bool;
    let _ = error::classify as fn(OpClass, &cli::GitOutput, &PostState) -> GitError;

    // executor
    let _ = cli::run as fn(&GitRuntime, cli::GitCommand, &OpContext) -> Result<cli::GitOutput>;
    let _ = cli::run_checked
        as fn(&Repo, cli::GitCommand, &OpContext, OpClass) -> Result<cli::GitOutput>;
    let _ = cli::run_checked_global
        as fn(&GitRuntime, cli::GitCommand, &OpContext, OpClass) -> Result<cli::GitOutput>;
    let _ = cli::run_simple as fn(&Repo, &[&str], &CancelToken) -> Result<Vec<u8>>;
    let _ = cli::GitCommand::new as fn(&Repo, cli::CommandKind) -> cli::GitCommand;
    let _ = cli::GitCommand::global as fn(std::path::PathBuf, cli::CommandKind) -> cli::GitCommand;
    let _ = cli::GitCommand::pathspecs as fn(cli::GitCommand, &[RelaPath]) -> cli::GitCommand;
    let _ = cli::GitCommand::literal_paths_after_dashdash
        as fn(cli::GitCommand, &[RelaPath]) -> cli::GitCommand;
    let _ = cli::GitCommand::stdin as fn(cli::GitCommand, Vec<u8>) -> cli::GitCommand;
    let _ = cli::GitCommand::with_progress as fn(cli::GitCommand) -> cli::GitCommand;
    let _ = cli::GitCommand::with_hooks as fn(cli::GitCommand) -> cli::GitCommand;
    let _ = cli::GitCommand::with_askpass as fn(cli::GitCommand) -> cli::GitCommand;
    let _ = cli::GitCommand::editor as fn(cli::GitCommand, cli::EditorMode) -> cli::GitCommand;
    let _ = cli::GitOutput::success as fn(&cli::GitOutput) -> bool;

    // credentials and helpers
    let _ = credentials::classify as fn(&str) -> PromptKind;
    let _ = Sensitive::new as fn(String) -> Sensitive;
    let _ = Sensitive::expose as fn(&Sensitive) -> &str;
    let _ = helper_main::askpass_main as fn() -> std::process::ExitCode;
    let _ = helper_main::editor_main as fn() -> std::process::ExitCode;

    // status / state
    let _ = status::status as fn(&Repo, &StatusOptions, &CancelToken) -> Result<Status>;
    let _ = status::refresh_index
        as fn(&Repo, &mut RepoExclusive, &CancelToken) -> Result<Option<FileStamp>>;
    let _ = state::repo_state as fn(&Repo) -> Result<RepoState>;
    let _ = state::parse_todo_line as fn(&str) -> Option<TodoLine>;
    let _ = state::has_conflicts as fn(&Repo) -> Result<bool>;

    // diff
    let _ =
        diff::file_diff as fn(&Repo, &DiffTarget, &DiffOptions, &CancelToken) -> Result<FileDiff>;
    let _ =
        diff::commit_details as fn(&Repo, ObjectId, CommitFilesOptions) -> Result<CommitDetails>;
    let _ = diff::commit_files
        as fn(&Repo, ObjectId, Against, CommitFilesOptions) -> Result<Arc<Vec<CommitFile>>>;
    let _ =
        diff::tree_files as fn(&Repo, Option<ObjectId>, ObjectId, bool) -> Result<Vec<CommitFile>>;
    let _ =
        diff::staged_patch_text as fn(&Repo, &PatchTextOptions, &CancelToken) -> Result<PatchText>;
    let _ = diff::export_patch as fn(&Repo, &ExportPatch, &CancelToken) -> Result<BString>;
    let _ = diff::read_blob as fn(&Repo, ObjectId, u64) -> Result<Arc<[u8]>>;

    // staging
    let _ = stage::stage_paths
        as fn(&Repo, &mut RepoExclusive, &OpContext, &[RelaPath]) -> Result<OpOutcome<()>>;
    let _ = stage::stage_all as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<()>>;
    let _ = stage::unstage_paths
        as fn(&Repo, &mut RepoExclusive, &OpContext, &[RelaPath]) -> Result<OpOutcome<()>>;
    let _ =
        stage::unstage_all as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<()>>;
    let _ = stage::stage_selection
        as fn(&Repo, &mut RepoExclusive, &OpContext, &SelectionRequest) -> Result<OpOutcome<()>>;
    let _ = stage::unstage_selection
        as fn(&Repo, &mut RepoExclusive, &OpContext, &SelectionRequest) -> Result<OpOutcome<()>>;
    let _ = stage::discard_paths
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &[RelaPath],
            DiscardMode,
        ) -> Result<OpOutcome<()>>;
    let _ = stage::discard_selection
        as fn(&Repo, &mut RepoExclusive, &OpContext, &SelectionRequest) -> Result<OpOutcome<()>>;
    let _ = stage::delete_untracked
        as fn(&Repo, &mut RepoExclusive, &OpContext, &[RelaPath]) -> Result<OpOutcome<()>>;
    let _ = stage::resolve_conflict
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &[RelaPath],
            Resolution,
        ) -> Result<OpOutcome<()>>;

    // log / graph
    let _ = LogWalker::new as fn(&Repo, &LogOptions, &CancelToken) -> Result<LogWalker>;
    let _ = LogWalker::first_parent as fn(&LogWalker) -> bool;
    let _ = LogWalker::next_page as fn(&mut LogWalker, usize, &CancelToken) -> Result<LogPage>;
    let _ = LogWalker::loaded as fn(&LogWalker) -> usize;
    let _ = LogWalker::is_exhausted as fn(&LogWalker) -> bool;
    let _ = LogWalker::stash_ids as fn(&LogWalker) -> &std::collections::HashSet<ObjectId>;
    let _ = LogWalker::is_shallow_boundary as fn(&LogWalker, &ObjectId) -> bool;
    let _ = gitcthulhu_git::log::search
        as fn(
            &Repo,
            &SearchQuery,
            &LogTips,
            usize,
            &CancelToken,
            &mut dyn FnMut(&CommitInfo),
        ) -> Result<usize>;
    let _ = gitcthulhu_git::log::commit_messages
        as fn(&Repo, Option<ObjectId>, ObjectId, usize) -> Result<Vec<String>>;
    let _ = gitcthulhu_git::log::head_message as fn(&Repo) -> Result<Option<String>>;
    let _ = gitcthulhu_git::log::messages
        as fn(&Repo, &[ObjectId], &CancelToken) -> Result<Vec<Box<str>>>;
    let _ = gitcthulhu_git::log::commit_info as fn(&Repo, ObjectId) -> Result<CommitInfo>;
    let _ = GraphBuilder::new as fn(bool) -> GraphBuilder;
    let _ = GraphBuilder::push_uncommitted as fn(&mut GraphBuilder, Option<ObjectId>);
    let _ = GraphBuilder::push as fn(&mut GraphBuilder, &CommitInfo, bool, bool);
    let _ = GraphBuilder::push_page as fn(&mut GraphBuilder, &LogPage, &LogWalker);
    let _ = GraphBuilder::layout as fn(&GraphBuilder) -> &GraphLayout;
    let _ = GraphBuilder::open_lanes as fn(&GraphBuilder) -> usize;
    let _ = GraphLayout::len as fn(&GraphLayout) -> usize;
    let _ = GraphLayout::row as fn(&GraphLayout, usize) -> RowView<'_>;
    let _ = GraphLayout::max_lane as fn(&GraphLayout) -> u16;
    let _ = GraphLayout::max_lane_in as fn(&GraphLayout, std::ops::Range<usize>) -> u16;
    let _ = |row: RowView<'_>| -> Vec<u16> { row.passing().collect() };
    let _ = |row: RowView<'_>| -> Vec<u16> { row.fork_ins().collect() };
    let _ = |row: RowView<'_>| -> Vec<MergeOut> { row.merge_outs().collect() };
    let _ = RowView::color_of as fn(u16) -> u16;

    // refs / config / remotes / worktrees / submodules
    let _ = refs::list as fn(&Repo, RefListOptions) -> Result<RefList>;
    let _ = refs::ahead_behind
        as fn(&Repo, &[(ObjectId, ObjectId)], u32, &CancelToken) -> Result<Vec<AheadBehind>>;
    let _ = refs::stash_list as fn(&Repo) -> Result<Vec<StashInfo>>;
    let _ = refs::resolve_commit as fn(&Repo, &str) -> Result<ObjectId>;
    let _ = refs::is_merged as fn(&Repo, ObjectId, Option<ObjectId>) -> Result<bool>;
    let _ = refs::decorations as fn(&RefList) -> Decorations;
    let _ = config::identity as fn(Option<&Repo>) -> Result<IdentityInfo>;
    let _ = config::set_identity
        as fn(Option<&Repo>, ConfigScope, Option<&Identity>) -> Result<RefreshSet>;
    let _ = config::get as fn(&Repo, &str) -> Result<Option<String>>;
    let _ = config::get_bool as fn(&Repo, &str) -> Result<Option<bool>>;
    let _ = config::set as fn(Option<&Repo>, ConfigScope, &str, Option<&str>) -> Result<RefreshSet>;
    let _ = config::signing as fn(&Repo) -> Result<SigningConfig>;
    let _ = config::pull_mode_default as fn(&Repo) -> Result<PullMode>;
    let _ = config::add_safe_directory as fn(&GitRuntime, &Path) -> Result<()>;
    let _ = config::commit_graph_present as fn(&Repo) -> bool;
    let _ = config::write_commit_graph
        as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<()>>;
    let _ = remote::add_remote
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &str,
            &str,
            Option<&str>,
        ) -> Result<OpOutcome<()>>;
    let _ = remote::set_remote_urls
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &str,
            &str,
            Option<&str>,
        ) -> Result<OpOutcome<()>>;
    let _ = remote::rename_remote
        as fn(&Repo, &mut RepoExclusive, &OpContext, &str, &str) -> Result<OpOutcome<()>>;
    let _ = remote::remove_remote
        as fn(&Repo, &mut RepoExclusive, &OpContext, &str) -> Result<OpOutcome<()>>;
    let _ = remote::forge_remotes as fn(&Repo) -> Result<ForgeRemotes>;
    let _ = remote::default_branch as fn(&Repo, &str) -> Result<Option<String>>;
    let _ = worktree::list as fn(&Repo) -> Result<Vec<WorktreeInfo>>;
    let _ = worktree::add
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &WorktreeAddOptions,
        ) -> Result<OpOutcome<WorktreeInfo>>;
    let _ = worktree::remove
        as fn(&Repo, &mut RepoExclusive, &OpContext, &Path, bool) -> Result<OpOutcome<String>>;
    let _ = worktree::prune
        as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<Vec<String>>>;
    let _ = worktree::lock
        as fn(&Repo, &mut RepoExclusive, &OpContext, &Path, Option<&str>) -> Result<OpOutcome<()>>;
    let _ = worktree::unlock
        as fn(&Repo, &mut RepoExclusive, &OpContext, &Path) -> Result<OpOutcome<()>>;
    let _ = submodule::list as fn(&Repo, bool, &CancelToken) -> Result<Vec<SubmoduleInfo>>;
    let _ = submodule::add
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &str,
            &RelaPath,
            Option<&str>,
            Option<&str>,
        ) -> Result<OpOutcome<()>>;
    let _ = submodule::init_update
        as fn(&Repo, &mut RepoExclusive, &OpContext, &[RelaPath], bool) -> Result<OpOutcome<()>>;
    let _ = submodule::update
        as fn(&Repo, &mut RepoExclusive, &OpContext, &[RelaPath], bool) -> Result<OpOutcome<()>>;
    let _ = submodule::sync
        as fn(&Repo, &mut RepoExclusive, &OpContext, &[RelaPath], bool) -> Result<OpOutcome<()>>;
    let _ = submodule::deinit
        as fn(&Repo, &mut RepoExclusive, &OpContext, &[RelaPath], bool) -> Result<OpOutcome<()>>;
    let _ = submodule::remove
        as fn(&Repo, &mut RepoExclusive, &OpContext, &RelaPath) -> Result<OpOutcome<()>>;

    // blame / history / watch
    let _ = gitcthulhu_git::blame::blame
        as fn(&Repo, &RelaPath, BlameRev, &BlameOptions, &CancelToken) -> Result<BlameResult>;
    let _ = FileHistoryWalker::new
        as fn(&Repo, &RelaPath, Option<ObjectId>, bool) -> Result<FileHistoryWalker>;
    let _ = FileHistoryWalker::next_page
        as fn(&mut FileHistoryWalker, usize, &CancelToken) -> Result<FileHistoryPage>;
    // `start` takes `impl Fn(WatchEvent) + Send + 'static`, which cannot be named in a fn pointer type.
    let _ =
        |repo: &Repo| -> Result<RepoWatcher> { RepoWatcher::start(repo, |_event: WatchEvent| {}) };
    let _ = RepoWatcher::suppress as fn(&RepoWatcher) -> SuppressGuard;
    let _ = RepoWatcher::flush as fn(&RepoWatcher);
    let _ = RepoWatcher::ignore_index_stamp as fn(&RepoWatcher, FileStamp);
    let _ = watch::RepoWatcher::flush as fn(&RepoWatcher);

    // commit / branch / tag / stash
    let _ = gitcthulhu_git::commit::commit
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &CommitOptions,
        ) -> Result<OpOutcome<CommitOutcome>>;
    let _ = gitcthulhu_git::commit::save_draft_message as fn(&Repo, Option<&str>) -> Result<()>;
    let _ = gitcthulhu_git::commit::load_draft_message as fn(&Repo) -> Result<Option<String>>;
    let _ = gitcthulhu_git::branch::checkout_branch
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &FullName,
            CheckoutOptions,
        ) -> Result<OpOutcome<()>>;
    let _ = gitcthulhu_git::branch::checkout_remote_branch
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &FullName,
            Option<&str>,
            CheckoutOptions,
        ) -> Result<OpOutcome<FullName>>;
    let _ = gitcthulhu_git::branch::checkout_detached
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            ObjectId,
            CheckoutOptions,
        ) -> Result<OpOutcome<()>>;
    let _ = gitcthulhu_git::branch::create_branch
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &str,
            &StartPoint,
            bool,
        ) -> Result<OpOutcome<FullName>>;
    let _ = gitcthulhu_git::branch::rename_branch
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &FullName,
            &str,
        ) -> Result<OpOutcome<FullName>>;
    let _ = gitcthulhu_git::branch::delete_branch
        as fn(&Repo, &mut RepoExclusive, &OpContext, &FullName, bool) -> Result<OpOutcome<()>>;
    let _ = gitcthulhu_git::branch::set_upstream
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &FullName,
            Option<&FullName>,
        ) -> Result<OpOutcome<()>>;
    let _ = gitcthulhu_git::branch::validate_branch_name as fn(&Repo, &str) -> Result<FullName>;
    let _ = tag::create_tag
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &str,
            ObjectId,
            &TagOptions,
        ) -> Result<OpOutcome<FullName>>;
    let _ =
        tag::delete_tag as fn(&Repo, &mut RepoExclusive, &OpContext, &str) -> Result<OpOutcome<()>>;
    let _ = tag::validate_tag_name as fn(&Repo, &str) -> Result<FullName>;
    let _ = stash::stash_push
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &StashPushOptions,
        ) -> Result<OpOutcome<Option<ObjectId>>>;
    let _ = stash::stash_apply
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            ObjectId,
            StashApplyOptions,
        ) -> Result<OpOutcome<StashApplyResult>>;
    let _ = stash::stash_pop
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            ObjectId,
            StashApplyOptions,
        ) -> Result<OpOutcome<StashApplyResult>>;
    let _ = stash::stash_drop
        as fn(&Repo, &mut RepoExclusive, &OpContext, ObjectId) -> Result<OpOutcome<()>>;
    let _ =
        stash::stash_clear as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<()>>;
    let _ = stash::untracked_paths as fn(&Repo, ObjectId) -> Result<Vec<RelaPath>>;

    // merge / rebase
    let _ = gitcthulhu_git::merge::merge
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &MergeOptions,
        ) -> Result<OpOutcome<MergeOutcome>>;
    let _ = gitcthulhu_git::merge::cherry_pick
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &[ObjectId],
            PickOptions,
        ) -> Result<OpOutcome<SequenceOutcome>>;
    let _ = gitcthulhu_git::merge::revert
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &[ObjectId],
            PickOptions,
        ) -> Result<OpOutcome<SequenceOutcome>>;
    let _ = gitcthulhu_git::merge::reset
        as fn(&Repo, &mut RepoExclusive, &OpContext, ObjectId, ResetMode) -> Result<OpOutcome<()>>;
    let _ = gitcthulhu_git::merge::continue_operation
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            Option<&str>,
        ) -> Result<OpOutcome<ContinueOutcome>>;
    let _ = gitcthulhu_git::merge::abort_operation
        as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<()>>;
    let _ = gitcthulhu_git::merge::skip_operation
        as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<ContinueOutcome>>;
    let _ = rebase::rebase
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &str,
            &RebaseOptions,
        ) -> Result<OpOutcome<RebaseOutcome>>;
    let _ = rebase::plan_interactive as fn(&Repo, Option<ObjectId>) -> Result<RebasePlan>;
    let _ = rebase::start_interactive
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &RebasePlan,
            &RebaseOptions,
        ) -> Result<OpOutcome<RebaseOutcome>>;
    let _ = rebase::continue_rebase
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            Option<&str>,
        ) -> Result<OpOutcome<RebaseOutcome>>;
    let _ = rebase::skip_rebase
        as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<RebaseOutcome>>;
    let _ =
        rebase::abort_rebase as fn(&Repo, &mut RepoExclusive, &OpContext) -> Result<OpOutcome<()>>;
    let _ = rebase::edit_todo
        as fn(&Repo, &mut RepoExclusive, &OpContext, &[TodoLine]) -> Result<OpOutcome<()>>;

    // network
    let _ = network::clone
        as fn(&Arc<GitRuntime>, &OpContext, &CloneOptions) -> Result<OpOutcome<Repo>>;
    let _ =
        network::fetch as fn(&Repo, &OpContext, &FetchOptions) -> Result<OpOutcome<FetchReport>>;
    let _ = network::pull
        as fn(
            &Repo,
            &mut RepoExclusive,
            &OpContext,
            &PullOptions,
        ) -> Result<OpOutcome<MergeOutcomeOrRebase>>;
    let _ = network::push as fn(&Repo, &OpContext, &PushOptions) -> Result<OpOutcome<PushReport>>;
    let _ = network::push_current_branch
        as fn(&Repo, &OpContext, ForceMode, TagPush, bool) -> Result<OpOutcome<PushReport>>;
    let _ = network::delete_remote_branch
        as fn(&Repo, &OpContext, &str, &str) -> Result<OpOutcome<PushReport>>;
    let _ = network::push_tag as fn(&Repo, &OpContext, &str, &str) -> Result<OpOutcome<PushReport>>;
    let _ = network::delete_remote_tag
        as fn(&Repo, &OpContext, &str, &str) -> Result<OpOutcome<PushReport>>;
}

/// The thread-safety promises of §1.4 that the GUI relies on: handles are shared across background tasks, walkers are
/// kept in a `Mutex` and driven from background tasks, contexts and errors cross the executor's threads.
#[test]
fn thread_safety_contracts() {
    fn send_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}
    fn clone_send_sync<T: Clone + Send + Sync>() {}

    clone_send_sync::<Repo>();
    clone_send_sync::<CancelToken>();
    clone_send_sync::<OpContext>();
    send_sync::<GitRuntime>();
    send_sync::<GitError>();
    send_sync::<cli::GitCommand>();
    send_sync::<cli::GitOutput>();
    send_sync::<TextDiff>();
    send_sync::<GraphLayout>();
    send::<GraphBuilder>();
    send::<LogWalker>();
    send::<FileHistoryWalker>();
    send::<RepoWatcher>();
    fn std_error<E: std::error::Error + Send + Sync + 'static>() {}
    std_error::<GitError>();
}
