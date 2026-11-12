//! The forge facade end to end: detection, URLs, account matching, error classification, and reviews, checks
//! and sign-in against a local mock of a GitHub Enterprise server (`tiny_http`). No test talks to the internet or
//! to the system keychain: tokens go to an in-memory keyring installed below.
#![cfg(feature = "forge")]

use std::any::Any;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use but_error::{Code, Context};
use but_forge_storage::settings::{GitHubAccount, GitLabAccount};
use but_secret::Sensitive;
use gitcthulhu_git::{CancelToken, ForgeRemotes, RemoteRef};
use gitcthulhu_services::Runtime;
use gitcthulhu_services::forge::{
    CheckoutPlan, CiConclusion, CiStatus, CreateReview, DevicePoll, ForgeAccountValidity,
    ForgeConfig, ForgeDirs, ForgeError, ForgeHub, ForgeName, ForgeUser, RepoAccountStatus,
    RepoForge, RepoForgeInput, RepoForgePrefs,
};
use serde_json::json;

// ---- in-memory keyring -------------------------------------------------------------------------------

static SECRETS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

struct MemoryEntry(String);

impl keyring::credential::CredentialApi for MemoryEntry {
    fn set_password(&self, password: &str) -> keyring::Result<()> {
        SECRETS
            .lock()
            .unwrap()
            .insert(self.0.clone(), password.to_owned());
        Ok(())
    }
    fn set_secret(&self, _secret: &[u8]) -> keyring::Result<()> {
        Err(keyring::Error::NoEntry)
    }
    fn get_password(&self) -> keyring::Result<String> {
        SECRETS
            .lock()
            .unwrap()
            .get(&self.0)
            .cloned()
            .ok_or(keyring::Error::NoEntry)
    }
    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        Err(keyring::Error::NoEntry)
    }
    fn delete_credential(&self) -> keyring::Result<()> {
        SECRETS
            .lock()
            .unwrap()
            .remove(&self.0)
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

fn install_keyring() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| keyring::set_default_credential_builder(Box::new(MemoryBuilder)));
}

// ---- mock server -------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    method: String,
    url: String,
    auth: Option<String>,
    body: String,
}

enum Reply {
    Json(u16, String),
    Bytes(u16, Vec<u8>),
    /// Never answer (until the mock is dropped).
    Hang,
}

type Handler = Box<dyn Fn(&Req) -> Reply + Send + Sync>;

struct Mock {
    port: u16,
    log: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mock {
    /// Serve with the handler `make(port)` builds, on a free local port.
    fn start(make: impl FnOnce(u16) -> Handler) -> Mock {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("a local port"));
        let port = server.server_addr().to_ip().expect("an IP address").port();
        let handler: Arc<Handler> = Arc::new(make(port));
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (log, stop) = (log.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(20))
                    else {
                        continue;
                    };
                    let (log, stop, handler) = (log.clone(), stop.clone(), handler.clone());
                    std::thread::spawn(move || {
                        let mut body = String::new();
                        request.as_reader().read_to_string(&mut body).ok();
                        let req = Req {
                            method: request.method().to_string(),
                            url: request.url().to_owned(),
                            auth: request
                                .headers()
                                .iter()
                                .find(|h| h.field.equiv("Authorization"))
                                .map(|h| h.value.to_string()),
                            body,
                        };
                        log.lock().unwrap().push(req.clone());
                        let (status, bytes) = match handler(&req) {
                            Reply::Json(status, body) => (status, body.into_bytes()),
                            Reply::Bytes(status, bytes) => (status, bytes),
                            Reply::Hang => {
                                while !stop.load(Ordering::SeqCst) {
                                    std::thread::sleep(Duration::from_millis(20));
                                }
                                return;
                            }
                        };
                        let content_type = tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap();
                        request
                            .respond(
                                tiny_http::Response::from_data(bytes)
                                    .with_status_code(status)
                                    .with_header(content_type),
                            )
                            .ok();
                    });
                }
            })
        };
        Mock {
            port,
            log,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }

    fn count(&self, matches: impl Fn(&Req) -> bool) -> usize {
        self.requests().iter().filter(|req| matches(req)).count()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

// ---- fixtures ----------------------------------------------------------------------------------------

fn hub_in(dir: &Path, client_id: Option<&str>) -> ForgeHub {
    install_keyring();
    ForgeHub::new(
        Runtime::new().expect("runtime"),
        ForgeDirs {
            app_data: dir.join("data"),
            cache: dir.join("cache"),
        },
        ForgeConfig {
            github_oauth_client_id: client_id.map(str::to_owned),
            user_agent: "GitCthulhu/0.0.0-test".into(),
        },
    )
    .expect("hub")
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository in `dir` with the given remotes.
fn init_repo(dir: &Path, remotes: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    for (name, url) in remotes {
        git(dir, &["remote", "add", name, url]);
    }
    dir.to_owned()
}

fn remotes(base: Option<&str>, push: Option<&str>) -> ForgeRemotes {
    let remote = |name: &str, url: &str| RemoteRef {
        name: name.into(),
        url: url.into(),
    };
    ForgeRemotes {
        base: base.map(|url| remote("origin", url)),
        push: push.map(|url| remote("fork", url)),
    }
}

fn input(repo_dir: &Path, remotes: ForgeRemotes, prefs: RepoForgePrefs) -> RepoForgeInput {
    RepoForgeInput {
        repo_dir: repo_dir.to_owned(),
        remotes,
        prefs,
    }
}

/// A hub and a repository handle of `base_url`, with no accounts and no repository on disk.
fn bare_repo(tmp: &Path, base_url: &str) -> (ForgeHub, RepoForge) {
    let hub = hub_in(tmp, None);
    let repo = hub.open_repo(input(
        tmp,
        remotes(Some(base_url), None),
        RepoForgePrefs::default(),
    ));
    (hub, repo)
}

fn user_json(login: &str) -> serde_json::Value {
    json!({
        "id": 1, "login": login, "name": "Octo Cat", "email": null,
        "avatar_url": "https://avatars.example/octocat.png",
    })
}

/// A pull request as the GitHub API lists it. `head_clone_url` is the clone URL of the head repository.
fn pull(
    number: i64,
    branch: &str,
    head_owner: &str,
    head_clone_url: &str,
    fork: bool,
    updated_at: &str,
) -> serde_json::Value {
    json!({
        "html_url": format!("https://ghe.invalid/acme/widgets/pull/{number}"),
        "number": number,
        "title": format!("PR {number}"),
        "body": "body",
        "user": user_json("octocat"),
        "labels": [],
        "draft": false,
        "merge_commit_sha": null,
        "head": {
            "ref": branch,
            "sha": "ABCDEF0000000000000000000000000000000001",
            "repo": {
                "ssh_url": "git@ghe.invalid:x/y.git",
                "clone_url": head_clone_url,
                "fork": fork,
                "owner": user_json(head_owner),
            },
        },
        "base": {"ref": "main", "sha": "0000000000000000000000000000000000000002"},
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": updated_at,
        "merged_at": null,
        "closed_at": null,
        "requested_reviewers": [],
    })
}

fn merged(mut pull: serde_json::Value, at: &str) -> serde_json::Value {
    pull["merged_at"] = json!(at);
    pull["closed_at"] = json!(at);
    pull
}

fn reply(body: &serde_json::Value) -> Reply {
    Reply::Json(200, body.to_string())
}

fn is(req: &Req, path_and_query_start: &str) -> bool {
    req.url.starts_with(path_and_query_start)
}

/// A GitHub Enterprise mock with the account endpoint and whatever `more` answers.
fn enterprise_mock(more: impl Fn(&Req, u16) -> Option<Reply> + Send + Sync + 'static) -> Mock {
    Mock::start(|port| {
        Box::new(move |req| {
            if req.url == "/api/v3/user" {
                return reply(&user_json("octocat"));
            }
            more(req, port).unwrap_or_else(|| Reply::Json(404, r#"{"message":"Not Found"}"#.into()))
        })
    })
}

/// The base remote URL of `acme/widgets` on the mock.
fn base_url(mock: &Mock) -> String {
    format!("{}/acme/widgets.git", mock.url())
}

struct World {
    _tmp: tempfile::TempDir,
    repo: RepoForge,
    repo_dir: PathBuf,
}

/// The mock's server signed in as `octocat`, and a repository tab for `acme/widgets` on it.
fn world(mock: &Mock, extra_remotes: &[(&str, &str)]) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), None);
    hub.add_github_enterprise(
        &format!("{}/api/v3", mock.url()),
        Sensitive("secret-token".into()),
    )
    .expect("sign in to the mock");
    let base = base_url(mock);
    let mut all = vec![("origin", base.as_str())];
    all.extend_from_slice(extra_remotes);
    let repo_dir = init_repo(&tmp.path().join("work"), &all);
    let repo = hub.open_repo(input(
        &repo_dir,
        remotes(Some(&base), None),
        RepoForgePrefs::default(),
    ));
    World {
        _tmp: tmp,
        repo,
        repo_dir,
    }
}

fn numbers(reviews: &[gitcthulhu_services::forge::ForgeReview]) -> Vec<i64> {
    let mut numbers: Vec<i64> = reviews.iter().map(|review| review.number).collect();
    numbers.sort_unstable();
    numbers
}

// ---- detection and URLs ------------------------------------------------------------------------------

#[test]
fn detect_from_ssh_https_scp_urls() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), None);
    for (url, forge, base) in [
        (
            "git@github.com:acme/widgets.git",
            ForgeName::GitHub,
            "https://github.com/acme/widgets",
        ),
        (
            "ssh://git@github.com/acme/widgets",
            ForgeName::GitHub,
            "https://github.com/acme/widgets",
        ),
        (
            "ssh://git@ssh.github.com:443/acme/widgets.git",
            ForgeName::GitHub,
            "https://github.com/acme/widgets",
        ),
        (
            "https://github.com/acme/widgets.git",
            ForgeName::GitHub,
            "https://github.com/acme/widgets",
        ),
        (
            "git@gitlab.com:group/sub/widgets.git",
            ForgeName::GitLab,
            "https://gitlab.com/group/sub/widgets",
        ),
        (
            "https://bitbucket.org/acme/widgets",
            ForgeName::Bitbucket,
            "https://bitbucket.org/acme/widgets",
        ),
    ] {
        let info = hub
            .detect(url, None)
            .unwrap_or_else(|| panic!("{url} is on a forge"));
        assert_eq!(info.name, forge, "forge of {url}");
        assert_eq!(info.base_url, base, "web URL of {url}");
    }
    for url in [
        "file:///srv/repo",
        "https://example.com/acme/widgets",
        "../repo",
    ] {
        assert!(
            hub.detect(url, None).is_none(),
            "{url} is on no known forge, and no override or account says otherwise"
        );
    }

    let gitlab = hub
        .detect(
            "https://git.example.com/group/widgets",
            Some(ForgeName::GitLab),
        )
        .expect("the override names the forge");
    assert_eq!(
        gitlab.name,
        ForgeName::GitLab,
        "the override decides for unknown hosts"
    );
    assert_eq!(gitlab.unit.symbol, "!", "GitLab calls them merge requests");
    assert!(gitlab.capabilities.checks, "GitLab has CI checks");
    let github = hub.detect("git@github.com:a/b.git", None).unwrap();
    assert_eq!(github.unit.symbol, "#", "GitHub calls them pull requests");
    assert!(
        github.capabilities.review_comments,
        "GitHub has a conversation"
    );
}

