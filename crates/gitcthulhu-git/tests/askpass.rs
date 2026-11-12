//! Tests of the askpass flow (spec §1.8, §6.1): helper program, askpass session, prompt classification and the
//! prompter.
//!
//! The git children are spawned here with the same ingredients the executor uses (an `AskpassSession` whose answers
//! come from a `CredentialPrompter` on tokio's blocking pool), so these tests do not depend on `cli::run` (WP-B0b).

mod support;

use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gitbutler_git::askpass_harness::{AskpassError, AskpassSession};
use gitcthulhu_git::{
    CancelToken, CredentialPrompter, CredentialRequest, CredentialResponse, PromptKind, Sensitive,
    credentials::classify,
};
use support::TestEnv;
use tokio::{io::AsyncWriteExt as _, process::Command};

const HELPER: &str = env!("CARGO_BIN_EXE_gitcthulhu-git-askpass");

/// Answers by prompt kind, records the requests, and optionally sleeps before answering.
struct KindPrompter {
    delay: Duration,
    requests: Mutex<Vec<CredentialRequest>>,
}

impl KindPrompter {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(KindPrompter {
            delay,
            requests: Mutex::default(),
        })
    }
}

impl CredentialPrompter for KindPrompter {
    fn prompt(&self, request: &CredentialRequest) -> CredentialResponse {
        self.requests
            .lock()
            .expect("not poisoned")
            .push(request.clone());
        std::thread::sleep(self.delay);
        let answer = match &request.kind {
            PromptKind::HttpsUsername { .. } => "alice",
            PromptKind::HttpsPassword { .. } => "s3cret",
            _ => return CredentialResponse::Cancel,
        };
        CredentialResponse::Answer(Sensitive::new(answer.to_owned()))
    }
}

struct Finished {
    success: bool,
    stdout: String,
    stderr: String,
    /// Set when the prompter cancelled (the session ended with `NeedsAuthorization`).
    refused: Option<String>,
}

