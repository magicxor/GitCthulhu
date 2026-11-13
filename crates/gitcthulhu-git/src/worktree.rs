//! Linked worktrees (WP-B5, D0.7): listing with gix, mutations through `git worktree`.

use std::path::{Path, PathBuf};

use crate::{
    GitError, HeadState, OpClass, OpContext, OpOutcome, RefreshSet, Repo, RepoExclusive, Result,
    StartPoint, WorktreeAddOptions, WorktreeInfo, WorktreeTarget,
    cli::{CommandKind, GitCommand},
    config::other,
    internal::{errors::gix_error, names, outcome::run_one},
};

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// HEAD of `repo` as the backend's own type.
pub(crate) fn head_state(repo: &gix::Repository) -> Result<HeadState> {
    let head = repo.head().map_err(gix_error)?;
    let name = head.referent_name().map(|name| name.to_owned());
    if head.is_unborn() {
        let name = name.ok_or_else(|| other("unborn HEAD without a name"))?;
        return Ok(HeadState::Unborn { name });
    }
    let target = head
        .id()
        .ok_or_else(|| other("HEAD does not point to a commit"))?
        .detach();
    Ok(match name {
        Some(name) => HeadState::Branch { name, target },
        None => HeadState::Detached { target },
    })
}

/// All worktrees: the main one (`id = None`) and the linked ones. gix: `repo.main_repo()?.worktrees()`.
pub fn list(repo: &Repo) -> Result<Vec<WorktreeInfo>> {
    let local = repo.local();
    let main = local.main_repo().map_err(gix_error)?;
    let mut out = Vec::new();
    let bare = main.workdir().is_none();
    out.push(WorktreeInfo {
        id: None,
        path: main
            .workdir()
            .map_or_else(|| main.git_dir().to_owned(), Path::to_owned),
        git_dir: main.git_dir().to_owned(),
        head: head_state(&main).ok(),
        is_current: same_path(main.git_dir(), repo.git_dir()),
        locked: None,
        prunable: false,
        bare,
    });
    let mut linked = Vec::new();
    for proxy in main.worktrees().map_err(gix_error)? {
        let id = proxy.id().map_err(gix_error)?.to_string();
        let git_dir = proxy.git_dir().to_owned();
        let base = proxy.base().ok();
        let locked = proxy.is_locked().then(|| {
            proxy
                .lock_reason()
                .map(|r| r.to_string())
                .unwrap_or_default()
        });
        let prunable = proxy.is_prunable() || base.is_none();
        let head = proxy
            .into_repo_with_possibly_inaccessible_worktree()
            .ok()
            .and_then(|r| head_state(&r).ok());
        linked.push(WorktreeInfo {
            id: Some(id),
            path: base.unwrap_or_else(|| git_dir.clone()),
            is_current: same_path(&git_dir, repo.git_dir()),
            git_dir,
            head,
            locked,
            prunable,
            bare: false,
        });
    }
    linked.sort_by(|a, b| a.id.cmp(&b.id));
    out.extend(linked);
    Ok(out)
}

fn absolute(repo: &Repo, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        repo.workdir().unwrap_or_else(|| repo.git_dir()).join(path)
    }
}

fn find_entry(repo: &Repo, path: &Path) -> Result<WorktreeInfo> {
    let path = absolute(repo, path);
    list(repo)?
        .into_iter()
        .find(|entry| same_path(&entry.path, &path))
        .ok_or_else(|| GitError::NotFound {
            what: "worktree",
            name: path.display().to_string(),
        })
}

fn check_target_dir(path: &Path) -> Result<()> {
    let already_exists = || GitError::AlreadyExists {
        what: "path",
        name: path.display().to_string(),
    };
    let io = |source| GitError::Io {
        path: path.to_owned(),
        source,
    };
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {
            if std::fs::read_dir(path).map_err(io)?.next().is_some() {
                return Err(already_exists());
            }
            Ok(())
        }
        Ok(_) => Err(already_exists()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io(err)),
    }
}

