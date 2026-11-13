use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use gitbutler_notify_debouncer::{Debouncer, NoCache, new_debouncer};
use gix::bstr::ByteSlice as _;
use notify::{RecommendedWatcher, Watcher};
use tracing::Level;

use crate::{
    events::{Checkout, InternalEvent, LinkedWorktree},
    watch_plan::{
        build_index_icase_accelerator_if_needed, compute_watch_plan_for_repo, is_tracked_in_index,
        is_watchable_directory, to_repo_relative_path,
    },
};

/// We will collect notifications for up to this amount of time at a very
/// maximum before releasing them. This duration will be hit if e.g. a build
/// is constantly running and producing a lot of file changes, we will process
/// them even if the build is still running. A change to Git's refs, HEAD or
/// index releases all pending events after FLUSH_AFTER_EMPTY ticks instead,
/// quiet or not, so a commit made meanwhile shows.
const DEBOUNCE_TIMEOUT: Duration = Duration::from_secs(60);

// The internal rate at which the debouncer will update its state.
// Keeping a higher timeout on Windows because of its slower file-system notifications.
const TICK_RATE: Duration = if cfg!(windows) {
    Duration::from_millis(250)
} else {
    Duration::from_millis(100)
};

// The number of TICK_RATE intervals required of "dead air" (i.e. no new events
// arriving) before we will automatically flush pending events. This means that
// after the disk is quiet for TICK_RATE * FLUSH_AFTER_EMPTY, we will process
// the pending events, even if DEBOUNCE_TIMEOUT hasn't expired yet
const FLUSH_AFTER_EMPTY: u32 = 3;

enum Command {
    Flush,
}

/// Handle for a running file monitor spawned with [`spawn()`].
///
/// Dropping this handle will stop the monitor as soon as it tries to send the next event, failing as there is no receiver.
pub struct FileMonitorHandle {
    cmd_tx: std::sync::mpsc::Sender<Command>,
}

impl FileMonitorHandle {
    /// Request that pending filesystem events are emitted immediately.
    pub fn flush(&self) -> Result<()> {
        self.cmd_tx
            .send(Command::Flush)
            .map_err(|_| anyhow!("file monitor stopped"))
    }
}

const ENV_WATCH_MODE: &str = "GITCTHULHU_WATCH_MODE";

/// Control how the filesystem watch should be established.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// Recursively watch the worktree (and an extra git-dir if the repo uses
    /// a linked worktree with a git-dir outside the worktree), using [`notify::RecursiveMode::Recursive`].
    Legacy,
    /// Ignore-aware watch plan: non-recursive watches of non-ignored worktree directories,
    /// plus explicit git-dir watches and dynamic watch additions for newly created directories.
    /// Each directory is watched with [`notify::RecursiveMode::NonRecursive`].
    Modern,
    /// Automatically pick a mode based on platform heuristics.
    ///
    #[default]
    /// Currently, this enables `Modern` on WSL (Windows Subsystem for Linux.) and `Legacy` elsewhere.
    Auto,
}

impl std::str::FromStr for WatchMode {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "legacy" => Self::Legacy,
            "modern" => Self::Modern,
            "auto" => Self::Auto,
            _ => {
                return Err(());
            }
        })
    }
}

impl WatchMode {
    /// Initialise the mode from `watch_mode_from_settings`, with environment variable override.
    /// If the environment variable `GITCTHULHU_WATCH_MODE` is set, it overrides the feature flag.
    /// Otherwise, the feature flag value is used.
    pub fn from_env_or_settings<F>(watch_mode_from_settings: &str, get_env_var: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let env_var = get_env_var(ENV_WATCH_MODE);
        env_var
            .as_deref()
            .and_then(|env_var_value| env_var_value.parse().ok())
            .or_else(|| watch_mode_from_settings.parse().ok())
            .unwrap_or_else(|| {
                tracing::warn!(
                    feature_flag = watch_mode_from_settings,
                    env_var = ?env_var,
                    "unknown watch mode from feature flag or environment variable; falling back to auto"
                );
                WatchMode::Auto
            })
    }
}

