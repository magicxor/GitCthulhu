//! Update service tests against a loopback manifest server and a throw-away cache directory.
#![cfg(feature = "update")]

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::{Duration, Instant},
};

use chrono::Utc;
use gitcthulhu_services::{
    CancelToken, Runtime, ServicesConfig, UpdateChannel,
    update::{AvailableUpdate, UpdateError, UpdateService},
};

/// What the loopback server does with one connection.
enum Reply {
    /// Answer at once with this manifest body.
    Body(String),
    /// Answer with this body after a delay (a slow server).
    After(Duration, String),
    /// Read the request and never answer (a hanging server).
    Hang,
}

/// A loopback HTTP server that handles one connection per [`Reply`], in order. Request heads arrive on
/// `requests`.
struct Server {
    url: String,
    requests: mpsc::Receiver<String>,
}

impl Server {
    fn start(replies: Vec<Reply>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback port is free");
        let url = format!(
            "http://{}/{{channel}}.json",
            listener.local_addr().expect("bound address")
        );
        let (tx, requests) = mpsc::channel();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                tx.send(String::from_utf8_lossy(&request).into_owned()).ok();
                let body = match reply {
                    Reply::Body(body) => body,
                    Reply::After(delay, body) => {
                        std::thread::sleep(delay);
                        body
                    }
                    Reply::Hang => {
                        held.push(stream);
                        continue;
                    }
                };
                let message = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(message.as_bytes()).ok();
            }
            // Keep hanging connections open until the test process ends.
            std::thread::sleep(Duration::from_secs(60));
        });
        Server { url, requests }
    }

    fn next_request(&self) -> String {
        self.requests
            .recv_timeout(Duration::from_secs(10))
            .expect("the server received a request")
    }

    /// Whether a request arrived that was not yet taken with [`Server::next_request`].
    fn has_unseen_request(&self) -> bool {
        self.requests.try_recv().is_ok()
    }
}

fn manifest_json(version: &str) -> String {
    format!(
        r#"{{"version": "{version}", "notes": "notes for {version}", "platforms": {{"{}": {{"url": "https://example.com/{version}", "signature": "sig-{version}"}}}}}}"#,
        but_update::current_platform_key()
    )
}

fn manifest(version: &str) -> Reply {
    Reply::Body(manifest_json(version))
}

fn config(
    cache: &tempfile::TempDir,
    manifest_url: Option<&str>,
    version: &str,
    channel: UpdateChannel,
) -> ServicesConfig {
    ServicesConfig {
        app_data_dir: cache.path().join("data"),
        cache_dir: cache.path().join("cache"),
        user_agent: format!("GitCthulhu/{version}"),
        app_version: semver::Version::parse(version).expect("valid version"),
        github_oauth_client_id: None,
        update_manifest_url: manifest_url.map(str::to_owned),
        update_channel: channel,
    }
}

fn service(
    rt: &Runtime,
    cache: &tempfile::TempDir,
    manifest_url: Option<&str>,
    version: &str,
) -> UpdateService {
    UpdateService::new(
        rt.clone(),
        &config(cache, manifest_url, version, UpdateChannel::Release),
    )
}

fn runtime() -> Runtime {
    Runtime::new().expect("runtime builds")
}

#[test]
fn manifest_parse() {
    let server = Server::start(vec![manifest("0.2.0")]);
    let cache = tempfile::tempdir().expect("temp dir");
    let svc = service(&runtime(), &cache, Some(&server.url), "0.1.0");

    let update = svc
        .check_now(&CancelToken::new())
        .expect("the check works")
        .expect("0.2.0 is newer than 0.1.0");
    assert_eq!(
        update,
        AvailableUpdate {
            version: semver::Version::new(0, 2, 0),
            notes: Some("notes for 0.2.0".into()),
            url: "https://example.com/0.2.0".into(),
            signature: Some("sig-0.2.0".into()),
        },
        "version, notes, url and signature come from this platform's manifest entry"
    );

    let request = server.next_request().to_ascii_lowercase();
    assert!(
        request.starts_with("get /release.json "),
        "{{channel}} in the manifest url becomes the channel name, got: {request}"
    );
    assert!(
        request.contains("user-agent: gitcthulhu/0.1.0"),
        "the configured user agent is sent, got: {request}"
    );
}

