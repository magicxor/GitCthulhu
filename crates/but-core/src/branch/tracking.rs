//! Find local branches by the upstream they track.

use anyhow::{Context as _, Result, bail};
use bstr::BStr;
use gix::refs::FullNameRef;

/// A Git reference name thats guaranteed to always be a local branch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalRefName(gix::refs::FullName);

impl LocalRefName {
    /// Create a new `LocalRefName` from a Git reference name.
    ///
    /// Returns an error if `reference` is not a local branch.
    pub fn new(reference: gix::refs::FullName) -> Result<LocalRefName> {
        match reference
            .category()
            .with_context(|| format!("{reference} has no category"))?
        {
            gix::refs::Category::LocalBranch => Ok(LocalRefName(reference)),
            gix::refs::Category::Tag
            | gix::refs::Category::RemoteBranch
            | gix::refs::Category::Note
            | gix::refs::Category::PseudoRef
            | gix::refs::Category::MainPseudoRef
            | gix::refs::Category::MainRef
            | gix::refs::Category::LinkedPseudoRef { .. }
            | gix::refs::Category::LinkedRef { .. }
            | gix::refs::Category::Bisect
            | gix::refs::Category::Rewritten
            | gix::refs::Category::WorktreePrivate => bail!("{reference} is not a local branch"),
        }
    }

    /// Like [`gix::refs::FullName::shorten`].
    pub fn shorten(&self) -> &BStr {
        self.0.shorten()
    }

    /// Convert into a [`gix::refs::FullNameRef`].
    ///
    /// Be careful passing this to functions that require remote refs.
    ///
    /// It is also this reasion that we don't implement `AsRef<gix::refs::FullNameRef> for
    /// LocalRefName`. That makes it too easy to pass this to functions that require remote refs.
    pub fn as_local_full_name_ref(&self) -> &FullNameRef {
        self.0.as_ref()
    }
}

impl std::fmt::Display for LocalRefName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[expect(clippy::infallible_try_from)]
impl<'a> TryFrom<&'a LocalRefName> for &'a gix::refs::PartialNameRef {
    type Error = <&'a gix::refs::PartialNameRef as TryFrom<&'a gix::refs::FullName>>::Error;

    fn try_from(value: &'a LocalRefName) -> std::result::Result<Self, Self::Error> {
        TryFrom::try_from(&value.0)
    }
}

/// Local branches configured to track a particular upstream ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalTrackingBranchMatch {
    /// No local branch has the requested upstream.
    None,
    /// The only local branch with the requested upstream, or of several, the one named as the
    /// branch on the remote, like main for origin/main.
    Unique(LocalRefName),
    /// At least two local branches have the requested upstream, and none is named as the branch on
    /// the remote.
    Ambiguous {
        /// The first matching branch.
        first: LocalRefName,
        /// The second matching branch; additional matches are not collected.
        second: LocalRefName,
    },
}

/// Find local branches whose configured upstream is `remote_ref`, without checking their tips.
/// Of several, the one named as the branch on the remote is picked, as branches made from the
/// remote branch, like with `git worktree add -b <name> origin/main`, track it too.
/// Repository and upstream-configuration errors propagate independently of the match result.
pub fn find_local_tracking_branch(
    remote_ref: &FullNameRef,
    repo: &gix::Repository,
) -> Result<LocalTrackingBranchMatch> {
    let references = repo.references()?;
    let mut first = None;
    let mut second = None;
    for reference in references.local_branches()? {
        let reference = reference?;
        let local_ref = reference.name();
        let upstream = repo
            .branch_remote_tracking_ref_name(local_ref, gix::remote::Direction::Fetch)
            .transpose()?;
        if upstream.as_ref().map(AsRef::as_ref) != Some(remote_ref) {
            continue;
        }
        let ref_on_remote = repo
            .branch_remote_ref_name(local_ref, gix::remote::Direction::Fetch)
            .transpose()?;
        let named_as_on_remote = ref_on_remote.as_ref().map(AsRef::as_ref) == Some(local_ref);
        let local_ref = LocalRefName::new(local_ref.to_owned())?;
        if named_as_on_remote {
            return Ok(LocalTrackingBranchMatch::Unique(local_ref));
        }
        if first.is_none() {
            first = Some(local_ref);
        } else if second.is_none() {
            second = Some(local_ref);
        }
    }

    Ok(match (first, second) {
        (Some(first), Some(second)) => LocalTrackingBranchMatch::Ambiguous { first, second },
        (Some(name), None) => LocalTrackingBranchMatch::Unique(name),
        (None, _) => LocalTrackingBranchMatch::None,
    })
}
