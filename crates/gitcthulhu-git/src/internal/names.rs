//! Branch, tag and remote name validation (WP-B0d), shared by `branch.rs`, `tag.rs`, `remote.rs` and `worktree.rs`.
//!
//! The pure checks mirror what `git branch`, `git tag` and `git remote add` accept, using `gix::validate`:
//! - branch: not starting with `-`, `refs/heads/<name>` a valid ref name and not `refs/heads/HEAD`;
//! - tag: not starting with `-`, not `HEAD`, `refs/tags/<name>` a valid ref name;
//! - remote: not starting with `-`, `refs/remotes/<name>/test` a valid ref name (Git's `valid_remote_name`).
//!   Git itself accepts `-foo` (only via `git remote add -- -foo`), but such a name is read as an option by every
//!   later `git fetch`/`git push`/`git remote` call that passes it positionally, so it is refused like for branches.
//!
//! The `new_*` variants also reject names that already exist, before any `git` process is spawned.

use bstr::{BStr, BString, ByteSlice};
use gix::refs::FullName;

use crate::{GitError, Result};

fn invalid(what: &'static str, name: &str) -> GitError {
    GitError::InvalidName {
        what,
        name: name.to_owned(),
    }
}

fn full_name(what: &'static str, name: &str, full: BString) -> Result<FullName> {
    FullName::try_from(full).map_err(|_| invalid(what, name))
}

/// `refs/heads/<name>` if `name` is a valid branch name (`InvalidName { what: "branch" }` otherwise).
pub(crate) fn branch_ref(name: &str) -> Result<FullName> {
    const WHAT: &str = "branch";
    if name.starts_with('-') {
        return Err(invalid(WHAT, name));
    }
    let full: BString = format!("refs/heads/{name}").into();
    gix::validate::reference::branch_name(full.as_bstr()).map_err(|_| invalid(WHAT, name))?;
    full_name(WHAT, name, full)
}

/// `refs/tags/<name>` if `name` is a valid tag name (`InvalidName { what: "tag" }` otherwise).
pub(crate) fn tag_ref(name: &str) -> Result<FullName> {
    const WHAT: &str = "tag";
    if name.starts_with('-') || name == "HEAD" {
        return Err(invalid(WHAT, name));
    }
    let full: BString = format!("refs/tags/{name}").into();
    gix::validate::reference::name(full.as_bstr()).map_err(|_| invalid(WHAT, name))?;
    full_name(WHAT, name, full)
}

/// `Ok` if `name` is a valid remote name (`InvalidName { what: "remote" }` otherwise).
pub(crate) fn remote_name(name: &str) -> Result<()> {
    if name.starts_with('-') {
        return Err(invalid("remote", name));
    }
    let probe: BString = format!("refs/remotes/{name}/test").into();
    gix::validate::reference::name(probe.as_bstr())
        .map(|_| ())
        .map_err(|_| invalid("remote", name))
}

/// Like [`branch_ref`], and the branch must not exist yet: `AlreadyExists { what: "branch" }` names the existing
/// branch, which is also reported for directory/file conflicts (`a` exists and `a/b` is requested, or vice versa),
/// which Git would refuse with "cannot lock ref".
pub(crate) fn new_branch(repo: &gix::Repository, name: &str) -> Result<FullName> {
    let full = branch_ref(name)?;
    ensure_absent(repo, &full, "branch", "refs/heads/")?;
    Ok(full)
}

/// Like [`tag_ref`], and the tag must not exist yet (`AlreadyExists { what: "tag" }`, also for directory/file
/// conflicts).
pub(crate) fn new_tag(repo: &gix::Repository, name: &str) -> Result<FullName> {
    let full = tag_ref(name)?;
    ensure_absent(repo, &full, "tag", "refs/tags/")?;
    Ok(full)
}

