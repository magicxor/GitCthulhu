//! Binding of an askpass session to the executor and the credential prompter (§1.8, §6.1).
//!
//! The executor (`cli::run`) does, for a command with askpass enabled:
//!
//! 1. `let askpass = Askpass::start(rt, ctx, repo).await?` before spawning, and adds `askpass.env()` to the child;
//! 2. runs `askpass.serve()` concurrently with the child. It never finishes while Git is happy; when it does, the
//!    [`Stop`] says why: [`Stop::Refused`] (the prompter cancelled; keep waiting for Git to fail by itself and report
//!    the prompt in `GitOutput::prompt_refused`) or [`Stop::Failed`] (infrastructure error).

#![allow(dead_code)] // The executor (WP-B0b) is the user.

use std::{collections::HashMap, mem::Discriminant, path::PathBuf};

use gitbutler_git::askpass_harness::{AskpassError, AskpassSession};

use crate::{
    CredentialRequest, CredentialResponse, GitError, GitRuntime, OpContext, PromptKind,
    credentials::classify,
};

/// An askpass server for one git invocation, answering through the operation's prompter.
pub(super) struct Askpass {
    session: AskpassSession,
    ctx: OpContext,
    repo: Option<PathBuf>,
}

/// Why [`Askpass::serve()`] ended.
#[derive(Debug)]
pub(super) enum Stop {
    /// The prompter declined (or was a `NoPrompter`) for this prompt.
    Refused(String),
    /// The askpass machinery itself failed.
    Failed(GitError),
}

impl Askpass {
    /// Create the session for a command run in `repo`. Fails when the helper program is missing.
    pub(super) async fn start(
        rt: &GitRuntime,
        ctx: &OpContext,
        repo: Option<PathBuf>,
    ) -> Result<Askpass, GitError> {
        let session = AskpassSession::new(rt.askpass_exe().to_owned())
            .await
            .map_err(infrastructure_error)?;
        Ok(Askpass {
            session,
            ctx: ctx.clone(),
            repo,
        })
    }

    /// `GITCTHULHU_ASKPASS_PIPE`, `GITCTHULHU_ASKPASS_SECRET`, `SSH_ASKPASS`, `SSH_ASKPASS_REQUIRE`.
    pub(super) fn env(&self) -> Vec<(String, String)> {
        self.session.env()
    }

    /// Answer prompts until the prompter refuses one or the machinery fails.
    ///
    /// Every prompt is classified, turned into a [`CredentialRequest`] and handed to the prompter on tokio's blocking
    /// pool: a modal dialog must not occupy a runtime worker that drives child I/O of other commands.
    pub(super) async fn serve(&self) -> Stop {
        let mut attempts: HashMap<Discriminant<PromptKind>, u32> = HashMap::new();
        let result = self
            .session
            .serve(|raw_prompt: String| {
                let kind = classify(&raw_prompt);
                let attempt = attempts.entry(std::mem::discriminant(&kind)).or_insert(0);
                *attempt += 1;
                let request = CredentialRequest {
                    kind,
                    raw_prompt,
                    operation: self.ctx.label.clone(),
                    repo: self.repo.clone(),
                    attempt: *attempt,
                    cancel: self.ctx.cancel.clone(),
                };
                let prompter = self.ctx.prompter.clone();
                async move {
                    let response =
                        tokio::task::spawn_blocking(move || prompter.prompt(&request)).await;
                    match response {
                        Ok(CredentialResponse::Answer(answer)) => {
                            // The protocol is line based.
                            Some(answer.expose().trim_end_matches(['\r', '\n']).to_owned())
                        }
                        // Cancelled, or the prompter panicked: treat both as a refusal.
                        Ok(CredentialResponse::Cancel) | Err(_) => None,
                    }
                }
            })
            .await;
        match result {
            Ok(never) => match never {},
            Err(AskpassError::NeedsAuthorization(prompt)) => Stop::Refused(prompt),
            Err(err) => Stop::Failed(infrastructure_error(err)),
        }
    }
}

