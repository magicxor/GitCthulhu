//! AI service tests against a local OpenAI-compatible server (LM Studio's endpoint shape) that speaks just
//! enough HTTP for one request per connection.
#![cfg(feature = "ai")]

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::Command,
    sync::{Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use gitcthulhu_git::{
    CancelToken, GitRuntime, PatchText, PatchTextOptions, Repo, RuntimeConfig, diff,
};
use gitcthulhu_services::{
    Runtime,
    ai::{
        AiError, AiMissing, AiProvider, AiService, AiSettings, AiStatus, BranchNameContext,
        CommitMessageRequest, CommitStyle, PrRequest, PrText, SecretSource,
    },
};

// ---------------------------------------------------------------------------------------------- mock server

/// A request as the mock read it.
struct Request {
    head: String,
    body: String,
}

impl Request {
    /// The text of the last (user) message of the chat request.
    fn user_prompt(&self) -> String {
        let body: serde_json::Value = serde_json::from_str(&self.body).expect("a JSON body");
        body["messages"]
            .as_array()
            .and_then(|messages| messages.last())
            .and_then(|message| message["content"].as_str())
            .expect("a user message")
            .to_owned()
    }
}

fn read_request(stream: &mut TcpStream) -> Request {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).expect("the request arrives");
        assert!(n > 0, "the client closed before finishing its request");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).expect("the body arrives");
        assert!(n > 0, "the client closed before finishing its body");
        buf.extend_from_slice(&chunk[..n]);
    }
    Request {
        head,
        body: String::from_utf8_lossy(&buf[head_end..head_end + length]).into_owned(),
    }
}

/// Run `handler` for the first connection on an ephemeral port. The handle yields what the handler returned.
fn serve<T: Send + 'static>(
    handler: impl FnOnce(Request, TcpStream) -> T + Send + 'static,
) -> (SocketAddr, JoinHandle<T>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is available");
    let addr = listener
        .local_addr()
        .expect("a bound socket has an address");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the client connects");
        let request = read_request(&mut stream);
        handler(request, stream)
    });
    (addr, handle)
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("the client reads the response");
}

fn start_event_stream(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        )
        .expect("the client reads the head");
}

/// One `data:` event carrying a content delta.
fn delta(text: &str) -> String {
    let chunk = serde_json::json!({"choices": [{"delta": {"content": text}}]});
    format!("data: {chunk}\n\n")
}

fn send(stream: &mut TcpStream, text: &str) {
    stream
        .write_all(text.as_bytes())
        .expect("the client reads the events");
    stream.flush().expect("flush");
}

/// Stream `pieces` as chat-completion deltas, then `[DONE]`.
fn stream_pieces(stream: &mut TcpStream, pieces: &[&str]) {
    start_event_stream(stream);
    for piece in pieces {
        send(stream, &delta(piece));
    }
    send(stream, "data: [DONE]\n\n");
}

// -------------------------------------------------------------------------------------------------- helpers

struct Secrets(Vec<(&'static str, &'static str)>, Mutex<Vec<String>>);

impl Secrets {
    fn none() -> Self {
        Secrets(Vec::new(), Mutex::default())
    }
    fn with(handle: &'static str, key: &'static str) -> Self {
        Secrets(vec![(handle, key)], Mutex::default())
    }
    fn asked(&self) -> Vec<String> {
        self.1.lock().expect("not poisoned").clone()
    }
}

impl SecretSource for Secrets {
    fn secret(&self, handle: &str) -> Option<String> {
        self.1.lock().expect("not poisoned").push(handle.to_owned());
        self.0
            .iter()
            .find(|(h, _)| *h == handle)
            .map(|(_, key)| (*key).to_owned())
    }
}

fn service() -> AiService {
    AiService::new(Runtime::new().expect("runtime starts"))
}

/// A service configured for LM Studio at `addr`.
fn service_at(addr: SocketAddr) -> AiService {
    let ai = service();
    let mut settings = AiSettings {
        provider: Some(AiProvider::LMStudio),
        ..AiSettings::default()
    };
    settings.lmstudio.endpoint = format!("http://{addr}/v1");
    settings.lmstudio.model = "test-model".into();
    ai.configure(&settings, &Secrets::none())
        .expect("a local provider needs no key");
    ai
}

