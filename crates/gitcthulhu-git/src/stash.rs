//! Stashes (WP-B6).
//!
//! `git stash` does the work. A stash entry is addressed by its commit id: the id is resolved to the current
//! `stash@{n}` right before the command runs (the list shifts when entries are dropped).

use bstr::ByteSlice;
use gix::ObjectId;

use crate::{
    GitError, GitVersion, OpClass, OpContext, OpOutcome, RefreshSet, RelaPath, Repo, RepoExclusive,
    Result, StashApplyOptions, StashApplyResult, StashPushOptions,
    branch::gix_error,
    cli::{self, CommandKind, GitCommand},
    commit::outcome,
    internal::post_state,
};

/// `git stash push`; `None` when there was nothing to stash.
///
/// `staged_only` needs Git 2.35 (`Unsupported` otherwise).
pub fn stash_push(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &StashPushOptions,
) -> Result<OpOutcome<Option<ObjectId>>> {
    let _ = perm;
    ctx.cancel.check()?;
    if repo.workdir().is_none() {
        return Err(GitError::BareRepository);
    }
    if opts.staged_only
        && repo.runtime().version()
            < (GitVersion {
                major: 2,
                minor: 35,
                patch: 0,
            })
    {
        return Err(GitError::Unsupported {
            what: "stashing only the staged changes needs Git 2.35 or newer",
        });
    }
    if repo.local().head().map_err(gix_error)?.is_unborn() {
        return Err(GitError::UnbornHead);
    }
    let before = stash_tip(repo)?;
    let mut cmd = GitCommand::new(repo, CommandKind::Mutation)
        .with_hooks()
        .args(["stash", "push"]);
    if let Some(message) = &opts.message {
        cmd = cmd.arg("-m").arg(message);
    }
    if opts.include_untracked {
        cmd = cmd.arg("-u");
    }
    if opts.keep_index {
        cmd = cmd.arg("--keep-index");
    }
    if opts.staged_only {
        cmd = cmd.arg("--staged");
    }
    if !opts.paths.is_empty() {
        cmd = cmd.pathspecs(&opts.paths);
    }
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Stash)?;
    let after = stash_tip(repo)?;
    let nothing_saved = after == before
        || String::from_utf8_lossy(&out.stdout).contains("No local changes to save");
    let value = if nothing_saved { None } else { after };
    Ok(outcome(
        value,
        RefreshSet::STATUS | RefreshSet::STASHES | RefreshSet::LOG,
        out,
    ))
}

/// `git stash apply`.
///
/// Conflicts and a refusal to overwrite existing untracked files are not errors: they are reported through
/// [`StashApplyResult`] (the stash entry is kept). `StashNotFound` when the id is no longer in the stash list.
pub fn stash_apply(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    stash: ObjectId,
    opts: StashApplyOptions,
) -> Result<OpOutcome<StashApplyResult>> {
    let _ = perm;
    apply_or_pop(
        repo,
        ctx,
        stash,
        opts,
        "apply",
        RefreshSet::STATUS | RefreshSet::REPO_STATE,
    )
}

/// `git stash pop`. Same outcome rules as [`stash_apply`]; with conflicts or a partial restore Git keeps the entry.
pub fn stash_pop(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    stash: ObjectId,
    opts: StashApplyOptions,
) -> Result<OpOutcome<StashApplyResult>> {
    let _ = perm;
    apply_or_pop(
        repo,
        ctx,
        stash,
        opts,
        "pop",
        RefreshSet::STATUS | RefreshSet::STASHES | RefreshSet::LOG | RefreshSet::REPO_STATE,
    )
}

/// `git stash drop`.
pub fn stash_drop(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    stash: ObjectId,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let index = stash_index(repo, stash)?;
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(["stash", "drop"])
        .arg(stash_spec(index));
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Stash)?;
    Ok(outcome((), RefreshSet::STASHES | RefreshSet::LOG, out))
}

/// `git stash clear`.
pub fn stash_clear(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let cmd = GitCommand::new(repo, CommandKind::Mutation).args(["stash", "clear"]);
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Stash)?;
    Ok(outcome((), RefreshSet::STASHES | RefreshSet::LOG, out))
}