#[test]
fn detect_knows_self_hosted_hosts_of_signed_in_accounts() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), None);
    but_forge_storage::Controller::from_path(tmp.path().join("data"))
        .add_gitlab_account(&GitLabAccount::SelfHosted {
            username: "tanuki".into(),
            host: "https://code.example.com:8443/api/v4".into(),
            access_token_key: "unused".into(),
        })
        .unwrap();
    let info = hub
        .detect("git@code.example.com:group/widgets.git", None)
        .expect("the account's host is a GitLab");
    assert_eq!(
        info.name,
        ForgeName::GitLab,
        "an account names the forge of its host"
    );
    assert_eq!(
        info.base_url, "https://code.example.com:8443/group/widgets",
        "the account's origin supplies the web scheme and port that an SSH remote lacks"
    );
}

#[test]
fn repo_forge_urls_commit_pr_compare_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let (_hub, github) = bare_repo(tmp.path(), "git@github.com:acme/widgets.git");
    assert_eq!(github.info().unwrap().name, ForgeName::GitHub);
    assert_eq!(
        github.url_commit("abc123").as_deref(),
        Some("https://github.com/acme/widgets/commit/abc123"),
        "commit URL"
    );
    assert_eq!(
        github.url_review(42).as_deref(),
        Some("https://github.com/acme/widgets/pull/42"),
        "pull request URL"
    );
    assert_eq!(
        github.url_new_review("main", "feat/x").as_deref(),
        Some("https://github.com/acme/widgets/compare/main...feat/x"),
        "compare URL"
    );
    assert_eq!(
        github.url_branch("feat/x y").as_deref(),
        Some("https://github.com/acme/widgets/tree/feat/x%20y"),
        "branch URL, path segments encoded"
    );

    let (_hub, gitlab) = bare_repo(tmp.path(), "https://gitlab.com/group/sub/widgets.git");
    assert_eq!(
        gitlab.url_commit("abc").as_deref(),
        Some("https://gitlab.com/group/sub/widgets/-/commit/abc")
    );
    assert_eq!(
        gitlab.url_review(3).as_deref(),
        Some("https://gitlab.com/group/sub/widgets/-/merge_requests/3")
    );
    assert_eq!(
        gitlab.url_new_review("main", "topic").as_deref(),
        Some("https://gitlab.com/group/sub/widgets/-/compare/main...topic")
    );
    assert_eq!(
        gitlab.url_branch("topic").as_deref(),
        Some("https://gitlab.com/group/sub/widgets/-/tree/topic")
    );

    let (_hub, bitbucket) = bare_repo(tmp.path(), "https://bitbucket.org/acme/widgets");
    assert_eq!(
        bitbucket.url_review(5).as_deref(),
        Some("https://bitbucket.org/acme/widgets/pull-requests/5")
    );
    assert_eq!(
        bitbucket.url_new_review("main", "topic").as_deref(),
        Some("https://bitbucket.org/acme/widgets/branch/topic?dest=main")
    );

    // From a fork the head of the compare URL is the fork's owner and branch.
    let hub = hub_in(tmp.path(), None);
    let fork = hub.open_repo(input(
        tmp.path(),
        remotes(
            Some("git@github.com:acme/widgets.git"),
            Some("git@github.com:bob/widgets.git"),
        ),
        RepoForgePrefs::default(),
    ));
    assert_eq!(
        fork.url_new_review("main", "topic").as_deref(),
        Some("https://github.com/acme/widgets/compare/main...bob:topic"),
        "the compare page of the upstream repository, from the fork's branch"
    );
    assert_eq!(
        fork.url_branch("topic").as_deref(),
        Some("https://github.com/bob/widgets/tree/topic"),
        "the branch lives in the repository it is pushed to"
    );

    let (_hub, none) = bare_repo(tmp.path(), "https://example.com/acme/widgets");
    assert!(none.info().is_none(), "no forge detected");
    assert!(
        none.url_commit("abc").is_none()
            && none.url_review(1).is_none()
            && none.url_new_review("main", "x").is_none()
            && none.url_branch("x").is_none(),
        "no forge, no web URLs"
    );
    let caps = none.capabilities();
    assert!(
        !(caps.checks || caps.repo_info || caps.pr_service || caps.list_service),
        "no forge, no capabilities"
    );
    assert!(
        matches!(none.cached_reviews(), Err(ForgeError::Unrecognized)),
        "reviews of a repository on no forge are Unrecognized, which stops polling"
    );
    assert_eq!(none.account_status(), RepoAccountStatus::NotAForge);
}