#[cfg(target_os = "linux")]
fn is_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }

    for path in ["/proc/sys/kernel/osrelease", "/proc/version"] {
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        let lower = contents.to_ascii_lowercase();
        if lower.contains("microsoft") || lower.contains("wsl") {
            return true;
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn is_wsl() -> bool {
    false
}

fn watch_backoff_policy() -> backoff::ExponentialBackoff {
    backoff::ExponentialBackoffBuilder::new()
        .with_max_elapsed_time(Some(std::time::Duration::from_secs(30)))
        .build()
}

fn setup_watch_plan(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    repo: &gix::Repository,
    worktree_path: &Path,
    git_dir: &Path,
    linked_git_dirs: &[&Path],
    problems: &mut Vec<String>,
) -> Result<()> {
    let mut limit_hit: Option<PathBuf> = None;
    // Start the watcher, but retry if there are transient errors.
    let result = backoff::retry(watch_backoff_policy(), || {
        let mut paths = debouncer.watcher().paths_mut();
        let mut add_error: Option<(std::path::PathBuf, notify::Error)> = None;
        limit_hit = None;
        compute_watch_plan_for_repo(repo, worktree_path, git_dir, linked_git_dirs, |path, mode| {
            if add_error.is_some() {
                return Ok(std::ops::ControlFlow::Break(()));
            }
            match paths.add(path, mode) {
                Ok(()) => Ok(std::ops::ControlFlow::Continue(())),
                Err(err) => match err.kind {
                    notify::ErrorKind::MaxFilesWatch => {
                        tracing::warn!(
                            path = %path.display(),
                            "OS file watch limit reached; continuing with partial watches. Monitoring coverage may be incomplete until restart."
                        );
                        limit_hit = Some(path.to_owned());
                        Ok(std::ops::ControlFlow::Break(()))
                    }
                    _ => {
                        add_error = Some((path.to_owned(), err));
                        Ok(std::ops::ControlFlow::Break(()))
                    }
                },
            }
        })
        .map_err(|err| backoff::Error::permanent(err.into_boxed_dyn_error()))?;

        if let Some((path, err)) = add_error {
            return Err(into_backoff_err(err, &path));
        }

        match paths.commit() {
            Ok(()) => Ok(()),
            Err(err) => Err(into_backoff_err(err, worktree_path))
        }
    })
        .map_err(backoff_err_to_anyhow).context("Watcher start failed");
    if result.is_ok()
        && let Some(path) = limit_hit
    {
        problems.push(format!(
            "The operating system's limit of file watches was reached at {}: changes in parts of the repository will not be noticed",
            path.display()
        ));
    }
    result
}

fn setup_legacy_watch(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    worktree_path: &Path,
    git_dir: &Path,
    common_dir: Option<&Path>,
    problems: &mut Vec<String>,
) -> Result<()> {
    let extra_git_dir_to_watch = {
        let mut enclosing_worktree_dir = git_dir.to_owned();
        enclosing_worktree_dir.pop();
        if enclosing_worktree_dir != worktree_path {
            Some(git_dir)
        } else {
            None
        }
    };

    // Start the watcher, but retry if there are transient errors.
    backoff::retry(watch_backoff_policy(), || {
        debouncer
            .watcher()
            .watch(worktree_path, notify::RecursiveMode::Recursive)
            .and_then(|()| {
                if let Some(git_dir) = extra_git_dir_to_watch {
                    debouncer
                        .watcher()
                        .watch(git_dir, notify::RecursiveMode::Recursive)
                } else {
                    Ok(())
                }
            })
            .map_err(|err| into_backoff_err(err, worktree_path))
    })
    .map_err(backoff_err_to_anyhow)
    .context("failed to start watcher")?;
    if let Some(common_dir) = common_dir {
        watch_common_dir(debouncer, common_dir, problems)?;
    }
    Ok(())
}

/// Watch the parts of the `common_dir` of a linked worktree that matter: refs, `packed-refs`, `config` and
/// friends, but never the object database.
fn watch_common_dir(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    common_dir: &Path,
    problems: &mut Vec<String>,
) -> Result<()> {
    let mut limit_hit: Option<PathBuf> = None;
    let result = backoff::retry(watch_backoff_policy(), || {
        let mut paths = debouncer.watcher().paths_mut();
        let mut add_error: Option<(PathBuf, notify::Error)> = None;
        limit_hit = None;
        let _flow = crate::watch_plan::emit_git_dir_watches(common_dir, &mut |path, mode| {
            if add_error.is_some() {
                return Ok(std::ops::ControlFlow::Break(()));
            }
            if let Err(err) = paths.add(path, mode) {
                if matches!(err.kind, notify::ErrorKind::MaxFilesWatch) {
                    tracing::warn!(
                        path = %path.display(),
                        "OS file watch limit reached; continuing with partial watches"
                    );
                    limit_hit = Some(path.to_owned());
                } else {
                    add_error = Some((path.to_owned(), err));
                }
                return Ok(std::ops::ControlFlow::Break(()));
            }
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .map_err(|err| backoff::Error::permanent(err.into_boxed_dyn_error()))?;
        if let Some((path, err)) = add_error {
            return Err(into_backoff_err(err, &path));
        }
        paths
            .commit()
            .map_err(|err| into_backoff_err(err, common_dir))
    })
    .map_err(backoff_err_to_anyhow)
    .context("failed to watch the common git directory");
    if result.is_ok()
        && let Some(path) = limit_hit
    {
        problems.push(format!(
            "The operating system's limit of file watches was reached at {}: changes of the shared git directory will not be noticed",
            path.display()
        ));
    }
    result
}

/// Listen to interesting filesystem events of files in `worktree_path` that are not `.gitignore`d, and classify
/// them.
/// These are sent through the passed `out` channel, to indicate either **Git** repository changes
/// or **ProjectWorktree** changes
/// Use `watch_mode` to control how exactly the directory is watched.
///
/// `common_dir` is the common git directory if `worktree_path` is a linked worktree: its refs, `packed-refs` and
/// `config` are watched as well, and changes there are reported as [`InternalEvent::CommonGitFilesChange`].
///
/// `linked_worktrees` lists the linked worktrees to watch besides the opened checkout, whose changes are
/// reported under their own [`Checkout`]. The list is obtained again whenever a linked worktree is
/// registered, moved or removed, and is left as it was if that fails.
///
/// The worker runs on its own thread named `filemonitor` and stops when the returned handle is dropped or `out`
/// has no receiver anymore.
///
/// ### Why is this not an iterator?
///
/// The internal `notify_rx` could be an iterator, which performs all transformations and returns them as item.
/// Due to closures being continuously created each time events come in, nested closures need to own
/// their resources, which means they are `Clone` or `Copy`. This isn't the case for `git::Repository`.
/// Even though `gix::Repository` is `Clone`, an efficient implementation of `is_path_ignored()` requires more state
/// that ideally is kept between invocations. For that reason, the current channel-based 'worker' architecture
/// is chosen to allow all these states to live on the stack.
///
/// Additionally, a channel plays better with how events are handled downstream.
pub fn spawn(
    worktree_path: &std::path::Path,
    common_dir: Option<&Path>,
    linked_worktrees: impl Fn() -> Result<Vec<LinkedWorktree>> + Send + 'static,
    out: std::sync::mpsc::Sender<InternalEvent>,
    watch_mode: WatchMode,
) -> Result<FileMonitorHandle> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (notify_tx, notify_rx) = std::sync::mpsc::channel();
    let worktree_path = gix::path::realpath(worktree_path)?;
    let common_dir = common_dir
        .map(gix::path::realpath)
        .transpose()
        .context("failed to resolve the common git directory")?;
    let repo = gix::open_opts(&worktree_path, gix::open::Options::isolated()).context(format!(
        "failed to open project repository to obtain git-dir: {}",
        worktree_path.display()
    ))?;
    let git_dir = repo.path().to_owned();
    // The directory under which linked worktrees are registered (`<common>/worktrees/<name>`).
    let registry_dir = common_dir.clone().unwrap_or_else(|| git_dir.clone());
    let mut debouncer = new_debouncer(
        DEBOUNCE_TIMEOUT,
        Some(TICK_RATE),
        Some(FLUSH_AFTER_EMPTY),
        {
            let git_dir = git_dir.clone();
            let common_dir = common_dir.clone();
            move |event: &notify::Event| {
                is_interesting_kind(event.kind)
                    && (event.paths.iter()).any(|path| {
                        classify_file(&git_dir, path) == FileKind::Git
                            || common_dir.as_deref().is_some_and(|common_dir| {
                                classify_file(common_dir, path) == FileKind::Git
                            })
                    })
            }
        },
        notify_tx,
    )
    .context("failed to create debouncer")?;
    let linked = linked_checkouts(&registry_dir, &linked_worktrees).unwrap_or_default();
    let mut extra_git_dirs: Vec<&Path> = linked
        .iter()
        .map(|checkout| checkout.git_dir.as_path())
        .collect();
    extra_git_dirs.extend(common_dir.as_deref());

    let mut effective_watch_mode = watch_mode;

    // What did not work while setting up, reported as `InternalEvent::WatchError` once the worker runs.
    let mut problems: Vec<String> = Vec::new();
    let plan = |debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
                problems: &mut Vec<String>| {
        setup_watch_plan(
            debouncer,
            &repo,
            &worktree_path,
            &git_dir,
            &extra_git_dirs,
            problems,
        )
    };
    let legacy = |debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
                  problems: &mut Vec<String>| {
        setup_legacy_watch(
            debouncer,
            &worktree_path,
            &git_dir,
            common_dir.as_deref(),
            problems,
        )
    };
    match watch_mode {
        WatchMode::Legacy => legacy(&mut debouncer, &mut problems)?,
        WatchMode::Modern => {
            if let Err(err) = plan(&mut debouncer, &mut problems) {
                tracing::warn!(
                    ?err,
                    "watch-plan setup failed; falling back to legacy watch mode"
                );
                effective_watch_mode = WatchMode::Legacy;
                legacy(&mut debouncer, &mut problems)?;
            }
        }
        WatchMode::Auto => {
            if is_wsl() {
                match plan(&mut debouncer, &mut problems) {
                    Ok(()) => {
                        effective_watch_mode = WatchMode::Modern;
                    }
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            "watch-plan setup failed; falling back to legacy watch mode"
                        );
                        effective_watch_mode = WatchMode::Legacy;
                        legacy(&mut debouncer, &mut problems)?;
                    }
                }
            } else {
                effective_watch_mode = WatchMode::Legacy;
                legacy(&mut debouncer, &mut problems)?;
            }
        }
    }
    tracing::debug!(
        requested = ?watch_mode,
        effective = ?effective_watch_mode,
        "file watcher started"
    );

    let dynamic_watch_enabled = matches!(effective_watch_mode, WatchMode::Modern);
    for checkout in &linked {
        watch_linked_workdir(
            &mut debouncer,
            &worktree_path,
            checkout,
            dynamic_watch_enabled,
            &mut problems,
        );
    }
    let main = WatchedCheckout {
        id: Checkout::Main,
        workdir: worktree_path,
        git_dir,
    };
    std::thread::Builder::new()
        .name("filemonitor".into())
        .spawn(move || {
            let _runtime = tracing::span!(Level::INFO, "file monitor").entered();
            run_worker(
                debouncer,
                notify_rx,
                cmd_rx,
                &main,
                common_dir.as_deref(),
                &registry_dir,
                linked,
                &linked_worktrees,
                &out,
                dynamic_watch_enabled,
                problems,
            );
        })
        .context("failed to start the file monitor thread")?;
    Ok(FileMonitorHandle { cmd_tx })
}

/// The body of the worker thread: turn debounced notify events into [`InternalEvent`]s until the handle is dropped
/// or the receiver of `out` is gone.
#[allow(clippy::too_many_arguments)]
fn run_worker(
    mut debouncer: Debouncer<RecommendedWatcher, NoCache>,
    notify_rx: std::sync::mpsc::Receiver<gitbutler_notify_debouncer::DebounceEventResult>,
    cmd_rx: std::sync::mpsc::Receiver<Command>,
    main: &WatchedCheckout,
    common_dir: Option<&Path>,
    registry_dir: &Path,
    mut linked: Vec<WatchedCheckout>,
    linked_worktrees: &impl Fn() -> Result<Vec<LinkedWorktree>>,
    out: &std::sync::mpsc::Sender<InternalEvent>,
    dynamic_watch_enabled: bool,
    mut problems: Vec<String>,
) {
    // Even though the watcher cannot 'double-watch', we keep track of newly added watches
    // to avoid it doing any work, and for better traces.
    let mut dynamically_watched_dirs: HashSet<std::path::PathBuf> = HashSet::new();
    'outer: loop {
        // Problems of the setup, and of the dynamic watches of the previous round.
        for message in problems.drain(..) {
            if out.send(InternalEvent::WatchError(message)).is_err() {
                break 'outer;
            }
        }
        // Handle control plane messages.
        loop {
            match cmd_rx.try_recv() {
                Ok(Command::Flush) => debouncer.flush_nonblocking(),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'outer,
            }
        }

        let result = match notify_rx.recv_timeout(TICK_RATE) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break 'outer,
        };
        let stats = tracing::span!(
            Level::INFO,
            "handle debounced events",
            ignored = tracing::field::Empty,
            project = tracing::field::Empty,
            git = tracing::field::Empty,
            git_noop = tracing::field::Empty,
            fs_events = tracing::field::Empty,
        )
        .entered();
        match result {
            Err(errors) => {
                for err in errors {
                    tracing::error!(?err, "file watcher error");
                    problems.push(format!("The file watcher reported an error: {err}"));
                }
            }
            Ok(events) => {
                let num_events = events.len();
                let (changes_by_checkout, common_changes) = changes_by_checkout(
                    std::iter::once(main).chain(&linked),
                    common_dir,
                    events
                        .into_iter()
                        .filter(|event| is_interesting_kind(event.kind))
                        .flat_map(|event| event.event.paths),
                );
                let count = |count_of: fn(&Changes) -> usize| -> usize {
                    changes_by_checkout
                        .iter()
                        .map(|(_, changes)| count_of(changes))
                        .sum()
                };
                stats.record("fs_events", num_events);
                stats.record("ignored", count(|changes| changes.ignored));
                stats.record("git_noop", count(|changes| changes.git_noop));
                stats.record("git", count(|changes| changes.git.len()));
                stats.record("project", count(|changes| changes.worktree.len()));
                let relisted = (changes_by_checkout
                    .iter()
                    .any(|(_, changes)| changes.linked_worktrees_may_differ())
                    || common_changes
                        .iter()
                        .any(|path| is_linked_worktree_registration(path)))
                .then(|| linked_checkouts(registry_dir, linked_worktrees))
                .flatten();
                if let Some(relisted) = &relisted {
                    if dynamic_watch_enabled {
                        update_dynamic_watches(
                            &mut debouncer,
                            &mut dynamically_watched_dirs,
                            std::iter::once(registry_dir.join(LINKED_WORKTREES_DIR)),
                            &mut problems,
                        );
                    }
                    // A moved worktree leaves as its old location and joins as its new one,
                    // which can be the same watch to the operating system: it must be
                    // unwatched first or the removal takes the new watch with it.
                    for removed in linked
                        .iter()
                        .filter(|checkout| !relisted.contains(checkout))
                    {
                        unwatch_linked_workdir(
                            &mut debouncer,
                            &mut dynamically_watched_dirs,
                            &main.workdir,
                            removed,
                            dynamic_watch_enabled,
                        );
                    }
                    for added in relisted
                        .iter()
                        .filter(|checkout| !linked.contains(checkout))
                    {
                        watch_linked_workdir(
                            &mut debouncer,
                            &main.workdir,
                            added,
                            dynamic_watch_enabled,
                            &mut problems,
                        );
                    }
                }

                for (checkout, changes) in changes_by_checkout {
                    if dynamic_watch_enabled && changes.ignore_filtering_ran {
                        update_dynamic_watches(
                            &mut debouncer,
                            &mut dynamically_watched_dirs,
                            changes
                                .worktree
                                .iter()
                                .map(|relative_path| checkout.workdir.join(relative_path)),
                            &mut problems,
                        );
                    }
                    if dynamic_watch_enabled {
                        update_dynamic_git_dir_watches(
                            &mut debouncer,
                            &mut dynamically_watched_dirs,
                            &checkout.git_dir,
                            &changes.git,
                            &mut problems,
                        );
                    }

                    if !changes.git.is_empty() {
                        let event = InternalEvent::GitFilesChange(
                            checkout.id.clone(),
                            changes.git.into_iter().collect(),
                        );
                        if out.send(event).is_err() {
                            tracing::info!("channel closed - stopping file watcher");
                            break 'outer;
                        }
                    }
                    if !changes.worktree.is_empty() {
                        let event = InternalEvent::ProjectFilesChange(
                            checkout.id.clone(),
                            changes.worktree.into_iter().collect(),
                        );
                        if out.send(event).is_err() {
                            tracing::info!("channel closed - stopping file watcher");
                            break 'outer;
                        }
                    }
                }
                if let Some(common_dir) = common_dir
                    && !common_changes.is_empty()
                {
                    if dynamic_watch_enabled {
                        update_dynamic_git_dir_watches(
                            &mut debouncer,
                            &mut dynamically_watched_dirs,
                            common_dir,
                            &common_changes,
                            &mut problems,
                        );
                    }
                    let event =
                        InternalEvent::CommonGitFilesChange(common_changes.into_iter().collect());
                    if out.send(event).is_err() {
                        tracing::info!("channel closed - stopping file watcher");
                        break 'outer;
                    }
                }
                if let Some(relisted) = relisted {
                    linked = relisted;
                }
            }
        }
    }
}

