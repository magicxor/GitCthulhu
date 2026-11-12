//! Tags (WP-B6).

use bstr::ByteSlice;
use gix::{ObjectId, refs::FullName};

use crate::{
    GitError, OpClass, OpContext, OpOutcome, RefreshSet, Repo, RepoExclusive, Result, SignMode,
    TagOptions,
    branch::gix_error,
    cli::{self, CommandKind, GitCommand},
    commit::outcome,
};

const TAGS: &str = "refs/tags/";

/// Create a lightweight or annotated tag.
///
/// A tag with a message (possibly empty) or with `SignMode::Sign` is annotated, anything else lightweight.
/// `SignMode::FromConfig` leaves signing of annotated tags to Git (`tag.gpgSign`); lightweight tags are never signed
/// (`--no-sign`, so `tag.gpgSign` cannot turn them into annotated tags). An existing tag is an
/// `AlreadyExists` error unless `opts.force` is set.
pub fn create_tag(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
    target: ObjectId,
    opts: &TagOptions,
) -> Result<OpOutcome<FullName>> {
    let _ = perm;
    ctx.cancel.check()?;
    let full = if opts.force {
        tag_ref_name(name)?
    } else {
        validate_tag_name(repo, name)?
    };
    let annotated = opts.message.is_some() || opts.sign == SignMode::Sign;
    let mut cmd = GitCommand::new(repo, CommandKind::Mutation).arg("tag");
    if annotated {
        cmd = cmd.arg("-a");
        match opts.sign {
            SignMode::FromConfig => {}
            SignMode::Sign => cmd = cmd.arg("-s"),
            SignMode::DontSign => cmd = cmd.arg("--no-sign"),
        }
    } else {
        // With `tag.gpgSign=true` a plain `git tag <name> <sha>` turns into a signed annotated tag, opens the (no-op)
        // editor and dies with "no tag message?". A lightweight tag stays lightweight.
        cmd = cmd.arg("--no-sign");
    }
    if opts.force {
        cmd = cmd.arg("-f");
    }
    if annotated {
        let message = opts.message.clone().unwrap_or_default();
        cmd = cmd.arg("-F").arg("-").stdin(message.into_bytes());
    }
    cmd = cmd.arg(name).arg(target.to_hex().to_string());
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Tag)?;
    Ok(outcome(full, RefreshSet::TAGS | RefreshSet::LOG, out))
}

/// Delete a tag (local only; remote deletion is `network::delete_remote_tag`).
///
/// `NotFound { what: "tag" }` if there is no such tag.
pub fn delete_tag(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let full = tag_ref_name(name)?;
    if !tag_exists(repo, &full)? {
        return Err(GitError::NotFound {
            what: "tag",
            name: name.to_owned(),
        });
    }
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(["tag", "-d"])
        .arg(name);
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Tag)?;
    Ok(outcome((), RefreshSet::TAGS | RefreshSet::LOG, out))
}

/// Pure check with `gix::validate::reference::name` + "refs/tags/" prefix; also rejects an existing tag.
///
/// Errors: `InvalidName { what: "tag" }`, `AlreadyExists { what: "tag" }`.
pub fn validate_tag_name(repo: &Repo, name: &str) -> Result<FullName> {
    let full = tag_ref_name(name)?;
    if tag_exists(repo, &full)? {
        return Err(GitError::AlreadyExists {
            what: "tag",
            name: name.to_owned(),
        });
    }
    Ok(full)
}

/// `refs/tags/<name>` after the syntax checks.
fn tag_ref_name(name: &str) -> Result<FullName> {
    let invalid = || GitError::InvalidName {
        what: "tag",
        name: name.to_owned(),
    };
    if name.is_empty() || name.starts_with('-') || name.trim() != name {
        return Err(invalid());
    }
    let full = format!("{TAGS}{name}");
    gix::validate::reference::name(full.as_bytes().as_bstr()).map_err(|_| invalid())?;
    FullName::try_from(full).map_err(|_| invalid())
}

fn tag_exists(repo: &Repo, full: &FullName) -> Result<bool> {
    Ok(repo
        .local()
        .try_find_reference(full.as_bstr())
        .map_err(gix_error)?
        .is_some())
}
