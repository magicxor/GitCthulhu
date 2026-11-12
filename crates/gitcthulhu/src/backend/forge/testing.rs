#![cfg_attr(not(test), allow(dead_code))]

//! Test and QA support for the forge GUI: an in-memory keychain, a local fake forge (a GitHub Enterprise server on
//! loopback) and a `ServicesHandle` over given directories. Nothing here talks to the internet or to the
//! system keychain.

use std::any::Any;
use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use crate::backend::services::ServicesHandle;

static SECRETS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

struct MemoryEntry(String);

impl keyring::credential::CredentialApi for MemoryEntry {
    fn set_password(&self, password: &str) -> keyring::Result<()> {
        if let Ok(mut secrets) = SECRETS.lock() {
            secrets.insert(self.0.clone(), password.to_owned());
        }
        Ok(())
    }
    fn set_secret(&self, _secret: &[u8]) -> keyring::Result<()> {
        Err(keyring::Error::NoEntry)
    }
    fn get_password(&self) -> keyring::Result<String> {
        SECRETS
            .lock()
            .ok()
            .and_then(|secrets| secrets.get(&self.0).cloned())
            .ok_or(keyring::Error::NoEntry)
    }
    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        Err(keyring::Error::NoEntry)
    }
    fn delete_credential(&self) -> keyring::Result<()> {
        SECRETS
            .lock()
            .ok()
            .and_then(|mut secrets| secrets.remove(&self.0))
            .map(|_| ())
            .ok_or(keyring::Error::NoEntry)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct MemoryBuilder;

impl keyring::credential::CredentialBuilderApi for MemoryBuilder {
    fn build(
        &self,
        _target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<keyring::Credential>> {
        Ok(Box::new(MemoryEntry(format!("{service}/{user}"))))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Routes every keychain access of the process to memory.
pub(crate) fn install_memory_keyring() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| keyring::set_default_credential_builder(Box::new(MemoryBuilder)));
}

/// A request the fake forge received.
#[derive(Clone, Debug)]
pub(crate) struct Req {
    /// `GET`, `POST`, ...
    pub method: String,
    /// Path and query.
    pub url: String,
    /// The `Authorization` header.
    pub auth: Option<String>,
    /// The body.
    pub body: String,
}

/// What the fake forge answers.
pub(crate) enum Reply {
    /// A JSON body with a status.
    Json(u16, String),
}

type Handler = Box<dyn Fn(&Req) -> Option<Reply> + Send + Sync>;

/// A fake forge on a free loopback port. Unknown paths answer 404.
pub(crate) struct MockForge {
    port: u16,
    log: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    offline: Arc<AtomicBool>,
    unauthorized: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut start = String::new();
    reader.read_line(&mut start).ok()?;
    let mut parts = start.split_whitespace();
    let method = parts.next()?.to_owned();
    let url = parts.next()?.to_owned();
    let mut length = 0usize;
    let mut auth = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "authorization" => auth = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Req {
        method,
        url,
        auth,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        401 => "Unauthorized",
        404 => "Not Found",
        422 => "Unprocessable Entity",
        _ => "Status",
    };
    let message = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(message.as_bytes()).ok();
    stream.flush().ok();
}

impl MockForge {
    /// Serves `handler`; the account endpoint (`/api/v3/user`, login `octocat`) is built in.
    pub(crate) fn start(handler: impl Fn(&Req) -> Option<Reply> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let port = listener.local_addr().expect("local address").port();
        let handler: Arc<Handler> = Arc::new(Box::new(handler));
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let offline = Arc::new(AtomicBool::new(false));
        let unauthorized = Arc::new(AtomicBool::new(false));
        let thread = {
            let (log, stop, offline, unauthorized) = (
                log.clone(),
                stop.clone(),
                offline.clone(),
                unauthorized.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    stream.set_nonblocking(false).ok();
                    if offline.load(Ordering::SeqCst) {
                        // The connection is dropped without an answer: a transport failure.
                        drop(stream);
                        continue;
                    }
                    let (log, handler, unauthorized) =
                        (log.clone(), handler.clone(), unauthorized.clone());
                    std::thread::spawn(move || {
                        let Some(req) = read_request(&mut stream) else {
                            return;
                        };
                        if let Ok(mut log) = log.lock() {
                            log.push(req.clone());
                        }
                        if unauthorized.load(Ordering::SeqCst) {
                            respond(&mut stream, 401, r#"{"message":"Bad credentials"}"#);
                            return;
                        }
                        if req.url == "/api/v3/user" {
                            respond(&mut stream, 200, &user_json("octocat").to_string());
                            return;
                        }
                        match handler(&req) {
                            Some(Reply::Json(status, body)) => respond(&mut stream, status, &body),
                            None => respond(&mut stream, 404, r#"{"message":"Not Found"}"#),
                        }
                    });
                }
            })
        };
        MockForge {
            port,
            log,
            stop,
            offline,
            unauthorized,
            thread: Some(thread),
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub(crate) fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The API base URL of the fake GitHub Enterprise server.
    pub(crate) fn api_url(&self) -> String {
        format!("{}/api/v3", self.url())
    }

    /// The remote URL of `acme/widgets` on the fake forge.
    pub(crate) fn remote_url(&self) -> String {
        format!("{}/acme/widgets.git", self.url())
    }

    /// Every request received so far.
    pub(crate) fn requests(&self) -> Vec<Req> {
        self.log.lock().map(|log| log.clone()).unwrap_or_default()
    }

    /// How many requests matched.
    pub(crate) fn count(&self, matches: impl Fn(&Req) -> bool) -> usize {
        self.requests().iter().filter(|req| matches(req)).count()
    }

    /// While set, every request is answered 401 (the token was revoked).
    pub(crate) fn set_unauthorized(&self, unauthorized: bool) {
        self.unauthorized.store(unauthorized, Ordering::SeqCst);
    }

    /// While offline, connections are dropped without an answer.
    pub(crate) fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::SeqCst);
    }
}

impl Drop for MockForge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// A GitHub user object.
pub(crate) fn user_json(login: &str) -> serde_json::Value {
    serde_json::json!({
        "id": 1, "login": login, "name": "Octo Cat", "email": null,
        "avatar_url": "https://avatars.example/octocat.png",
    })
}

/// A check run of a fake pull request: `(name, status, conclusion)` as the GitHub API says them.
pub(crate) type FakeCheck = (String, String, Option<String>);

/// A pull request of the fake forge.
#[derive(Clone, Debug)]
pub(crate) struct FakePr {
    /// The number.
    pub number: i64,
    /// The head branch.
    pub branch: String,
    /// The title.
    pub title: String,
    /// Draft.
    pub draft: bool,
    /// Merged (listed as closed with `merged_at`).
    pub merged: bool,
    /// The head commit (hex).
    pub sha: String,
    /// The CI check runs of the branch.
    pub checks: Vec<FakeCheck>,
}

impl FakePr {
    fn json(&self) -> serde_json::Value {
        let at = if self.merged {
            // Recent: the cache drops merged reviews after 15 days.
            serde_json::json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        } else {
            serde_json::Value::Null
        };
        serde_json::json!({
            "html_url": format!("https://ghe.invalid/acme/widgets/pull/{}", self.number),
            "number": self.number,
            "title": self.title,
            "body": "body",
            "user": user_json("octocat"),
            "labels": [],
            "draft": self.draft,
            "merge_commit_sha": null,
            "head": {
                "ref": self.branch,
                "sha": self.sha,
                "repo": {
                    "ssh_url": "git@ghe.invalid:acme/widgets.git",
                    "clone_url": "https://ghe.invalid/acme/widgets.git",
                    "fork": false,
                    "owner": user_json("acme"),
                },
            },
            "base": {"ref": "main", "sha": "0000000000000000000000000000000000000002"},
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-04-01T00:00:00Z",
            "merged_at": at,
            "closed_at": at,
            "requested_reviewers": [],
        })
    }
}

fn check_run(id: i64, check: &FakeCheck) -> serde_json::Value {
    serde_json::json!({
        "id": id, "name": check.0, "status": check.1, "conclusion": check.2,
        "html_url": format!("https://ci.invalid/{id}"),
        "head_sha": "abc", "started_at": "2026-04-01T00:00:00Z",
        "completed_at": null, "url": "u", "details_url": "d",
    })
}

/// "success" checks: every check passed.
pub(crate) fn passing() -> Vec<FakeCheck> {
    vec![
        ("build".into(), "completed".into(), Some("success".into())),
        ("test".into(), "completed".into(), Some("success".into())),
    ]
}

/// A failed and a passed check.
pub(crate) fn failing() -> Vec<FakeCheck> {
    vec![
        ("build".into(), "completed".into(), Some("success".into())),
        ("lint".into(), "completed".into(), Some("failure".into())),
    ]
}

/// A check that is still running.
pub(crate) fn running() -> Vec<FakeCheck> {
    vec![("build".into(), "in_progress".into(), None)]
}

fn decoded(url: &str) -> String {
    url.replace("%2F", "/").replace("%2f", "/")
}

/// A fake GitHub Enterprise forge for `acme/widgets`: lists, creates and merges pull requests, answers check
/// runs. The pull requests can be changed while it runs.
pub(crate) struct FakeForge {
    /// The server.
    pub server: MockForge,
    prs: Arc<Mutex<Vec<FakePr>>>,
}

impl FakeForge {
    /// Starts the fake forge on a free loopback port.
    pub(crate) fn start() -> Self {
        let prs: Arc<Mutex<Vec<FakePr>>> = Arc::new(Mutex::new(Vec::new()));
        let shared = prs.clone();
        let server = MockForge::start(move |req| {
            let prs = shared.lock().ok()?.clone();
            let base = "/api/v3/repos/acme/widgets/";
            let path = decoded(&req.url);
            let rest = path.strip_prefix(base)?;
            if req.method == "POST" && rest.starts_with("pulls") {
                let body: serde_json::Value = serde_json::from_str(&req.body).ok()?;
                let head = body["head"].as_str()?;
                let branch = head.rsplit(':').next()?.to_owned();
                let mut all = shared.lock().ok()?;
                let number = all.iter().map(|pr| pr.number).max().unwrap_or(0) + 1;
                let pr = FakePr {
                    number,
                    branch,
                    title: body["title"].as_str()?.to_owned(),
                    draft: body["draft"].as_bool().unwrap_or(false),
                    merged: false,
                    sha: "abcdef0000000000000000000000000000000001".into(),
                    checks: Vec::new(),
                };
                let json = pr.json();
                all.push(pr);
                return Some(Reply::Json(201, json.to_string()));
            }
            if rest.starts_with("pulls?state=open") {
                let list: Vec<_> = prs
                    .iter()
                    .filter(|pr| !pr.merged)
                    .map(FakePr::json)
                    .collect();
                return Some(Reply::Json(200, serde_json::json!(list).to_string()));
            }
            if rest.starts_with("pulls?state=closed") {
                let list: Vec<_> = prs
                    .iter()
                    .filter(|pr| pr.merged)
                    .map(FakePr::json)
                    .collect();
                return Some(Reply::Json(200, serde_json::json!(list).to_string()));
            }
            if let Some(number) = rest.strip_prefix("pulls/")
                && let Ok(number) = number.split('?').next()?.parse::<i64>()
            {
                let pr = prs.iter().find(|pr| pr.number == number)?;
                return Some(Reply::Json(200, pr.json().to_string()));
            }
            if let Some(commit) = rest.strip_prefix("commits/")
                && let Some(reference) = commit.split("/check-runs").next()
            {
                let found = prs
                    .iter()
                    .find(|pr| pr.branch == reference || pr.sha == reference);
                // Check ids are unique per forge: the cache keys them.
                let runs: Vec<_> = found
                    .map(|pr| {
                        pr.checks
                            .iter()
                            .enumerate()
                            .map(|(i, c)| check_run(pr.number * 100 + i as i64 + 1, c))
                            .collect()
                    })
                    .unwrap_or_default();
                return Some(Reply::Json(
                    200,
                    serde_json::json!({"total_count": runs.len(), "check_runs": runs}).to_string(),
                ));
            }
            None
        });
        FakeForge { server, prs }
    }

    /// Opens a pull request for `branch`; returns its number.
    pub(crate) fn add(
        &self,
        branch: &str,
        title: &str,
        draft: bool,
        sha: &str,
        checks: Vec<FakeCheck>,
    ) -> i64 {
        let mut prs = self.prs.lock().expect("fake forge state");
        let number = prs.iter().map(|pr| pr.number).max().unwrap_or(0) + 1;
        prs.push(FakePr {
            number,
            branch: branch.to_owned(),
            title: title.to_owned(),
            draft,
            merged: false,
            sha: sha.to_owned(),
            checks,
        });
        number
    }

    /// Merges the pull request of `branch`.
    pub(crate) fn merge(&self, branch: &str) {
        if let Ok(mut prs) = self.prs.lock() {
            for pr in prs.iter_mut().filter(|pr| pr.branch == branch) {
                pr.merged = true;
            }
        }
    }

    /// The pull requests now.
    pub(crate) fn prs(&self) -> Vec<FakePr> {
        self.prs.lock().map(|prs| prs.clone()).unwrap_or_default()
    }
}

/// Services over `data` and `cache` with an in-memory keychain. `client_id` enables the device flow.
pub(crate) fn services_with(data: &Path, cache: &Path, client_id: Option<&str>) -> ServicesHandle {
    use gitcthulhu_services::{Services, ServicesConfig, UpdateChannel};
    install_memory_keyring();
    let services = Services::new(ServicesConfig {
        app_data_dir: data.to_owned(),
        cache_dir: cache.to_owned(),
        user_agent: "GitCthulhu/0.0.0-test".into(),
        app_version: semver::Version::new(0, 0, 0),
        github_oauth_client_id: client_id.map(str::to_owned),
        update_manifest_url: None,
        update_channel: UpdateChannel::Release,
    })
    .expect("services start");
    ServicesHandle {
        services: Arc::new(services),
        github_device_flow: client_id.is_some(),
        updates_enabled: false,
    }
}

/// Services over `dir` (account list, caches) with an in-memory keychain.
pub(crate) fn services_in(dir: &Path, client_id: Option<&str>) -> ServicesHandle {
    services_with(&dir.join("data"), &dir.join("cache"), client_id)
}