/// Watch the git-dir directories that come into existence while we run, given the `git_dir`-relative `changed`
/// paths: `rebase-merge/` (and its siblings), `modules/`, `modules/<name>/` and `modules/<name>/refs/`.
/// Never `modules/**/objects/`, a fetch in a submodule would flood the debouncer.
fn update_dynamic_git_dir_watches(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    dynamically_watched_dirs: &mut HashSet<PathBuf>,
    git_dir: &Path,
    changed: &HashSet<PathBuf>,
    problems: &mut Vec<String>,
) {
    let mut non_recursive = Vec::new();
    let mut recursive = Vec::new();
    for rel in changed {
        let mut components = rel.components();
        let Some(first) = components.next() else {
            continue;
        };
        let first = first.as_os_str();
        if STATE_DIRS.iter().any(|dir| first == *dir) {
            if components.next().is_none() {
                non_recursive.push(git_dir.join(rel));
            }
        } else if first == MODULES_DIR {
            let modules_dir = git_dir.join(MODULES_DIR);
            non_recursive.push(modules_dir.clone());
            if let Some(name) = components.next() {
                let module_dir = modules_dir.join(name.as_os_str());
                recursive.push(module_dir.join("refs"));
                non_recursive.push(module_dir);
            }
        }
    }
    update_dynamic_watches(
        debouncer,
        dynamically_watched_dirs,
        non_recursive.into_iter(),
        problems,
    );
    for dir in recursive {
        if dynamically_watched_dirs.contains(&dir) || !dir.is_dir() {
            continue;
        }
        match debouncer
            .watcher()
            .watch(&dir, notify::RecursiveMode::Recursive)
        {
            Ok(()) => {
                dynamically_watched_dirs.insert(dir);
            }
            Err(err) => {
                tracing::warn!(?dir, ?err, "failed to watch a submodule's refs");
                problems.push(format!(
                    "Could not watch the refs of the submodule at {}: {err}",
                    dir.display()
                ));
            }
        }
    }
}