fn patch(text: &str) -> PatchText {
    PatchText {
        files: Vec::new(),
        text: text.to_owned(),
        truncated_files: 0,
    }
}

fn commit_request(text: &str) -> CommitMessageRequest {
    CommitMessageRequest {
        patch: patch(text),
        notes: String::new(),
        style: CommitStyle::Conventional,
    }
}

// ------------------------------------------------------------------------------------------ configuration

#[test]
fn not_configured_reports_missing_key() {
    let ai = service();
    assert_eq!(
        ai.status(),
        AiStatus::NotConfigured {
            missing: AiMissing::Provider
        },
        "a fresh service has no provider"
    );

    let mut settings = AiSettings {
        provider: Some(AiProvider::OpenAi),
        ..AiSettings::default()
    };
    let secrets = Secrets::none();
    let err = ai
        .configure(&settings, &secrets)
        .expect_err("OpenAI without a key is not usable");
    assert!(
        matches!(err, AiError::NotConfigured(AiMissing::Key)),
        "the key is what is missing: {err:?}"
    );
    assert_eq!(
        ai.status(),
        AiStatus::NotConfigured {
            missing: AiMissing::Key
        },
        "status reports the same"
    );
    assert_eq!(
        secrets.asked(),
        ["aiOpenAIKey"],
        "only the selected provider's key is requested from the keychain"
    );

    let err = ai
        .generate_commit_message(&commit_request("x"), &CancelToken::new(), &mut |_| {})
        .expect_err("nothing can be generated");
    assert!(
        matches!(err, AiError::NotConfigured(AiMissing::Key)),
        "generating says what is missing: {err:?}"
    );

    ai.configure(&settings, &Secrets::with("aiOpenAIKey", "sk-test"))
        .expect("a key makes OpenAI usable");
    assert_eq!(
        ai.status(),
        AiStatus::Ready {
            provider: "OpenAI".into(),
            model: "gpt-5.4-nano".into()
        },
        "ready with the default model"
    );

    settings.provider = Some(AiProvider::Anthropic);
    ai.configure(&settings, &Secrets::with("aiAnthropicKey", "k"))
        .expect("anthropic key");
    settings.provider = Some(AiProvider::OpenRouter);
    let err = ai
        .configure(&settings, &Secrets::with("aiAnthropicKey", "k"))
        .expect_err("another provider's key does not count");
    assert!(matches!(err, AiError::NotConfigured(AiMissing::Key)));

    settings.provider = Some(AiProvider::Ollama);
    settings.ollama.endpoint = "not an endpoint".into();
    let err = ai
        .configure(&settings, &Secrets::none())
        .expect_err("a broken endpoint");
    assert!(matches!(err, AiError::NotConfigured(AiMissing::Endpoint)));
    settings.ollama.endpoint = "127.0.0.1:11434".into();
    settings.ollama.model = "  ".into();
    let err = ai
        .configure(&settings, &Secrets::none())
        .expect_err("a blank model");
    assert!(matches!(err, AiError::NotConfigured(AiMissing::Model)));

    settings.provider = None;
    ai.configure(&settings, &Secrets::none())
        .expect("selecting no provider switches AI off without an error");
    assert_eq!(
        ai.status(),
        AiStatus::NotConfigured {
            missing: AiMissing::Provider
        },
        "and the state follows"
    );
}

#[test]
fn local_providers_ask_the_keychain_for_nothing() {
    let ai = service();
    for provider in [AiProvider::Ollama, AiProvider::LMStudio] {
        let secrets = Secrets::none();
        ai.configure(
            &AiSettings {
                provider: Some(provider),
                ..AiSettings::default()
            },
            &secrets,
        )
        .expect("the defaults of a local provider are usable");
        assert_eq!(
            secrets.asked(),
            Vec::<String>::new(),
            "{provider:?} needs no key, so no keychain prompt appears"
        );
        assert!(
            matches!(ai.status(), AiStatus::Ready { .. }),
            "{provider:?} is ready"
        );
    }
}

