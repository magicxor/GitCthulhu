use but_update::CheckError;

use crate::support::{SCOPE, cached_result, in_memory_cache, save, version};

#[test]
fn suppress_too_many_hours_fails() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(false, "1.2.3", Some(SCOPE), None),
    );

    let err = but_update::suppress_update(&mut cache, 721).expect_err("721 hours is too long");
    assert!(
        matches!(&err, CheckError::Cache(msg) if msg.contains("cannot exceed 720 hours")),
        "an out-of-range snooze is rejected, got {err:?}"
    );
    Ok(())
}

#[test]
fn suppress_zero_hours_fails() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(false, "1.2.3", Some(SCOPE), None),
    );

    let err = but_update::suppress_update(&mut cache, 0).expect_err("0 hours is not a snooze");
    assert!(
        matches!(&err, CheckError::Cache(msg) if msg.contains("at least 1 hour")),
        "a zero snooze is rejected, got {err:?}"
    );
    Ok(())
}

#[test]
fn suppress_without_a_check_fails() {
    let mut cache = in_memory_cache();
    let err = but_update::suppress_update(&mut cache, 24)
        .expect_err("nothing to snooze before the first check");
    assert!(
        matches!(err, CheckError::Cache(_)),
        "the missing check is a cache error, got {err:?}"
    );
}

#[test]
fn suppress_hides_the_update_now() -> anyhow::Result<()> {
    let mut cache = in_memory_cache();
    save(
        &mut cache,
        &cached_result(false, "1.2.3", Some(SCOPE), None),
    );
    let current = version("0.1.0");
    assert!(
        but_update::available_update(&cache, &current, SCOPE)?.is_some(),
        "visible before the snooze"
    );

    but_update::suppress_update(&mut cache, 1)?;
    assert!(
        but_update::available_update(&cache, &current, SCOPE)?.is_none(),
        "hidden right after snoozing for one hour"
    );
    for hours in [1, 720] {
        but_update::suppress_update(&mut cache, hours)?;
    }
    Ok(())
}