#[test]
fn newer_semver_only() {
    let rt = runtime();
    for (running, published, expect_update) in [
        ("0.9.0", "0.10.0", true),
        ("0.10.0", "0.9.0", false),
        ("1.0.0", "1.0.0", false),
        ("1.0.0", "1.0.1", true),
        ("1.0.0-rc.1", "1.0.0", true),
        ("1.0.0", "1.0.0-rc.1", false),
        ("1.0.0", "1.0.0+build.7", false),
    ] {
        let server = Server::start(vec![manifest(published)]);
        let cache = tempfile::tempdir().expect("temp dir");
        let svc = service(&rt, &cache, Some(&server.url), running);
        let found = svc.check_now(&CancelToken::new()).expect("the check works");
        assert_eq!(
            found.is_some(),
            expect_update,
            "running {running}, published {published}"
        );
        assert_eq!(
            svc.cached().is_some(),
            expect_update,
            "the cache agrees with the check for running {running}, published {published}"
        );
    }
}

#[test]
fn platform_key_macos_maps_to_darwin() {
    assert_eq!(
        but_update::platform_key("macos", "aarch64"),
        "darwin-aarch64",
        "the manifest spells macOS darwin"
    );
    // A manifest that only lists other platforms has nothing for this machine.
    let server = Server::start(vec![Reply::Body(
        r#"{"version": "9.0.0", "platforms": {"plan9-mips": {"url": "https://example.com/x"}}}"#
            .into(),
    )]);
    let cache = tempfile::tempdir().expect("temp dir");
    let svc = service(&runtime(), &cache, Some(&server.url), "0.1.0");
    assert_eq!(
        svc.check_now(&CancelToken::new()).expect("the check works"),
        None,
        "no build for this platform means nothing to announce"
    );
}

#[test]
fn disabled_without_manifest_url() {
    let cache = tempfile::tempdir().expect("temp dir");
    let svc = service(&runtime(), &cache, None, "0.1.0");
    assert!(
        matches!(
            svc.check_now(&CancelToken::new()),
            Err(UpdateError::Disabled)
        ),
        "dev builds without a manifest URL never check"
    );
    assert!(
        matches!(svc.snooze(24), Err(UpdateError::Disabled)),
        "there is nothing to snooze when checks are disabled"
    );
    assert!(svc.cached().is_none(), "nothing is cached");
    assert!(
        !cache.path().join("cache").exists(),
        "a disabled service does not even create its cache"
    );
}

#[test]
fn snooze_hides_until_expiry() {
    let server = Server::start(vec![manifest("0.2.0"), manifest("0.2.0")]);
    let cache = tempfile::tempdir().expect("temp dir");
    let svc = service(&runtime(), &cache, Some(&server.url), "0.1.0");

    assert!(
        matches!(svc.snooze(24), Err(UpdateError::Cache(_))),
        "snoozing before any check is an error"
    );
    assert!(
        svc.check_now(&CancelToken::new())
            .expect("the check works")
            .is_some(),
        "the update is announced"
    );
    for hours in [0, 721] {
        assert!(
            matches!(svc.snooze(hours), Err(UpdateError::InvalidArgument(_))),
            "{hours} hours is outside 1..=720"
        );
    }
    assert!(svc.cached().is_some(), "invalid snoozes change nothing");

    svc.snooze(24).expect("snoozing works");
    assert!(svc.cached().is_none(), "the snoozed update is hidden");
    assert!(
        svc.check_now(&CancelToken::new())
            .expect("the check works")
            .is_none(),
        "a re-check keeps the running snooze"
    );

    // Expiry: rewrite the snooze as started 25 hours ago.
    let mut db = but_db::AppCacheHandle::new_in_directory(Some(cache.path().join("cache")));
    let mut row = db.update_check().get().expect("the check result is stored");
    row.suppressed_at = Some(Utc::now() - chrono::Duration::hours(25));
    row.suppress_duration_hours = Some(24);
    db.update_check_mut()
        .expect("savepoint opens")
        .save(&row)
        .expect("saving works");
    drop(db);
    assert_eq!(
        svc.cached().map(|update| update.version),
        Some(semver::Version::new(0, 2, 0)),
        "after the snooze elapsed the update is shown again"
    );
}

