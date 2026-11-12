//! The executor: building and running `git` child processes.
//!
//! The public types and the [`GitCommand`] builder are final. The `run*` functions are stubs (implemented by
//! WP-B0b, which also owns the `cli/*` submodules).

use std::{ffi::OsString, path::PathBuf};

use crate::{
    CancelToken, CommandRecord, GitError, GitRuntime, HookRun, OpClass, OpContext, OpWarning,
    RelaPath, Repo, Result, internal::pathspec,
};

mod askpass;
mod env;
mod kill;
mod porcelain;
mod progress;
mod trace2;

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
/// askpass infrastructure errors (Other), Io.
///
/// Stub (WP-B0b).
pub fn run(rt: &GitRuntime, cmd: GitCommand, ctx: &OpContext) -> Result<GitOutput> {
    let _ = (rt, cmd, ctx);
    Err(GitError::NotImplemented { op: "cli::run" })
}

/// `run`, then on non-zero exit: (1) post-hook-only failure => Ok with `warnings` (§1.11); (2) otherwise
/// `error::classify(class, &out, &internal::post_state::read(repo)?)`.
///
/// Stub (WP-B0b).
pub fn run_checked(
    repo: &Repo,
    cmd: GitCommand,
    ctx: &OpContext,
    class: OpClass,
) -> Result<GitOutput> {
    let _ = (repo, cmd, ctx, class);
    Err(GitError::NotImplemented {
        op: "cli::run_checked",
    })
}

/// Same for commands without an open repository (clone, `config --global`): `PostState::default()`.
///
/// Stub (WP-B0b).
pub fn run_checked_global(
    rt: &GitRuntime,
    cmd: GitCommand,
    ctx: &OpContext,
    class: OpClass,
) -> Result<GitOutput> {
    let _ = (rt, cmd, ctx, class);
    Err(GitError::NotImplemented {
        op: "cli::run_checked_global",
    })
}

/// Escape hatch for the GUI ("Run git command" / oplog adapter, 17 §4.1 GitRunner): plain args,
/// `CommandKind::Mutation` (so askpass with `NoPrompter`, i.e. prompts are refused), returns stdout on exit 0 else
/// `CommandFailed`.
///
/// Stub (WP-B0b).
pub fn run_simple(repo: &Repo, args: &[&str], cancel: &CancelToken) -> Result<Vec<u8>> {
    let _ = (repo, args, cancel);
    Err(GitError::NotImplemented {
        op: "cli::run_simple",
    })
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
}
