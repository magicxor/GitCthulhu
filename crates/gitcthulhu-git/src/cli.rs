//! The executor: building and running `git` child processes.
//!
//! Every command is spawned on the runtime's private tokio runtime with piped stdout/stderr, streaming readers, a
//! Job Object (Windows) or process group (Unix) for cancellation, an optional askpass session, and an optional
//! `GIT_TRACE2_EVENT` directory to attribute hook failures (§1.6, §1.7, §1.11, §2.3). The calling thread blocks
//! until the command is done; the public functions are blocking like the rest of the crate.

use std::{
    borrow::Cow,
    collections::VecDeque,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{
    CancelToken, CommandRecord, GitError, GitRuntime, HookRun, OpClass, OpContext, OpWarning,
    PostState, Progress, ProgressSink, RelaPath, Repo, Result,
    error::{classify, combined_output, command_failed},
    internal::{pathspec, post_state},
};

mod askpass;
mod env;
mod kill;
pub(crate) mod porcelain;
mod progress;
pub(crate) mod trace2;

/// How a command interacts with the repository and the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// Reads only; no askpass, `GIT_OPTIONAL_LOCKS=0`.
    Read,
    /// Writes the local repository.
    Mutation,
    /// Talks to a remote.
    Network,
}

/// Which editor Git gets for the command.
#[derive(Debug, Clone)]
pub enum EditorMode {
    /// `GIT_EDITOR=:`.
    Noop,
    /// The rebase editor helper, with the directory that holds the plan.
    RebaseHelper {
        /// The directory with the plan the helper swaps in.
        plan_dir: PathBuf,
    },
}

/// A `git` command line plus everything the executor needs to run it.
#[derive(Debug, Clone)]
pub struct GitCommand {
    /// Without "git".
    pub args: Vec<OsString>,
    /// Working directory.
    pub cwd: PathBuf,
    /// How it interacts with the repository.
    pub kind: CommandKind,
    /// Written then closed; `None` => null stdin.
    pub stdin: Option<Vec<u8>>,
    /// Parse stderr into `Progress` events.
    pub progress: bool,
    /// Set `GIT_TRACE2_EVENT` (§1.11).
    pub trace_hooks: bool,
    /// Run an `AskpassSession` bound to `ctx.prompter`; `new` sets it to `kind != Read` (§1.7); `with_askpass`
    /// forces it on.
    pub askpass: bool,
    /// Which editor Git gets.
    pub editor: EditorMode,
    /// Extra environment variables.
    pub extra_env: Vec<(OsString, OsString)>,
    /// Default 64 MiB; excess => `CommandFailed("output too large")`.
    pub stdout_cap: usize,
}

impl GitCommand {
    /// The default for [`GitCommand::stdout_cap`].
    pub const DEFAULT_STDOUT_CAP: usize = 64 * 1024 * 1024;

    /// A command in `repo`: cwd = workdir or git dir.
    pub fn new(repo: &Repo, kind: CommandKind) -> Self {
        let cwd = repo.workdir().unwrap_or_else(|| repo.git_dir()).to_owned();
        Self::global(cwd, kind)
    }

    /// A command without an open repository (clone, `config --global`, `safe.directory`).
    pub fn global(cwd: PathBuf, kind: CommandKind) -> Self {
        GitCommand {
            args: Vec::new(),
            cwd,
            kind,
            stdin: None,
            progress: false,
            trace_hooks: false,
            askpass: kind != CommandKind::Read,
            editor: EditorMode::Noop,
            extra_env: Vec::new(),
            stdout_cap: Self::DEFAULT_STDOUT_CAP,
        }
    }

