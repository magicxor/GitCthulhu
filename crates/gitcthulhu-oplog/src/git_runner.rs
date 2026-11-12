//! Running the git executable for the few steps Git does best (`git <op> --quit`, `git stash store`).

use std::path::Path;

use bstr::ByteSlice as _;

use crate::OplogError;

/// Runs `git` for the oplog. The GUI injects an adapter over its own Git runtime, so the user's Git path and
/// environment rules apply; [`SystemGit`] is the default.
pub trait GitRunner: Send + Sync {
    /// Run `git <args>` in `workdir`; return Ok(stdout) on exit status 0, else Err(OplogError::GitCommand{args, stderr}).
    fn run(&self, workdir: &Path, args: &[&str]) -> Result<Vec<u8>, OplogError>;
}

/// Runs the git executable found by `gix::path::env::exe_invocation()`, without a console window on Windows,
/// with prompts disabled and English messages.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemGit;

impl GitRunner for SystemGit {
    fn run(&self, workdir: &Path, args: &[&str]) -> Result<Vec<u8>, OplogError> {
        let mut cmd = std::process::Command::new(gix::path::env::exe_invocation());
        cmd.current_dir(workdir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LANGUAGE", "en")
            .env("LC_MESSAGES", "C")
            .stdin(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let output = cmd.output()?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(OplogError::GitCommand {
                args: args.join(" "),
                stderr: output.stderr.to_str_lossy().trim_end().to_owned(),
            })
        }
    }
}
