use bstr::ByteSlice;
use but_error::Code;
use gix::prelude::ObjectIdExt;

use crate::{Commit, CommitOwned, RepositoryExt};

/// Determines how to sign the commit.
#[derive(Default, PartialEq, Copy, Clone, Debug)]
pub enum SignCommit {
    /// Unconditionally sign the commit. Note that this places responsibility on the caller to
    /// ensure that commit signing is configured, or at least handling that it was not.
    Yes,
    /// Do not sign the commit.
    No,
    /// Sign the commit only if `commit.gpgSign=true`, just like `git commit` would.
    #[default]
    IfSignCommitsEnabled,
}

/// Write `commit` into `repo`, removing any existing commit signature first, optionally creating a
/// new one based on repository configuration, and optionally updating `update_ref` to the new ID.
///
/// Apply any desired message or header mutations before calling this helper.
/// Signing failures are reported with [`Code::CommitSigningFailed`].
pub fn create(
    repo: &gix::Repository,
    mut commit: gix::objs::Commit,
    update_ref: Option<&gix::refs::FullNameRef>,
    sign_commit: SignCommit,
) -> anyhow::Result<gix::ObjectId> {
    let signature_field = gix::objs::commit::signature_field_name(commit.tree.kind());
    if let Some(pos) = commit.extra_headers().find_pos(signature_field) {
        commit.extra_headers.remove(pos);
    }

    let should_sign = match sign_commit {
        SignCommit::Yes => true,
        SignCommit::No => false,
        SignCommit::IfSignCommitsEnabled => repo.git_settings()?.sign_commits.unwrap_or(false),
    };
    let commit = if should_sign {
        let signed = repo
            .commit_signing_options()
            .map_err(anyhow::Error::from)
            .and_then(|opts| commit.sign(opts).map_err(anyhow::Error::from));
        match signed {
            Ok(commit) => commit,
            Err(err) => {
                tracing::warn!("Commit signing failed with sign_commit={sign_commit:?}");
                return Err(err
                    .context("Failed to sign commit")
                    .context(Code::CommitSigningFailed));
            }
        }
    } else {
        commit
    };

    let oid = repo.write_object(&commit)?.detach();
    if let Some(update_ref) = update_ref {
        repo.reference(
            update_ref,
            oid,
            gix::refs::transaction::PreviousValue::Any,
            commit.message.as_bstr(),
        )?;
    }
    Ok(oid)
}

/// Lifecycle
impl<'repo> Commit<'repo> {
    /// Decode the object at `commit_id` and keep its data for later query.
    pub fn from_id(commit_id: gix::Id<'repo>) -> anyhow::Result<Self> {
        commit_id.object()?.try_into_commit()?.try_into()
    }
}

impl<'repo> TryFrom<gix::Commit<'repo>> for Commit<'repo> {
    type Error = anyhow::Error;

    fn try_from(value: gix::Commit<'repo>) -> Result<Self, Self::Error> {
        let id = value.id();
        let commit = value.decode()?.try_into()?;
        Ok(Commit { id, inner: commit })
    }
}

impl From<Commit<'_>> for CommitOwned {
    fn from(Commit { id, inner }: Commit<'_>) -> Self {
        CommitOwned {
            id: id.detach(),
            inner,
        }
    }
}

impl std::ops::Deref for Commit<'_> {
    type Target = gix::objs::Commit;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for Commit<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl std::ops::Deref for CommitOwned {
    type Target = gix::objs::Commit;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for CommitOwned {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl CommitOwned {
    /// Attach `repo` to this instance to be able to do way more with it.
    pub fn attach(self, repo: &gix::Repository) -> Commit<'_> {
        let CommitOwned { id, inner } = self;
        Commit {
            id: id.attach(repo),
            inner,
        }
    }
}

/// Access
impl Commit<'_> {
    /// Remove the `repo` reference to become a fully owned instance.
    pub fn detach(self) -> CommitOwned {
        self.into()
    }
}
