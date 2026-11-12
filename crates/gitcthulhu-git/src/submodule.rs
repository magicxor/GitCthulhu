//! Submodules (WP-B5): listing with gix, mutations through `git submodule`.

use std::path::Path;

use bstr::ByteSlice as _;

use crate::{
    CancelToken, GitError, GitVersion, OpClass, OpContext, OpOutcome, RefreshSet, RelaPath, Repo,
    RepoExclusive, Result, SubmoduleInfo, SubmoduleState,
    cli::{CommandKind, GitCommand, run_checked},
    config::{OpAccumulator, other},
};

/// `git submodule add --progress` is only passed from this version on (verified on 2.56; 2.34 is unverified).
pub const SUBMODULE_ADD_PROGRESS_MIN: GitVersion = GitVersion {
    major: 2,
    minor: 37,
    patch: 0,
};

fn to_path(path: &RelaPath) -> Result<std::borrow::Cow<'_, Path>> {
    gix::path::from_bstr(path.as_bstr()).map_err(|_| GitError::InvalidName {
        what: "submodule path",
        name: path.to_str_lossy().into_owned(),
    })
}

/// The submodules of the repository; `with_dirty` also checks whether their worktrees are dirty.
pub fn list(repo: &Repo, with_dirty: bool, cancel: &CancelToken) -> Result<Vec<SubmoduleInfo>> {
    let gix_repo = repo.local();
    let Some(submodules) = gix_repo.submodules().map_err(other)? else {
        return Ok(Vec::new());
    };
    let workdir = repo.workdir().map(Path::to_owned);
    let mut out = Vec::new();
    for submodule in submodules {
        cancel.check()?;
        let path: RelaPath = submodule.path().map_err(other)?;
        let index_commit = submodule.index_id().ok().flatten();
        let state = submodule.state().map_err(other)?;
        let missing = index_commit.is_some()
            && workdir
                .as_ref()
                .zip(to_path(&path).ok())
                .is_some_and(|(workdir, rela)| !workdir.join(rela).exists());
        let state = if missing {
            SubmoduleState::Missing
        } else if !state.superproject_configuration {
            SubmoduleState::Uninitialized
        } else if state.repository_exists && state.worktree_checkout {
            SubmoduleState::CheckedOut
        } else {
            SubmoduleState::Initialized
        };
        let dirty = (with_dirty && state == SubmoduleState::CheckedOut).then(|| {
            submodule
                .status(gix::submodule::config::Ignore::None, true)
                .ok()
                .and_then(|status| status.is_dirty())
                .unwrap_or(false)
        });
        out.push(SubmoduleInfo {
            name: submodule.name().to_str_lossy().into_owned(),
            path,
            url: submodule
                .url()
                .ok()
                .map(|url| url.to_bstring().to_str_lossy().into_owned()),
            branch: submodule
                .branch()
                .ok()
                .flatten()
                .map(|branch| match branch {
                    gix::submodule::config::Branch::CurrentInSuperproject => ".".to_owned(),
                    gix::submodule::config::Branch::Name(name) => name.to_str_lossy().into_owned(),
                }),
            state,
            index_commit,
            // gix's `head_id()` is the id in the superproject's HEAD; the commit that is checked out lives in the
            // submodule's own repository.
            head_commit: submodule
                .open()
                .ok()
                .flatten()
                .and_then(|sub| sub.head_id().ok().map(|id| id.detach())),
            dirty,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn command(repo: &Repo, kind: CommandKind) -> GitCommand {
    GitCommand::new(repo, kind).arg("submodule")
}

fn with_paths(cmd: GitCommand, paths: &[RelaPath]) -> GitCommand {
    if paths.is_empty() {
        cmd
    } else {
        cmd.literal_paths_after_dashdash(paths)
    }
}

fn run_with(
    repo: &Repo,
    cmd: GitCommand,
    ctx: &OpContext,
    refresh: RefreshSet,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    let mut acc = OpAccumulator::default();
    acc.push(run_checked(repo, cmd, ctx, OpClass::Submodule)?);
    repo.reload()?;
    Ok(acc.finish((), refresh))
}

/// `git submodule add`.
pub fn add(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    url: &str,
    path: &RelaPath,
    name: Option<&str>,
    branch: Option<&str>,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    if url.trim().is_empty() || url.starts_with('-') {
        return Err(GitError::InvalidName {
            what: "submodule URL",
            name: url.to_owned(),
        });
    }
    if path.trim().is_empty() {
        return Err(GitError::InvalidName {
            what: "submodule path",
            name: String::new(),
        });
    }
    let rela = to_path(path)?;
    if let Some(workdir) = repo.workdir() {
        let target = workdir.join(&*rela);
        let occupied = match std::fs::read_dir(&target) {
            Ok(mut entries) => entries.next().is_some(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            // Exists but is not a directory.
            Err(_) => target.exists(),
        };
        if occupied {
            return Err(GitError::AlreadyExists {
                what: "path",
                name: path.to_str_lossy().into_owned(),
            });
        }
    }
    let mut cmd = command(repo, CommandKind::Network).arg("add").with_hooks();
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        cmd = cmd.arg("--name").arg(name);
    }
    if let Some(branch) = branch.filter(|b| !b.is_empty()) {
        cmd = cmd.arg("-b").arg(branch);
    }
    if repo.runtime().version() >= SUBMODULE_ADD_PROGRESS_MIN {
        cmd = cmd.arg("--progress").with_progress();
    }
    // `<path>` is a plain destination path here, not a pathspec.
    cmd = cmd.arg("--").arg(url).arg(rela.as_os_str());
    run_with(
        repo,
        cmd,
        ctx,
        RefreshSet::SUBMODULES | RefreshSet::STATUS | RefreshSet::CONFIG,
    )
}

/// `git submodule update --init`.
pub fn init_update(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    recursive: bool,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    let mut cmd = command(repo, CommandKind::Network)
        .args(["update", "--init"])
        .with_hooks();
    if recursive {
        cmd = cmd.arg("--recursive");
    }
    cmd = with_paths(cmd.arg("--progress").with_progress(), paths);
    run_with(repo, cmd, ctx, RefreshSet::SUBMODULES | RefreshSet::STATUS)
}

/// `git submodule update`.
pub fn update(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    recursive: bool,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    let mut cmd = command(repo, CommandKind::Network)
        .arg("update")
        .with_hooks();
    if recursive {
        cmd = cmd.arg("--recursive");
    }
    cmd = with_paths(cmd.arg("--progress").with_progress(), paths);
    run_with(repo, cmd, ctx, RefreshSet::SUBMODULES | RefreshSet::STATUS)
}

/// `git submodule sync`.
pub fn sync(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    recursive: bool,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    let mut cmd = command(repo, CommandKind::Mutation).arg("sync");
    if recursive {
        cmd = cmd.arg("--recursive");
    }
    run_with(
        repo,
        with_paths(cmd, paths),
        ctx,
        RefreshSet::SUBMODULES | RefreshSet::CONFIG,
    )
}

/// `git submodule deinit`.
pub fn deinit(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    force: bool,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    let mut cmd = command(repo, CommandKind::Mutation).arg("deinit");
    if force {
        cmd = cmd.arg("-f");
    }
    // Without paths Git wants `--all`; a bare `deinit` is an error.
    cmd = if paths.is_empty() {
        cmd.arg("--all")
    } else {
        cmd.literal_paths_after_dashdash(paths)
    };
    run_with(
        repo,
        cmd,
        ctx,
        RefreshSet::SUBMODULES | RefreshSet::STATUS | RefreshSet::CONFIG,
    )
}

/// Remove a submodule completely.
pub fn remove(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    path: &RelaPath,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let info = list(repo, false, &ctx.cancel)?
        .into_iter()
        .find(|info| info.path == *path)
        .ok_or_else(|| GitError::NotFound {
            what: "submodule",
            name: path.to_str_lossy().into_owned(),
        })?;
    let paths = std::slice::from_ref(path);
    let mut acc = OpAccumulator::default();
    let deinit = command(repo, CommandKind::Mutation)
        .args(["deinit", "-f"])
        .literal_paths_after_dashdash(paths);
    acc.push(run_checked(repo, deinit, ctx, OpClass::Submodule)?);
    let rm = GitCommand::new(repo, CommandKind::Mutation)
        .args(["rm", "-f", "-q"])
        .literal_paths_after_dashdash(paths);
    acc.push(run_checked(repo, rm, ctx, OpClass::Submodule)?);

    // The cloned repository of the submodule lives on in `<common_dir>/modules/<name>`.
    let name = Path::new(&info.name);
    if name
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        let modules = repo.common_dir().join("modules").join(name);
        match std::fs::remove_dir_all(&modules) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(GitError::Io {
                    path: modules,
                    source,
                });
            }
        }
    }
    repo.reload()?;
    Ok(acc.finish(
        (),
        RefreshSet::SUBMODULES | RefreshSet::STATUS | RefreshSet::CONFIG,
    ))
}