/// Watch the files of the `linked` worktree: with its own watch plan if `use_watch_plan` is set, and
/// recursively otherwise, unless the recursive watch of `main_workdir` already covers it.
fn watch_linked_workdir(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    main_workdir: &Path,
    linked: &WatchedCheckout,
    use_watch_plan: bool,
    problems: &mut Vec<String>,
) {
    let res = if use_watch_plan {
        gix::open_opts(&linked.workdir, gix::open::Options::isolated())
            .map_err(anyhow::Error::from)
            .and_then(|repo| {
                setup_watch_plan(
                    debouncer,
                    &repo,
                    &linked.workdir,
                    &linked.git_dir,
                    &[],
                    problems,
                )
            })
    } else if linked.has_own_recursive_watch(main_workdir, use_watch_plan) {
        debouncer
            .watcher()
            .watch(&linked.workdir, notify::RecursiveMode::Recursive)
            .map_err(Into::into)
    } else {
        Ok(())
    };
    if let Err(err) = res {
        tracing::warn!(
            ?err,
            workdir = ?linked.workdir,
            "failed to watch linked worktree; changes to its files will be missed"
        );
        problems.push(format!(
            "Could not watch the linked worktree at {}: changes to its files will not be noticed ({err:#})",
            linked.workdir.display()
        ));
    }
}