    /// Append one argument.
    pub fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.args.push(a.into());
        self
    }

    /// Append several arguments.
    pub fn args<I, S>(mut self, a: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(a.into_iter().map(Into::into));
        self
    }

    /// Append `--pathspec-from-file=- --pathspec-file-nul` and feed the paths through stdin (avoids the Windows
    /// command-line limit of 32 KiB), each written as `:(literal)<path>\0` (`internal::pathspec::literal_nul_list`).
    ///
    /// Panics in debug builds if stdin is already set.
    pub fn pathspecs(mut self, paths: &[RelaPath]) -> Self {
        debug_assert!(self.stdin.is_none(), "stdin is already in use");
        self.args.push("--pathspec-from-file=-".into());
        self.args.push("--pathspec-file-nul".into());
        self.stdin = Some(pathspec::literal_nul_list(paths));
        self
    }

    /// Append `--` and then each path as a `:(literal)<path>` argument (for commands without
    /// `--pathspec-from-file`: `git diff`, `git submodule ...`, `git rm` with few paths). Never a bare path.
    pub fn literal_paths_after_dashdash(mut self, paths: &[RelaPath]) -> Self {
        self.args.push("--".into());
        self.args.extend(paths.iter().map(pathspec::literal_arg));
        self
    }

    /// Feed `bytes` to stdin.
    pub fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    /// Parse stderr into `Progress` events.
    pub fn with_progress(mut self) -> Self {
        self.progress = true;
        self
    }

    /// Trace hooks with `GIT_TRACE2_EVENT` so a failing hook can be named.
    pub fn with_hooks(mut self) -> Self {
        self.trace_hooks = true;
        self
    }

    /// Run an askpass session for this command, whatever its kind.
    pub fn with_askpass(mut self) -> Self {
        self.askpass = true;
        self
    }

    /// Choose the editor Git gets.
    pub fn editor(mut self, mode: EditorMode) -> Self {
        self.editor = mode;
        self
    }
}

/// The result of a finished git command.
#[derive(Debug, Clone)]
pub struct GitOutput {
    /// `None` = killed by signal.
    pub exit_code: Option<i32>,
    /// Captured stdout.
    pub stdout: Vec<u8>,
    /// Lossy UTF-8; head 64 KiB + "\n[...]\n" + tail 1 MiB if longer.
    pub stderr: String,
    /// The hooks Git ran.
    pub hooks: Vec<HookRun>,
    /// The command as recorded in the log.
    pub record: CommandRecord,
    /// Askpass NeedsAuthorization prompt.
    pub prompt_refused: Option<String>,
    /// Lock files deleted after a cancel.
    pub removed_stale_locks: Vec<PathBuf>,
    /// Filled by `run_checked` when only post hooks failed (§1.11); the exit code is then non-zero but the
    /// command is treated as successful.
    pub warnings: Vec<OpWarning>,
}

impl GitOutput {
    /// Exit code 0, or `run_checked` accepted it as success-with-post-hook-warnings.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) || !self.warnings.is_empty()
    }
}

/// Spawn, stream, wait. Ok for any exit code (callers classify). Err only for: GitNotFound (spawn), Cancelled,
/// askpass infrastructure errors (Other), Io; plus `CommandFailed` when stdout exceeds `GitCommand::stdout_cap`.
///
/// Blocks the calling thread; never call it on a thread of the runtime's own tokio runtime. On cancel the whole
/// process tree is terminated and the lock files the command left behind are removed (§1.6).
pub fn run(rt: &GitRuntime, cmd: GitCommand, ctx: &OpContext) -> Result<GitOutput> {
    execute(rt, cmd, ctx, Target::Cwd)
}

/// `run`, then on non-zero exit: (1) post-hook-only failure => Ok with `warnings` (§1.11); (2) otherwise
/// `error::classify(class, &out, &internal::post_state::read(repo)?)`.
///
/// A successful command whose post hooks failed (Git ignores the exit code of `post-commit` and `post-merge`) also
/// gets `warnings`, so the GUI can surface every failing hook (D0.6).
pub fn run_checked(
    repo: &Repo,
    cmd: GitCommand,
    ctx: &OpContext,
    class: OpClass,
) -> Result<GitOutput> {
    let out = execute(repo.runtime(), cmd, ctx, Target::Repo(repo))?;
    check(out, class, || post_state::read(repo))
}