// ---------------------------------------------------------------------------------------------- generation

#[test]
fn commit_message_streams_cleaned_text_and_returns_it_wrapped() {
    let long = "word ".repeat(30);
    let (addr, server) = serve(move |request, mut stream| {
        stream_pieces(
            &mut stream,
            &[
                "<think>Let me look at the diff.</think>\n\n",
                "```\nAdd pagination\n\n",
                &long,
                "\n```",
            ],
        );
        request
    });
    let ai = service_at(addr);

    let mut seen: Vec<String> = Vec::new();
    let message = ai
        .generate_commit_message(
            &CommitMessageRequest {
                patch: patch("Changed files:\n- src/a.rs (modified) +1 -0\n"),
                notes: "pagination for orders".into(),
                style: CommitStyle::Conventional,
            },
            &CancelToken::new(),
            &mut |text| seen.push(text.to_owned()),
        )
        .expect("generation succeeds");

    assert!(
        message.starts_with("Add pagination\n\nword word"),
        "fence and reasoning are gone: {message:?}"
    );
    assert!(
        message.lines().all(|line| line.chars().count() <= 72),
        "the final text is wrapped to 72 columns: {message:?}"
    );
    assert!(
        seen.iter()
            .all(|text| !text.contains("think") && !text.contains("```")),
        "partial texts never show reasoning or fences: {seen:?}"
    );
    assert!(
        seen.windows(2).all(|pair| pair[0] != pair[1]),
        "identical texts are not reported twice: {seen:?}"
    );
    assert_eq!(
        seen.first().map(String::as_str),
        Some("Add pagination"),
        "nothing is shown while the model reasons"
    );

    let request = server.join().expect("server thread");
    let prompt = request.user_prompt();
    assert!(
        prompt.contains("semantic commit prefix"),
        "the conventional prompt: {prompt}"
    );
    assert!(
        prompt.contains("The author's notes on it:\n```\npagination for orders\n```"),
        "the notes are passed: {prompt}"
    );
    assert!(
        request.head.starts_with("POST /v1/chat/completions "),
        "{}",
        request.head
    );
}

#[test]
fn plain_imperative_style_changes_the_instructions() {
    let (addr, server) = serve(|request, mut stream| {
        stream_pieces(&mut stream, &["Add search"]);
        request
    });
    let ai = service_at(addr);
    ai.generate_commit_message(
        &CommitMessageRequest {
            style: CommitStyle::PlainImperative,
            ..commit_request("diff")
        },
        &CancelToken::new(),
        &mut |_| {},
    )
    .expect("generation succeeds");
    let prompt = server.join().expect("server thread").user_prompt();
    assert!(
        prompt.contains("imperative mood") && !prompt.contains("semantic commit prefix"),
        "{prompt}"
    );
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn prompt_contains_file_list_and_truncation_marker() {
    // A real staged change set cut down by a tight budget, as the GUI builds it.
    let dir = tempfile::tempdir().expect("temp dir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("keep.txt"), "k\n").expect("write");
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "one"]);
    for name in ["a.txt", "b.txt", "c.txt"] {
        let body: String = (0..100).map(|i| format!("line {i} of {name}\n")).collect();
        std::fs::write(dir.path().join(name), body).expect("write");
    }
    git(dir.path(), &["add", "-A"]);
    let rt = GitRuntime::new(RuntimeConfig::default()).expect("git runtime");
    let repo = Repo::open(&rt, dir.path()).expect("repo opens");
    let patch = diff::staged_patch_text(
        &repo,
        &PatchTextOptions {
            budget_chars: 150,
            per_file_min_chars: 40,
            skip_globs: Vec::new(),
        },
        &CancelToken::new(),
    )
    .expect("patch text");
    assert!(
        patch.truncated_files > 0,
        "the budget cut files: {}",
        patch.text
    );

    let (addr, server) = serve(|request, mut stream| {
        stream_pieces(&mut stream, &["Add three files"]);
        request
    });
    let ai = service_at(addr);
    ai.generate_commit_message(
        &CommitMessageRequest {
            patch,
            notes: String::new(),
            style: CommitStyle::Conventional,
        },
        &CancelToken::new(),
        &mut |_| {},
    )
    .expect("generation succeeds");

    let prompt = server.join().expect("server thread").user_prompt();
    for name in ["a.txt", "b.txt", "c.txt"] {
        assert!(
            prompt.contains(&format!("- {name} (added) +100 -0")),
            "the file list names {name} even if its diff was cut: {prompt}"
        );
    }
    assert!(
        prompt.contains("[diff truncated:"),
        "the model is told that files were cut: {prompt}"
    );
}