/// Stop watching the files of the `linked` worktree, undoing [`watch_linked_workdir()`] and
/// forgetting the directories that were dynamically watched since.
///
/// With a watch plan, the directories to stop watching are those the plan yields now. One that
/// became ignored in the meantime remains watched.
fn unwatch_linked_workdir(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    dynamically_watched_dirs: &mut HashSet<PathBuf>,
    main_workdir: &Path,
    linked: &WatchedCheckout,
    use_watch_plan: bool,
) {
    if linked.has_own_recursive_watch(main_workdir, use_watch_plan) {
        debouncer.watcher().unwatch(&linked.workdir).ok();
    }
    if !use_watch_plan {
        return;
    }
    let mut paths = debouncer.watcher().paths_mut();
    dynamically_watched_dirs.retain(|dir| {
        let is_in_workdir = dir.starts_with(&linked.workdir);
        if is_in_workdir {
            paths.remove(dir).ok();
        }
        !is_in_workdir
    });
    if let Ok(repo) = gix::open_opts(&linked.workdir, gix::open::Options::isolated()) {
        compute_watch_plan_for_repo(
            &repo,
            &linked.workdir,
            &linked.git_dir,
            &[],
            |path, _mode| {
                paths.remove(path).ok();
                Ok(std::ops::ControlFlow::Continue(()))
            },
        )
        .ok();
    }
    paths.commit().ok();
}

/// Watch the directories among the changed `paths` that aren't watched yet, and stop watching those that are gone.
///
/// There is an inherent race condition here where files created in a new directory before the
/// watch is established will be missed. That's not a problem right now as we don't really care
/// about the paths.
fn update_dynamic_watches(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    dynamically_watched_dirs: &mut HashSet<PathBuf>,
    paths: impl Iterator<Item = PathBuf>,
    problems: &mut Vec<String>,
) {
    #[derive(Debug)]
    enum Mode {
        AddWatch,
        RemoveWatch,
    }
    for path in paths {
        let mode = match path.symlink_metadata() {
            Ok(md)
                if is_watchable_directory(md.file_type())
                    && !dynamically_watched_dirs.contains(&path) =>
            {
                Mode::AddWatch
            }
            // We don't care if was dynamically watched, it might be watched during initial computation.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Mode::RemoveWatch,
            _ => continue,
        };
        tracing::trace!(?path, ?mode, "adding or removing dynamic watch");
        let res = match mode {
            Mode::AddWatch => debouncer
                .watcher()
                .watch(&path, notify::RecursiveMode::NonRecursive),
            Mode::RemoveWatch => debouncer.watcher().unwatch(&path),
        }
        .inspect_err(|err| {
            tracing::warn!(
                ?path,
                ?mode,
                ?err,
                "failed to add or remove watch; changes may be missed until restart"
            );
            problems.push(format!(
                "Could not update the watch of {}: changes there may not be noticed ({err})",
                path.display()
            ));
        });
        match mode {
            Mode::AddWatch if res.is_ok() => {
                dynamically_watched_dirs.insert(path);
            }
            _ => {
                // If adding OR removing a watch didn't work, just remove it from our list.
                // On linux, it seems to manage to remove the watch, but fails to communicate it,
                // so our own tracking list would be stale.
                dynamically_watched_dirs.remove(&path);
            }
        }
    }
}