/// Read: paths stored in the stash's untracked-files commit (parent 3), empty if none. The GUI passes them as
/// `UntrackedCapture::Paths` when recording `StashApply`/`StashPop` (17 §1.3), so undo removes them again.
pub fn untracked_paths(repo: &Repo, stash: ObjectId) -> Result<Vec<RelaPath>> {
    let gix_repo = repo.local();
    let commit = gix_repo
        .find_commit(stash)
        .map_err(|_| GitError::StashNotFound { id: stash })?;
    let Some(untracked_id) = commit.parent_ids().nth(2) else {
        return Ok(Vec::new());
    };
    let tree = gix_repo
        .find_commit(untracked_id)
        .map_err(gix_error)?
        .tree()
        .map_err(gix_error)?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut recorder)
        .map_err(gix_error)?;
    let mut paths: Vec<RelaPath> = recorder
        .records
        .into_iter()
        .filter(|entry| !entry.mode.is_tree())
        .map(|entry| entry.filepath)
        .collect();
    paths.sort();
    Ok(paths)
}

fn stash_spec(index: usize) -> String {
    format!("stash@{{{index}}}")
}

fn apply_or_pop(
    repo: &Repo,
    ctx: &OpContext,
    stash: ObjectId,
    opts: StashApplyOptions,
    verb: &str,
    refresh: RefreshSet,
) -> Result<OpOutcome<StashApplyResult>> {
    ctx.cancel.check()?;
    let index = stash_index(repo, stash)?;
    let mut cmd = GitCommand::new(repo, CommandKind::Mutation)
        .with_hooks()
        .args(["stash", verb]);
    if opts.restore_index {
        cmd = cmd.arg("--index");
    }
    cmd = cmd.arg(stash_spec(index));
    let out = cli::run(repo.runtime(), cmd, ctx)?;
    if out.success() {
        return Ok(outcome(StashApplyResult::Applied, refresh, out));
    }
    let post = post_state::read(repo)?;
    let text = format!("{}\n{}", out.stderr, String::from_utf8_lossy(&out.stdout));
    let reported: Vec<&str> = text
        .lines()
        .filter_map(|line| line.trim_end().strip_suffix(" already exists, no checkout"))
        .map(|path| path.trim_start_matches("error: "))
        .collect();
    // stderr is lossy UTF-8: map each reported path back to the exact bytes stored in the stash's untracked tree.
    let stored = if reported.is_empty() {
        Vec::new()
    } else {
        untracked_paths(repo, stash).unwrap_or_default()
    };
    let not_restored: Vec<RelaPath> = reported
        .into_iter()
        .map(|path| {
            stored
                .iter()
                .find(|candidate| candidate.to_str_lossy() == path)
                .cloned()
                .unwrap_or_else(|| RelaPath::from(path))
        })
        .collect();
    let result = if !not_restored.is_empty() {
        StashApplyResult::Partial {
            conflicts: post.conflicted,
            untracked_not_restored: not_restored,
        }
    } else if text.contains("CONFLICT") && !post.conflicted.is_empty() {
        StashApplyResult::Conflicts {
            paths: post.conflicted,
        }
    } else {
        return Err(crate::error::classify(OpClass::Stash, &out, &post));
    };
    Ok(outcome(result, refresh, out))
}

/// The id `refs/stash` points to, `None` without stashes.
fn stash_tip(repo: &Repo) -> Result<Option<ObjectId>> {
    let gix_repo = repo.local();
    let Some(mut reference) = gix_repo
        .try_find_reference("refs/stash")
        .map_err(gix_error)?
    else {
        return Ok(None);
    };
    Ok(reference.peel_to_id().ok().map(|id| id.detach()))
}

/// Position of `stash` in the current stash list (`stash@{n}`), found by scanning the `refs/stash` reflog.
fn stash_index(repo: &Repo, stash: ObjectId) -> Result<usize> {
    let not_found = || GitError::StashNotFound { id: stash };
    let gix_repo = repo.local();
    let reference = gix_repo
        .try_find_reference("refs/stash")
        .map_err(gix_error)?
        .ok_or_else(not_found)?;
    let mut log = reference.log_iter();
    let log_path = repo.common_dir().join("logs").join("refs").join("stash");
    let lines = log
        .rev()
        .map_err(|err| GitError::io(&log_path, err))?
        .ok_or_else(not_found)?;
    for (index, line) in lines.enumerate() {
        let line = line.map_err(gix_error)?;
        if line.new_oid == stash {
            return Ok(index);
        }
    }
    Err(not_found())
}