/// Same for commands without an open repository (clone, `config --global`): `PostState::default()`.
pub fn run_checked_global(
    rt: &GitRuntime,
    cmd: GitCommand,
    ctx: &OpContext,
    class: OpClass,
) -> Result<GitOutput> {
    let out = execute(rt, cmd, ctx, Target::Global)?;
    check(out, class, || Ok(PostState::default()))
}

/// Escape hatch for the GUI ("Run git command" / oplog adapter, 17 §4.1 GitRunner): plain args,
/// `CommandKind::Mutation` (so askpass with `NoPrompter`, i.e. prompts are refused), returns stdout on exit 0 else
/// `CommandFailed`.
pub fn run_simple(repo: &Repo, args: &[&str], cancel: &CancelToken) -> Result<Vec<u8>> {
    let cmd = GitCommand::new(repo, CommandKind::Mutation).args(args.iter().copied());
    let out = execute(
        repo.runtime(),
        cmd,
        &OpContext::silent(cancel.clone()),
        Target::Repo(repo),
    )?;
    if out.exit_code == Some(0) {
        Ok(out.stdout)
    } else {
        Err(command_failed(&out))
    }
}

/// The post-processing of `run_checked*`: success, success with post-hook warnings, or a classified error.
fn check(
    mut out: GitOutput,
    class: OpClass,
    post: impl FnOnce() -> Result<PostState>,
) -> Result<GitOutput> {
    let mut failing = out
        .hooks
        .iter()
        .filter(|hook| hook.exit_code != Some(0))
        .peekable();
    let only_post_hooks_failed = failing.peek().is_some()
        && failing.all(|hook| trace2::hook_class(&hook.name) == trace2::HookClass::Post);
    if out.exit_code == Some(0) || only_post_hooks_failed {
        out.warnings = post_hook_warnings(&out);
        return Ok(out);
    }
    Err(classify(class, &out, &post()?))
}

/// One `PostHookFailed` per failing post hook.
fn post_hook_warnings(out: &GitOutput) -> Vec<OpWarning> {
    let mut failing = out
        .hooks
        .iter()
        .filter(|hook| {
            hook.exit_code != Some(0) && trace2::hook_class(&hook.name) == trace2::HookClass::Post
        })
        .peekable();
    if failing.peek().is_none() {
        return Vec::new();
    }
    let output = combined_output(out);
    failing
        .map(|hook| OpWarning::PostHookFailed {
            hook: hook.name.clone(),
            exit_code: hook.exit_code,
            output: output.clone(),
        })
        .collect()
}

/// What a command runs against; decides where stale locks are looked for and where configuration is read.
#[derive(Clone, Copy)]
enum Target<'a> {
    /// An open repository.
    Repo(&'a Repo),
    /// Whatever repository `GitCommand::cwd` is the root of (`run`).
    Cwd,
    /// No repository (clone, global configuration).
    Global,
}

/// Where lock files of a killed command are looked for.
enum LockDirs {
    Known(Vec<PathBuf>),
    OfCwd,
    None,
}

/// Askpass for one command.
struct AskpassSpec {
    exe: PathBuf,
    repo: Option<PathBuf>,
}

/// Everything the runtime task needs, owned.
struct Spawn {
    program: PathBuf,
    args: Vec<OsString>,
    vars: Vec<(OsString, OsString)>,
    extra_env: Vec<(OsString, OsString)>,
    cwd: PathBuf,
    stdin: Option<Vec<u8>>,
    stdout_cap: usize,
    progress: Option<Arc<dyn ProgressSink>>,
    askpass: Option<AskpassSpec>,
    lock_dirs: LockDirs,
    ctx: OpContext,
}

/// Why the executor killed a command.
#[derive(Debug, Clone, Copy)]
enum KillReason {
    Cancelled,
    StdoutOverflow,
}

/// How the process ended.
enum DriveEnd {
    Exited,
    Killed {
        reason: KillReason,
        observed: Vec<kill::LockObservation>,
    },
}

/// The result of the runtime task.
struct Driven {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    /// Git wrote more than `stdout_cap` bytes; `stdout` is incomplete. Also set when git exited before the overflow
    /// was noticed (an exit wins the race against the reader), so truncated output is never passed off as complete.
    stdout_overflow: bool,
    stderr: String,
    prompt_refused: Option<String>,
    askpass_error: Option<GitError>,
    end: DriveEnd,
}