#[test]
fn cache_valid_for_version() {
    let rt = runtime();
    let cache = tempfile::tempdir().expect("temp dir");
    let server = Server::start(vec![manifest("0.2.0")]);
    let old = service(&rt, &cache, Some(&server.url), "0.1.0");
    assert!(
        old.check_now(&CancelToken::new())
            .expect("the check works")
            .is_some(),
        "0.1.0 learns about 0.2.0"
    );
    assert!(old.cached().is_some(), "and remembers it");

    let upgraded = service(&rt, &cache, Some(&server.url), "0.2.0");
    assert!(
        upgraded.cached().is_none(),
        "after upgrading to 0.2.0 the cached notice is stale"
    );

    let nightly = UpdateService::new(
        rt,
        &config(&cache, Some(&server.url), "0.1.0", UpdateChannel::Nightly),
    );
    assert!(
        nightly.cached().is_none(),
        "a result found on the release channel is not shown on nightly"
    );
    assert!(
        old.cached().is_some(),
        "the original service still sees its own result"
    );
}

#[test]
fn failed_check_is_classified_and_keeps_the_cache() {
    let rt = runtime();
    let cache = tempfile::tempdir().expect("temp dir");
    let server = Server::start(vec![
        manifest("0.2.0"),
        Reply::Body("<html>not json</html>".into()),
    ]);
    let svc = service(&rt, &cache, Some(&server.url), "0.1.0");
    svc.check_now(&CancelToken::new())
        .expect("the first check works");

    let err = svc
        .check_now(&CancelToken::new())
        .expect_err("garbage is not a manifest");
    assert!(
        matches!(err, UpdateError::Manifest(_)),
        "an unparsable manifest is a manifest error, got {err:?}"
    );
    assert!(
        svc.cached().is_some(),
        "the earlier result survives a failed check"
    );

    let unreachable = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback port is free");
        format!("http://{}/m.json", listener.local_addr().expect("address"))
    };
    let err = service(&rt, &cache, Some(&unreachable), "0.1.0")
        .check_now(&CancelToken::new())
        .expect_err("nobody listens there");
    assert!(
        matches!(err, UpdateError::Network(_)),
        "a refused connection is a network error, got {err:?}"
    );

    let err = service(&rt, &cache, Some("http://example.com/m.json"), "0.1.0")
        .check_now(&CancelToken::new())
        .expect_err("plain http to a remote host is refused");
    assert!(
        matches!(err, UpdateError::Manifest(_)),
        "an insecure manifest url is refused without a request, got {err:?}"
    );
}

#[test]
fn cancel_interrupts_a_hanging_server() {
    let server = Server::start(vec![Reply::Hang]);
    let cache = tempfile::tempdir().expect("temp dir");
    let svc = service(&runtime(), &cache, Some(&server.url), "0.1.0");
    let cancel = CancelToken::new();
    let canceller = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel();
        })
    };
    let started = Instant::now();
    let err = svc
        .check_now(&cancel)
        .expect_err("the server never answers");
    canceller.join().expect("canceller finishes");
    assert!(
        matches!(err, UpdateError::Cancelled),
        "a cancelled check reports Cancelled, got {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "cancellation does not wait for the 30 s request timeout ({:?})",
        started.elapsed()
    );

    let already = CancelToken::new();
    already.cancel();
    assert!(
        matches!(svc.check_now(&already), Err(UpdateError::Cancelled)),
        "an already cancelled token cancels immediately"
    );
}

