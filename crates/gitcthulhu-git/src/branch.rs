//! Branches and checkout (WP-B6).
//!
//! Reads (does a branch exist, which branch tracks what) run in-process with gix; every mutation is `git switch` /
//! `git branch`, so hooks (post-checkout), progress and configuration behave natively.

use bstr::{BStr, ByteSlice};
use gix::{ObjectId, refs::FullName};

use crate::{
    CheckoutOptions, GitError, OpClass, OpContext, OpOutcome, RefreshSet, Repo, RepoExclusive,
    Result, StartPoint,
    cli::{self, CommandKind, GitCommand},
    internal::{
        errors::gix_error,
        names,
        outcome::{no_op, single},
    },
};

const HEADS: &str = "refs/heads/";
const REMOTES: &str = "refs/remotes/";

/// What a checkout can change.
const CHECKOUT_REFRESH: RefreshSet = RefreshSet::STATUS
    .union(RefreshSet::LOG)
    .union(RefreshSet::BRANCHES)
    .union(RefreshSet::REPO_STATE)
    .union(RefreshSet::SUBMODULES)
    .union(RefreshSet::WORKTREES);

/// Switch to a local branch.
///
/// Switching to the branch that is already checked out is a no-op. Errors classified by the executor:
/// `WouldOverwrite`, `BranchCheckedOutElsewhere`, `OperationInProgress`; `NotFound` when the branch does not exist.
pub fn checkout_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
    opts: CheckoutOptions,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    require_worktree(repo)?;
    let short = short_branch_name(branch)?;
    let gix_repo = repo.local();
    if gix_repo.head_name().map_err(gix_error)?.as_ref() == Some(branch) {
        return Ok(no_op(()));
    }
    if !reference_exists(&gix_repo, branch.as_bstr())? {
        return Err(GitError::NotFound {
            what: "branch",
            name: short,
        });
    }
    let cmd = switch_command(repo, opts).arg(short);
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Checkout)?;
    Ok(single((), CHECKOUT_REFRESH, out))
}

/// Remote-tracking ref -> local tracking branch. If a local branch already tracks it, check that one out.
///
/// `local_name` defaults to the branch part of the remote ref (`refs/remotes/origin/feature/x` -> `feature/x`).
/// A local branch of that name that tracks something else yields `AlreadyExists { what: "branch" }`; a remote ref
/// that does not exist (any more) yields `NotFound { what: "remote branch" }`. Returns the local branch that is now
/// checked out.
pub fn checkout_remote_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    remote_ref: &FullName,
    local_name: Option<&str>,
    opts: CheckoutOptions,
) -> Result<OpOutcome<FullName>> {
    ctx.cancel.check()?;
    require_worktree(repo)?;
    // The tracking configuration may have been changed by another Git process.
    repo.reload()?;
    let remote_ref_text = remote_ref
        .as_bstr()
        .to_str()
        .map_err(|_| invalid_branch_name(remote_ref.as_bstr()))?
        .to_owned();
    let (_remote, branch_part) = split_remote_ref(repo, &remote_ref_text)?;
    if !reference_exists(&repo.local(), remote_ref.as_bstr())? {
        return Err(GitError::NotFound {
            what: "remote branch",
            name: remote_ref_text,
        });
    }

    let preferred_name = local_name.unwrap_or(&branch_part);
    let candidates = local_branches_tracking(repo, remote_ref)?;
    if let Some(first) = candidates.first() {
        let chosen = candidates
            .iter()
            .find(|c| {
                c.as_bstr()
                    .strip_prefix(HEADS.as_bytes())
                    .is_some_and(|short| short == preferred_name.as_bytes())
            })
            .unwrap_or(first)
            .clone();
        let result = checkout_branch(repo, perm, ctx, &chosen, opts)?;
        return Ok(OpOutcome {
            value: chosen,
            refresh: result.refresh,
            log: result.log,
            warnings: result.warnings,
        });
    }

    let local_full = validate_branch_name(repo, preferred_name)?;
    let cmd = switch_command(repo, opts)
        .arg("-c")
        .arg(preferred_name)
        .arg("--track")
        .arg(remote_ref_text);
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Checkout)?;
    repo.reload()?;
    Ok(single(
        local_full,
        CHECKOUT_REFRESH | RefreshSet::CONFIG,
        out,
    ))
}