/// Head of the stderr kept in `GitOutput::stderr`.
const STDERR_HEAD: usize = 64 * 1024;
/// Tail of the stderr kept in `GitOutput::stderr`.
const STDERR_TAIL: usize = 1024 * 1024;
/// Cap of `CommandRecord::stderr` (the log keeps many records).
const RECORD_STDERR: usize = 64 * 1024;
/// After git exited, how long to wait for processes that inherited its output pipes before giving up on them.
const READER_GRACE: Duration = Duration::from_secs(2);
/// `CREATE_NO_WINDOW`: no console window flashes up for the child.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn execute(
    rt: &GitRuntime,
    cmd: GitCommand,
    ctx: &OpContext,
    target: Target<'_>,
) -> Result<GitOutput> {
    ctx.cancel.check()?;
    if !cmd.cwd.is_dir() {
        return Err(GitError::io(
            &cmd.cwd,
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the working directory of the git command does not exist",
            ),
        ));
    }
    let shown_args = display_args(&cmd.args);
    let trace_dir = if cmd.trace_hooks {
        Some(
            tempfile::Builder::new()
                .prefix("gc-trace2-")
                .tempdir()
                .map_err(|err| GitError::io(std::env::temp_dir(), err))?,
        )
    } else {
        None
    };

    let mut vars = env::child_env(rt, &cmd, trace_dir.as_ref().map(|dir| dir.path()));
    let mut args = env::global_args(rt, cmd.kind);
    if cmd.kind != CommandKind::Read {
        let config_dir = match target {
            Target::Repo(_) | Target::Cwd => Some(cmd.cwd.as_path()),
            Target::Global => None,
        };
        let (transport_env, transport_args) =
            askpass::transport_settings(config_dir, rt.config().host_key_policy);
        vars.extend(transport_env);
        args.extend(transport_args);
    }
    let GitCommand {
        args: cmd_args,
        cwd,
        kind: _,
        stdin,
        progress,
        trace_hooks: _,
        askpass,
        editor: _,
        extra_env,
        stdout_cap,
    } = cmd;
    args.extend(cmd_args);

    let (lock_dirs, prompt_repo) = match target {
        Target::Repo(repo) => (
            LockDirs::Known(vec![
                repo.git_dir().to_owned(),
                repo.common_dir().to_owned(),
            ]),
            Some(repo.workdir().unwrap_or(repo.git_dir()).to_owned()),
        ),
        Target::Cwd => (LockDirs::OfCwd, Some(cwd.clone())),
        Target::Global => (LockDirs::None, None),
    };
    let spawn = Spawn {
        program: rt.git_path().to_owned(),
        args,
        vars,
        extra_env,
        cwd: cwd.clone(),
        stdin,
        stdout_cap,
        progress: progress.then(|| ctx.progress.clone()),
        askpass: (askpass && rt.has_askpass()).then(|| AskpassSpec {
            exe: rt.askpass_exe().to_owned(),
            repo: prompt_repo,
        }),
        lock_dirs,
        ctx: ctx.clone(),
    };

    ctx.progress.progress(Progress::Command(shown_args.clone()));
    tracing::debug!(args = %shown_args, cwd = %cwd.display(), "Running git");
    let started = Instant::now();
    let driven = block_on(rt, drive(spawn))??;
    let duration = started.elapsed();

    let output_too_large = |args: String| GitError::CommandFailed {
        args,
        exit_code: driven.exit_code,
        stderr: format!("output too large: more than {stdout_cap} bytes on stdout"),
        stdout_tail: String::new(),
    };
    if let DriveEnd::Killed { reason, observed } = &driven.end {
        testing::run_after_kill(&cwd);
        let removed = kill::remove_stale_locks(observed);
        tracing::debug!(args = %shown_args, ?reason, ?removed, "Killed git");
        return Err(match reason {
            KillReason::Cancelled => GitError::Cancelled,
            KillReason::StdoutOverflow => output_too_large(shown_args),
        });
    }
    if driven.stdout_overflow {
        return Err(output_too_large(shown_args));
    }

    let hooks = trace_dir
        .as_ref()
        .map(|dir| trace2::collect(dir.path()))
        .unwrap_or_default();
    drop(trace_dir);
    if let Some(err) = driven.askpass_error
        && driven.exit_code != Some(0)
    {
        return Err(err);
    }
    tracing::debug!(args = %shown_args, exit_code = ?driven.exit_code, ?duration, "git finished");
    Ok(GitOutput {
        exit_code: driven.exit_code,
        stdout: driven.stdout,
        record: CommandRecord {
            args: shown_args,
            exit_code: driven.exit_code,
            duration,
            stderr: record_stderr(&driven.stderr),
        },
        stderr: driven.stderr,
        hooks,
        prompt_refused: driven.prompt_refused,
        removed_stale_locks: Vec::new(),
        warnings: Vec::new(),
    })
}