#[test]
fn stream_cancel_closes_connection() {
    let (closed_tx, closed_rx) = mpsc::channel();
    let (addr, server) = serve(move |_request, mut stream| {
        start_event_stream(&mut stream);
        send(&mut stream, &delta("Add"));
        // Hold the response open, as a model that keeps generating would, until the client goes away.
        let mut buf = [0u8; 64];
        let closed_at = loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break Instant::now(),
                Ok(_) => {}
            }
        };
        closed_tx.send(closed_at).ok();
    });
    let ai = service_at(addr);
    let cancel = CancelToken::new();
    let (first_tx, first_rx) = mpsc::channel();
    let worker = std::thread::spawn({
        let cancel = cancel.clone();
        move || {
            ai.generate_commit_message(&commit_request("diff"), &cancel, &mut |text| {
                first_tx.send(text.to_owned()).ok();
            })
        }
    });

    let first = first_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the first text arrives while the response is still open");
    assert_eq!(first, "Add", "the streamed prefix is delivered");
    let cancelled_at = Instant::now();
    cancel.cancel();
    let result = worker.join().expect("worker thread");
    assert!(
        matches!(result, Err(AiError::Cancelled)),
        "cancelling reports Cancelled: {result:?}"
    );
    let closed_at = closed_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the server sees the connection close");
    assert!(
        closed_at.saturating_duration_since(cancelled_at) < Duration::from_secs(2),
        "the connection closes right after the cancel, not when the model finishes"
    );
    server.join().expect("server thread");
}

#[test]
fn already_cancelled_token_does_not_touch_the_network() {
    // Nothing listens on this port; a request would fail with a network error rather than Cancelled.
    let ai = service_at("127.0.0.1:9".parse().expect("address"));
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = ai
        .generate_commit_message(&commit_request("diff"), &cancel, &mut |_| {})
        .expect_err("cancelled");
    assert!(matches!(err, AiError::Cancelled), "{err:?}");
}

#[test]
fn empty_answer_error() {
    for pieces in [
        &[][..],
        &["<think>only thoughts</think>"][..],
        &["```\n```"][..],
    ] {
        let (addr, server) = serve(move |_request, mut stream| {
            stream_pieces(&mut stream, pieces);
        });
        let ai = service_at(addr);
        let err = ai
            .generate_commit_message(&commit_request("diff"), &CancelToken::new(), &mut |_| {})
            .expect_err("nothing usable was said");
        assert!(
            matches!(err, AiError::EmptyAnswer),
            "{pieces:?} is an empty answer, got {err:?}"
        );
        server.join().expect("server thread");
    }
}

#[test]
fn pr_text_streams_title_and_body() {
    let (addr, server) = serve(|request, mut stream| {
        stream_pieces(
            &mut stream,
            &[
                "Add sea",
                "rch\n\n",
                "# Add search\n- Fast",
                "\n- Typo tolerant",
            ],
        );
        request
    });
    let ai = service_at(addr);
    let mut seen: Vec<PrText> = Vec::new();
    let text = ai
        .generate_pr_text(
            &PrRequest {
                title: "wip".into(),
                body: String::new(),
                commit_messages: vec!["Add index".into(), "Add query parser".into()],
            },
            &CancelToken::new(),
            &mut |text| seen.push(text.clone()),
        )
        .expect("generation succeeds");
    assert_eq!(
        text,
        PrText {
            title: "Add search".into(),
            body: "- Fast\n- Typo tolerant".into()
        },
        "the echoed title heading is dropped from the body"
    );
    assert_eq!(
        seen.first().map(|t| t.title.as_str()),
        Some("Add sea"),
        "partial titles stream"
    );
    let prompt = server.join().expect("server thread").user_prompt();
    assert!(
        prompt.contains("Working title:\n```\nwip\n```")
            && prompt.contains("Add index\n\n---\n\nAdd query parser"),
        "title and the commit messages (oldest first) are in the prompt: {prompt}"
    );
}

