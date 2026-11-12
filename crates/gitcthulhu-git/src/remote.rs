//! Remote management and forge inputs (WP-B5).

use bstr::ByteSlice as _;
use gix::remote::Direction;

use crate::{
    ForgeRemotes, GitError, OpClass, OpContext, OpOutcome, RefreshSet, RemoteRef, Repo,
    RepoExclusive, Result,
    cli::{CommandKind, GitCommand},
    config::{OpAccumulator, other, run_one},
};

const REMOTE_MUTATED: RefreshSet = RefreshSet::REMOTES.union(RefreshSet::CONFIG);
const REMOTE_RENAMED: RefreshSet = REMOTE_MUTATED
    .union(RefreshSet::BRANCHES)
    .union(RefreshSet::LOG);

fn check_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && !name.starts_with('-')
        && !name.chars().any(|c| c.is_whitespace() || c.is_control())
        && gix::validate::reference::name_partial(name.as_bytes().as_bstr()).is_ok();
    if valid {
        Ok(())
    } else {
        Err(GitError::InvalidName {
            what: "remote",
            name: name.to_owned(),
        })
    }
}

fn check_url(url: &str) -> Result<()> {
    if url.trim().is_empty() || url.starts_with('-') {
        return Err(GitError::InvalidName {
            what: "remote URL",
            name: url.to_owned(),
        });
    }
    Ok(())
}

fn remote_exists(repo: &Repo, name: &str) -> bool {
    repo.local()
        .remote_names()
        .iter()
        .any(|existing| existing.as_bytes() == name.as_bytes())
}

fn require_remote(repo: &Repo, name: &str) -> Result<()> {
    check_name(name)?;
    if remote_exists(repo, name) {
        Ok(())
    } else {
        Err(GitError::NotFound {
            what: "remote",
            name: name.to_owned(),
        })
    }
}

fn git(repo: &Repo) -> GitCommand {
    GitCommand::new(repo, CommandKind::Mutation).arg("remote")
}

/// `git remote add`.
pub fn add_remote(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
    fetch_url: &str,
    push_url: Option<&str>,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    check_name(name)?;
    check_url(fetch_url)?;
    if let Some(push) = push_url {
        check_url(push)?;
    }
    if remote_exists(repo, name) {
        return Err(GitError::AlreadyExists {
            what: "remote",
            name: name.to_owned(),
        });
    }
    let mut acc = run_one(
        repo,
        git(repo).args(["add", "--", name, fetch_url]),
        ctx,
        OpClass::Remote,
    )?;
    if let Some(push) = push_url.filter(|push| *push != fetch_url) {
        acc = push_all(
            acc,
            repo,
            git(repo).args(["set-url", "--push", "--", name, push]),
            ctx,
        )?;
    }
    repo.reload()?;
    Ok(acc.finish((), REMOTE_MUTATED))
}

fn push_all(
    mut acc: OpAccumulator,
    repo: &Repo,
    cmd: GitCommand,
    ctx: &OpContext,
) -> Result<OpAccumulator> {
    acc.push(crate::cli::run_checked(repo, cmd, ctx, OpClass::Remote)?);
    Ok(acc)
}

/// Whether the repository-local configuration has `remote.<name>.pushurl` (read from disk, not the snapshot).
fn has_local_push_url(repo: &Repo, name: &str) -> Result<bool> {
    let file =
        but_core::git_config::open_repo_local_config_for_reading(&repo.local()).map_err(other)?;
    Ok(file
        .raw_values_by("remote", Some(name.as_bytes().as_bstr()), "pushurl")
        .is_ok())
}

/// Change the URLs of a remote.
pub fn set_remote_urls(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
    fetch_url: &str,
    push_url: Option<&str>,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    check_url(fetch_url)?;
    if let Some(push) = push_url {
        check_url(push)?;
    }
    require_remote(repo, name)?;
    let mut acc = run_one(
        repo,
        git(repo).args(["set-url", "--", name, fetch_url]),
        ctx,
        OpClass::Remote,
    )?;
    match push_url {
        Some(push) => {
            acc = push_all(
                acc,
                repo,
                git(repo).args(["set-url", "--push", "--", name, push]),
                ctx,
            )?;
        }
        None if has_local_push_url(repo, name)? => {
            let cmd = GitCommand::new(repo, CommandKind::Mutation).args([
                "config",
                "--unset-all",
                &format!("remote.{name}.pushurl"),
            ]);
            acc = push_all(acc, repo, cmd, ctx)?;
        }
        None => {}
    }
    repo.reload()?;
    Ok(acc.finish((), REMOTE_MUTATED))
}