/// Run `fut` on the runtime's tokio runtime and block the calling thread until it is done. Works from any thread
/// that is not one of that runtime's workers.
fn block_on<T: Send + 'static>(
    rt: &GitRuntime,
    fut: impl std::future::Future<Output = T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    rt.tokio().spawn(async move {
        // The receiver only goes away if the calling thread died; nothing to report to then.
        tx.send(fut.await).ok();
    });
    rx.recv().map_err(|_| {
        GitError::Other(anyhow::anyhow!(
            "The git executor task ended without a result (it panicked or the runtime shut down)"
        ))
    })
}

/// Spawn the child and drive it to its end: stream its output, serve askpass, react to cancellation.
async fn drive(spawn: Spawn) -> Result<Driven> {
    let Spawn {
        program,
        args,
        vars,
        extra_env,
        cwd,
        stdin,
        stdout_cap,
        progress,
        askpass,
        lock_dirs,
        ctx,
    } = spawn;
    let session = match &askpass {
        Some(spec) => Some(askpass::Askpass::start(&spec.exe, &ctx, spec.repo.clone()).await?),
        None => None,
    };

    let mut command = tokio::process::Command::new(&program);
    command
        .args(&args)
        .env_clear()
        .envs(vars)
        .current_dir(&cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(session) = &session {
        command.envs(session.env());
    }
    command.envs(extra_env);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    command.process_group(0);

    let started_at = SystemTime::now();
    let mut child = command.spawn().map_err(|err| spawn_error(&program, err))?;
    let mut tree = kill::ProcessTree::attach(&child);

    let stdout_buf = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(parking_lot::Mutex::new(CappedText::default()));
    let mut stdout_task = tokio::spawn(read_stdout(
        child.stdout.take(),
        stdout_buf.clone(),
        stdout_cap,
    ));
    let mut stderr_task = tokio::spawn(read_stderr(
        child.stderr.take(),
        stderr_buf.clone(),
        progress,
    ));
    let stdin_task = match (stdin, child.stdin.take()) {
        (Some(bytes), Some(mut pipe)) => Some(tokio::spawn(async move {
            // Git may exit without reading everything (e.g. on an error); that is not our failure.
            if let Err(err) = pipe.write_all(&bytes).await {
                tracing::debug!(%err, "Could not write all of git's stdin");
            }
        })),
        _ => None,
    };

    let mut stdout_done = false;
    let mut stdout_overflow = false;
    let mut prompt_refused = None;
    let mut askpass_error = None;
    let ending = {
        let cancelled = ctx.cancel.cancelled();
        tokio::pin!(cancelled);
        // Owns the askpass session, so the session (and its server) lives exactly as long as git runs.
        let serve = async move {
            match &session {
                Some(session) => session.serve().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(serve);
        let mut serving = true;
        loop {
            tokio::select! {
                // An exit that raced with a cancel wins: the operation did complete.
                biased;
                status = child.wait() => break Ok(status),
                () = &mut cancelled => break Err(KillReason::Cancelled),
                joined = &mut stdout_task, if !stdout_done => {
                    stdout_done = true;
                    stdout_overflow = matches!(joined, Ok(true));
                    if stdout_overflow {
                        break Err(KillReason::StdoutOverflow);
                    }
                }
                served = &mut serve, if serving => {
                    serving = false;
                    match served {
                        // Git fails by itself without an answer; keep waiting for it.
                        askpass::Stop::Refused(prompt) => prompt_refused = Some(prompt),
                        askpass::Stop::Failed(err) => {
                            tracing::warn!(%err, "The askpass server failed; prompts can no longer be answered");
                            askpass_error = Some(err);
                        }
                    }
                }
            }
        }
    };

    let (exit_code, end) = match ending {
        Ok(status) => {
            let status = status.map_err(|err| GitError::io(&program, err))?;
            // Windows: closing the job kills stragglers that still hold our pipes.
            tree.release();
            (status.code(), DriveEnd::Exited)
        }
        Err(reason) => {
            let dirs = match &lock_dirs {
                LockDirs::Known(dirs) => dirs.clone(),
                LockDirs::OfCwd => kill::git_dirs_of_cwd(&cwd),
                LockDirs::None => Vec::new(),
            };
            let observed = kill::observe_locks(&dirs, started_at);
            tree.terminate(&mut child).await;
            tree.release();
            let code = child.try_wait().ok().flatten().and_then(|s| s.code());
            (code, DriveEnd::Killed { reason, observed })
        }
    };

    // The pipes close once git and everything that inherited them have exited.
    let drained = tokio::time::timeout(READER_GRACE, async {
        if !stdout_done {
            // The reader may have hit the cap just as git exited (the exit wins the `select!`).
            stdout_overflow = matches!((&mut stdout_task).await, Ok(true));
        }
        (&mut stderr_task).await.ok();
    })
    .await;
    if drained.is_err() {
        tracing::warn!(
            "A process started by git still holds its output pipes; not waiting for it any longer"
        );
        stdout_task.abort();
        stderr_task.abort();
    }
    if let Some(task) = stdin_task {
        task.abort();
    }

    let stdout = std::mem::take(&mut *stdout_buf.lock());
    let stderr = stderr_buf.lock().finish();
    Ok(Driven {
        exit_code,
        stdout,
        stdout_overflow,
        stderr,
        prompt_refused,
        askpass_error,
        end,
    })
}

/// Collect stdout up to `cap` bytes; `true` if git wrote more.
async fn read_stdout(
    pipe: Option<tokio::process::ChildStdout>,
    buf: Arc<parking_lot::Mutex<Vec<u8>>>,
    cap: usize,
) -> bool {
    let Some(mut pipe) = pipe else {
        return false;
    };
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => return false,
            Ok(n) => {
                let mut out = buf.lock();
                if out.len() + n > cap {
                    return true;
                }
                out.extend_from_slice(&chunk[..n]);
            }
            Err(err) => {
                tracing::debug!(%err, "Reading git's stdout failed");
                return false;
            }
        }
    }
}

/// Split stderr into segments, emit progress events if wanted, and keep every non-progress line.
async fn read_stderr(
    pipe: Option<tokio::process::ChildStderr>,
    buf: Arc<parking_lot::Mutex<CappedText>>,
    progress: Option<Arc<dyn ProgressSink>>,
) {
    let Some(mut pipe) = pipe else {
        return;
    };
    let on_segment = |segment: &str| {
        if let Some(sink) = &progress {
            match progress::parse(segment) {
                Some(phase) => {
                    sink.progress(phase);
                    return;
                }
                None => sink.progress(Progress::Message(segment.to_owned())),
            }
        }
        buf.lock().push_line(segment);
    };
    let mut splitter = progress::SegmentSplitter::default();
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => splitter.push(&chunk[..n], on_segment),
            Err(err) => {
                tracing::debug!(%err, "Reading git's stderr failed");
                break;
            }
        }
    }
    splitter.finish(on_segment);
}

