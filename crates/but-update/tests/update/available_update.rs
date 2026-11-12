use chrono::{Duration, Utc};

use crate::support::{SCOPE, cached_result, in_memory_cache, save, version};

#[test]
fn nothing_cached_means_no_update() -> anyhow::Result<()> {
    let cache = in_memory_cache();
    assert_eq!(
        but_update::available_update(&cache, &version("0.1.0"), SCOPE)?,
        None,
        "no check was made yet"
    );
    assert_eq!(but_update::last_checked(&cache)?, None, "no check yet");
    Ok(())
}

#[test]
fn cache_is_ignored_for_another_scope() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(false, "1.2.3", Some("0.0.9/release"), None),
    );
    assert_eq!(
        but_update::available_update(&cache, &version("0.1.0"), SCOPE)?,
        None,
        "a result produced for another version or channel is stale"
    );

    save(&mut cache, &cached_result(false, "1.2.3", None, None));
    assert_eq!(
        but_update::available_update(&cache, &version("0.1.0"), SCOPE)?,
        None,
        "a result without a scope cannot be trusted"
    );
    Ok(())
}

#[test]
fn returns_update_when_scope_matches() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(false, "1.2.3", Some(SCOPE), None),
    );

    let update = but_update::available_update(&cache, &version("0.1.0"), SCOPE)?
        .expect("an update is available");
    assert_eq!(update.current_version, version("0.1.0"));
    assert_eq!(update.available_version, version("1.2.3"));
    assert_eq!(
        update.release_notes.as_deref(),
        Some("Bug fixes and improvements")
    );
    assert_eq!(update.url.as_deref(), Some("https://example.com/download"));
    assert_eq!(update.signature.as_deref(), Some("signature123"));
    assert!(
        but_update::last_checked(&cache)?.is_some(),
        "the check time is known"
    );
    Ok(())
}

#[test]
fn up_to_date_result_offers_nothing() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    save(&mut cache, &cached_result(true, "0.1.0", Some(SCOPE), None));
    assert_eq!(
        but_update::available_update(&cache, &version("0.1.0"), SCOPE)?,
        None,
        "up to date"
    );
    Ok(())
}

#[test]
fn unparsable_cached_version_offers_nothing() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(false, "banana", Some(SCOPE), None),
    );
    assert_eq!(
        but_update::available_update(&cache, &version("0.1.0"), SCOPE)?,
        None,
        "a corrupt cached version is treated as no update"
    );
    Ok(())
}

#[test]
fn snooze_hides_until_expiry() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    let snoozed_at = Utc::now() - Duration::hours(10);
    save(
        &mut cache,
        &cached_result(false, "1.2.3", Some(SCOPE), Some((snoozed_at, 24))),
    );
    let current = version("0.1.0");

    for (label, now, hidden) in [
        ("right after snoozing", snoozed_at, true),
        (
            "one minute before expiry",
            snoozed_at + Duration::hours(24) - Duration::minutes(1),
            true,
        ),
        ("exactly at expiry", snoozed_at + Duration::hours(24), true),
        (
            "one minute after expiry",
            snoozed_at + Duration::hours(24) + Duration::minutes(1),
            false,
        ),
    ] {
        let found = but_update::available_update_at(&cache, &current, SCOPE, now)?;
        assert_eq!(
            found.is_none(),
            hidden,
            "{label}: the update is {} while snoozed for 24h",
            if hidden { "hidden" } else { "visible" }
        );
    }
    Ok(())
}