#[test]
fn default_review_target_comes_from_the_remote_or_is_main() {
    let tmp = tempfile::tempdir().unwrap();
    let (_hub, repo) = bare_repo(tmp.path(), "git@github.com:acme/widgets.git");
    assert_eq!(repo.default_review_target(Some("develop")), "develop");
    assert_eq!(repo.default_review_target(Some("  ")), "main");
    assert_eq!(repo.default_review_target(None), "main");
}

// ---- accounts ----------------------------------------------------------------------------------------

/// Accounts in the storage of `tmp`: no secrets, the account list only.
fn store_accounts(tmp: &Path) {
    let storage = but_forge_storage::Controller::from_path(tmp.join("data"));
    for account in [
        GitHubAccount::Pat {
            username: "alice".into(),
            access_token_key: "unused-1".into(),
        },
        GitHubAccount::Enterprise {
            username: "bob".into(),
            host: "https://ghe.example.com/api/v3".into(),
            access_token_key: "unused-2".into(),
        },
        GitHubAccount::Enterprise {
            username: "carol".into(),
            host: "https://ghe.example.com/api/v3".into(),
            access_token_key: "unused-3".into(),
        },
    ] {
        storage.add_github_account(&account).unwrap();
    }
    storage
        .add_gitlab_account(&GitLabAccount::SelfHosted {
            username: "tanuki".into(),
            host: "https://gl.example.com:8443/api/v4".into(),
            access_token_key: "unused-4".into(),
        })
        .unwrap();
}

fn gh(account: &GitHubAccount) -> ForgeUser {
    ForgeUser::GitHub(account.into())
}

#[test]
fn account_matching_by_host_and_preferred_user() {
    let tmp = tempfile::tempdir().unwrap();
    store_accounts(tmp.path());
    let hub = hub_in(tmp.path(), None);
    let open = |url: &str, preferred: Option<ForgeUser>| {
        hub.open_repo(input(
            tmp.path(),
            remotes(Some(url), None),
            RepoForgePrefs {
                preferred_user: preferred,
                ..Default::default()
            },
        ))
    };
    let label = |status: RepoAccountStatus| match status {
        RepoAccountStatus::Matching(account) => account.label,
        other => format!("{other:?}"),
    };
    let bob = gh(&GitHubAccount::Enterprise {
        username: "bob".into(),
        host: "https://ghe.example.com/api/v3".into(),
        access_token_key: String::new(),
    });
    let carol = gh(&GitHubAccount::Enterprise {
        username: "carol".into(),
        host: "https://ghe.example.com/api/v3".into(),
        access_token_key: String::new(),
    });
    let alice = gh(&GitHubAccount::Pat {
        username: "alice".into(),
        access_token_key: String::new(),
    });

    assert_eq!(
        label(open("https://github.com/acme/widgets", None).account_status()),
        "alice (token)",
        "the public account serves github.com, not the Enterprise ones"
    );
    assert_eq!(
        label(open("git@ghe.example.com:acme/widgets.git", None).account_status()),
        "bob on ghe.example.com",
        "without a preference the first account of the host is used"
    );
    assert_eq!(
        label(open("https://ghe.example.com/acme/widgets", Some(carol)).account_status()),
        "carol on ghe.example.com",
        "the preferred user of the host wins"
    );
    assert_eq!(
        open("https://ghe.example.com/acme/widgets", Some(alice)).account_status(),
        RepoAccountStatus::NoMatchingAccount,
        "a preferred user of another instance is never used instead"
    );
    assert_eq!(
        open("https://ghe.example.com/acme/widgets", Some(bob)).account_status(),
        RepoAccountStatus::Matching(
            hub.accounts()
                .unwrap()
                .into_iter()
                .find(|account| account.label.starts_with("bob"))
                .unwrap()
        ),
        "the status carries the account"
    );
    assert_eq!(
        label(open("https://gl.example.com:8443/group/widgets", None).account_status()),
        "tanuki on gl.example.com:8443",
        "a self-hosted GitLab is recognised by the account's host, and matched by its port too"
    );
    assert_eq!(
        open("https://gl.example.com/group/widgets", None).account_status(),
        RepoAccountStatus::NotAForge,
        "the same host on the default port is another server: no forge is known there"
    );
    assert_eq!(
        open("https://gitlab.com/group/widgets", None).account_status(),
        RepoAccountStatus::NoMatchingAccount,
        "gitlab.com has no account"
    );
    assert_eq!(
        open("https://example.com/group/widgets", None).account_status(),
        RepoAccountStatus::NotAForge,
        "an unknown host is no forge"
    );
}

#[test]
fn account_for_host() {
    let tmp = tempfile::tempdir().unwrap();
    store_accounts(tmp.path());
    let hub = hub_in(tmp.path(), None);
    let label = |host: &str| hub.account_for_host(host).map(|account| account.label);

    assert_eq!(label("github.com").as_deref(), Some("alice (token)"));
    assert_eq!(
        label("ghe.example.com").as_deref(),
        Some("bob on ghe.example.com"),
        "an Enterprise account answers for the host of its API URL"
    );
    assert_eq!(
        label("gl.example.com:8443").as_deref(),
        Some("tanuki on gl.example.com:8443"),
        "a host with the account's port"
    );
    assert_eq!(
        label("gl.example.com"),
        None,
        "the default HTTPS port is not the account's 8443"
    );
    for host in [
        "gitlab.com",
        "bitbucket.org",
        "example.org",
        "",
        "github.com.evil.example",
    ] {
        assert_eq!(label(host), None, "no account answers for '{host}'");
    }
    assert_eq!(
        hub.account_for_host("github.com").unwrap().forge,
        ForgeName::GitHub
    );
}

#[test]
fn device_flow_needs_a_client_id() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), None);
    assert!(
        matches!(hub.github_device_start(), Err(ForgeError::Other(_))),
        "without an OAuth app only tokens can sign in"
    );
    let code = gitcthulhu_services::forge::DeviceCode {
        user_code: "ABCD-1234".into(),
        device_code: "dc".into(),
        verification_uri: "https://github.com/login/device".into(),
        interval: 5,
        expires_in: 900,
    };
    assert!(
        matches!(
            hub.github_device_poll(&code, &CancelToken::new()),
            Err(ForgeError::Other(_))
        ),
        "polling needs it as well"
    );
}

