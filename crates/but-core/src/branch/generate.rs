use std::collections::BTreeSet;

use anyhow::Context;
use bstr::{BString, ByteSlice, ByteVec};
use but_error::bail_precondition;

use crate::extract_remote_name_and_short_name;

/// Produce a name that at the time of return from this function didn't exist in the repository.
/// It uses `template` as starting point for its search, appending or incrementing a number until a
/// reference of that name doesn't exist anymore in `repo`.
/// `rn` is returned unchanged if the reference *already* doesn't exist.
///
/// ### Note
///
/// This function is racy and two callers could see the same name. Only one will then be able
/// to crate the branch. This is acceptable and common, i.e. the name is first presented to the user,
/// then later it's used.
pub fn find_unique_refname(
    repo: &gix::Repository,
    template: &gix::refs::FullNameRef,
) -> anyhow::Result<gix::refs::FullName> {
    find_unique_refname_excluding(repo, template, |_| false)
}

/// Like [`find_unique_refname`], but also skips every name that `is_taken` claims, for names that
/// are reserved without existing in `repo` yet.
pub fn find_unique_refname_excluding(
    repo: &gix::Repository,
    template: &gix::refs::FullNameRef,
    is_taken: impl Fn(&gix::refs::FullNameRef) -> bool,
) -> anyhow::Result<gix::refs::FullName> {
    // TODO(perf): ideally we remove that special case of auto-associating local branches with seemingly matching
    //             RTBs, and avoid this lookup and reference traversal.
    let remote_names = repo.remote_names();
    let rtb_lut: BTreeSet<_> = repo
        .references()?
        .prefixed("refs/remotes/")?
        .filter_map(Result::ok)
        .filter_map(|rn| {
            extract_remote_name_and_short_name(rn.name(), &remote_names)
                .map(|(_rn, short_name)| short_name)
        })
        .collect();

    // Check if the original name is available
    let short_name = template.shorten();
    if !rtb_lut.contains(short_name)
        && !is_taken(template)
        && repo.try_find_reference(template)?.is_none()
    {
        return Ok(template.to_owned());
    }

    // Extract base name and number if it already has a numerical suffix
    let trailing_number = short_name
        .rfind("-")
        .map(|pos| (&short_name[pos + 1..], pos))
        .and_then(|(maybe_num, pos)| {
            let num = maybe_num.to_str().ok()?.parse::<usize>().ok()?;
            Some((&short_name[..pos], num + 1))
        });
    let (base, start_num) = trailing_number.unwrap_or((short_name, 1));

    // Try incrementing numbers until we find one that doesn't exist
    let category = template
        .category()
        .with_context(|| format!("Input branch {template} could not be categorized"))?;
    let mut candidate_short = BString::default();
    for num in start_num.. {
        candidate_short.clear();
        candidate_short.push_str(base);
        candidate_short.push(b'-');
        candidate_short.push_str(num.to_string());

        if rtb_lut.contains(candidate_short.as_bstr()) {
            continue;
        }
        let candidate_full = category.to_full_name(candidate_short.as_bstr())?;

        if !is_taken(candidate_full.as_ref()) && repo.try_find_reference(&candidate_full)?.is_none()
        {
            return Ok(candidate_full);
        }
    }
    unreachable!("infinite loop should find a unique name")
}

/// Refuse `name` when another reference in `repo` keeps it from being created, as Git can't
/// hold both `a` and `a/b`: one named like a directory `name` would be in, or one in the
/// directory `name` would replace. `renamed`, the reference becoming `name`, is no obstacle.
///
/// A ref edit only notices a loose one, and with a packed one would write both.
pub fn refuse_colliding_refname(
    repo: &gix::Repository,
    name: &gix::refs::FullNameRef,
    renamed: Option<&gix::refs::FullNameRef>,
) -> anyhow::Result<()> {
    if let Some(colliding) = colliding_refname(repo, name, renamed)? {
        bail_precondition!(
            "Branch name '{}' collides with existing branch '{}'",
            name.shorten(),
            colliding.shorten()
        );
    }
    Ok(())
}

fn colliding_refname(
    repo: &gix::Repository,
    name: &gix::refs::FullNameRef,
    renamed: Option<&gix::refs::FullNameRef>,
) -> anyhow::Result<Option<gix::refs::FullName>> {
    let full_name = name.as_bstr();
    let other = |name: &gix::refs::FullNameRef| renamed != Some(name);
    // Past `refs/` and its category, as `refs/heads/`.
    for at in full_name.find_iter("/").skip(2) {
        let outer = gix::refs::FullName::try_from(full_name[..at].as_bstr())?;
        if other(outer.as_ref()) && repo.try_find_reference(outer.as_ref())?.is_some() {
            return Ok(Some(outer));
        }
    }
    let mut prefix = BString::from(full_name);
    prefix.push_byte(b'/');
    let inner = (repo.references()?)
        .prefixed(&prefix)?
        .filter_map(Result::ok)
        .map(|inner| inner.name().to_owned())
        .find(|inner| other(inner.as_ref()));
    Ok(inner)
}