/// `git remote rename`.
pub fn rename_remote(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    old: &str,
    new: &str,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    check_name(new)?;
    require_remote(repo, old)?;
    if remote_exists(repo, new) {
        return Err(GitError::AlreadyExists {
            what: "remote",
            name: new.to_owned(),
        });
    }
    let acc = run_one(
        repo,
        git(repo).args(["rename", "--", old, new]),
        ctx,
        OpClass::Remote,
    )?;
    repo.reload()?;
    Ok(acc.finish((), REMOTE_RENAMED))
}

/// `git remote remove`.
pub fn remove_remote(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    require_remote(repo, name)?;
    let acc = run_one(
        repo,
        git(repo).args(["remove", "--", name]),
        ctx,
        OpClass::Remote,
    )?;
    repo.reload()?;
    Ok(acc.finish((), REMOTE_RENAMED))
}

fn remote_ref(gix_repo: &gix::Repository, name: &str) -> Option<RemoteRef> {
    let remote = gix_repo.find_remote(name).ok()?;
    let url = remote.url(Direction::Fetch)?;
    Some(RemoteRef {
        name: name.to_owned(),
        url: url.to_bstring().to_str_lossy().into_owned(),
    })
}

/// The remotes the forge facade works with (rule of 15 §6.1). Pure read.
pub fn forge_remotes(repo: &Repo) -> Result<ForgeRemotes> {
    let gix_repo = repo.local();
    let snapshot = gix_repo.config_snapshot();
    let names: Vec<String> = gix_repo
        .remote_names()
        .iter()
        .map(|name| name.to_str_lossy().into_owned())
        .collect();
    let exists = |name: &str| names.iter().any(|n| n == name);
    let string = |key: String| {
        snapshot
            .string(key.as_str())
            .map(|v| v.to_str_lossy().into_owned())
            .filter(|v| exists(v))
    };
    let branch = gix_repo
        .head_name()
        .ok()
        .flatten()
        .map(|name| name.shorten().to_string());
    let upstream = branch
        .as_ref()
        .and_then(|b| string(format!("branch.{b}.remote")));
    let base_name = upstream
        .or_else(|| exists("upstream").then(|| "upstream".to_owned()))
        .or_else(|| exists("origin").then(|| "origin".to_owned()))
        .or_else(|| (names.len() == 1).then(|| names[0].clone()));
    let push_name = branch
        .as_ref()
        .and_then(|b| string(format!("branch.{b}.pushRemote")))
        .or_else(|| string("remote.pushDefault".to_owned()))
        .or_else(|| base_name.clone());
    Ok(ForgeRemotes {
        base: base_name.and_then(|n| remote_ref(&gix_repo, &n)),
        push: push_name.and_then(|n| remote_ref(&gix_repo, &n)),
    })
}

/// The branch `refs/remotes/<remote>/HEAD` points to, if that symbolic ref exists.
pub(crate) fn symref_head_branch(gix_repo: &gix::Repository, remote: &str) -> Option<String> {
    let prefix = format!("refs/remotes/{remote}/");
    let reference = gix_repo
        .try_find_reference(format!("{prefix}HEAD").as_str())
        .ok()
        .flatten()?;
    match reference.target() {
        gix::refs::TargetRef::Symbolic(target) => target
            .as_bstr()
            .strip_prefix(prefix.as_bytes())
            .map(|rest| rest.to_str_lossy().into_owned()),
        gix::refs::TargetRef::Object(_) => None,
    }
}

/// Short name of the remote's default branch: refs/remotes/<r>/HEAD target, else "main"/"master" if present.
pub fn default_branch(repo: &Repo, remote: &str) -> Result<Option<String>> {
    let gix_repo = repo.local();
    if let Some(branch) = symref_head_branch(&gix_repo, remote) {
        return Ok(Some(branch));
    }
    for candidate in ["main", "master"] {
        let full = format!("refs/remotes/{remote}/{candidate}");
        if gix_repo
            .try_find_reference(full.as_str())
            .map_err(other)?
            .is_some()
        {
            return Ok(Some(candidate.to_owned()));
        }
    }
    Ok(None)
}