#[test]
fn concurrent_check_waits_for_the_running_one_instead_of_requesting_again() {
    let rt = runtime();
    let cache = tempfile::tempdir().expect("temp dir");
    let seed = Server::start(vec![manifest("0.2.0")]);
    service(&rt, &cache, Some(&seed.url), "0.1.0")
        .check_now(&CancelToken::new())
        .expect("seeding works");

    // Only one reply: a second request would hang until the 30 s request timeout.
    let slow = Server::start(vec![Reply::After(
        Duration::from_millis(1500),
        manifest_json("0.3.0"),
    )]);
    let svc = std::sync::Arc::new(service(&rt, &cache, Some(&slow.url), "0.1.0"));
    assert!(
        svc.cached().is_some(),
        "the seeded result is visible (opens the cache)"
    );
    let first = {
        let svc = svc.clone();
        std::thread::spawn(move || svc.check_now(&CancelToken::new()))
    };
    slow.next_request();

    let started = Instant::now();
    assert_eq!(
        svc.cached().map(|update| update.version),
        Some(semver::Version::new(0, 2, 0)),
        "cached() answers from the cache while a check is in flight"
    );
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "cached() did not wait for the running check ({:?})",
        started.elapsed()
    );
    let second = svc
        .check_now(&CancelToken::new())
        .expect("the concurrent check does not fail");
    assert_eq!(
        second.map(|update| update.version),
        Some(semver::Version::new(0, 3, 0)),
        "the concurrent check reports what the running check found, not the stale cache"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "it waited for the running check, not for a request timeout ({:?})",
        started.elapsed()
    );
    let first = first
        .join()
        .expect("the checking thread finishes")
        .expect("the first check works");
    assert_eq!(
        first.map(|update| update.version),
        Some(semver::Version::new(0, 3, 0)),
        "the first check found 0.3.0"
    );
    assert!(
        !slow.has_unseen_request(),
        "the manifest was requested only once"
    );
}

#[test]
fn concurrent_check_after_a_cancelled_one_requests_itself() {
    let rt = runtime();
    let cache = tempfile::tempdir().expect("temp dir");
    let server = Server::start(vec![Reply::Hang, manifest("0.3.0")]);
    let svc = std::sync::Arc::new(service(&rt, &cache, Some(&server.url), "0.1.0"));
    let cancel_first = CancelToken::new();
    let first = {
        let svc = svc.clone();
        let cancel = cancel_first.clone();
        std::thread::spawn(move || svc.check_now(&cancel))
    };
    server.next_request();

    // A caller waiting for the running check can give up.
    let cancel_waiting = CancelToken::new();
    let canceller = {
        let cancel = cancel_waiting.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.cancel();
        })
    };
    let started = Instant::now();
    let err = svc
        .check_now(&cancel_waiting)
        .expect_err("cancelled while waiting for the running check");
    canceller.join().expect("canceller finishes");
    assert!(
        matches!(err, UpdateError::Cancelled),
        "waiting is cancellable, got {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the cancel interrupted the wait ({:?})",
        started.elapsed()
    );

    let second = std::thread::spawn(move || svc.check_now(&CancelToken::new()));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !second.is_finished(),
        "the second check waits while the first one runs"
    );
    assert!(
        !server.has_unseen_request(),
        "and does not request the manifest meanwhile"
    );

    cancel_first.cancel();
    let outcome = first.join().expect("the first thread finishes");
    assert!(
        matches!(outcome, Err(UpdateError::Cancelled)),
        "the first check was cancelled, got {outcome:?}"
    );
    let second = second
        .join()
        .expect("the second thread finishes")
        .expect("the second check works");
    assert_eq!(
        second.map(|update| update.version),
        Some(semver::Version::new(0, 3, 0)),
        "the cancelled check stored nothing, so the waiting one asked the server itself"
    );
    server.next_request();
}

#[test]
fn snooze_ends_when_a_newer_version_is_published() {
    let server = Server::start(vec![
        manifest("0.2.0"),
        manifest("0.2.0"),
        manifest("0.3.0"),
    ]);
    let cache = tempfile::tempdir().expect("temp dir");
    let svc = service(&runtime(), &cache, Some(&server.url), "0.1.0");
    svc.check_now(&CancelToken::new())
        .expect("the check works")
        .expect("0.2.0 is announced");
    svc.snooze(720).expect("snoozing works");
    assert_eq!(
        svc.check_now(&CancelToken::new()).expect("the check works"),
        None,
        "0.2.0 stays snoozed"
    );
    assert_eq!(
        svc.check_now(&CancelToken::new())
            .expect("the check works")
            .map(|update| update.version),
        Some(semver::Version::new(0, 3, 0)),
        "the snooze was for 0.2.0; 0.3.0 is announced"
    );
}