#[cfg(target_family = "unix")]
fn is_interesting_kind(kind: notify::EventKind) -> bool {
    matches!(
        kind,
        // Of any kind: FSEvents reports a symlink, hard link or APFS clone (Finder's Duplicate,
        // `cp -c`) as `Other`, and one it can't tell as `Any`.
        notify::EventKind::Create(_)
            | notify::EventKind::Modify(notify::event::ModifyKind::Data(_))
            | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
            // This makes many more events happen, but we won't miss `touch this && rm this` kind of events.
            | notify::EventKind::Modify(notify::event::ModifyKind::Metadata(_))
            | notify::EventKind::Remove(_)
    )
}

#[cfg(target_os = "windows")]
fn is_interesting_kind(kind: notify::EventKind) -> bool {
    matches!(
        kind,
        notify::EventKind::Create(_) | notify::EventKind::Modify(_) | notify::EventKind::Remove(_)
    )
}

fn into_backoff_err(
    err: notify::Error,
    path: &Path,
) -> backoff::Error<Box<dyn std::error::Error + Send + Sync + 'static>> {
    match err.kind {
        notify::ErrorKind::PathNotFound => backoff::Error::permanent(
            anyhow!("{} not found", path.display()).into_boxed_dyn_error(),
        ),
        notify::ErrorKind::Io(_) | notify::ErrorKind::InvalidConfig(_) => {
            backoff::Error::permanent(anyhow::Error::from(err).into_boxed_dyn_error())
        }
        _ => backoff::Error::transient(anyhow::Error::from(err).into_boxed_dyn_error()),
    }
}

fn backoff_err_to_anyhow(
    err: backoff::Error<Box<dyn std::error::Error + Send + Sync + 'static>>,
) -> anyhow::Error {
    anyhow::Error::from_boxed(Box::from(err.to_string()))
}

/// The directory of all refs, relative to a git dir.
pub const REFS_DIR: &str = "refs";
/// `FETCH_HEAD`, relative to a git dir.
pub const FETCH_HEAD: &str = "FETCH_HEAD";
/// `HEAD`, relative to a git dir.
pub const HEAD: &str = "HEAD";
/// The reflog of `HEAD`, relative to a git dir.
pub const HEAD_ACTIVITY: &str = "logs/HEAD";
/// The reflog of the stash, relative to a git dir.
pub const STASH_ACTIVITY: &str = "logs/refs/stash";
/// The index, relative to a git dir.
pub const INDEX: &str = "index";
/// Where linked worktrees are registered, relative to the common git dir.
pub const LINKED_WORKTREES_DIR: &str = "worktrees";
/// The file in a linked worktree's git dir that names its checkout.
pub const LINKED_WORKTREE_GITDIR: &str = "gitdir";
/// Where submodule repositories live, relative to the common git dir.
pub const MODULES_DIR: &str = "modules";

/// Single files directly in a git dir whose change matters.
const INTERESTING_GIT_DIR_FILES: &[&str] = &[
    "HEAD",
    "index",
    "packed-refs",
    "config",
    "FETCH_HEAD",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "BISECT_LOG",
];

/// Directories in a git dir that exist only while an operation is in progress, and everything below them matters.
pub(crate) const STATE_DIRS: &[&str] = &["rebase-merge", "rebase-apply", "sequencer"];

#[derive(PartialEq)]
struct WatchedCheckout {
    id: Checkout,
    workdir: PathBuf,
    git_dir: PathBuf,
}

#[derive(Default)]
struct Changes {
    git: HashSet<PathBuf>,
    worktree: HashSet<PathBuf>,
    ignored: usize,
    git_noop: usize,
    ignore_filtering_ran: bool,
}