/// Runs `git <args>` in `env`'s isolated world with an askpass session answered by `prompter`.
async fn run_git(
    env: &TestEnv,
    args: &[&str],
    stdin: &[u8],
    prompter: Arc<dyn CredentialPrompter>,
    extra_env: &[(&str, &str)],
) -> Finished {
    let session = AskpassSession::new(HELPER.into())
        .await
        .expect("the helper binary exists");
    let mut cmd = Command::new(env.rt.git_path());
    cmd.args(args)
        .current_dir(&env.home)
        .env_remove("GIT_ASKPASS")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &env.global_config)
        .env("HOME", &env.home)
        .env("USERPROFILE", &env.home)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in session.env().into_iter().chain(
        extra_env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
    ) {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = cmd.spawn().expect("git can be spawned");
    let mut child_stdin = child.stdin.take().expect("stdin is piped");
    let stdin = stdin.to_vec();
    tokio::spawn(async move {
        child_stdin.write_all(&stdin).await.ok();
    });
    let output = child.wait_with_output();

    let cancel = CancelToken::new();
    let mut attempts = 0u32;
    let serving = session.serve(|raw: String| {
        attempts += 1;
        let request = CredentialRequest {
            kind: classify(&raw),
            raw_prompt: raw,
            operation: "test".into(),
            repo: None,
            attempt: attempts,
            cancel: cancel.clone(),
        };
        let prompter = prompter.clone();
        async move {
            match tokio::task::spawn_blocking(move || prompter.prompt(&request)).await {
                Ok(CredentialResponse::Answer(answer)) => Some(answer.expose().to_owned()),
                _ => None,
            }
        }
    });
    tokio::pin!(output);
    let mut refused = None;
    let output = tokio::select! {
        output = &mut output => output,
        served = serving => {
            match served {
                Err(AskpassError::NeedsAuthorization(prompt)) => refused = Some(prompt),
                other => panic!("serving ended unexpectedly: {:?}", other.err()),
            }
            // Git fails by itself once the helper got no answer.
            output.await
        }
    }
    .expect("git finishes");
    Finished {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        refused,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn askpass_prompt_roundtrip() {
    let env = TestEnv::new();
    let prompter = KindPrompter::new(Duration::ZERO);
    let done = run_git(
        &env,
        &["credential", "fill"],
        b"protocol=https\nhost=example.invalid\n\n",
        prompter.clone(),
        &[],
    )
    .await;
    assert!(
        done.success,
        "git credential fill succeeds: {}",
        done.stderr
    );
    assert!(
        done.stdout.contains("username=alice"),
        "the answer to the username prompt reaches git: {}",
        done.stdout
    );
    assert!(
        done.stdout.contains("password=s3cret"),
        "the answer to the password prompt reaches git: {}",
        done.stdout
    );
    let requests = prompter.requests.lock().expect("not poisoned").clone();
    assert_eq!(requests.len(), 2, "username and password were asked");
    assert!(
        matches!(&requests[0].kind, PromptKind::HttpsUsername { url } if url.contains("example.invalid")),
        "classified as a username prompt: {:?}",
        requests[0]
    );
    assert!(
        matches!(&requests[1].kind, PromptKind::HttpsPassword { username: Some(user), .. } if user == "alice"),
        "classified as a password prompt for alice: {:?}",
        requests[1]
    );
    assert_eq!(requests[0].attempt, 1, "first prompt of its kind");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_prompt_is_reported_and_git_fails() {
    let env = TestEnv::new();
    // Cancels everything: the prompter answers `Cancel` for kinds it does not know; make it know none.
    struct Refuse;
    impl CredentialPrompter for Refuse {
        fn prompt(&self, _: &CredentialRequest) -> CredentialResponse {
            CredentialResponse::Cancel
        }
    }
    let done = run_git(
        &env,
        &["credential", "fill"],
        b"protocol=https\nhost=example.invalid\n\n",
        Arc::new(Refuse),
        &[],
    )
    .await;
    assert!(!done.success, "git fails without an answer");
    let refused = done.refused.expect("the refused prompt is reported");
    assert!(
        refused.starts_with("Username for 'https://example.invalid"),
        "the prompt text is kept: {refused:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn askpass_used_for_mutations() {
    // A `!`-alias sees the environment the session adds: the helper is what Git (and ssh) would run.
    let env = TestEnv::new();
    let done = run_git(
        &env,
        &["-c", "alias.show-env=!env", "show-env"],
        b"",
        KindPrompter::new(Duration::ZERO),
        &[],
    )
    .await;
    assert!(done.success, "the alias runs: {}", done.stderr);
    let ssh_askpass = done
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("SSH_ASKPASS="))
        .expect("SSH_ASKPASS is set");
    assert!(
        ssh_askpass.replace('\\', "/").ends_with(
            &HELPER
                .replace('\\', "/")
                .rsplit('/')
                .next()
                .expect("a file name")
                .to_owned()
        ),
        "SSH_ASKPASS points at the helper: {ssh_askpass}"
    );
    assert!(
        done.stdout.contains("SSH_ASKPASS_REQUIRE=force"),
        "ssh is forced to use it: {}",
        done.stdout
    );
    assert!(
        done.stdout.contains("GITCTHULHU_ASKPASS_PIPE="),
        "the pipe is announced"
    );
    assert!(
        !done.stdout.lines().any(|l| l.starts_with("GIT_ASKPASS=")),
        "GIT_ASKPASS is left alone so that core.askPass keeps priority"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_concurrent_prompts_do_not_block_io() {
    let env = Arc::new(TestEnv::new());
    let started = Instant::now();
    let prompters = [
        KindPrompter::new(Duration::from_secs(2)),
        KindPrompter::new(Duration::from_secs(2)),
    ];
    let prompting: Vec<_> = prompters
        .iter()
        .map(|prompter| {
            let env = env.clone();
            let prompter = prompter.clone();
            tokio::spawn(async move {
                run_git(
                    &env,
                    &["credential", "fill"],
                    b"protocol=https\nhost=example.invalid\n\n",
                    prompter,
                    &[],
                )
                .await
            })
        })
        .collect();

    // Wait until both prompters are blocked in their first prompt, then run a command that does not prompt.
    let deadline = Instant::now() + Duration::from_secs(20);
    while prompters
        .iter()
        .any(|p| p.requests.lock().expect("not poisoned").is_empty())
    {
        assert!(
            Instant::now() < deadline,
            "both commands reach their first prompt"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let quick = Instant::now();
    let version = Command::new(env.rt.git_path())
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .expect("git --version runs");
    assert!(version.status.success(), "git --version succeeds");
    assert!(
        quick.elapsed() < Duration::from_millis(1500),
        "a command without prompt finished while the prompts were blocked: {:?}",
        quick.elapsed()
    );

    for handle in prompting {
        let done = handle.await.expect("task joins");
        assert!(done.success, "prompting command succeeds: {}", done.stderr);
    }
    // Two prompts of 2 s each per command (username, password) on separate threads: ~4 s, not 8 s.
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "the two commands prompted concurrently: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_binary_forwards_multi_line_prompts_and_prints_the_answer() {
    // As OpenSSH starts it for a host key confirmation: the prompt is one argument spanning several lines.
    let session = AskpassSession::new(HELPER.into())
        .await
        .expect("the helper binary exists");
    let prompt = "The authenticity of host 'example.org (192.0.2.1)' can't be established.\r\nED25519 key fingerprint is SHA256:abc.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ";
    let mut cmd = Command::new(HELPER);
    cmd.arg(prompt)
        .envs(session.env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let helper = cmd.output();
    tokio::pin!(helper);
    let (seen_tx, seen_rx) = std::sync::mpsc::channel();
    let output = tokio::select! {
        output = &mut helper => output,
        served = session.serve(move |p: String| {
            seen_tx.send(p).ok();
            std::future::ready(Some("yes".to_owned()))
        }) => panic!("serving ended: {:?}", served.err()),
    }
    .expect("the helper runs");
    assert!(
        output.status.success(),
        "the helper succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "yes\n",
        "the answer is printed as one line"
    );
    let seen = seen_rx.try_recv().expect("the prompt reached the session");
    assert_eq!(
        seen,
        prompt.replace("\r\n", "\n"),
        "all three lines arrive (CRLF as LF)"
    );
    assert!(
        matches!(classify(&seen), PromptKind::HostKey { ref host, .. } if host == "example.org"),
        "the complete prompt is classified as a host key confirmation"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_binary_fails_without_output_when_refused() {
    let session = AskpassSession::new(HELPER.into())
        .await
        .expect("the helper binary exists");
    let mut cmd = Command::new(HELPER);
    cmd.arg("Password: ")
        .envs(session.env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (served, output) = tokio::join!(
        session.serve(|_| std::future::ready(None::<String>)),
        cmd.output()
    );
    let output = output.expect("the helper runs");
    assert!(
        matches!(served, Err(AskpassError::NeedsAuthorization(ref p)) if p == "Password: "),
        "the refusal is reported: {served:?}"
    );
    assert!(!output.status.success(), "the helper fails");
    assert!(output.stdout.is_empty(), "and prints no answer for Git");
}

#[test]
fn helper_binary_refuses_to_run_without_a_session() {
    let output = std::process::Command::new(HELPER)
        .arg("Password: ")
        .env_remove("GITCTHULHU_ASKPASS_PIPE")
        .env_remove("GITCTHULHU_ASKPASS_SECRET")
        .output()
        .expect("the helper runs");
    assert!(!output.status.success(), "the helper fails");
    assert!(output.stdout.is_empty(), "and prints no answer");
}

#[test]
fn missing_helper_is_reported_before_spawning() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let err = runtime
        .block_on(AskpassSession::new(
            "definitely/not/there/gitcthulhu-askpass".into(),
        ))
        .err()
        .expect("a missing helper is an error");
    assert!(
        matches!(err, AskpassError::AskpassExecutableNotFound { .. }),
        "got {err:?}"
    );
}