/// Stderr text, capped: the first 64 KiB and the last 1 MiB, with a marker where bytes were dropped.
#[derive(Default)]
struct CappedText {
    head: String,
    tail: VecDeque<u8>,
    dropped: u64,
}

impl CappedText {
    fn push_line(&mut self, line: &str) {
        if self.tail.is_empty() && self.head.len() + line.len() < STDERR_HEAD {
            self.head.push_str(line);
            self.head.push('\n');
            return;
        }
        self.tail.extend(line.as_bytes());
        self.tail.push_back(b'\n');
        if self.tail.len() > STDERR_TAIL {
            let excess = self.tail.len() - STDERR_TAIL;
            self.tail.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    fn finish(&mut self) -> String {
        let mut out = std::mem::take(&mut self.head);
        if self.dropped > 0 {
            out.push_str("\n[…]\n");
        }
        out.push_str(&String::from_utf8_lossy(self.tail.make_contiguous()));
        self.tail.clear();
        out
    }
}

/// `CommandRecord::stderr`: the whole stderr if short, else its last 64 KiB.
fn record_stderr(stderr: &str) -> String {
    if stderr.len() <= RECORD_STDERR {
        return stderr.to_owned();
    }
    let mut start = stderr.len() - RECORD_STDERR;
    while !stderr.is_char_boundary(start) {
        start += 1;
    }
    format!("[…]\n{}", &stderr[start..])
}

/// A spawn failure: the executable is missing (or not executable) => `GitNotFound`, anything else => `Io`.
fn spawn_error(program: &Path, err: std::io::Error) -> GitError {
    match err.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
            GitError::GitNotFound {
                searched: format!("{} ({err})", program.display()),
            }
        }
        _ => GitError::io(program, err),
    }
}