/// Like [`remote_name`], and no remote of that name may be configured (`AlreadyExists { what: "remote" }`).
pub(crate) fn new_remote(repo: &gix::Repository, name: &str) -> Result<()> {
    remote_name(name)?;
    let exists = repo.remote_names().iter().any(|existing| {
        let existing: &BStr = existing.as_ref();
        existing == name.as_bytes().as_bstr()
    });
    if exists {
        return Err(GitError::AlreadyExists {
            what: "remote",
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Fail with `AlreadyExists` if `full` exists, if one of its parent "directories" is a ref, or if refs exist below it.
fn ensure_absent(
    repo: &gix::Repository,
    full: &FullName,
    what: &'static str,
    namespace: &str,
) -> Result<()> {
    let already = |existing: &BStr| GitError::AlreadyExists {
        what,
        name: existing
            .strip_prefix(namespace.as_bytes())
            .unwrap_or(existing)
            .to_str_lossy()
            .into_owned(),
    };
    let bytes = full.as_bstr();
    // The ref itself and every parent prefix (`refs/heads/a` for `refs/heads/a/b`).
    let first_component = namespace.len();
    let prefixes = bytes[first_component..]
        .find_iter("/")
        .map(|pos| first_component + pos)
        .chain(std::iter::once(bytes.len()));
    for end in prefixes {
        let candidate = bytes[..end].as_bstr();
        if repo.try_find_reference(candidate)?.is_some() {
            return Err(already(candidate));
        }
    }
    // Refs below it (`refs/heads/a/b` blocks `refs/heads/a`).
    let mut below: BString = bytes.to_owned();
    below.push(b'/');
    let platform = repo.references()?;
    let mut iter = platform.prefixed(below.as_bstr())?;
    if let Some(existing) = iter.next() {
        let existing = existing.map_err(|err| GitError::Other(anyhow::anyhow!(err)))?;
        return Err(already(existing.name().as_bstr()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GitErrorKind, internal::content::fixture};

    fn accepted(check: fn(&str) -> Result<()>, names: &[&str]) -> Vec<String> {
        names
            .iter()
            .filter(|n| check(n).is_ok())
            .map(|n| n.to_string())
            .collect()
    }

    /// Verified against Git 2.56 (`git branch`, `git tag`, `git remote add -- <name>` with each name).
    const NAMES: &[&str] = &[
        "@",
        "HEAD",
        "-foo",
        "-",
        "a..b",
        "a.lock",
        "a/",
        "/a",
        "a//b",
        ".a",
        "a/.b",
        "a b",
        "a~b",
        "a^b",
        "a:b",
        "a?b",
        "a*b",
        "a[b",
        "a\\b",
        "a@{b",
        "a.",
        "ä",
        "feature/x",
        "a@b",
        "@{-1}",
        "refs/heads/x",
        "x.lock/y",
        "",
        "a.b",
        "HEAD/x",
        "x/HEAD",
        "tab\there",
        "ctrl\u{1}",
    ];

    #[test]
    fn names_branch_validation_matches_git() {
        let ok = accepted(|n| branch_ref(n).map(|_| ()), NAMES);
        assert_eq!(
            ok,
            [
                "@",
                "ä",
                "feature/x",
                "a@b",
                "refs/heads/x",
                "a.b",
                "HEAD/x",
                "x/HEAD"
            ],
            "exactly the names `git branch` accepts"
        );
        assert_eq!(
            branch_ref("feature/x").expect("valid").as_bstr(),
            "refs/heads/feature/x",
            "the full name is under refs/heads"
        );
        let err = branch_ref("a..b").expect_err("invalid");
        assert!(
            matches!(&err, GitError::InvalidName { what: "branch", name } if name == "a..b"),
            "the error names the kind and the input: {err:?}"
        );
    }

    #[test]
    fn names_tag_validation_matches_git() {
        let ok = accepted(|n| tag_ref(n).map(|_| ()), NAMES);
        assert_eq!(
            ok,
            [
                "@",
                "ä",
                "feature/x",
                "a@b",
                "refs/heads/x",
                "a.b",
                "HEAD/x",
                "x/HEAD"
            ],
            "exactly the names `git tag` accepts"
        );
        assert_eq!(
            tag_ref("v1.0").expect("valid").as_bstr(),
            "refs/tags/v1.0",
            "under refs/tags"
        );
    }

    #[test]
    fn names_remote_validation_matches_git() {
        let ok = accepted(remote_name, NAMES);
        assert_eq!(
            ok,
            [
                "@",
                "HEAD",
                "a.",
                "ä",
                "feature/x",
                "a@b",
                "refs/heads/x",
                "a.b",
                "HEAD/x",
                "x/HEAD"
            ],
            "the names `git remote add` accepts, minus those starting with '-'"
        );
    }

    #[test]
    fn names_existing_refs_and_remotes_are_rejected() {
        let repo = fixture::repo();
        repo.git(&["commit", "--allow-empty", "-m", "init"]);
        repo.git(&["branch", "a"]);
        repo.git(&["branch", "dir/leaf"]);
        repo.git(&["tag", "v1"]);
        repo.git(&["remote", "add", "origin", "https://example.com/r.git"]);
        let local = repo.open();

        let kind_and_name = |res: Result<FullName>| match res {
            Err(GitError::AlreadyExists { what, name }) => Some((what, name)),
            Err(other) => panic!("unexpected error {other:?}"),
            Ok(_) => None,
        };
        assert_eq!(
            kind_and_name(new_branch(&local, "a")),
            Some(("branch", "a".into())),
            "branch exists"
        );
        assert_eq!(
            kind_and_name(new_branch(&local, "a/b")),
            Some(("branch", "a".into())),
            "'a' blocks 'a/b' (directory/file conflict)"
        );
        assert_eq!(
            kind_and_name(new_branch(&local, "dir")),
            Some(("branch", "dir/leaf".into())),
            "'dir/leaf' blocks 'dir'"
        );
        assert_eq!(
            new_branch(&local, "fresh").expect("not taken").as_bstr(),
            "refs/heads/fresh",
            "a free name passes"
        );
        assert_eq!(
            kind_and_name(new_tag(&local, "v1")),
            Some(("tag", "v1".into())),
            "tag exists"
        );
        assert!(
            new_tag(&local, "a").is_ok(),
            "a branch name is free as a tag name"
        );
        assert_eq!(
            new_remote(&local, "origin")
                .map_err(|e| e.kind())
                .expect_err("exists"),
            GitErrorKind::AlreadyExists,
            "the remote is configured"
        );
        assert!(
            new_remote(&local, "upstream").is_ok(),
            "a free remote name passes"
        );
        assert_eq!(
            new_branch(&local, "bad..name")
                .map_err(|e| e.kind())
                .expect_err("invalid"),
            GitErrorKind::InvalidName,
            "validity is checked before existence"
        );
    }
}
