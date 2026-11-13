//! The askpass harness: everything a caller needs to run `git` so that credential prompts of Git and OpenSSH end up
//! in the host application instead of a terminal.
//!
//! * [`AskpassSession`] is one askpass server (Unix socket or Windows named pipe) for the lifetime of one `git`
//!   invocation. Its [`env()`](AskpassSession::env) points `SSH_ASKPASS` at the helper program, and
//!   [`serve()`](AskpassSession::serve) answers every prompt the helper forwards, after checking that the peer is the
//!   helper executable and knows the session secret.
//! * [`ssh_env()`] and [`http_stall_limit_args()`] provide the transport defaults (SSH host key policy and keep-alives,
//!   HTTP stall limits) unless the user configured their own.
//!
//! The helper program itself lives in `gitcthulhu-git` (`helper_main::askpass_main`); this crate only has the server
//! side. The line protocol is: the helper sends the secret and the prompt (each one line, see [`encode_prompt()`]),
//! the server answers with one line, or closes the connection to refuse.

use std::{
    convert::Infallible,
    path::{Path, PathBuf},
};

use gix::bstr::ByteSlice;
use rand::{Rng, SeedableRng};

use crate::executor::tokio::{TokioAskpassServer, stat};
use crate::executor::{AskpassServer, FileStat, Pid, Socket};

/// The number of characters in the secret used for checking askpass invocations by ssh/git when connecting to our
/// process.
const ASKPASS_SECRET_LENGTH: usize = 24;

/// The unit separator, standing in for `\n` inside a prompt on the one-line wire protocol.
const LINE_BREAK_STAND_IN: char = '\u{1F}';

/// The file name of the askpass helper that [`askpass_exe_next_to_current_exe()`] looks for.
pub const ASKPASS_EXE_NAME: &str = "gitcthulhu-askpass";

/// Where a command runs, for config lookups of `core.sshCommand` / `http.lowSpeed*`.
#[derive(Debug, Clone, Copy)]
pub enum HarnessLocation<'a> {
    /// Inside the repository whose worktree or `.git` directory is given; its local configuration applies.
    Repo(&'a Path),
    /// Outside of any repository (clone, global configuration only).
    Global,
}

/// How unknown SSH host keys are treated by the default `GIT_SSH_COMMAND`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// `StrictHostKeyChecking=accept-new`: trust unknown hosts on first use, refuse changed keys.
    #[default]
    AcceptNew,
    /// `StrictHostKeyChecking=ask`: confirm every unknown host through the askpass prompt.
    Ask,
}

impl HostKeyPolicy {
    fn as_ssh_option(self) -> &'static str {
        match self {
            HostKeyPolicy::AcceptNew => "accept-new",
            HostKeyPolicy::Ask => "ask",
        }
    }
}