#[test]
fn a_device_flow_poll_before_the_interval_asks_nothing() {
    // A client id is configured, but the code was never started by this hub and its interval has not run
    // out: nothing may reach the network, and the answer is "pending" as GitHub's would be.
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), Some("client-id"));
    let code = gitcthulhu_services::forge::DeviceCode {
        user_code: "ABCD-1234".into(),
        device_code: "dc".into(),
        verification_uri: "https://github.com/login/device".into(),
        interval: 3600,
        expires_in: 900,
    };
    let cancelled = CancelToken::new();
    cancelled.cancel();
    // A code unknown to the hub is polled at once ...
    assert!(
        matches!(
            hub.github_device_poll(&code, &cancelled),
            Err(ForgeError::Cancelled)
        ),
        "the first poll goes out (here: cancelled before any request)"
    );
    // ... and the next one has to wait for the interval.
    assert_eq!(
        hub.github_device_poll(&code, &CancelToken::new()).unwrap(),
        DevicePoll::Pending,
        "a poll inside the interval is answered without a request"
    );
}

#[test]
fn signing_in_forgetting_and_checking_an_account() {
    let valid = Arc::new(AtomicBool::new(true));
    let mock = {
        let valid = valid.clone();
        Mock::start(move |_| {
            Box::new(move |req| {
                if req.url == "/api/v3/user" {
                    return if valid.load(Ordering::SeqCst) {
                        reply(&user_json("octocat"))
                    } else {
                        Reply::Json(401, r#"{"message":"Bad credentials"}"#.into())
                    };
                }
                Reply::Json(404, "{}".into())
            })
        })
    };
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), None);
    let api = format!("{}/api/v3/", mock.url());

    let bad_url = hub.add_github_enterprise("ghe.example.com", Sensitive("t".into()));
    assert!(
        matches!(bad_url, Err(ForgeError::Other(_))),
        "an API URL needs a scheme"
    );
    assert!(
        hub.accounts().unwrap().is_empty(),
        "a failed sign-in stores nothing"
    );

    let account = hub
        .add_github_enterprise(&api, Sensitive("  secret-token\n".into()))
        .expect("sign in");
    assert_eq!(account.forge, ForgeName::GitHub);
    assert_eq!(
        account.label,
        format!("octocat on 127.0.0.1:{}", mock.port),
        "Enterprise accounts are labelled with their host"
    );
    assert_eq!(
        account.avatar_url.as_deref(),
        Some("https://avatars.example/octocat.png"),
        "the avatar comes from the profile cached at sign-in"
    );
    let user_requests = mock.requests();
    assert_eq!(
        user_requests[0].auth.as_deref(),
        Some("Bearer secret-token"),
        "the token is sent as a bearer token, without the pasted whitespace"
    );
    assert_eq!(hub.accounts().unwrap(), vec![account.clone()]);

    // The secret is in the keychain under a key with the user name in it, and nowhere in the settings file.
    let settings =
        std::fs::read_to_string(tmp.path().join("data").join("forge_settings.json")).unwrap();
    assert!(
        !settings.contains("secret-token"),
        "tokens never reach the settings file"
    );
    let key = but_forge_storage::Controller::from_path(tmp.path().join("data"))
        .github_accounts()
        .unwrap()
        .into_iter()
        .map(|account| match account {
            GitHubAccount::Enterprise {
                access_token_key, ..
            } => access_token_key,
            other => panic!("unexpected account {other:?}"),
        })
        .next()
        .unwrap();
    assert!(
        key.starts_with("github_enterprise_octocat_"),
        "the key tells users of one host apart: {key}"
    );
    let stored = but_secret::secret::retrieve(&key, but_secret::secret::Namespace::BuildKind)
        .unwrap()
        .expect("the token is stored");
    assert_eq!(stored.0, "secret-token", "the keychain holds the token");

    assert_eq!(
        hub.check(&account.user).unwrap(),
        ForgeAccountValidity::Valid,
        "the forge accepts the token"
    );
    valid.store(false, Ordering::SeqCst);
    assert_eq!(
        hub.check(&account.user).unwrap(),
        ForgeAccountValidity::Invalid,
        "a revoked token is invalid"
    );

    hub.forget(&account.user).expect("forget");
    assert!(hub.accounts().unwrap().is_empty(), "the account is gone");
    assert!(
        but_secret::secret::retrieve(&key, but_secret::secret::Namespace::BuildKind)
            .unwrap()
            .is_none(),
        "the token is gone from the keychain"
    );
    assert_eq!(
        hub.check(&account.user).unwrap(),
        ForgeAccountValidity::NoCredentials,
        "a forgotten account has no credentials"
    );
}