#[test]
fn set_channel_switches_the_manifest_and_the_cache_scope_without_a_restart() {
    let server = Server::start(vec![manifest("0.2.0"), manifest("0.3.0")]);
    let cache = tempfile::tempdir().expect("temp dir");
    let svc = service(&runtime(), &cache, Some(&server.url), "0.1.0");
    svc.check_now(&CancelToken::new())
        .expect("the release check works")
        .expect("0.2.0 is announced");
    assert!(
        server.next_request().starts_with("GET /release.json "),
        "the service starts on the channel it was configured with"
    );
    assert!(svc.cached().is_some() && svc.cached_at().is_some());

    svc.set_channel(UpdateChannel::Nightly);
    assert!(
        svc.cached().is_none() && svc.cached_at().is_none(),
        "the release result is not the nightly one"
    );
    let found = svc
        .check_now(&CancelToken::new())
        .expect("the nightly check works")
        .expect("0.3.0 is announced");
    assert_eq!(found.version, semver::Version::new(0, 3, 0));
    assert!(
        server.next_request().starts_with("GET /nightly.json "),
        "the next check asks the nightly manifest"
    );
    assert!(svc.cached_at().is_some(), "the nightly check is remembered");

    svc.set_channel(UpdateChannel::Release);
    assert_eq!(
        svc.cached().map(|update| update.version),
        None,
        "the cache holds the nightly outcome only (one record)"
    );
}

#[test]
fn cached_at_is_the_stored_check_time_and_survives_a_restart() {
    let rt = runtime();
    let server = Server::start(vec![manifest("0.1.0")]);
    let cache = tempfile::tempdir().expect("temp dir");
    let svc = service(&rt, &cache, Some(&server.url), "0.1.0");
    assert_eq!(svc.cached_at(), None, "nothing was checked yet");

    let before = std::time::SystemTime::now() - Duration::from_secs(2);
    assert_eq!(
        svc.check_now(&CancelToken::new()).expect("up to date"),
        None
    );
    let at = svc.cached_at().expect("the check time is stored");
    assert!(
        at >= before && at <= std::time::SystemTime::now() + Duration::from_secs(2),
        "it is the time of the check: {at:?}"
    );

    let restarted = service(&rt, &cache, Some(&server.url), "0.1.0");
    assert_eq!(
        restarted.cached_at(),
        Some(at),
        "a new service reads the time from the cache database"
    );
    let upgraded = service(&rt, &cache, Some(&server.url), "0.1.1");
    assert_eq!(
        upgraded.cached_at(),
        None,
        "another version's check does not count"
    );
    let disabled = service(&rt, &cache, None, "0.1.0");
    assert_eq!(disabled.cached_at(), None, "disabled checks have no time");
}

#[test]
fn a_check_waiting_for_one_of_the_old_channel_asks_the_new_channel_itself() {
    let rt = runtime();
    let cache = tempfile::tempdir().expect("temp dir");
    let server = Server::start(vec![
        Reply::After(Duration::from_millis(1500), manifest_json("0.2.0")),
        manifest("0.3.0"),
    ]);
    let svc = std::sync::Arc::new(service(&rt, &cache, Some(&server.url), "0.1.0"));
    let first = {
        let svc = svc.clone();
        std::thread::spawn(move || svc.check_now(&CancelToken::new()))
    };
    assert!(server.next_request().starts_with("GET /release.json "));

    svc.set_channel(UpdateChannel::Nightly);
    let second = svc
        .check_now(&CancelToken::new())
        .expect("the nightly check works");
    assert_eq!(
        second.map(|update| update.version),
        Some(semver::Version::new(0, 3, 0)),
        "the release answer of the running check is not the nightly one"
    );
    assert!(
        server.next_request().starts_with("GET /nightly.json "),
        "the waiting check asked the nightly manifest after the release check ended"
    );
    first
        .join()
        .expect("the checking thread finishes")
        .expect("the release check works");
}