/// Distribute `paths` over the `checkouts` (the innermost root wins). Paths in `common_dir` that no checkout
/// claims are returned separately, relative to `common_dir`, if they are interesting.
fn changes_by_checkout<'a>(
    checkouts: impl Iterator<Item = &'a WatchedCheckout>,
    common_dir: Option<&Path>,
    paths: impl IntoIterator<Item = PathBuf>,
) -> (Vec<(&'a WatchedCheckout, Changes)>, HashSet<PathBuf>) {
    let mut paths_by_checkout: Vec<_> = checkouts.map(|checkout| (checkout, Vec::new())).collect();
    let mut common_changes = HashSet::new();
    for path in paths {
        let innermost = paths_by_checkout
            .iter_mut()
            .filter_map(|(checkout, paths)| {
                Some((checkout.depth_of_root_containing(&path)?, paths))
            })
            .max_by_key(|(depth, _)| *depth);
        if let Some((_, paths)) = innermost {
            paths.push(path);
        } else if let Some(common_dir) = common_dir
            && classify_file(common_dir, &path) == FileKind::Git
            && let Ok(relative) = path.strip_prefix(common_dir)
        {
            common_changes.insert(relative.to_owned());
        }
    }
    (
        paths_by_checkout
            .into_iter()
            .map(|(checkout, paths)| (checkout, checkout.changes(paths)))
            .collect(),
        common_changes,
    )
}

fn linked_checkouts(
    main_git_dir: &Path,
    linked_worktrees: &impl Fn() -> Result<Vec<LinkedWorktree>>,
) -> Option<Vec<WatchedCheckout>> {
    let worktrees = linked_worktrees()
        .inspect_err(|err| tracing::warn!(?err, "failed to list linked worktrees"))
        .ok()?;
    Some(
        worktrees
            .into_iter()
            .map(|worktree| WatchedCheckout::linked(main_git_dir, worktree))
            .collect(),
    )
}

impl Changes {
    fn linked_worktrees_may_differ(&self) -> bool {
        self.git.iter().any(|path| {
            path.starts_with(LINKED_WORKTREES_DIR) || path == Path::new(LINKED_WORKTREE_GITDIR)
        })
    }
}

impl WatchedCheckout {
    fn linked(main_git_dir: &Path, worktree: LinkedWorktree) -> Self {
        WatchedCheckout {
            git_dir: main_git_dir
                .join(LINKED_WORKTREES_DIR)
                .join(worktree.name.to_path_lossy()),
            workdir: gix::path::realpath(&worktree.workdir).unwrap_or(worktree.workdir),
            id: Checkout::Linked(worktree.name),
        }
    }

    fn has_own_recursive_watch(&self, main_workdir: &Path, use_watch_plan: bool) -> bool {
        !use_watch_plan && !self.workdir.starts_with(main_workdir)
    }

    fn depth_of_root_containing(&self, path: &Path) -> Option<usize> {
        [&self.git_dir, &self.workdir]
            .into_iter()
            .filter(|root| {
                path.strip_prefix(root)
                    .is_ok_and(|relative_path| !relative_path.as_os_str().is_empty())
            })
            .map(|root| root.components().count())
            .max()
    }

    fn changes(&self, paths: impl IntoIterator<Item = PathBuf>) -> Changes {
        let mut classified: Vec<_> = paths
            .into_iter()
            .map(|path| {
                let kind = classify_file(&self.git_dir, &path);
                (path, kind)
            })
            .collect();
        let mut changes = Changes {
            ignore_filtering_ran: self.mark_ignored(&mut classified),
            ..Default::default()
        };
        for (path, kind) in classified {
            match kind {
                FileKind::ProjectIgnored => changes.ignored += 1,
                FileKind::GitUninteresting => changes.git_noop += 1,
                FileKind::Git => {
                    if let Ok(relative_path) = path.strip_prefix(&self.git_dir) {
                        changes.git.insert(relative_path.to_owned());
                    }
                }
                FileKind::Project => {
                    if let Ok(relative_path) = path.strip_prefix(&self.workdir) {
                        changes.worktree.insert(relative_path.to_owned());
                    }
                }
            }
        }
        changes
    }

    fn mark_ignored(&self, classified: &mut [(PathBuf, FileKind)]) -> bool {
        if classified
            .iter()
            .any(|(_, kind)| *kind == FileKind::Project)
            && let Ok(repo) = gix::open(&self.workdir)
            && let Ok(index) = repo.index_or_empty()
            && let Ok(mut excludes) = repo.excludes(
                &index,
                None,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            )
        {
            let icase_acc = build_index_icase_accelerator_if_needed(&repo, &index);
            for (path, kind) in classified
                .iter_mut()
                .filter(|(_, kind)| *kind == FileKind::Project)
            {
                if let Ok(relative_path) = path.strip_prefix(&self.workdir) {
                    let is_dir = path.is_dir();
                    let is_excluded = excludes
                        .at_path(
                            relative_path,
                            is_dir.then_some(gix::index::entry::Mode::DIR),
                        )
                        .map(|platform| platform.is_excluded())
                        .unwrap_or(false);
                    if is_excluded
                        && to_repo_relative_path(relative_path).is_ok_and(|repo_relative_path| {
                            !is_tracked_in_index(
                                repo_relative_path.as_ref(),
                                is_dir,
                                &index,
                                icase_acc.as_ref(),
                            )
                        })
                    {
                        *kind = FileKind::ProjectIgnored
                    }
                }
            }
            true
        } else {
            false
        }
    }
}

/// A classification for a changed file.
#[derive(Debug, Eq, PartialEq)]
enum FileKind {
    /// A file in the `.git` repository of the current project itself.
    Git,
    /// Like `Git`, but shouldn't have any effect.
    GitUninteresting,
    /// A file in the worktree of the current project.
    Project,
    /// A file that was ignored in the project, and thus shouldn't trigger a computation.
    ProjectIgnored,
}

fn classify_file(git_dir: &Path, file_path: &Path) -> FileKind {
    if let Ok(check_file_path) = file_path.strip_prefix(git_dir) {
        if is_interesting_git_path(check_file_path) {
            FileKind::Git
        } else {
            FileKind::GitUninteresting
        }
    } else {
        FileKind::Project
    }
}

/// Whether a change of `rel`, a path relative to a git dir, can alter what a Git client shows.
///
/// Objects, `COMMIT_EDITMSG`, `MERGE_MSG`, `SQUASH_MSG`, lock files and our own drafts are not interesting.
fn is_interesting_git_path(rel: &Path) -> bool {
    let mut components = rel.components();
    let Some(first) = components.next().map(|c| c.as_os_str()) else {
        return false;
    };
    let rest_is_empty = components.as_path().as_os_str().is_empty();
    if rest_is_empty && INTERESTING_GIT_DIR_FILES.iter().any(|name| first == *name) {
        return true;
    }
    if first == REFS_DIR {
        return !rest_is_empty;
    }
    if STATE_DIRS.iter().any(|dir| first == *dir) {
        return true;
    }
    if first == "logs" {
        return rel == Path::new(HEAD_ACTIVITY) || rel == Path::new(STASH_ACTIVITY);
    }
    if first == LINKED_WORKTREE_GITDIR && rest_is_empty {
        return true;
    }
    if first == MODULES_DIR {
        return is_interesting_module_path(components.as_path());
    }
    is_linked_worktree_registration(rel)
}

/// `rel` is relative to `modules/`: the repository of a submodule. Its `HEAD`, `index` and refs count, and the
/// appearance of the directories on the way (so they can be watched), but never its object database.
fn is_interesting_module_path(rel: &Path) -> bool {
    let names: Vec<_> = rel.components().map(|c| c.as_os_str()).collect();
    if names
        .iter()
        .any(|name| *name == "objects" || *name == "logs")
    {
        return false;
    }
    match names.as_slice() {
        // `modules` itself and `modules/<name>`.
        [] | [_] => true,
        [.., last] => {
            *last == "HEAD" || *last == "index" || names[1..].iter().any(|n| *n == REFS_DIR)
        }
    }
}

fn is_linked_worktree_registration(git_dir_relative_path: &Path) -> bool {
    git_dir_relative_path
        .strip_prefix(LINKED_WORKTREES_DIR)
        .is_ok_and(|name_and_file| {
            let mut components = name_and_file.components();
            components.next();
            let file = components.as_path();
            file.as_os_str().is_empty() || file == Path::new(LINKED_WORKTREE_GITDIR)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn git_dir() -> &'static Path {
        Path::new("/repo/.git")
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn links_and_clones_count_as_created_and_removed() {
        use notify::{EventKind, event::CreateKind, event::RemoveKind};
        assert!(
            is_interesting_kind(EventKind::Create(CreateKind::Other)),
            "FSEvents reports a new symlink or APFS clone as `Other`"
        );
        assert!(
            is_interesting_kind(EventKind::Remove(RemoveKind::Other)),
            "and a removed symlink too"
        );
    }

    #[test]
    fn classify_local_ref() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/.git/refs/heads/main")),
            FileKind::Git
        );
    }

    #[test]
    fn classify_remote_ref() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/.git/refs/remotes/origin/main")),
            FileKind::Git
        );
    }

    #[test]
    fn classify_nested_remote_ref() {
        assert_eq!(
            classify_file(
                git_dir(),
                Path::new("/repo/.git/refs/remotes/origin/feature/branch")
            ),
            FileKind::Git
        );
    }

    #[test]
    fn classify_head() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/.git/HEAD")),
            FileKind::Git
        );
    }

    #[test]
    fn classify_objects_as_uninteresting() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/.git/objects/ab/cdef1234567890")),
            FileKind::GitUninteresting
        );
    }

    #[test]
    fn classify_worktree_file() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/src/main.rs")),
            FileKind::Project
        );
    }

    fn kind(path: &str) -> FileKind {
        classify_file(git_dir(), &Path::new("/repo/.git").join(path))
    }

    #[test]
    fn classify_everything_a_git_client_shows_as_interesting() {
        for path in [
            "HEAD",
            "index",
            "packed-refs",
            "config",
            "FETCH_HEAD",
            "ORIG_HEAD",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "REBASE_HEAD",
            "BISECT_LOG",
            "logs/HEAD",
            "logs/refs/stash",
            "refs/heads/main",
            "refs/tags/v1",
            "refs/stash",
            "refs/gitcthulhu/undo/1",
            "rebase-merge",
            "rebase-merge/git-rebase-todo",
            "rebase-apply/next",
            "sequencer/todo",
            "modules",
            "modules/sub",
            "modules/sub/HEAD",
            "modules/sub/index",
            "modules/sub/refs/remotes/origin/main",
            "modules/nested/name/HEAD",
        ] {
            assert_eq!(kind(path), FileKind::Git, "{path} is interesting");
        }
    }

    #[test]
    fn classify_noise_as_uninteresting() {
        for path in [
            "COMMIT_EDITMSG",
            "MERGE_MSG",
            "SQUASH_MSG",
            "gitcthulhu/commit-draft",
            "gitcthulhu",
            "index.lock",
            "refs",
            "objects/pack/pack-1.pack",
            "logs/refs/heads/main",
            "modules/sub/objects/ab/cdef",
            "modules/sub/objects",
            "modules/sub/FETCH_HEAD",
            "modules/sub/logs/HEAD",
            "gitbutler/REFRESH",
        ] {
            assert_eq!(kind(path), FileKind::GitUninteresting, "{path} is noise");
        }
    }

    #[test]
    fn classify_ignores_the_directory_itself() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/.git")),
            FileKind::GitUninteresting,
            "the git dir itself has no relative path"
        );
    }

    #[test]
    fn common_dir_changes_of_a_linked_worktree_are_split_off() {
        let main = WatchedCheckout {
            id: Checkout::Main,
            workdir: PathBuf::from("/linked"),
            git_dir: PathBuf::from("/repo/.git/worktrees/linked"),
        };
        let common = Path::new("/repo/.git");
        let (by_checkout, common_changes) = changes_by_checkout(
            std::iter::once(&main),
            Some(common),
            [
                PathBuf::from("/repo/.git/refs/tags/v1"),
                PathBuf::from("/repo/.git/packed-refs"),
                PathBuf::from("/repo/.git/objects/ab/cd"),
                PathBuf::from("/repo/.git/worktrees/linked/index"),
            ],
        );
        let mut common_changes: Vec<_> = common_changes.into_iter().collect();
        common_changes.sort();
        assert_eq!(
            common_changes,
            [PathBuf::from("packed-refs"), PathBuf::from("refs/tags/v1")],
            "refs and packed-refs of the common dir, relative to it; objects are noise"
        );
        assert_eq!(by_checkout.len(), 1, "one checkout");
        assert!(
            by_checkout[0].1.git.contains(Path::new("index")),
            "the index belongs to the linked git dir, not to the common dir"
        );
    }

    #[test]
    fn classify_linked_worktree_registration() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/.git/worktrees/name")),
            FileKind::Git
        );
    }

    #[test]
    fn classify_linked_worktree_location() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/.git/worktrees/name/gitdir")),
            FileKind::Git,
            "seen from the main git dir, for a linked worktree that isn't watched"
        );
        assert_eq!(
            classify_file(
                Path::new("/repo/.git/worktrees/name"),
                Path::new("/repo/.git/worktrees/name/gitdir")
            ),
            FileKind::Git,
            "seen from the git dir of a watched linked worktree"
        );
    }

    #[test]
    fn classify_linked_worktree_contents_as_uninteresting() {
        assert_eq!(
            classify_file(git_dir(), Path::new("/repo/.git/worktrees/name/HEAD")),
            FileKind::GitUninteresting
        );
    }
}