#[test]
fn a_rejected_token_is_an_auth_error_and_is_not_stored() {
    let mock =
        Mock::start(|_| Box::new(|_| Reply::Json(401, r#"{"message":"Bad credentials"}"#.into())));
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), None);
    let err = hub
        .add_github_enterprise(&format!("{}/api/v3", mock.url()), Sensitive("wrong".into()))
        .unwrap_err();
    assert!(
        matches!(err, ForgeError::Auth(_)) && err.is_terminal() && !err.may_pass(),
        "GitHub's 401 is a terminal authentication error: {err:?}"
    );
    assert!(hub.accounts().unwrap().is_empty(), "nothing was stored");
}

// ---- errors ------------------------------------------------------------------------------------------

fn coded(code: Code, message: &'static str) -> anyhow::Error {
    anyhow::anyhow!("underlying failure").context(Context::new_static(code, message))
}

#[test]
fn forge_error_classification_from_codes() {
    use reqwest::StatusCode as S;

    let cases: Vec<(anyhow::Error, &str)> = vec![
        (coded(Code::NetworkError, "Unable to connect"), "network"),
        (coded(Code::GitHubRateLimited, "slow down"), "rate"),
        (coded(Code::GitHubTokenExpired, "expired"), "auth"),
        (coded(Code::GitLabUnauthorized, "no"), "auth"),
        (coded(Code::GitHubDeviceCodeExpired, "late"), "auth"),
        (coded(Code::GitHubDeviceAccessDenied, "denied"), "auth"),
        (coded(Code::GitHubDeviceFlowRejected, "rejected"), "auth"),
        (coded(Code::GitHubOrgSamlRestricted, "saml"), "permission"),
        (coded(Code::GitHubOrgOAuthRestricted, "oauth"), "permission"),
        (
            coded(Code::GitHubInsufficientPermissions, "scope"),
            "permission",
        ),
        (
            coded(Code::GitHubTokenLifetimeRestricted, "lifetime"),
            "permission",
        ),
        (coded(Code::GitLabForbidden, "forbidden"), "permission"),
        (coded(Code::GitLabProjectNotFound, "gone"), "not-found"),
        (
            coded(Code::ForgeNotAuthenticated, "none"),
            "not-authenticated",
        ),
        (coded(Code::ForgeUnrecognized, "none"), "unrecognized"),
        (coded(Code::PreconditionFailed, "other"), "other"),
        (anyhow::anyhow!("something else"), "other"),
        (
            but_github::HttpStatusError {
                status: S::UNAUTHORIZED,
            }
            .into(),
            "auth",
        ),
        (
            but_github::HttpStatusError {
                status: S::FORBIDDEN,
            }
            .into(),
            "permission",
        ),
        (
            anyhow::Error::from(but_github::HttpStatusError {
                status: S::FORBIDDEN,
            })
            .context("403 Forbidden: API rate limit exceeded for user"),
            "rate",
        ),
        (
            but_github::HttpStatusError {
                status: S::NOT_FOUND,
            }
            .into(),
            "not-found",
        ),
        (
            but_github::HttpStatusError {
                status: S::TOO_MANY_REQUESTS,
            }
            .into(),
            "rate",
        ),
        (
            but_github::HttpStatusError {
                status: S::BAD_GATEWAY,
            }
            .into(),
            "network",
        ),
        (
            but_github::HttpStatusError {
                status: S::UNPROCESSABLE_ENTITY,
            }
            .into(),
            "other",
        ),
        (
            but_gitlab::HttpStatusError {
                status: S::UNAUTHORIZED,
            }
            .into(),
            "auth",
        ),
        (
            but_gitlab::HttpStatusError {
                status: S::SERVICE_UNAVAILABLE,
            }
            .into(),
            "network",
        ),
        (
            but_bitbucket::HttpStatusError {
                status: S::FORBIDDEN,
            }
            .into(),
            "permission",
        ),
        (
            but_bitbucket::HttpStatusError {
                status: S::INTERNAL_SERVER_ERROR,
            }
            .into(),
            "network",
        ),
    ];
    for (err, expected) in cases {
        let shown = format!("{err:#}");
        let classified = ForgeError::from(err);
        let actual = match &classified {
            ForgeError::Network(_) => "network",
            ForgeError::RateLimited { .. } => "rate",
            ForgeError::Auth(_) => "auth",
            ForgeError::Permission(_) => "permission",
            ForgeError::NotFound => "not-found",
            ForgeError::NotAuthenticated => "not-authenticated",
            ForgeError::Unrecognized => "unrecognized",
            ForgeError::Other(_) => "other",
            ForgeError::Cancelled => "cancelled",
            _ => "unknown variant",
        };
        assert_eq!(actual, expected, "classification of '{shown}'");
    }

    // The message of the forge crates reaches the user.
    match ForgeError::from(coded(
        Code::GitHubOrgSamlRestricted,
        "Authorize the app on the SSO page",
    )) {
        ForgeError::Permission(message) => assert_eq!(message, "Authorize the app on the SSO page"),
        other => panic!("{other:?}"),
    }

    // A connection that cannot be made is a network failure.
    let rt = Runtime::new().unwrap();
    let refused = rt
        .block_on(reqwest::Client::new().get("http://127.0.0.1:1/").send())
        .unwrap_err();
    let refused_again = rt
        .block_on(reqwest::Client::new().get("http://127.0.0.1:1/").send())
        .unwrap_err();
    assert!(
        matches!(
            ForgeError::from(anyhow::Error::from(refused)),
            ForgeError::Network(_)
        ),
        "a refused connection is a network error"
    );

    // A crate may wrap the failure in an error type of its own: the cause still decides.
    #[derive(Debug)]
    struct Wrapped(Box<dyn std::error::Error + Send + Sync>);
    impl std::fmt::Display for Wrapped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("the forge call failed")
        }
    }
    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&*self.0)
        }
    }
    assert!(
        matches!(
            ForgeError::from(anyhow::Error::new(Wrapped(Box::new(refused_again)))),
            ForgeError::Network(_)
        ),
        "a wrapped refused connection is a network error"
    );
    assert!(
        matches!(
            ForgeError::from(
                anyhow::Error::new(Wrapped(Box::new(but_gitlab::HttpStatusError {
                    status: S::UNAUTHORIZED,
                })))
                .context("Failed to list merge requests")
            ),
            ForgeError::Auth(_)
        ),
        "a wrapped 401 is an authentication error"
    );

    assert!(matches!(
        ForgeError::from(gitcthulhu_services::Cancelled),
        ForgeError::Cancelled
    ));

    // What the GUI does with them: asking again is worth it only after a network failure; polling stops for
    // account problems; a rate limit is waited out.
    let may_pass = |e: ForgeError| (e.may_pass(), e.is_terminal());
    assert_eq!(may_pass(ForgeError::Network("down".into())), (true, false));
    assert_eq!(
        may_pass(ForgeError::RateLimited { reset_at: Some(1) }),
        (false, false)
    );
    assert_eq!(may_pass(ForgeError::NotAuthenticated), (false, true));
    assert_eq!(may_pass(ForgeError::Unrecognized), (false, true));
    assert_eq!(may_pass(ForgeError::Auth("x".into())), (false, true));
    assert_eq!(may_pass(ForgeError::Permission("x".into())), (false, true));
    assert_eq!(may_pass(ForgeError::NotFound), (false, false));
    assert_eq!(may_pass(ForgeError::Cancelled), (false, false));
    assert_eq!(
        may_pass(ForgeError::Other(anyhow::anyhow!("x"))),
        (false, false)
    );
}

// ---- reviews against the mock ------------------------------------------------------------------------