/// Detach HEAD at a commit.
pub fn checkout_detached(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    commit: ObjectId,
    opts: CheckoutOptions,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    require_worktree(repo)?;
    let cmd = switch_command(repo, opts)
        .arg("--detach")
        .arg(commit.to_hex().to_string());
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Checkout)?;
    Ok(single((), CHECKOUT_REFRESH, out))
}

/// Create a branch, optionally checking it out.
///
/// The name is validated first (`InvalidName`, `AlreadyExists`) without spawning Git. A remote-tracking start point
/// gets its upstream like `git switch -c x origin/x` (Git decides, honouring `branch.autoSetupMerge`); a commit or
/// `HEAD` never does.
pub fn create_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
    start: &StartPoint,
    checkout: bool,
) -> Result<OpOutcome<FullName>> {
    let _ = perm;
    ctx.cancel.check()?;
    let full = validate_branch_name(repo, name)?;
    let start_arg = match start {
        StartPoint::Head => "HEAD".to_owned(),
        StartPoint::Commit(id) => id.to_hex().to_string(),
        StartPoint::Ref(full_ref) => full_ref.as_bstr().to_str_lossy().into_owned(),
    };
    let (cmd, refresh) = if checkout {
        require_worktree(repo)?;
        (
            GitCommand::new(repo, CommandKind::Mutation)
                .with_hooks()
                .with_progress()
                .args(["switch", "--progress", "-c", name])
                .arg(start_arg),
            CHECKOUT_REFRESH | RefreshSet::CONFIG,
        )
    } else {
        (
            GitCommand::new(repo, CommandKind::Mutation)
                .with_hooks()
                .args(["branch", name])
                .arg(start_arg),
            RefreshSet::BRANCHES | RefreshSet::LOG | RefreshSet::CONFIG,
        )
    };
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Branch)?;
    repo.reload()?;
    Ok(single(full, refresh, out))
}

/// Rename a branch.
///
/// `git branch -m` keeps the reflog and moves the `branch.<name>.*` configuration with it.
pub fn rename_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
    new_name: &str,
) -> Result<OpOutcome<FullName>> {
    let _ = perm;
    ctx.cancel.check()?;
    let old_short = short_branch_name(branch)?;
    // A case-only rename finds "itself" on a case-insensitive file system: let Git decide there.
    let new_full = if old_short.eq_ignore_ascii_case(new_name) && old_short != new_name {
        names::branch_ref(new_name)?
    } else {
        validate_branch_name(repo, new_name)?
    };
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .with_hooks()
        .args(["branch", "-m"])
        .arg(old_short)
        .arg(new_name);
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Branch)?;
    repo.reload()?;
    Ok(single(
        new_full,
        RefreshSet::BRANCHES | RefreshSet::LOG | RefreshSet::WORKTREES | RefreshSet::CONFIG,
        out,
    ))
}

/// Delete a branch.
///
/// Safe deletion (`-d`) unless `force`: an unmerged branch yields `NotFullyMerged`, so that the caller can ask and
/// call again with `force`. The current branch, or one checked out in another worktree, yields
/// `BranchCheckedOutElsewhere`.
pub fn delete_branch(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
    force: bool,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let short = short_branch_name(branch)?;
    let gix_repo = repo.local();
    if gix_repo.head_name().map_err(gix_error)?.as_ref() == Some(branch) {
        return Err(GitError::BranchCheckedOutElsewhere {
            branch: short,
            worktree: repo.workdir().unwrap_or_else(|| repo.git_dir()).to_owned(),
        });
    }
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .with_hooks()
        .args(["branch", if force { "-D" } else { "-d" }])
        .arg(short);
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Branch)?;
    repo.reload()?;
    Ok(single(
        (),
        RefreshSet::BRANCHES | RefreshSet::LOG | RefreshSet::CONFIG,
        out,
    ))
}

/// Set (or with `None` unset) the upstream of a branch.
///
/// `upstream` should be a remote-tracking ref (a local branch is allowed by Git as well). Unsetting a branch that has
/// no upstream runs no command and succeeds with an empty refresh.
pub fn set_upstream(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
    upstream: Option<&FullName>,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let short = short_branch_name(branch)?;
    let cmd = match upstream {
        Some(upstream) => GitCommand::new(repo, CommandKind::Mutation)
            .arg("branch")
            .arg(format!(
                "--set-upstream-to={}",
                upstream.as_bstr().to_str_lossy()
            ))
            .arg(short),
        None => {
            repo.reload()?;
            let has_upstream = repo
                .local()
                .config_snapshot()
                .string(&format!("branch.{short}.merge"))
                .is_some();
            if !has_upstream {
                return Ok(no_op(()));
            }
            GitCommand::new(repo, CommandKind::Mutation)
                .args(["branch", "--unset-upstream"])
                .arg(short)
        }
    };
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Branch)?;
    repo.reload()?;
    Ok(single((), RefreshSet::BRANCHES | RefreshSet::CONFIG, out))
}