/// The command line for logs and `CommandRecord::args`: URL credentials replaced, arguments with whitespace quoted.
fn display_args(args: &[OsString]) -> String {
    let mut out = String::new();
    for (idx, arg) in args.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        let arg = arg.to_string_lossy();
        let arg = sanitize_arg(&arg);
        if arg.is_empty() || arg.contains(char::is_whitespace) {
            out.push('"');
            out.push_str(&arg);
            out.push('"');
        } else {
            out.push_str(&arg);
        }
    }
    out
}

/// Replace the user info of every URL in `arg` (`https://user:token@host` => `https://***@host`).
fn sanitize_arg(arg: &str) -> Cow<'_, str> {
    if !arg.contains("://") {
        return Cow::Borrowed(arg);
    }
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(idx) = rest.find("://") {
        let (before, after) = rest.split_at(idx + 3);
        out.push_str(before);
        let authority_end = after
            .find(['/', '?', '#', ' ', '\''])
            .unwrap_or(after.len());
        let authority = &after[..authority_end];
        match authority.rfind('@') {
            Some(at) => {
                out.push_str("***");
                out.push_str(&authority[at..]);
            }
            None => out.push_str(authority),
        }
        rest = &after[authority_end..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Seams for the integration tests. Not part of the API.
#[doc(hidden)]
pub mod testing {
    use std::{
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    type Hook = Arc<dyn Fn() + Send + Sync>;

    static AFTER_KILL: parking_lot::Mutex<Vec<(u64, PathBuf, Hook)>> =
        parking_lot::const_mutex(Vec::new());
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    /// Unregisters its hook when dropped.
    #[must_use = "the hook is unregistered when this is dropped"]
    pub struct AfterKillHook(u64);

    impl Drop for AfterKillHook {
        fn drop(&mut self) {
            AFTER_KILL.lock().retain(|(id, ..)| *id != self.0);
        }
    }

    /// Run `hook` after a killed command whose working directory is `cwd` was terminated and before its stale lock
    /// files are removed.
    pub fn after_kill(
        cwd: impl Into<PathBuf>,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> AfterKillHook {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        AFTER_KILL.lock().push((id, cwd.into(), Arc::new(hook)));
        AfterKillHook(id)
    }

    pub(super) fn run_after_kill(cwd: &Path) {
        let hooks: Vec<Hook> = {
            let registered = AFTER_KILL.lock();
            if registered.is_empty() {
                return;
            }
            let canonical = std::fs::canonicalize(cwd).ok();
            registered
                .iter()
                .filter(|(_, path, _)| {
                    path == cwd
                        || canonical.is_some() && std::fs::canonicalize(path).ok() == canonical
                })
                .map(|(.., hook)| hook.clone())
                .collect()
        };
        for hook in hooks {
            hook();
        }
    }

    /// The events the executor derives from a complete stderr byte stream (progress phases and messages).
    pub fn progress_events(stderr: &[u8]) -> Vec<crate::Progress> {
        super::progress::events(stderr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(kind: CommandKind) -> GitCommand {
        GitCommand::global(PathBuf::from("."), kind)
    }

    #[test]
    fn askpass_is_on_for_everything_but_reads() {
        assert!(!command(CommandKind::Read).askpass, "reads never prompt");
        assert!(
            command(CommandKind::Mutation).askpass,
            "mutations may run LFS filters"
        );
        assert!(
            command(CommandKind::Network).askpass,
            "network commands prompt"
        );
        assert!(
            command(CommandKind::Read).with_askpass().askpass,
            "with_askpass forces it on"
        );
    }

    #[test]
    fn pathspecs_go_through_stdin_as_literal_nul_list() {
        let cmd = command(CommandKind::Mutation)
            .arg("add")
            .pathspecs(&["[12].txt".into(), "b".into()]);
        assert_eq!(
            cmd.args,
            ["add", "--pathspec-from-file=-", "--pathspec-file-nul"],
            "paths are not on the command line"
        );
        assert_eq!(
            cmd.stdin.as_deref(),
            Some(&b":(literal)[12].txt\0:(literal)b\0"[..]),
            "paths are literal and NUL separated"
        );
    }

    #[test]
    fn dashdash_paths_are_literal_arguments() {
        let cmd = command(CommandKind::Read)
            .arg("diff")
            .literal_paths_after_dashdash(&[":!x".into()]);
        assert_eq!(
            cmd.args,
            ["diff", "--", ":(literal):!x"],
            "never a bare path"
        );
    }

    #[test]
    fn url_credentials_are_masked() {
        assert_eq!(
            sanitize_arg("https://alice:s3cret@example.com/r.git"),
            "https://***@example.com/r.git",
            "user and password"
        );
        assert_eq!(
            sanitize_arg("http.proxy=http://token@proxy:8080 and ssh://git@host/x"),
            "http.proxy=http://***@proxy:8080 and ssh://***@host/x",
            "every URL in the argument, a bare user too"
        );
        assert_eq!(
            sanitize_arg("https://example.com/a@b"),
            "https://example.com/a@b",
            "an @ in the path is not user info"
        );
        assert_eq!(
            display_args(&["commit".into(), "-m".into(), "two words".into(), "".into()]),
            r#"commit -m "two words" """#,
            "arguments with whitespace are quoted"
        );
    }

    #[test]
    fn stderr_keeps_head_and_tail_with_marker() {
        let mut text = CappedText::default();
        text.push_line("first");
        assert_eq!(text.finish(), "first\n", "short output is kept whole");

        let mut text = CappedText::default();
        let line = "x".repeat(1023);
        for _ in 0..(STDERR_HEAD + STDERR_TAIL) / 1024 + 10 {
            text.push_line(&line);
        }
        text.push_line("the end");
        let out = text.finish();
        assert!(out.contains("\n[…]\n"), "dropped bytes are marked");
        assert!(out.ends_with("the end\n"), "the tail is kept");
        assert!(
            out.len() <= STDERR_HEAD + STDERR_TAIL + 16,
            "the size is capped: {}",
            out.len()
        );
    }

    #[test]
    fn record_stderr_is_capped_on_a_char_boundary() {
        let long = "ö".repeat(RECORD_STDERR);
        let record = record_stderr(&long);
        assert!(record.starts_with("[…]\n"), "truncation is marked");
        assert!(
            record.len() <= RECORD_STDERR + 8,
            "capped: {}",
            record.len()
        );
    }
}
