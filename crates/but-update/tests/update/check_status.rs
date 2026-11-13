use but_update::{CheckError, CheckParams, check_status, current_platform_key};
use chrono::{Duration, Utc};

use crate::support::{
    SCOPE, Server, cached_result, in_memory_cache, ok_json, response, save, version,
};

const USER_AGENT: &str = "GitCthulhu/0.1.0";

fn manifest_json(latest: &str) -> String {
    format!(
        r#"{{"version": "{latest}", "notes": "notes", "platforms": {{"{}": {{"url": "https://example.com/build", "signature": "sig"}}}}}}"#,
        current_platform_key()
    )
}

fn params<'a>(url: &'a str, current: &'a semver::Version) -> CheckParams<'a> {
    CheckParams {
        manifest_url: url,
        current_version: current,
        cache_scope: SCOPE,
        user_agent: USER_AGENT,
    }
}

#[test]
fn newer_manifest_is_cached_and_sent_with_our_user_agent() -> anyhow::Result<()> {
    let server = Server::start(vec![ok_json(&manifest_json("0.2.0"))]);
    let mut cache = in_memory_cache();
    let current = version("0.1.0");

    let status = check_status(&params(&server.url, &current), &mut cache)?
        .expect("nobody else holds the lock");
    assert!(!status.up_to_date, "0.2.0 is newer than 0.1.0");
    assert_eq!(status.url.as_deref(), Some("https://example.com/build"));

    let request = server.next_request().to_ascii_lowercase();
    assert!(
        request.starts_with("get /latest.json "),
        "a plain GET of the manifest, got: {request}"
    );
    assert!(
        request.contains("user-agent: gitcthulhu/0.1.0"),
        "the user agent identifies GitCthulhu, got: {request}"
    );
    assert!(
        !request.contains("x-auth-token"),
        "no credentials are sent, got: {request}"
    );

    let update = but_update::available_update(&cache, &current, SCOPE)?
        .expect("the result was cached for the scope");
    assert_eq!(update.available_version, version("0.2.0"));
    assert_eq!(update.signature.as_deref(), Some("sig"));
    assert!(
        but_update::last_checked(&cache)?.is_some(),
        "the check time is recorded"
    );
    Ok(())
}

#[test]
fn same_version_manifest_is_up_to_date() -> anyhow::Result<()> {
    let server = Server::start(vec![ok_json(&manifest_json("0.1.0"))]);
    let mut cache = in_memory_cache();
    let current = version("0.1.0");

    let status = check_status(&params(&server.url, &current), &mut cache)?
        .expect("nobody else holds the lock");
    assert!(status.up_to_date, "an equal version offers nothing");
    assert_eq!(
        but_update::available_update(&cache, &current, SCOPE)?,
        None,
        "nothing cached to show"
    );
    Ok(())
}

#[test]
fn failures_are_classified_and_keep_the_cache() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(false, "1.2.3", Some(SCOPE), None),
    );
    let current = version("0.1.0");

    let server = Server::start(vec![
        response("500 Internal Server Error", "boom"),
        ok_json("this is not a manifest"),
    ]);
    let err = check_status(&params(&server.url, &current), &mut cache)
        .expect_err("a 500 is a failed check");
    assert!(
        matches!(err, CheckError::Network(_)),
        "an HTTP error status is a network error, got {err:?}"
    );
    let err = check_status(&params(&server.url, &current), &mut cache)
        .expect_err("garbage is a failed check");
    assert!(
        matches!(err, CheckError::Manifest(_)),
        "an unparsable body is a manifest error, got {err:?}"
    );

    let unreachable = {
        // A port that was free a moment ago and has no listener now.
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        format!("http://{}/latest.json", listener.local_addr()?)
    };
    let err = check_status(&params(&unreachable, &current), &mut cache)
        .expect_err("a refused connection is a failed check");
    assert!(
        matches!(err, CheckError::Network(_)),
        "a refused connection is a network error, got {err:?}"
    );

    assert_eq!(
        but_update::available_update(&cache, &current, SCOPE)?
            .map(|update| update.available_version),
        Some(version("1.2.3")),
        "failed checks leave the previous result in place"
    );
    Ok(())
}

#[test]
fn manifest_url_must_be_https_or_loopback() {
    let mut cache = in_memory_cache();
    let current = version("0.1.0");
    for url in [
        "http://example.com/latest.json",
        "ftp://example.com/latest.json",
        "file:///c:/latest.json",
        "not a url",
    ] {
        let err = check_status(&params(url, &current), &mut cache)
            .expect_err("only https manifests are fetched");
        assert!(
            matches!(err, CheckError::Manifest(_)),
            "{url}: classified as a manifest error, got {err:?}"
        );
    }
}

#[test]
fn snooze_survives_a_later_check_until_it_expires() -> anyhow::Result<()> {
    let current = version("0.1.0");
    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(
            false,
            "0.2.0",
            Some(SCOPE),
            Some((Utc::now() - Duration::hours(1), 24)),
        ),
    );
    let server = Server::start(vec![ok_json(&manifest_json("0.2.0"))]);
    check_status(&params(&server.url, &current), &mut cache)?.expect("check ran");
    assert_eq!(
        but_update::available_update(&cache, &current, SCOPE)?,
        None,
        "a snooze that is still running survives a re-check"
    );

    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(
            false,
            "0.2.0",
            Some(SCOPE),
            Some((Utc::now() - Duration::hours(30), 24)),
        ),
    );
    let server = Server::start(vec![ok_json(&manifest_json("0.2.0"))]);
    check_status(&params(&server.url, &current), &mut cache)?.expect("check ran");
    assert!(
        but_update::available_update(&cache, &current, SCOPE)?.is_some(),
        "an expired snooze is dropped by the next check"
    );

    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(
            false,
            "0.2.0",
            Some(SCOPE),
            Some((Utc::now() - Duration::hours(1), 24)),
        ),
    );
    let server = Server::start(vec![ok_json(&manifest_json("0.3.0"))]);
    check_status(&params(&server.url, &current), &mut cache)?.expect("check ran");
    assert_eq!(
        but_update::available_update(&cache, &current, SCOPE)?
            .map(|update| update.available_version),
        Some(version("0.3.0")),
        "a snooze of 0.2.0 does not hide 0.3.0 published during the snooze"
    );
    let row = cache.update_check().get().expect("the check is stored");
    assert_eq!(
        (row.suppressed_at, row.suppress_duration_hours),
        (None, None),
        "the snooze of the older version is gone"
    );
    Ok(())
}

#[tokio::test]
async fn fetch_status_leaves_the_cache_alone_until_recorded() -> anyhow::Result<()> {
    let server = Server::start(vec![ok_json(&manifest_json("0.2.0"))]);
    let mut cache = in_memory_cache();
    let current = version("0.1.0");
    let params = params(&server.url, &current);

    let status = but_update::fetch_status(&params).await?;
    assert!(!status.up_to_date, "0.2.0 is newer than 0.1.0");
    assert_eq!(
        but_update::available_update(&cache, &current, SCOPE)?,
        None,
        "fetching alone stores nothing"
    );

    but_update::record_status(&mut cache, &params, &status)?;
    assert_eq!(
        but_update::available_update(&cache, &current, SCOPE)?
            .map(|update| update.available_version),
        Some(version("0.2.0")),
        "recording makes the result available"
    );
    Ok(())
}