/// Pure check of a new branch name (`internal::names`); also rejects an existing branch.
///
/// Errors: `InvalidName { what: "branch" }`, `AlreadyExists { what: "branch" }` (also when an existing branch name is
/// a path prefix of `name` or lives below it, which Git cannot represent).
pub fn validate_branch_name(repo: &Repo, name: &str) -> Result<FullName> {
    names::new_branch(&repo.local(), name)
}

/// `refs/heads/<short>` -> `<short>`; anything else is `InvalidName`.
/// The short name passed to `git switch` / `git branch`. A short name starting with '-' (a ref such as
/// `refs/heads/-x` made with `git update-ref`) is refused: git would read it as an option.
fn short_branch_name(branch: &FullName) -> Result<String> {
    branch
        .as_bstr()
        .strip_prefix(HEADS.as_bytes())
        .and_then(|short| short.to_str().ok())
        .filter(|short| !short.starts_with('-'))
        .map(str::to_owned)
        .ok_or_else(|| invalid_branch_name(branch.as_bstr()))
}

fn invalid_branch_name(name: &BStr) -> GitError {
    GitError::InvalidName {
        what: "branch",
        name: name.to_str_lossy().into_owned(),
    }
}

fn require_worktree(repo: &Repo) -> Result<()> {
    if repo.workdir().is_none() {
        return Err(GitError::BareRepository);
    }
    Ok(())
}

fn reference_exists(repo: &gix::Repository, full_name: &BStr) -> Result<bool> {
    Ok(repo
        .try_find_reference(full_name)
        .map_err(gix_error)?
        .is_some())
}

/// `git switch --progress [-m | --discard-changes]`; `--discard-changes` wins over `-m` (they exclude each other).
fn switch_command(repo: &Repo, opts: CheckoutOptions) -> GitCommand {
    let mut cmd = GitCommand::new(repo, CommandKind::Mutation)
        .with_hooks()
        .with_progress()
        .args(["switch", "--progress"]);
    if opts.force {
        cmd = cmd.arg("--discard-changes");
    } else if opts.carry_changes {
        cmd = cmd.arg("-m");
    }
    cmd
}

/// Split `remote_ref` (`refs/remotes/<remote>/<branch>`) into the remote name and the branch part. Remote names may
/// contain slashes, so the longest configured remote name that is a path prefix wins.
fn split_remote_ref(repo: &Repo, remote_ref: &str) -> Result<(String, String)> {
    let invalid = || GitError::InvalidName {
        what: "remote branch",
        name: remote_ref.to_owned(),
    };
    let rest = remote_ref.strip_prefix(REMOTES).ok_or_else(invalid)?;
    let gix_repo = repo.local();
    let remote = gix_repo
        .remote_names()
        .iter()
        .filter_map(|name| name.to_str().ok())
        .filter(|name| {
            rest.strip_prefix(*name)
                .is_some_and(|tail| tail.starts_with('/'))
        })
        .max_by_key(|name| name.len())
        .map(str::to_owned)
        .or_else(|| rest.split_once('/').map(|(remote, _)| remote.to_owned()))
        .ok_or_else(invalid)?;
    let branch = rest[remote.len()..].trim_start_matches('/').to_owned();
    if branch.is_empty() {
        return Err(invalid());
    }
    Ok((remote, branch))
}

/// The local branches whose fetch tracking ref is `remote_ref`, sorted by name.
fn local_branches_tracking(repo: &Repo, remote_ref: &FullName) -> Result<Vec<FullName>> {
    let gix_repo = repo.local();
    let mut found = Vec::new();
    let platform = gix_repo.references().map_err(gix_error)?;
    for reference in platform.local_branches().map_err(gix_error)? {
        let reference = reference.map_err(gix_error)?;
        let name = reference.name().to_owned();
        let tracking =
            gix_repo.branch_remote_tracking_ref_name(name.as_ref(), gix::remote::Direction::Fetch);
        if let Some(Ok(tracking)) = tracking
            && tracking.as_bstr() == remote_ref.as_bstr()
        {
            found.push(name);
        }
    }
    found.sort_by(|a, b| a.as_bstr().cmp(b.as_bstr()));
    Ok(found)
}