#[test]
fn refresh_reviews_against_local_mock() {
    let open = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let closed = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let mock = {
        let (open, closed) = (open.clone(), closed.clone());
        enterprise_mock(move |req, _| {
            if is(req, "/api/v3/repos/acme/widgets/pulls?state=open") {
                return Some(reply(&json!(*open.lock().unwrap())));
            }
            if is(req, "/api/v3/repos/acme/widgets/pulls?state=closed") {
                return Some(reply(&json!(*closed.lock().unwrap())));
            }
            None
        })
    };
    let base = base_url(&mock);
    let fork_clone = "https://github.com/octocat/widgets.git";
    *open.lock().unwrap() = vec![
        pull(
            7,
            "fix-it",
            "octocat",
            fork_clone,
            true,
            "2026-04-01T00:00:00Z",
        ),
        pull(8, "topic", "acme", &base, false, "2026-04-02T00:00:00Z"),
    ];
    let world = world(&mock, &[("octocat", fork_clone)]);
    let repo = &world.repo;
    assert_eq!(
        repo.info().map(|info| info.name.clone()),
        Some(ForgeName::GitHub),
        "the account's host identifies a GitHub Enterprise repository"
    );
    assert!(
        matches!(repo.account_status(), RepoAccountStatus::Matching(_)),
        "the signed-in account serves the repository"
    );
    assert!(
        repo.cached_reviews().unwrap().is_empty(),
        "nothing is cached before the first refresh"
    );

    let first = repo.refresh_reviews(&CancelToken::new()).expect("refresh");
    assert_ne!(first.fingerprint, 0, "two reviews make a fingerprint");
    assert_eq!(first.last_merged, None, "nothing merged yet");
    let cached = repo.cached_reviews().unwrap();
    assert_eq!(numbers(&cached), vec![7, 8], "both open reviews are cached");
    let seventh = cached.iter().find(|review| review.number == 7).unwrap();
    assert_eq!(
        (
            seventh.source_branch.as_str(),
            seventh.head_repo_is_fork,
            seventh.title.as_str()
        ),
        ("fix-it", true, "PR 7"),
        "the review's fields come from the API"
    );
    for request in mock
        .requests()
        .iter()
        .filter(|req| req.url.contains("/pulls"))
    {
        assert_eq!(
            request.auth.as_deref(),
            Some("Bearer secret-token"),
            "every API request carries the stored token"
        );
    }

    let again = repo.refresh_reviews(&CancelToken::new()).unwrap();
    assert_eq!(
        again.fingerprint, first.fingerprint,
        "an unchanged listing keeps the fingerprint: nothing to reload"
    );
    open.lock().unwrap()[1]["title"] = json!("PR 8, renamed");
    let renamed = repo.refresh_reviews(&CancelToken::new()).unwrap();
    assert_ne!(
        renamed.fingerprint, first.fingerprint,
        "a changed title changes the fingerprint"
    );

    // The fork's review is tied to the remote that fetches from the fork, not to the base remote.
    let git_repo = gix::open(&world.repo_dir).unwrap();
    assert_eq!(
        repo.review_for_branch(&git_repo, "octocat/fix-it"),
        Some((7, true, None)),
        "review 7 belongs to the fork's remote"
    );
    assert_eq!(
        repo.review_for_branch(&git_repo, "origin/fix-it"),
        None,
        "a same-named branch of the base remote is not the fork's review"
    );
    assert_eq!(
        repo.review_for_branch(&git_repo, "origin/topic"),
        Some((8, true, None)),
        "review 8 is a branch of the base repository"
    );
    let all = repo.review_associations(&git_repo).unwrap();
    assert_eq!(
        all.len(),
        3,
        "origin/topic, octocat/topic, octocat/fix-it: {all:?}"
    );

    // Review 7 leaves the open listing because it merged: its fate is looked up and recorded.
    let remaining = open.lock().unwrap()[1].clone();
    *open.lock().unwrap() = vec![remaining];
    // Merged reviews are kept 15 days: the merge has to be recent.
    let merged_at = (chrono::Utc::now() - chrono::Duration::hours(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    *closed.lock().unwrap() = vec![merged(
        pull(
            7,
            "fix-it",
            "octocat",
            fork_clone,
            true,
            "2026-04-03T00:00:00Z",
        ),
        &merged_at,
    )];
    let after_merge = repo.refresh_reviews(&CancelToken::new()).unwrap();
    assert_eq!(
        after_merge.last_merged,
        chrono::DateTime::parse_from_rfc3339(&merged_at)
            .ok()
            .map(|at| at.timestamp()),
        "the merge time of the most recently merged cached review"
    );
    assert_eq!(
        numbers(&repo.cached_reviews().unwrap()),
        vec![8],
        "a merged review is not an open one"
    );
    assert_eq!(
        repo.review_for_branch(&git_repo, "octocat/fix-it"),
        Some((
            7,
            false,
            Some("abcdef0000000000000000000000000000000001".into())
        )),
        "the merged review stays associated with its branch, with the commit it merged at"
    );

    assert!(
        matches!(repo.review(99), Err(ForgeError::NotFound)),
        "an unknown review is NotFound"
    );
}

#[test]
fn refresh_reviews_cancel() {
    let hang = Arc::new(AtomicBool::new(true));
    let mock = {
        let hang = hang.clone();
        enterprise_mock(move |req, _| {
            if is(req, "/api/v3/repos/acme/widgets/pulls?state=open") {
                return Some(if hang.load(Ordering::SeqCst) {
                    Reply::Hang
                } else {
                    reply(&json!([]))
                });
            }
            None
        })
    };
    let world = world(&mock, &[]);
    let World { repo, _tmp, .. } = world;

    // Cancelled before it starts: no request at all.
    let token = CancelToken::new();
    token.cancel();
    assert!(
        matches!(repo.refresh_reviews(&token), Err(ForgeError::Cancelled)),
        "a cancelled token ends the refresh before it asks"
    );
    assert_eq!(
        mock.count(|req| is(req, "/api/v3/repos/acme/widgets/pulls")),
        0,
        "nothing was requested"
    );

    // Cancelled while the forge does not answer.
    let token = CancelToken::new();
    let worker = {
        let (repo, token) = (repo.clone(), token.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            (repo.refresh_reviews(&token), started.elapsed())
        })
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while mock.count(|req| is(req, "/api/v3/repos/acme/widgets/pulls")) == 0 {
        assert!(
            Instant::now() < deadline,
            "the refresh never asked the mock"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let cancelled_at = Instant::now();
    token.cancel();
    let (result, _) = worker.join().expect("the worker finishes");
    assert!(
        matches!(result, Err(ForgeError::Cancelled)),
        "cancelling a hanging refresh answers Cancelled, got {result:?}"
    );
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(5),
        "the refresh returns promptly after the cancel, not after the HTTP timeout"
    );
    assert!(
        repo.cached_reviews().unwrap().is_empty(),
        "a cancelled refresh leaves the cache alone"
    );

    // The refresh turn is free again: the next refresh is not stuck behind the cancelled one.
    hang.store(false, Ordering::SeqCst);
    let outcome = repo
        .refresh_reviews(&CancelToken::new())
        .expect("the next refresh runs");
    assert_eq!(
        outcome.fingerprint, 0,
        "an empty listing has the fingerprint of nothing"
    );
}

#[test]
fn refreshes_run_one_at_a_time_and_the_wait_is_cancellable() {
    let mock = enterprise_mock(move |req, _| {
        if is(req, "/api/v3/repos/acme/widgets/pulls?state=open") {
            return Some(Reply::Hang);
        }
        if is(req, "/api/v3/repos/acme/widgets/commits/topic/check-runs") {
            return Some(reply(&json!({"total_count": 0, "check_runs": []})));
        }
        None
    });
    let world = world(&mock, &[]);
    let World { repo, _tmp, .. } = world;
    let asked = |path: &'static str| mock.count(move |req| is(req, path));

    // A review refresh that the forge never answers holds the hub's refresh turn ...
    let reviews_token = CancelToken::new();
    let reviews = {
        let (repo, token) = (repo.clone(), reviews_token.clone());
        std::thread::spawn(move || repo.refresh_reviews(&token))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while asked("/api/v3/repos/acme/widgets/pulls") == 0 {
        assert!(
            Instant::now() < deadline,
            "the review refresh never asked the mock"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // ... so a check refresh of the same hub waits for it without asking the forge.
    let checks_token = CancelToken::new();
    let checks = {
        let (repo, token) = (repo.clone(), checks_token.clone());
        std::thread::spawn(move || repo.refresh_checks("topic", None, &token))
    };
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        asked("/api/v3/repos/acme/widgets/commits/"),
        0,
        "the second refresh must not run while the first holds the turn"
    );
    assert!(!checks.is_finished(), "the second refresh is still waiting");

    // Waiting for the turn is cancellable.
    let cancelled_at = Instant::now();
    checks_token.cancel();
    let waited = checks.join().expect("the check refresh finishes");
    assert!(
        matches!(waited, Err(ForgeError::Cancelled)),
        "a refresh cancelled while waiting for its turn answers Cancelled, got {waited:?}"
    );
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(5),
        "the wait ends promptly after the cancel"
    );
    assert_eq!(
        asked("/api/v3/repos/acme/widgets/commits/"),
        0,
        "the cancelled refresh never asked the forge"
    );

    reviews_token.cancel();
    assert!(
        matches!(
            reviews.join().expect("the review refresh finishes"),
            Err(ForgeError::Cancelled)
        ),
        "the hanging refresh is cancelled too"
    );

    // With the turn free again, the next refresh runs at once.
    let checks = repo
        .refresh_checks("topic", None, &CancelToken::new())
        .expect("the turn is free after the cancelled refreshes");
    assert!(checks.is_empty(), "the forge reports no checks");
    assert_eq!(
        asked("/api/v3/repos/acme/widgets/commits/topic/check-runs"),
        1,
        "the branch is asked about once"
    );
}

#[test]
fn creating_a_review_sends_the_fork_head_and_caches_it() {
    let mock = enterprise_mock(|req, port| {
        if req.method == "POST" && is(req, "/api/v3/repos/acme/widgets/pulls") {
            return Some(Reply::Json(
                201,
                pull(
                    12,
                    "topic",
                    "bob",
                    &format!("http://127.0.0.1:{port}/bob/widgets.git"),
                    true,
                    "2026-04-02T00:00:00Z",
                )
                .to_string(),
            ));
        }
        None
    });
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), None);
    hub.add_github_enterprise(&format!("{}/api/v3", mock.url()), Sensitive("t".into()))
        .unwrap();
    let repo = hub.open_repo(input(
        tmp.path(),
        ForgeRemotes {
            base: Some(RemoteRef {
                name: "origin".into(),
                url: base_url(&mock),
            }),
            push: Some(RemoteRef {
                name: "fork".into(),
                url: format!("{}/bob/widgets.git", mock.url()),
            }),
        },
        RepoForgePrefs::default(),
    ));
    let review = repo
        .create_review(CreateReview {
            title: "Add topic".into(),
            body: "Why".into(),
            source_branch: "topic".into(),
            target_branch: "main".into(),
            draft: true,
        })
        .expect("create");
    assert_eq!(review.number, 12);
    let created = mock
        .requests()
        .into_iter()
        .find(|req| req.method == "POST")
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&created.body).unwrap();
    assert_eq!(
        (
            body["head"].as_str(),
            body["head_repo"].as_str(),
            body["base"].as_str(),
            body["draft"].as_bool()
        ),
        (Some("bob:topic"), Some("widgets"), Some("main"), Some(true)),
        "a pull request from the fork names the fork's owner and repository as its head"
    );
    assert_eq!(
        numbers(&repo.cached_reviews().unwrap()),
        vec![12],
        "the new review is cached right away"
    );
}

#[test]
fn checkout_plan_remote_naming() {
    let mock = enterprise_mock(|req, port| {
        if is(req, "/api/v3/repos/acme/widgets/pulls?state=open") {
            let own = format!("http://127.0.0.1:{port}/acme/widgets.git");
            let mut deleted_fork = pull(
                9,
                "gone",
                "ghost",
                "https://github.com/ghost/widgets.git",
                true,
                "2026-04-01T00:00:00Z",
            );
            deleted_fork["head"]["repo"] = serde_json::Value::Null;
            return Some(reply(&json!([
                pull(
                    7,
                    "fix-it",
                    "Octo Cat",
                    "https://github.com/octocat/widgets.git",
                    true,
                    "2026-04-01T00:00:00Z"
                ),
                pull(8, "topic", "acme", &own, false, "2026-04-02T00:00:00Z"),
                deleted_fork,
                pull(
                    10,
                    "friend-branch",
                    "friend",
                    "https://github.com/friend/widgets.git",
                    true,
                    "2026-04-03T00:00:00Z"
                ),
            ])));
        }
        None
    });
    let world = world(
        &mock,
        &[("friend-remote", "git@github.com:friend/widgets.git")],
    );
    world
        .repo
        .refresh_reviews(&CancelToken::new())
        .expect("refresh");

    let fork: CheckoutPlan = world.repo.checkout_plan(7).unwrap();
    assert_eq!(
        (fork.remote_name.as_str(), fork.remote_url.as_str()),
        ("octo-cat", "https://github.com/octocat/widgets.git"),
        "a fork gets a new remote named after its owner, sanitised"
    );
    assert_eq!(
        fork.fetch_refspec,
        "+refs/heads/fix-it:refs/remotes/octo-cat/fix-it"
    );
    assert_eq!(
        fork.local_branch, "octo-cat-fix-it",
        "a fork's branch is namespaced"
    );
    assert_eq!(
        fork.upstream.as_deref(),
        Some("refs/remotes/octo-cat/fix-it")
    );

    let inside = world.repo.checkout_plan(8).unwrap();
    assert_eq!(
        (inside.remote_name.as_str(), inside.local_branch.as_str()),
        ("origin", "topic"),
        "a review of the repository itself uses the base remote and the plain branch name"
    );

    let reused = world.repo.checkout_plan(10).unwrap();
    assert_eq!(
        (reused.remote_name.as_str(), reused.remote_url.as_str()),
        ("friend-remote", "git@github.com:friend/widgets.git"),
        "an existing remote of the same repository is reused, SSH or HTTPS alike"
    );

    let gone = world.repo.checkout_plan(9).unwrap();
    assert_eq!(
        (gone.fetch_refspec.as_str(), gone.upstream.as_ref()),
        ("+refs/pull/9/head:refs/pull/9/head", None),
        "without the fork the pull request's own ref is fetched, and nothing becomes the upstream"
    );

    assert!(
        world.repo.checkout_plan(404).is_err(),
        "an unknown pull request has no plan"
    );
}

// ---- checks against the mock -------------------------------------------------------------------------

#[test]
fn checks_refresh_cache_summary_and_prune() {
    let unresolvable = Arc::new(AtomicBool::new(false));
    let mock = {
        let unresolvable = unresolvable.clone();
        enterprise_mock(move |req, _| {
            if is(req, "/api/v3/repos/acme/widgets/commits/") && req.url.contains("/check-runs") {
                if unresolvable.load(Ordering::SeqCst) {
                    return Some(Reply::Json(422, r#"{"message":"No commit found"}"#.into()));
                }
                let run = |id: i64, name: &str, status: &str, conclusion: Option<&str>| {
                    json!({
                        "id": id, "name": name, "status": status, "conclusion": conclusion,
                        "html_url": format!("https://ci.invalid/{id}"),
                        "head_sha": "abc", "started_at": "2026-04-01T00:00:00Z",
                        "completed_at": null, "url": "u", "details_url": "d",
                    })
                };
                return Some(reply(&json!({
                    "total_count": 3,
                    "check_runs": [
                        run(1, "build", "completed", Some("success")),
                        run(2, "lint", "completed", Some("failure")),
                        run(3, "e2e", "in_progress", None),
                    ],
                })));
            }
            None
        })
    };
    let world = world(&mock, &[]);
    let repo = &world.repo;
    assert!(
        repo.cached_checks("topic").unwrap().is_empty(),
        "nothing cached before the first refresh"
    );
    let checks = repo
        .refresh_checks("topic", Some("abc"), &CancelToken::new())
        .expect("refresh checks");
    assert_eq!(checks.len(), 3, "three check runs");
    assert!(
        mock.requests()
            .iter()
            .any(|req| is(req, "/api/v3/repos/acme/widgets/commits/abc/check-runs")),
        "a fork's head commit is asked about instead of the branch"
    );
    let summary = RepoForge::checks_summary(&checks);
    assert_eq!(
        (
            summary.passed,
            summary.failed,
            summary.running,
            summary.queued
        ),
        (1, 1, 1, 0),
        "one passed, one failed, one running"
    );
    assert!(
        matches!(
            checks[1].status,
            CiStatus::Complete {
                conclusion: CiConclusion::Failure,
                ..
            }
        ),
        "the failure is a failure: {:?}",
        checks[1].status
    );
    let cached = repo.cached_checks("topic").unwrap();
    assert_eq!(
        cached.len(),
        3,
        "the checks are cached under the branch name"
    );

    // An unresolvable ref (a transient 422) shows no checks but keeps the good ones in the cache.
    unresolvable.store(true, Ordering::SeqCst);
    let none = repo
        .refresh_checks("topic", Some("abc"), &CancelToken::new())
        .unwrap();
    assert!(
        none.is_empty(),
        "no checks are shown for an unresolvable ref"
    );
    assert_eq!(
        repo.cached_checks("topic").unwrap().len(),
        3,
        "the cache keeps the good checks"
    );

    repo.prune_checks(&HashSet::from(["topic".to_owned()]))
        .unwrap();
    assert_eq!(
        repo.cached_checks("topic").unwrap().len(),
        3,
        "a kept reference stays"
    );
    repo.prune_checks(&HashSet::new()).unwrap();
    assert!(
        repo.cached_checks("topic").unwrap().is_empty(),
        "a reference that is not kept is dropped"
    );
}

#[test]
fn updating_and_merging_a_review() {
    let mock = enterprise_mock(|req, port| {
        let own = format!("http://127.0.0.1:{port}/acme/widgets.git");
        match (req.method.as_str(), req.url.as_str()) {
            ("PATCH", "/api/v3/repos/acme/widgets/pulls/8") => Some(reply(&pull(
                8,
                "topic",
                "acme",
                &own,
                false,
                "2026-04-05T00:00:00Z",
            ))),
            ("PUT", "/api/v3/repos/acme/widgets/pulls/8/merge") => Some(reply(
                &json!({"merged": true, "sha": "abc", "message": "merged"}),
            )),
            ("PUT", "/api/v3/repos/acme/widgets/pulls/9/merge") => Some(Reply::Json(
                405,
                r#"{"message":"Pull Request is not mergeable"}"#.into(),
            )),
            _ => None,
        }
    });
    let world = world(&mock, &[]);
    let repo = &world.repo;
    let body_of = |method: &str, url: &str| -> serde_json::Value {
        let req = mock
            .requests()
            .into_iter()
            .find(|req| req.method == method && req.url == url)
            .unwrap_or_else(|| panic!("no {method} {url}"));
        serde_json::from_str(&req.body).expect("a JSON body")
    };

    repo.update_review(
        8,
        gitcthulhu_services::forge::ReviewUpdatePayload::new(
            Some("New title".into()),
            None,
            Some(gitcthulhu_services::forge::ReviewState::Closed),
            Some("develop".into()),
        ),
    )
    .expect("update");
    assert_eq!(
        body_of("PATCH", "/api/v3/repos/acme/widgets/pulls/8"),
        json!({"title": "New title", "state": "closed", "base": "develop"}),
        "only what changes is sent, the body stays as it is"
    );

    repo.merge_review(
        8,
        Some(gitcthulhu_services::forge::ReviewMergeMethod::Squash),
    )
    .expect("merge");
    assert_eq!(
        body_of("PUT", "/api/v3/repos/acme/widgets/pulls/8/merge")["merge_method"],
        json!("squash"),
        "the chosen merge method reaches GitHub"
    );

    let refused = repo.merge_review(9, None).unwrap_err();
    assert!(
        !refused.may_pass() && !refused.is_terminal(),
        "a refused merge is neither retried nor an account problem: {refused:?}"
    );
    assert!(
        format!("{refused:#}").contains("not mergeable"),
        "GitHub's reason reaches the user: {refused:#}"
    );
    assert!(
        matches!(repo.merge_review(-1, None), Err(ForgeError::NotFound)),
        "a negative number is no review"
    );
}

#[test]
fn repo_info_comes_from_the_forge() {
    let mock = enterprise_mock(|req, _| {
        if req.url == "/api/v3/repos/acme/widgets" {
            return Some(reply(&json!({
                "fork": false, "private": true, "delete_branch_on_merge": true,
                "permissions": {"admin": false, "maintain": false, "push": true, "triage": true, "pull": true},
            })));
        }
        None
    });
    let world = world(&mock, &[]);
    let info = world.repo.repo_info().expect("repo info");
    assert_eq!(info.private, Some(true), "the repository is private");
    assert_eq!(info.delete_branch_on_merge, Some(true));
    assert!(info.permissions.unwrap().push, "the account may push");
}

#[test]
fn only_github_has_a_conversation() {
    let tmp = tempfile::tempdir().unwrap();
    let (_hub, gitlab) = bare_repo(tmp.path(), "https://gitlab.com/group/widgets.git");
    let conversation = gitlab
        .conversation(3)
        .expect("an empty conversation, not an error");
    assert!(
        conversation.comments.is_empty()
            && conversation.threads.is_empty()
            && conversation.submissions.is_empty()
            && conversation.reactions.is_empty(),
        "GitLab has no conversation API here, so it reads as empty without asking anybody: {conversation:?}"
    );
    assert!(
        matches!(gitlab.conversation(-1), Err(ForgeError::NotFound)),
        "a negative review number is no review"
    );
    let (_hub, github) = bare_repo(tmp.path(), "https://github.com/acme/widgets.git");
    assert!(
        matches!(github.conversation(3), Err(ForgeError::NotAuthenticated)),
        "GitHub's conversation needs an account, and without one polling should stop"
    );
}

// ---- images ------------------------------------------------------------------------------------------

#[test]
fn fetch_image_downloads_within_limits() {
    let mock = Mock::start(|_| {
        Box::new(|req| match req.url.as_str() {
            "/avatar.png" => Reply::Bytes(200, b"\x89PNG-bytes".to_vec()),
            "/huge.png" => Reply::Bytes(200, vec![0; (16 << 20) + 1]),
            _ => Reply::Json(404, "{}".into()),
        })
    });
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), None);
    assert_eq!(
        hub.fetch_image(&format!("{}/avatar.png", mock.url()))
            .unwrap(),
        b"\x89PNG-bytes",
        "the image's bytes"
    );
    assert!(
        hub.fetch_image(&format!("{}/huge.png", mock.url()))
            .is_err(),
        "an image over the cap is refused"
    );
    assert!(
        matches!(
            hub.fetch_image(&format!("{}/missing.png", mock.url())),
            Err(ForgeError::NotFound)
        ),
        "a missing image is NotFound"
    );
    for url in [
        "file:///etc/passwd",
        "ftp://example.com/a.png",
        "not a url",
        "",
    ] {
        assert!(hub.fetch_image(url).is_err(), "'{url}' is no image URL");
    }
    assert_eq!(
        mock.count(|req| req.auth.is_some()),
        0,
        "no token goes to a host that is not GitHub"
    );
}

// ---- templates ---------------------------------------------------------------------------------------

#[test]
fn templates_are_listed_and_read_safely() {
    let tmp = tempfile::tempdir().unwrap();
    let (_hub, repo) = bare_repo(tmp.path(), "git@github.com:acme/widgets.git");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(work.join(".github")).unwrap();
    std::fs::write(
        work.join(".github").join("pull_request_template.md"),
        "## What\n",
    )
    .unwrap();
    std::fs::write(work.join("README.md"), "no template").unwrap();
    std::fs::write(tmp.path().join("secret.md"), "outside").unwrap();

    let templates = repo.templates(&work);
    assert_eq!(templates.len(), 1, "one template: {templates:?}");
    let template = &templates[0];
    assert_eq!(
        repo.read_template(&work, template).unwrap(),
        "## What\n",
        "the template's text"
    );
    for bad in [
        "README.md",
        "../secret.md",
        ".github/../../secret.md",
        "/etc/passwd",
        ".github/pull_request_template.txt",
    ] {
        assert!(
            repo.read_template(&work, bad).is_err(),
            "'{bad}' is not a readable template"
        );
    }
    assert!(
        matches!(
            repo.read_template(&work, ".github/PULL_REQUEST_TEMPLATE/missing.md"),
            Err(ForgeError::NotFound)
        ),
        "a missing template is NotFound"
    );
    std::fs::write(
        work.join(".github").join("pull_request_template.md"),
        [0xff, 0xfe, 0x00],
    )
    .unwrap();
    assert!(
        repo.read_template(&work, template).is_err(),
        "a template that is not UTF-8 is refused"
    );
    let cap = 1 << 20;
    std::fs::write(
        work.join(".github").join("pull_request_template.md"),
        vec![b'a'; cap],
    )
    .unwrap();
    assert_eq!(
        repo.read_template(&work, template)
            .map(|text| text.len())
            .ok(),
        Some(cap),
        "a template of exactly 1 MiB is read in full"
    );
    std::fs::write(
        work.join(".github").join("pull_request_template.md"),
        vec![b'a'; cap + 1],
    )
    .unwrap();
    assert!(
        repo.read_template(&work, template).is_err(),
        "a template over 1 MiB is refused"
    );
    let (_hub, plain) = bare_repo(tmp.path(), "https://example.com/acme/widgets");
    assert!(plain.templates(&work).is_empty(), "no forge, no templates");
}