/// `git worktree add`.
pub fn add(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &WorktreeAddOptions,
) -> Result<OpOutcome<WorktreeInfo>> {
    let _ = perm;
    ctx.cancel.check()?;
    let path = absolute(repo, &opts.path);
    // Before spawning: Git would create the new branch first and then fail on the path.
    check_target_dir(&path)?;

    let mut cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(["worktree", "add"])
        .with_hooks();
    if let Some(reason) = &opts.lock_reason {
        cmd = cmd.arg("--lock");
        if !reason.is_empty() {
            cmd = cmd.arg("--reason").arg(reason);
        }
    }
    let start: Option<String>;
    match &opts.target {
        WorktreeTarget::ExistingBranch(full) => {
            if !full.as_bstr().starts_with(b"refs/heads/") {
                return Err(GitError::InvalidName {
                    what: "branch",
                    name: full.as_bstr().to_string(),
                });
            }
            let short = full.shorten().to_string();
            // The HEAD of a bare main repository checks nothing out (Git skips bare entries too).
            let elsewhere = list(repo)?.into_iter().find(|entry| match &entry.head {
                Some(HeadState::Branch { name, .. } | HeadState::Unborn { name }) => {
                    !entry.bare && name == full
                }
                _ => false,
            });
            if let Some(entry) = elsewhere {
                return Err(GitError::BranchCheckedOutElsewhere {
                    branch: short,
                    worktree: entry.path,
                });
            }
            start = Some(short);
        }
        WorktreeTarget::NewBranch {
            name,
            start: start_point,
        } => {
            // Before spawning: Git would create the branch and then fail on the path.
            names::new_branch(&repo.local(), name)?;
            cmd = cmd.arg("-b").arg(name);
            start = match start_point {
                StartPoint::Head => None,
                StartPoint::Commit(id) => Some(id.to_string()),
                StartPoint::Ref(full) => Some(full.as_bstr().to_string()),
            };
        }
        WorktreeTarget::Detached(id) => {
            cmd = cmd.arg("--detach");
            start = Some(id.to_string());
        }
    }
    cmd = cmd.arg("--").arg(&path);
    if let Some(start) = start {
        cmd = cmd.arg(start);
    }
    let acc = run_one(repo, cmd, ctx, OpClass::Worktree)?;
    let entry = list(repo)?
        .into_iter()
        .find(|entry| same_path(&entry.path, &path))
        .ok_or_else(|| other("the new worktree is not listed"))?;
    Ok(acc.finish(entry, RefreshSet::WORKTREES | RefreshSet::BRANCHES))
}

/// Returns the id (directory name under `<common>/worktrees/`) of the removed worktree. The GUI then calls
/// `gitcthulhu_oplog::Oplog::clear_linked_worktree(repo, id, perm)` (17 §4.1) so its undo chain does not live on.
pub fn remove(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    path: &Path,
    force: bool,
) -> Result<OpOutcome<String>> {
    let _ = perm;
    ctx.cancel.check()?;
    let entry = find_entry(repo, path)?;
    let Some(id) = entry.id.clone() else {
        return Err(GitError::MainWorktree);
    };
    if entry.is_current {
        return Err(GitError::CurrentWorktree);
    }
    let mut cmd = GitCommand::new(repo, CommandKind::Mutation).args(["worktree", "remove"]);
    if force {
        cmd = cmd.arg("-f");
        if entry.locked.is_some() {
            // A locked worktree needs the flag twice.
            cmd = cmd.arg("-f");
        }
    }
    cmd = cmd.arg("--").arg(&entry.path);
    let acc = run_one(repo, cmd, ctx, OpClass::Worktree)?;
    Ok(acc.finish(id, RefreshSet::WORKTREES | RefreshSet::BRANCHES))
}

/// Returns the ids of pruned worktrees (ids listed by gix before minus after); the GUI clears their oplog chains.
pub fn prune(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<Vec<String>>> {
    let _ = perm;
    ctx.cancel.check()?;
    let ids = |entries: Vec<WorktreeInfo>| -> Vec<String> {
        entries.into_iter().filter_map(|entry| entry.id).collect()
    };
    let before = ids(list(repo)?);
    let cmd = GitCommand::new(repo, CommandKind::Mutation).args(["worktree", "prune"]);
    let acc = run_one(repo, cmd, ctx, OpClass::Worktree)?;
    let after = ids(list(repo)?);
    let pruned = before
        .into_iter()
        .filter(|id| !after.contains(id))
        .collect();
    Ok(acc.finish(pruned, RefreshSet::WORKTREES | RefreshSet::BRANCHES))
}

fn linked_entry(repo: &Repo, path: &Path) -> Result<WorktreeInfo> {
    let entry = find_entry(repo, path)?;
    if entry.id.is_none() {
        return Err(GitError::Unsupported {
            what: "locking or unlocking the main worktree",
        });
    }
    Ok(entry)
}

/// `git worktree lock`.
pub fn lock(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    path: &Path,
    reason: Option<&str>,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let entry = linked_entry(repo, path)?;
    let mut cmd = GitCommand::new(repo, CommandKind::Mutation).args(["worktree", "lock"]);
    if let Some(reason) = reason.filter(|r| !r.is_empty()) {
        cmd = cmd.arg("--reason").arg(reason);
    }
    cmd = cmd.arg("--").arg(&entry.path);
    let acc = run_one(repo, cmd, ctx, OpClass::Worktree)?;
    Ok(acc.finish((), RefreshSet::WORKTREES))
}

/// `git worktree unlock`.
pub fn unlock(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    path: &Path,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let entry = linked_entry(repo, path)?;
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(["worktree", "unlock", "--"])
        .arg(&entry.path);
    let acc = run_one(repo, cmd, ctx, OpClass::Worktree)?;
    Ok(acc.finish((), RefreshSet::WORKTREES))
}