/// Errors of the askpass machinery.
#[derive(Debug, thiserror::Error)]
pub enum AskpassError {
    /// The askpass server could not be created or failed to accept a connection.
    #[error("failed to create or accept connections on the askpass server: {0}")]
    AskpassServer(#[source] std::io::Error),
    /// Reading from or writing to the askpass helper failed.
    #[error("i/o error communicating with the askpass helper: {0}")]
    AskpassIo(#[source] std::io::Error),
    /// The path of the running executable could not be determined.
    #[error("failed to determine path to this executable: {0}")]
    NoSelfExe(#[source] std::io::Error),
    /// The connecting helper sent the wrong secret.
    #[error("askpass secret mismatch")]
    AskpassSecretMismatch,
    /// The prompt handler gave no answer.
    #[error("git requires authorization credentials but none were provided: prompt was {0:?}")]
    NeedsAuthorization(String),
    /// The process id of the connecting peer is unknown.
    #[error("unable to determine PID of the askpass peer: {0}")]
    NoPid(#[source] std::io::Error),
    /// The connecting peer's process is gone or its image path is unreadable.
    #[error("failed to perform the askpass security check; no such PID: {0}")]
    NoSuchPid(Pid),
    /// The peer executable is on another device than the helper.
    #[error("failed to perform the askpass security check; device mismatch")]
    AskpassDeviceMismatch,
    /// The peer executable is not the helper.
    #[error("failed to perform the askpass security check; executable mismatch")]
    AskpassExecutableMismatch,
    /// The helper program does not exist.
    #[error("the askpass helper was not found at '{path}'")]
    AskpassExecutableNotFound {
        /// Where it was expected.
        path: String,
    },
}

/// Git's HTTP transport has no timeout, so a remote that accepts the connection and then goes silent hangs the
/// operation forever. These settings abort a transfer that moves less than 1 byte per second for 60 seconds, while
/// slow transfers that still make progress keep going.
const HTTP_STALL_LIMITS: [(&str, &str); 2] =
    [("http.lowSpeedLimit", "1"), ("http.lowSpeedTime", "60")];

/// Returns `-c http.lowSpeedLimit=1 -c http.lowSpeedTime=60` for each key the user hasn't configured.
pub fn http_stall_limit_args(loc: HarnessLocation<'_>) -> Vec<String> {
    let config = match loc {
        HarnessLocation::Repo(repo_path) => gix::open(repo_path)
            .ok()
            .map(|repo| repo.config_snapshot().plumbing().clone()),
        HarnessLocation::Global => gix::config::File::from_globals().ok().map(|mut config| {
            // Git applies `GIT_CONFIG_*` overrides on top; a repository's config has them already.
            if let Ok(overrides) = gix::config::File::from_environment_overrides() {
                config.append(overrides).ok();
            }
            config
        }),
    };
    HTTP_STALL_LIMITS
        .into_iter()
        .filter(|(key, _)| {
            config
                .as_ref()
                .is_none_or(|config| config.raw_value(*key).is_err())
        })
        .flat_map(|(key, value)| ["-c".to_owned(), format!("{key}={value}")])
        .collect()
}

/// The environment pairs that guarantee the SSH command Git uses is configured in either `GIT_SSH_COMMAND` or
/// `GIT_SSH`.
///
/// Resolution order reflects Git: `GIT_SSH_COMMAND` -> `core.sshCommand` -> `GIT_SSH`, see
/// <https://github.com/git/git/blob/60f07c4f5c5f81c8a994d9e06b31a4a3a1679864/connect.c#L1382-L1397>.
///
/// If the user configured ssh in any of these places, that choice is passed on unchanged and nothing is added.
/// Otherwise `GIT_SSH_COMMAND` becomes `ssh -o StrictHostKeyChecking=<policy> -o KbdInteractiveAuthentication=no -o
/// ServerAliveInterval=15 -o ServerAliveCountMax=4`.
pub fn ssh_env(loc: HarnessLocation<'_>, host_key_policy: HostKeyPolicy) -> Vec<(String, String)> {
    ssh_env_in(loc, host_key_policy, &|key| std::env::var(key).ok())
}

/// [`ssh_env()`] for a child whose environment is not the live process environment: `env` looks up the variables
/// (`GIT_SSH_COMMAND`, `GIT_SSH`) the way the child will see them.
pub fn ssh_env_in(
    loc: HarnessLocation<'_>,
    host_key_policy: HostKeyPolicy,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    // Neither GIT_SSH_COMMAND nor GIT_SSH are required to be unicode; a value that is not valid UTF-8 is treated as
    // unconfigured here.
    if let Ok(Some(git_ssh_command)) = configured_git_ssh_command(loc, env) {
        return vec![("GIT_SSH_COMMAND".into(), git_ssh_command)];
    }
    if let Some(git_ssh) = env("GIT_SSH") {
        return vec![("GIT_SSH".into(), git_ssh)];
    }

    // In test environments, we don't want to pollute the user's known hosts file.
    let additional_options = if cfg!(test) {
        " -o UserKnownHostsFile=/dev/null"
    } else {
        ""
    };
    // Keepalives close the connection once the server hasn't answered for 60 seconds (4 probes, 15 seconds apart);
    // without them a half-open connection hangs forever.
    let git_ssh_command = format!(
        "ssh -o StrictHostKeyChecking={} -o KbdInteractiveAuthentication=no -o ServerAliveInterval=15 -o ServerAliveCountMax=4{additional_options}",
        host_key_policy.as_ssh_option()
    );
    vec![("GIT_SSH_COMMAND".into(), git_ssh_command)]
}

/// `GIT_SSH_COMMAND` from the environment first, configuration second.
///
/// The environment is checked explicitly: `gix::config::File::from_globals()` (used outside of a repository) does not
/// apply environment overrides, and a returned default would otherwise replace the user's own `GIT_SSH_COMMAND` in
/// the child.
fn configured_git_ssh_command(
    loc: HarnessLocation<'_>,
    env: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<String>> {
    if let Some(git_ssh_command) = env("GIT_SSH_COMMAND") {
        return Ok(Some(git_ssh_command));
    }
    match loc {
        HarnessLocation::Repo(repo_path) => Ok(gix::open(repo_path)?
            .config_snapshot()
            .trusted_program(gix::config::tree::Core::SSH_COMMAND)?
            .and_then(|program| program.to_str().map(String::from))),
        HarnessLocation::Global => Ok(gix::config::File::from_globals()?
            .string(gix::config::tree::Core::SSH_COMMAND)
            .and_then(|program| program.to_str().ok().map(String::from))),
    }
}

/// Prepare `prompt` for the one-line wire protocol: `\r` is removed and `\n` becomes a unit separator (`\u{1F}`), so
/// that multi-line prompts (SSH host key confirmations) arrive complete. [`AskpassSession::serve()`] undoes this.
pub fn encode_prompt(prompt: &str) -> String {
    prompt
        .chars()
        .filter(|c| *c != '\r')
        .map(|c| if c == '\n' { LINE_BREAK_STAND_IN } else { c })
        .collect()
}

/// The inverse of [`encode_prompt()`] (up to the removed `\r`).
fn decode_prompt(line: &str) -> String {
    line.replace(LINE_BREAK_STAND_IN, "\n")
}

/// The askpass helper next to the running executable: `<dir of current_exe>/gitcthulhu-askpass[.exe]`. Under
/// `cargo test` the test runner lives in `target/<profile>/deps`; the binaries are one directory up.
pub fn askpass_exe_next_to_current_exe() -> Result<PathBuf, AskpassError> {
    let mut current_exe = std::env::current_exe().map_err(AskpassError::NoSelfExe)?;
    // On Windows, we get a `\\?\` prefix that has issues; leave the path alone there.
    if cfg!(unix) {
        current_exe = current_exe.canonicalize().unwrap_or(current_exe);
    }
    if current_exe.iter().nth_back(1) == Some("deps".as_ref())
        && let Some(parent) = current_exe.parent()
    {
        // `with_file_name` below then replaces the `deps` component.
        current_exe = parent.to_path_buf();
    }
    Ok(current_exe
        .with_file_name(ASKPASS_EXE_NAME)
        .with_extension(std::env::consts::EXE_EXTENSION))
}

/// How long a peer has to identify itself (send its secret and the prompt) after connecting. The helper does so at once;
/// a silent peer must not block the prompts of the command.
const AUTHENTICATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One askpass server for the lifetime of one git invocation.
pub struct AskpassSession {
    server: TokioAskpassServer,
    secret: String,
    exe: PathBuf,
    exe_stat: FileStat,
}

impl AskpassSession {
    /// Create the server and the session secret.
    ///
    /// Fails with [`AskpassError::AskpassExecutableNotFound`] if `askpass_exe` is missing.
    pub async fn new(askpass_exe: PathBuf) -> Result<Self, AskpassError> {
        let not_found = || AskpassError::AskpassExecutableNotFound {
            path: askpass_exe.display().to_string(),
        };
        // The peer's image path is always the resolved file, so compare against the resolved helper too: a helper
        // installed as a symbolic link (distribution packages, `/usr/local/bin` links) must still be accepted.
        let resolved_exe = tokio::fs::canonicalize(&askpass_exe)
            .await
            .map_err(|_| not_found())?;
        let exe_stat = stat(&resolved_exe).await.map_err(|_| not_found())?;
        if !exe_stat.is_regular_file {
            return Err(not_found());
        }

        #[cfg(unix)]
        let server = TokioAskpassServer::new().await;
        #[cfg(windows)]
        let server = TokioAskpassServer::new();
        let server = server.map_err(AskpassError::AskpassServer)?;

        // NB: StdRng is always a cryptographically secure random generator.
        let secret = rand::rngs::StdRng::from_os_rng()
            .sample_iter(&rand::distr::Alphanumeric)
            .take(ASKPASS_SECRET_LENGTH)
            .map(char::from)
            .collect::<String>();

        Ok(AskpassSession {
            server,
            secret,
            exe: askpass_exe,
            exe_stat,
        })
    }

    /// Environment to add to the git child: `GITCTHULHU_ASKPASS_PIPE`, `GITCTHULHU_ASKPASS_SECRET`, `SSH_ASKPASS` and
    /// `SSH_ASKPASS_REQUIRE=force`.
    ///
    /// `GIT_ASKPASS` is NOT set: a user's `core.askPass` keeps priority, and Git falls back to `SSH_ASKPASS`
    /// otherwise.
    ///
    /// `SSH_ASKPASS_REQUIRE` is available since OpenSSH 8.4 (2020), which every supported platform ships. On
    /// Windows, OpenSSH appears to require `force` to look at `SSH_ASKPASS` at all (there is no controlling terminal),
    /// see <https://github.com/PowerShell/Win32-OpenSSH/issues/2115>. With `force` neither `DISPLAY` nor detaching
    /// from the controlling terminal is needed.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("GITCTHULHU_ASKPASS_PIPE".into(), self.server.to_string()),
            ("GITCTHULHU_ASKPASS_SECRET".into(), self.secret.clone()),
            ("SSH_ASKPASS".into(), ssh_askpass_value(&self.exe)),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
        ]
    }

    /// Accept connections forever, verifying peer executable and secret, and answer each prompt with `on_prompt`.
    ///
    /// `None` as answer closes the connection without a reply and ends serving with
    /// [`AskpassError::NeedsAuthorization`], carrying the prompt. A connection that fails the security check (another
    /// executable, a wrong secret, a peer that vanished or stays silent, i/o trouble with it) is closed without an
    /// answer and logged, and serving goes on: any local process may connect to the pipe, and an unrelated one must not
    /// be able to cancel the prompts of the command. Only a failure of the server itself ends serving.
    ///
    /// The `on_prompt` futures must be `Send + 'static`: they are driven on the runtime, and may move an `Arc` clone
    /// of the prompter into `tokio::task::spawn_blocking`.
    pub async fn serve<F, Fut>(&self, mut on_prompt: F) -> Result<Infallible, AskpassError>
    where
        F: FnMut(String) -> Fut + Send,
        Fut: std::future::Future<Output = Option<String>> + Send + 'static,
    {
        loop {
            let mut sock = self
                .server
                .accept()
                .await
                .map_err(AskpassError::AskpassServer)?;
            let authenticated =
                tokio::time::timeout(AUTHENTICATION_TIMEOUT, self.authenticate(&mut sock)).await;
            let prompt = match authenticated {
                Ok(Ok(prompt)) => prompt,
                Ok(Err(err)) => {
                    tracing::warn!(%err, "Rejected a connection to the askpass server");
                    continue;
                }
                Err(_) => {
                    tracing::warn!(
                        "Rejected a connection to the askpass server: it did not identify itself in time"
                    );
                    continue;
                }
            };
            match on_prompt(prompt.clone()).await {
                Some(response) => {
                    if let Err(err) = sock.write_line(&response).await {
                        // The helper went away (git was killed); the next prompt, if any, comes on a new connection.
                        tracing::warn!(%err, "Could not send the answer to the askpass helper");
                    }
                }
                None => return Err(AskpassError::NeedsAuthorization(prompt)),
            }
        }
    }

    /// Verify the peer of `sock` and read the prompt it forwards.
    async fn authenticate<S>(&self, sock: &mut S) -> Result<String, AskpassError>
    where
        S: Socket<Error = std::io::Error>,
    {
        let peer_pid = sock.pid().map_err(AskpassError::NoPid)?;
        let peer_path = process_image_path(peer_pid)?;

        // `stat` does not follow symbolic links; compare file identities.
        let peer_stat = stat(&peer_path)
            .await
            .map_err(|_| AskpassError::NoSuchPid(peer_pid))?;
        if peer_stat.ino != self.exe_stat.ino {
            return Err(AskpassError::AskpassExecutableMismatch);
        }
        if peer_stat.dev != self.exe_stat.dev {
            return Err(AskpassError::AskpassDeviceMismatch);
        }

        let peer_secret = sock.read_line().await.map_err(AskpassError::AskpassIo)?;
        if peer_secret.trim() != self.secret {
            return Err(AskpassError::AskpassSecretMismatch);
        }

        let line = sock.read_line().await.map_err(AskpassError::AskpassIo)?;
        Ok(decode_prompt(&line))
    }
}

/// The value of `SSH_ASKPASS` for the helper at `exe`.
///
/// Git and OpenSSH start `SSH_ASKPASS` directly (no shell), so the path is not quoted. On Windows it uses forward
/// slashes, and a verbatim prefix (`\\?\C:\…`, `\\?\UNC\server\share\…`, as `canonicalize` returns) is removed:
/// the MSYS2 programs of Git for Windows cannot start such paths.
fn ssh_askpass_value(exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    if !cfg!(windows) {
        return exe.into_owned();
    }
    let exe = if let Some(unc) = exe.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = exe
        .strip_prefix(r"\\?\")
        .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
    {
        local.to_owned()
    } else {
        exe.into_owned()
    };
    exe.replace('\\', "/")
}

/// The full image path of process `pid`.
fn process_image_path(pid: Pid) -> Result<PathBuf, AskpassError> {
    #[cfg(unix)]
    let pid_u32: u32 = pid.try_into().map_err(|_| AskpassError::NoSuchPid(pid))?;
    #[cfg(windows)]
    let pid_u32: u32 = pid;
    let sysinfo_pid = sysinfo::Pid::from_u32(pid_u32);
    let mut system = sysinfo::System::new();
    // Only the image path of this one process is needed.
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[sysinfo_pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
    );
    system
        .process(sysinfo_pid)
        .and_then(|p| p.exe().map(Path::to_path_buf))
        .ok_or(AskpassError::NoSuchPid(pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the helper program: connects, sends secret and encoded prompt, reads the answer.
    async fn fake_helper(pipe: String, secret: String, prompt: &str) -> std::io::Result<String> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufStream};
        #[cfg(unix)]
        let stream = tokio::net::UnixStream::connect(&pipe).await?;
        #[cfg(windows)]
        let stream = tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe)?;
        let mut stream = BufStream::new(stream);
        stream
            .write_all(format!("{secret}\n{}\n", encode_prompt(prompt)).as_bytes())
            .await?;
        stream.flush().await?;
        let mut answer = String::new();
        AsyncBufReadExt::read_line(&mut stream, &mut answer).await?;
        Ok(answer.trim_end_matches(['\r', '\n']).to_owned())
    }

    fn env_value(env: &[(String, String)], key: &str) -> String {
        env.iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("{key} is set"))
            .1
            .clone()
    }

    #[test]
    fn prompt_encoding_roundtrips_multi_line_prompts() {
        let prompt = "The authenticity of host 'example.org (192.0.2.1)' can't be established.\nED25519 key fingerprint is SHA256:abc.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ";
        let encoded = encode_prompt(prompt);
        assert!(
            !encoded.contains('\n'),
            "the wire protocol is line based, so no line break may remain: {encoded:?}"
        );
        assert_eq!(decode_prompt(&encoded), prompt, "3 lines survive");
    }

    #[test]
    fn prompt_encoding_drops_carriage_returns() {
        assert_eq!(
            decode_prompt(&encode_prompt("a\r\nb")),
            "a\nb",
            "CRLF becomes LF"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiline_prompt_survives_askpass() {
        // The test process plays the helper, so it must be the executable the session accepts.
        let session = AskpassSession::new(std::env::current_exe().expect("current exe"))
            .await
            .expect("the session can be created");
        let env = session.env();
        let prompt = "line one\nline two\nline three? ";
        let client = tokio::spawn(fake_helper(
            env_value(&env, "GITCTHULHU_ASKPASS_PIPE"),
            env_value(&env, "GITCTHULHU_ASKPASS_SECRET"),
            prompt,
        ));

        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        // `serve` runs forever; stop it once the client got its answer.
        let answer = tokio::select! {
            served = session.serve(move |p| {
                seen_tx.send(p).ok();
                std::future::ready(Some("yes".to_owned()))
            }) => panic!("serving ended: {:?}", served.err()),
            answer = client => answer.expect("client task joins").expect("client i/o works"),
        };
        assert_eq!(answer, "yes", "the answer reaches the helper");
        assert_eq!(
            seen_rx.try_recv().expect("the prompt handler ran"),
            prompt,
            "the three lines arrive unchanged"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refused_prompt_ends_serving_with_the_prompt_and_no_answer() {
        let session = AskpassSession::new(std::env::current_exe().expect("current exe"))
            .await
            .expect("the session can be created");
        let env = session.env();
        let client = fake_helper(
            env_value(&env, "GITCTHULHU_ASKPASS_PIPE"),
            env_value(&env, "GITCTHULHU_ASKPASS_SECRET"),
            "Password for 'https://example.org': ",
        );
        let (served, answer) = tokio::join!(
            session.serve(|_| std::future::ready(None::<String>)),
            client
        );
        assert!(
            matches!(&served, Err(AskpassError::NeedsAuthorization(p)) if p == "Password for 'https://example.org': "),
            "the refused prompt is reported: {served:?}"
        );
        assert_eq!(
            answer.expect("client i/o works"),
            "",
            "the helper sees the connection closed without an answer"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wrong_secret_is_rejected_and_serving_goes_on() {
        let session = AskpassSession::new(std::env::current_exe().expect("current exe"))
            .await
            .expect("the session can be created");
        let env = session.env();
        let intruder = fake_helper(
            env_value(&env, "GITCTHULHU_ASKPASS_PIPE"),
            "not-the-secret".into(),
            "Password: ",
        );
        let real = async {
            // The real helper connects after the unrelated process was turned away.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            fake_helper(
                env_value(&env, "GITCTHULHU_ASKPASS_PIPE"),
                env_value(&env, "GITCTHULHU_ASKPASS_SECRET"),
                "Password for 'https://example.org': ",
            )
            .await
        };
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let (intruder_answer, real_answer) = tokio::select! {
            served = session.serve(move |p| {
                seen_tx.send(p).ok();
                std::future::ready(Some("hunter2".to_owned()))
            }) => panic!("serving ended: {:?}", served.err()),
            answers = async { tokio::join!(intruder, real) } => answers,
        };
        assert_eq!(
            intruder_answer.expect("client i/o works"),
            "",
            "the unauthenticated peer gets no answer"
        );
        assert_eq!(
            real_answer.expect("client i/o works"),
            "hunter2",
            "the real helper is still served"
        );
        assert_eq!(
            seen_rx.try_iter().collect::<Vec<_>>(),
            ["Password for 'https://example.org': "],
            "the prompt handler only saw the authenticated prompt"
        );
    }

    /// Regression: slow Git operations died as "failed to create askpass server: timed out" when the askpass server
    /// had an accept deadline. A paused clock makes any deadline fire at once.
    #[tokio::test]
    async fn serving_has_no_deadline() {
        let session = AskpassSession::new(std::env::current_exe().expect("current exe"))
            .await
            .expect("the session can be created");
        tokio::time::pause();
        let env = session.env();
        let serving = session.serve(|_| std::future::ready(Some("late".to_owned())));
        tokio::pin!(serving);
        tokio::select! {
            served = &mut serving => panic!("serving ended while idle: {:?}", served.err()),
            () = tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)) => {}
        }
        // Still serving after a day of idleness.
        let client = fake_helper(
            env_value(&env, "GITCTHULHU_ASKPASS_PIPE"),
            env_value(&env, "GITCTHULHU_ASKPASS_SECRET"),
            "Password: ",
        );
        let answer = tokio::select! {
            served = &mut serving => panic!("serving ended: {:?}", served.err()),
            answer = client => answer.expect("client i/o works"),
        };
        assert_eq!(answer, "late", "the late prompt is answered");
    }

    #[test]
    fn ssh_askpass_is_the_plain_helper_path() {
        let exe = if cfg!(windows) {
            r"C:\Program Files\GitCthulhu\gitcthulhu-askpass.exe"
        } else {
            "/opt/git cthulhu/gitcthulhu-askpass"
        };
        let expected = if cfg!(windows) {
            "C:/Program Files/GitCthulhu/gitcthulhu-askpass.exe"
        } else {
            exe
        };
        assert_eq!(
            ssh_askpass_value(Path::new(exe)),
            expected,
            "not quoted, forward slashes on Windows"
        );
    }

    #[cfg(windows)]
    #[test]
    fn ssh_askpass_drops_verbatim_prefixes() {
        assert_eq!(
            ssh_askpass_value(Path::new(r"\\?\C:\a\askpass.exe")),
            "C:/a/askpass.exe",
            "a local verbatim path becomes a plain drive path"
        );
        assert_eq!(
            ssh_askpass_value(Path::new(r"\\?\UNC\server\share\askpass.exe")),
            "//server/share/askpass.exe",
            "a verbatim UNC path becomes a plain UNC path"
        );
    }

    #[test]
    fn ssh_env_default_follows_the_host_key_policy() {
        if std::env::var_os("GIT_SSH").is_some() || std::env::var_os("GIT_SSH_COMMAND").is_some() {
            return; // the user's own configuration wins; nothing to assert
        }
        let dir = but_testsupport::gix_testtools::tempfile::tempdir().expect("temp dir");
        gix::init(dir.path()).expect("repository can be initialised");
        let env = ssh_env(HarnessLocation::Repo(dir.path()), HostKeyPolicy::Ask);
        let command = env_value(&env, "GIT_SSH_COMMAND");
        assert!(
            command.contains("StrictHostKeyChecking=ask"),
            "policy is applied: {command}"
        );
        assert!(
            command.contains("ServerAliveInterval=15"),
            "keep-alives are set: {command}"
        );
    }

    #[test]
    fn ssh_env_reads_the_given_environment_not_the_live_one() {
        let dir = but_testsupport::gix_testtools::tempfile::tempdir().expect("temp dir");
        gix::init(dir.path()).expect("repository can be initialised");
        let loc = HarnessLocation::Repo(dir.path());
        let env = ssh_env_in(loc, HostKeyPolicy::Ask, &|key| {
            (key == "GIT_SSH_COMMAND").then(|| "my-ssh -q".to_owned())
        });
        assert_eq!(
            env,
            [("GIT_SSH_COMMAND".to_owned(), "my-ssh -q".to_owned())],
            "the command of the given environment is passed on unchanged"
        );
    }
}