/// Askpass infrastructure errors are `Other`, with the cause as message.
fn infrastructure_error(err: AskpassError) -> GitError {
    GitError::Other(anyhow::Error::new(err).context("The askpass helper could not be used"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{CancelToken, CredentialPrompter, RuntimeConfig, Sensitive, types::NoProgress};

    /// Answers the user name with a trailing line break, the first password with "first", and refuses any later
    /// password prompt; records every request.
    #[derive(Default)]
    struct Recorder {
        requests: Mutex<Vec<CredentialRequest>>,
    }

    impl CredentialPrompter for Recorder {
        fn prompt(&self, request: &CredentialRequest) -> CredentialResponse {
            self.requests
                .lock()
                .expect("not poisoned")
                .push(request.clone());
            match (&request.kind, request.attempt) {
                (PromptKind::HttpsUsername { .. }, _) => {
                    CredentialResponse::Answer(Sensitive::new("alice\r\n".into()))
                }
                (PromptKind::HttpsPassword { .. }, 1) => {
                    CredentialResponse::Answer(Sensitive::new("first".into()))
                }
                _ => CredentialResponse::Cancel,
            }
        }
    }

    /// Plays the helper program: one connection per prompt, returns the answers ("" = refused).
    async fn helper(pipe: String, secret: String, prompts: &[&str]) -> Vec<String> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufStream};
        let mut answers = Vec::new();
        for prompt in prompts {
            #[cfg(unix)]
            let stream = tokio::net::UnixStream::connect(&pipe)
                .await
                .expect("socket connects");
            #[cfg(windows)]
            let stream = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&pipe)
                .expect("pipe connects");
            let mut stream = BufStream::new(stream);
            stream
                .write_all(format!("{secret}\n{prompt}\n").as_bytes())
                .await
                .expect("write works");
            stream.flush().await.expect("flush works");
            let mut answer = String::new();
            stream.read_line(&mut answer).await.expect("read works");
            answers.push(answer);
        }
        answers
    }

    fn env_value(env: &[(String, String)], key: &str) -> String {
        env.iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("{key} is set"))
            .1
            .clone()
    }

    #[test]
    fn prompts_are_classified_counted_per_kind_and_refusal_stops_serving() {
        // The test process plays the helper, so it is the executable the session accepts.
        let rt = GitRuntime::new(RuntimeConfig {
            askpass_exe: Some(std::env::current_exe().expect("current exe")),
            ..RuntimeConfig::default()
        })
        .expect("git is installed");
        let recorder = Arc::new(Recorder::default());
        let ctx = OpContext {
            cancel: CancelToken::new(),
            progress: Arc::new(NoProgress),
            prompter: recorder.clone(),
            label: "Fetch origin".into(),
        };
        let repo = PathBuf::from("some/repo");

        let (stop, answers) = rt.tokio().block_on(async {
            let askpass = Askpass::start(&rt, &ctx, Some(repo.clone()))
                .await
                .expect("the session starts");
            let env = askpass.env();
            let helper = helper(
                env_value(&env, "GITCTHULHU_ASKPASS_PIPE"),
                env_value(&env, "GITCTHULHU_ASKPASS_SECRET"),
                &[
                    "Username for 'https://example.org': ",
                    "Password for 'https://alice@example.org': ",
                    "Password for 'https://alice@example.org': ",
                ],
            );
            tokio::join!(askpass.serve(), helper)
        });

        assert_eq!(
            answers,
            ["alice\n", "first\n", ""],
            "one line per answer (the prompter's line break is not doubled), nothing for the refused prompt"
        );
        assert!(
            matches!(&stop, Stop::Refused(p) if p == "Password for 'https://alice@example.org': "),
            "the refused prompt ends serving: {stop:?}"
        );
        let requests = recorder.requests.lock().expect("not poisoned");
        let seen: Vec<_> = requests
            .iter()
            .map(|r| (r.kind.clone(), r.attempt))
            .collect();
        let password = PromptKind::HttpsPassword {
            url: "https://alice@example.org".into(),
            username: Some("alice".into()),
        };
        assert_eq!(
            seen,
            [
                (
                    PromptKind::HttpsUsername {
                        url: "https://example.org".into()
                    },
                    1
                ),
                (password.clone(), 1),
                (password, 2),
            ],
            "classified, and attempts are counted per kind"
        );
        assert!(
            requests.iter().all(|r| r.operation == "Fetch origin"
                && r.repo.as_deref() == Some(repo.as_path())
                && r.raw_prompt.ends_with("': ")),
            "label, repository and raw prompt are passed on: {requests:?}"
        );
    }

    #[test]
    fn missing_helper_is_an_infrastructure_error() {
        let rt = GitRuntime::new(RuntimeConfig {
            askpass_exe: Some(PathBuf::from("definitely/not/there/askpass")),
            ..RuntimeConfig::default()
        })
        .expect("git is installed");
        let ctx = OpContext::silent(CancelToken::new());
        let err = rt
            .tokio()
            .block_on(Askpass::start(&rt, &ctx, None))
            .err()
            .expect("a missing helper fails the start");
        assert!(
            matches!(&err, GitError::Other(e) if format!("{e:#}").contains("definitely/not/there/askpass")),
            "the error names the expected path: {err:?}"
        );
    }
}