#[test]
fn branch_name_is_sanitized_and_invalid_names_are_an_empty_answer() {
    for (answer, expected) in [
        ("```\nFeat/Add Search\n```", Some("feat/add-search")),
        ("fix..oops", None),
    ] {
        let (addr, server) = serve(move |request, mut stream| {
            respond(
                &mut stream,
                "200 OK",
                &serde_json::json!({"choices": [{"message": {"role": "assistant", "content": answer}}]})
                    .to_string(),
            );
            request
        });
        let ai = service_at(addr);
        let result = ai.suggest_branch_name(
            &BranchNameContext::CommitMessage("Add search".into()),
            &CancelToken::new(),
        );
        match expected {
            Some(name) => assert_eq!(result.expect("a name"), name),
            None => assert!(matches!(result, Err(AiError::EmptyAnswer)), "{result:?}"),
        }
        let request = server.join().expect("server thread");
        let body: serde_json::Value = serde_json::from_str(&request.body).expect("JSON");
        assert_eq!(
            body["stream"],
            serde_json::Value::Null,
            "branch names are not streamed"
        );
    }
}

#[test]
fn branch_name_from_the_staged_patch_sends_the_patch() {
    let (addr, server) = serve(|request, mut stream| {
        respond(
            &mut stream,
            "200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"fix/null-check"}}]}"#,
        );
        request
    });
    let name = service_at(addr)
        .suggest_branch_name(
            &BranchNameContext::Staged(patch("+    if x.is_none() { return; }\n")),
            &CancelToken::new(),
        )
        .expect("a name");
    assert_eq!(name, "fix/null-check");
    let prompt = server.join().expect("server thread").user_prompt();
    assert!(
        prompt.contains("```diff\n+    if x.is_none() { return; }\n```"),
        "the staged patch is the context: {prompt}"
    );
}

#[test]
fn test_connection_maps_provider_failures() {
    let cases = [
        ("401 Unauthorized", "auth"),
        ("429 Too Many Requests", "rate"),
        ("500 Internal Server Error", "provider"),
    ];
    for (status, expected) in cases {
        let (addr, server) = serve(move |_request, mut stream| {
            respond(&mut stream, status, r#"{"error":{"message":"nope"}}"#);
        });
        let err = service_at(addr)
            .test_connection(&CancelToken::new())
            .expect_err("an error status fails the check");
        let got = match err {
            AiError::Auth(_) => "auth",
            AiError::RateLimited => "rate",
            AiError::Provider(_) => "provider",
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(got, expected, "{status}");
        server.join().expect("server thread");
    }

    let (addr, server) = serve(|request, mut stream| {
        respond(
            &mut stream,
            "200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":""},"finish_reason":"length"}]}"#,
        );
        request
    });
    service_at(addr)
        .test_connection(&CancelToken::new())
        .expect("an answer that is empty still proves the provider answers");
    let body: serde_json::Value =
        serde_json::from_str(&server.join().expect("server thread").body).expect("JSON");
    assert_eq!(body["max_tokens"], 16, "the check asks for a tiny answer");
}

#[test]
fn network_failure_is_a_network_error() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("port");
    let addr = listener.local_addr().expect("addr");
    drop(listener); // Nothing listens any more.
    let err = service_at(addr)
        .generate_commit_message(&commit_request("diff"), &CancelToken::new(), &mut |_| {})
        .expect_err("connection refused");
    assert!(matches!(err, AiError::Network(_)), "{err:?}");
}
