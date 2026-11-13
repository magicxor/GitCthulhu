use but_core::branch::{LocalRefName, LocalTrackingBranchMatch, find_local_tracking_branch};
use but_testsupport::read_only_in_memory_scenario;

fn repo() -> anyhow::Result<gix::Repository> {
    read_only_in_memory_scenario("local-branch-tracking")
}

#[test]
fn tracking_lookup_returns_unique_local_branch() -> anyhow::Result<()> {
    let actual = find_local_tracking_branch("refs/remotes/origin/trunk".try_into()?, &repo()?)?;
    assert_eq!(
        actual,
        LocalTrackingBranchMatch::Unique(LocalRefName::new("refs/heads/main".try_into()?)?),
        "the lookup follows configured upstreams, not matching branch names"
    );
    Ok(())
}

#[test]
fn tracking_lookup_returns_no_match() -> anyhow::Result<()> {
    let actual = find_local_tracking_branch("refs/remotes/origin/missing".try_into()?, &repo()?)?;
    assert_eq!(
        actual,
        LocalTrackingBranchMatch::None,
        "a missing counterpart is a match result, not a repository error"
    );
    Ok(())
}

#[test]
fn tracking_lookup_reports_ambiguous_branches() -> anyhow::Result<()> {
    let mut repo = repo()?;
    {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value("branch.other.remote", "origin")?;
        config.set_raw_value("branch.other.merge", "refs/heads/trunk")?;
    }
    let actual = find_local_tracking_branch("refs/remotes/origin/trunk".try_into()?, &repo)?;
    assert_eq!(
        actual,
        LocalTrackingBranchMatch::Ambiguous {
            first: LocalRefName::new("refs/heads/main".try_into()?)?,
            second: LocalRefName::new("refs/heads/other".try_into()?)?,
        },
        "ambiguity preserves both names for explicit-resolution diagnostics"
    );
    Ok(())
}

#[test]
fn tracking_lookup_prefers_branch_named_as_on_remote() -> anyhow::Result<()> {
    let mut repo = repo()?;
    {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value("branch.main.merge", "refs/heads/other")?;
        config.set_raw_value("branch.other.remote", "origin")?;
        config.set_raw_value("branch.other.merge", "refs/heads/other")?;
    }
    let actual = find_local_tracking_branch("refs/remotes/origin/other".try_into()?, &repo)?;
    assert_eq!(
        actual,
        LocalTrackingBranchMatch::Unique(LocalRefName::new("refs/heads/other".try_into()?)?),
        "of the branches tracking origin/other, local other is its counterpart, \
         even though main is found first"
    );
    Ok(())
}

#[test]
fn local_ref_name_rejects_remote_branch() -> anyhow::Result<()> {
    assert!(
        LocalRefName::new("refs/remotes/origin/main".try_into()?).is_err(),
        "the type must reject remote-tracking refs"
    );
    Ok(())
}

#[test]
fn local_ref_name_rejects_tag() -> anyhow::Result<()> {
    assert!(
        LocalRefName::new("refs/tags/main".try_into()?).is_err(),
        "the type must reject tags"
    );
    Ok(())
}
