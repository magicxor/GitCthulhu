//! Merge, cherry-pick, revert, reset and operation control (WP-B7).
//!
//! Every function runs the `git` executable (hooks, signing and filters behave natively) under the exclusive
//! repository guard the caller holds (`perm`). Conflicts are reported by the classifier as `Err(Conflicts)` with the
//! operation left in progress, so the GUI can offer continue / abort / skip.

use gix::ObjectId;

use crate::{
    CommandLog, ContinueOutcome, FastForward, GitError, InProgress, MergeOptions, MergeOutcome,
    OpClass, OpContext, OpOutcome, OpWarning, OverwriteKind, PickOptions, RefreshSet, RelaPath,
    Repo, RepoExclusive, ResetMode, Result, SequenceOutcome,
    cli::{self, CommandKind, GitCommand, GitOutput},
    rebase,
};

/// Hooks that run after the operation happened: their failure is a warning, never an error.
const POST_HOOKS: &[&str] = &[
    "post-checkout",
    "post-merge",
    "post-commit",
    "post-rewrite",
    "post-applypatch",
    "post-index-change",
];

/// Collects what the commands of one operation produced, to be turned into an [`OpOutcome`].
#[derive(Default)]
pub(crate) struct Collected {
    log: CommandLog,
    warnings: Vec<OpWarning>,
}

impl Collected {
    /// Record a finished command and hand its output back for parsing.
    pub(crate) fn add(&mut self, out: GitOutput) -> GitOutput {
        self.log.commands.push(out.record.clone());
        self.log.hooks.extend(out.hooks.iter().cloned());
        self.log
            .removed_stale_locks
            .extend(out.removed_stale_locks.iter().cloned());
        self.warnings.extend(out.warnings.iter().cloned());
        // Git ignores the exit status of some post hooks (`post-merge`), so the command succeeds without the
        // executor having seen a failure: report them from the hook runs.
        for hook in &out.hooks {
            let reported = self.warnings.iter().any(
                |w| matches!(w, OpWarning::PostHookFailed { hook: name, .. } if *name == hook.name),
            );
            if hook.exit_code != Some(0) && POST_HOOKS.contains(&hook.name.as_str()) && !reported {
                self.warnings.push(OpWarning::PostHookFailed {
                    hook: hook.name.clone(),
                    exit_code: hook.exit_code,
                    output: format!("{}{}", out.stderr, String::from_utf8_lossy(&out.stdout)),
                });
            }
        }
        out
    }

    /// The outcome of the operation: `value` plus everything collected.
    pub(crate) fn finish<T>(self, value: T, refresh: RefreshSet) -> OpOutcome<T> {
        OpOutcome {
            value,
            refresh,
            log: self.log,
            warnings: self.warnings,
        }
    }
}

/// `Err(Cancelled)` if the operation was cancelled before it started.
pub(crate) fn check_cancel(ctx: &OpContext) -> Result<()> {
    if ctx.cancel.is_cancelled() {
        Err(GitError::Cancelled)
    } else {
        Ok(())
    }
}

/// The commit HEAD points to; `UnbornHead` for an unborn branch.
pub(crate) fn head_commit_id(repo: &Repo) -> Result<ObjectId> {
    let local = repo.local();
    let head = local
        .head()
        .map_err(|err| GitError::Other(anyhow::Error::new(err)))?;
    if head.is_unborn() {
        return Err(GitError::UnbornHead);
    }
    let id = local
        .head_id()
        .map_err(|err| GitError::Other(anyhow::Error::new(err)))?;
    Ok(id.detach())
}

/// The operation Git is in the middle of, from the marker files in the (per-worktree) git directory.
pub(crate) fn current_operation(repo: &Repo) -> Option<InProgress> {
    repo.local().state().map(InProgress::from)
}

/// A revision handed to Git as a positional argument must not look like an option.
fn check_rev(rev: &str, what: &'static str) -> Result<()> {
    if rev.trim().is_empty() || rev.starts_with('-') {
        return Err(GitError::InvalidName {
            what,
            name: rev.to_owned(),
        });
    }
    Ok(())
}

/// `git merge`.
///
/// `Err(Conflicts)` leaves the repository in the MERGING state (an autostash stays in `MERGE_AUTOSTASH`).
pub fn merge(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &MergeOptions,
) -> Result<OpOutcome<MergeOutcome>> {
    let _ = perm;
    check_cancel(ctx)?;
    check_rev(&opts.rev, "revision")?;

    let mut cmd = GitCommand::new(repo, CommandKind::Mutation)
        .arg("merge")
        .with_progress()
        .with_hooks()
        .arg("--progress")
        .arg("--no-edit");
    cmd = cmd.arg(match opts.ff {
        FastForward::Auto => "--ff",
        FastForward::NoFf => "--no-ff",
        FastForward::FfOnly => "--ff-only",
    });
    if opts.squash {
        cmd = cmd.arg("--squash");
    }
    if opts.autostash {
        cmd = cmd.arg("--autostash");
    }
    if opts.skip_hooks {
        cmd = cmd.arg("--no-verify");
    }

    // `git merge -F -` does not read stdin, so a custom message goes through a file that is removed afterwards.
    let message_file = match &opts.message {
        Some(message) if !message.trim().is_empty() => {
            let dir = repo.git_dir().join("gitcthulhu");
            std::fs::create_dir_all(&dir).map_err(|err| GitError::io(&dir, err))?;
            let path = dir.join("merge-msg");
            std::fs::write(&path, message.as_bytes()).map_err(|err| GitError::io(&path, err))?;
            cmd = cmd.arg("-F").arg(path.as_os_str());
            Some(path)
        }
        _ => None,
    };
    cmd = cmd.arg(&opts.rev);

    let result = cli::run_checked(repo, cmd, ctx, OpClass::Merge);
    if let Some(path) = message_file {
        // Best effort: a leftover file is overwritten by the next merge.
        std::fs::remove_file(path).ok();
    }

    let mut collected = Collected::default();
    let out = collected.add(result?);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Older Git: "Applying autostash resulted in conflicts."; newer: "Your local changes are stashed, however
    // applying them resulted in conflicts." (line-wrapped).
    let autostash_conflict = [out.stderr.as_str(), &stdout].iter().any(|text| {
        text.contains("Applying autostash resulted in conflicts")
            || text.contains("Your local changes are stashed, however applying them")
    });
    let value = if stdout.contains("Already up to date") || stdout.contains("Already up-to-date") {
        MergeOutcome::UpToDate
    } else if stdout.contains("Squash commit -- not updating HEAD") {
        MergeOutcome::Squashed
    } else {
        let new_head = head_commit_id(repo)?;
        if autostash_conflict {
            MergeOutcome::AutostashConflict { new_head }
        } else if stdout.lines().any(|line| line.starts_with("Fast-forward")) {
            MergeOutcome::FastForwarded { new_head }
        } else {
            MergeOutcome::Merged { new_head }
        }
    };
    Ok(collected.finish(value, RefreshSet::OPERATION_STATE))
}

/// Reject merge commits without a mainline parent before Git is asked.
fn check_mainline(
    repo: &Repo,
    commits: &[ObjectId],
    opts: PickOptions,
    what: &'static str,
) -> Result<()> {
    if opts.mainline.is_some() {
        return Ok(());
    }
    let local = repo.local();
    for id in commits {
        let commit = local
            .find_commit(*id)
            .map_err(|err| GitError::Other(anyhow::Error::new(err)))?;
        if commit.parent_ids().count() > 1 {
            return Err(GitError::Unsupported { what });
        }
    }
    Ok(())
}

fn pick_or_revert(
    repo: &Repo,
    ctx: &OpContext,
    verb: &'static str,
    class: OpClass,
    commits: &[ObjectId],
    opts: PickOptions,
) -> Result<OpOutcome<SequenceOutcome>> {
    check_cancel(ctx)?;
    if commits.is_empty() {
        return Err(GitError::InvalidName {
            what: "commit list (empty)",
            name: String::new(),
        });
    }
    let needs_mainline = if verb == "revert" {
        "revert of a merge commit needs a mainline parent"
    } else {
        "cherry-pick of a merge commit needs a mainline parent"
    };
    check_mainline(repo, commits, opts, needs_mainline)?;
    let mut cmd = GitCommand::new(repo, CommandKind::Mutation)
        .arg(verb)
        .with_hooks();
    if verb == "revert" {
        cmd = cmd.arg("--no-edit");
    }
    if let Some(parent) = opts.mainline {
        cmd = cmd.arg("-m").arg(parent.to_string());
    }
    if opts.no_commit {
        cmd = cmd.arg("-n");
    }
    cmd = cmd.args(commits.iter().map(|id| id.to_string()));

    let out = cli::run_checked(repo, cmd, ctx, class)?;
    let mut collected = Collected::default();
    collected.add(out);
    let new_head = head_commit_id(repo)?;
    Ok(collected.finish(
        SequenceOutcome::Completed { new_head },
        RefreshSet::OPERATION_STATE,
    ))
}

/// `git cherry-pick`.
///
/// A merge commit needs `opts.mainline` (`Unsupported` otherwise). A pick that becomes empty stops Git with the
/// sequencer state kept: `Err(EmptyPick)`, to be resolved with [`skip_operation`] or an empty commit.
pub fn cherry_pick(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    commits: &[ObjectId],
    opts: PickOptions,
) -> Result<OpOutcome<SequenceOutcome>> {
    let _ = perm;
    pick_or_revert(repo, ctx, "cherry-pick", OpClass::CherryPick, commits, opts)
}

/// `git revert`.
pub fn revert(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    commits: &[ObjectId],
    opts: PickOptions,
) -> Result<OpOutcome<SequenceOutcome>> {
    let _ = perm;
    pick_or_revert(repo, ctx, "revert", OpClass::Revert, commits, opts)
}

/// `git reset`. `--hard` never touches untracked files; `--keep` refuses to overwrite local changes.
pub fn reset(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    target: ObjectId,
    mode: ResetMode,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    check_cancel(ctx)?;
    let flag = match mode {
        ResetMode::Soft => "--soft",
        ResetMode::Mixed => "--mixed",
        ResetMode::Hard => "--hard",
        ResetMode::Keep => "--keep",
    };
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(["reset", flag, "-q"])
        .arg(target.to_string())
        .with_hooks();
    let out = cli::run_checked(repo, cmd, ctx, OpClass::Reset).map_err(|err| match mode {
        ResetMode::Keep => keep_refusal(err),
        ResetMode::Soft | ResetMode::Mixed | ResetMode::Hard => err,
    })?;
    let mut collected = Collected::default();
    collected.add(out);
    Ok(collected.finish((), RefreshSet::OPERATION_STATE))
}

/// `git reset --keep` refuses with unpack-trees messages that are not in the classifier's table:
/// `error: Entry '<p>' not uptodate. Cannot merge.` (a local change) and
/// `error: Untracked working tree file '<p>' would be overwritten by merge.`. Both become `WouldOverwrite`.
fn keep_refusal(err: GitError) -> GitError {
    let GitError::CommandFailed { stderr, .. } = &err else {
        return err;
    };
    let quoted_path = |line: &str, prefix: &str, suffix: &str| -> Option<RelaPath> {
        let line = line.trim_end().strip_suffix(suffix)?;
        let start = line.find(prefix)? + prefix.len();
        Some(RelaPath::from(&line[start..]))
    };
    let (mut local, mut untracked) = (Vec::new(), Vec::new());
    for line in stderr.lines() {
        if let Some(path) = quoted_path(line, "Entry '", "' not uptodate. Cannot merge.") {
            local.push(path);
        } else if let Some(path) = quoted_path(
            line,
            "Untracked working tree file '",
            "' would be overwritten by merge.",
        ) {
            untracked.push(path);
        }
    }
    let (kind, paths) = if !local.is_empty() {
        (OverwriteKind::LocalChanges, local)
    } else if !untracked.is_empty() {
        (OverwriteKind::Untracked, untracked)
    } else {
        return err;
    };
    GitError::WouldOverwrite {
        kind,
        paths,
        stderr: stderr.clone(),
    }
}

/// Write `message` to `MERGE_MSG` of the (per-worktree) git directory.
fn write_merge_msg(repo: &Repo, message: &str) -> Result<()> {
    let mut text = message.to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let path = repo.git_dir().join("MERGE_MSG");
    std::fs::write(&path, text.as_bytes()).map_err(|err| GitError::io(&path, err))
}

/// Run one control command (`git <verb> --continue|--abort|--skip`) for the operation in progress.
fn run_control(
    repo: &Repo,
    ctx: &OpContext,
    args: &[&str],
    class: OpClass,
    collected: &mut Collected,
) -> Result<()> {
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(args.iter().copied())
        .with_hooks();
    let out = cli::run_checked(repo, cmd, ctx, class)?;
    collected.add(out);
    Ok(())
}

fn no_operation() -> GitError {
    GitError::NoOperationInProgress { expected: None }
}

/// Dispatch on the current state: merge / cherry-pick / revert / rebase / am. `message` (if Some) is written to
/// MERGE_MSG (merge, cherry-pick, revert) before continuing.
///
/// Unresolved conflicts make Git fail; the classifier reports them as `Err(Conflicts)`. Without an operation the
/// result is `Err(NoOperationInProgress)`.
pub fn continue_operation(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    message: Option<&str>,
) -> Result<OpOutcome<ContinueOutcome>> {
    check_cancel(ctx)?;
    let Some(state) = current_operation(repo) else {
        return Err(no_operation());
    };
    let mut collected = Collected::default();
    let (args, class): (&[&str], OpClass) = match state {
        InProgress::Merge => (&["merge", "--continue"], OpClass::Merge),
        InProgress::CherryPick | InProgress::CherryPickSequence => {
            (&["cherry-pick", "--continue"], OpClass::CherryPick)
        }
        InProgress::Revert | InProgress::RevertSequence => {
            (&["revert", "--continue"], OpClass::Revert)
        }
        InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase => {
            let outcome = rebase::continue_rebase(repo, perm, ctx, message)?;
            return Ok(map_outcome(outcome, ContinueOutcome::Rebase));
        }
        InProgress::ApplyMailbox => (&["am", "--continue"], OpClass::Generic),
        InProgress::Bisect => return Err(no_operation()),
    };
    if let Some(message) = message
        && matches!(
            state,
            InProgress::Merge
                | InProgress::CherryPick
                | InProgress::CherryPickSequence
                | InProgress::Revert
                | InProgress::RevertSequence
        )
    {
        write_merge_msg(repo, message)?;
    }
    run_control(repo, ctx, args, class, &mut collected)?;
    Ok(collected.finish(ContinueOutcome::Completed, RefreshSet::OPERATION_STATE))
}

/// Change the value of an outcome, keeping everything else.
pub(crate) fn map_outcome<T, U>(outcome: OpOutcome<T>, f: impl FnOnce(T) -> U) -> OpOutcome<U> {
    OpOutcome {
        value: f(outcome.value),
        refresh: outcome.refresh,
        log: outcome.log,
        warnings: outcome.warnings,
    }
}

/// Abort the operation in progress; Git restores the state from before the operation exactly.
pub fn abort_operation(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    check_cancel(ctx)?;
    let Some(state) = current_operation(repo) else {
        return Err(no_operation());
    };
    let (args, class): (&[&str], OpClass) = match state {
        InProgress::Merge => (&["merge", "--abort"], OpClass::Merge),
        InProgress::CherryPick | InProgress::CherryPickSequence => {
            (&["cherry-pick", "--abort"], OpClass::CherryPick)
        }
        InProgress::Revert | InProgress::RevertSequence => {
            (&["revert", "--abort"], OpClass::Revert)
        }
        InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase => {
            return rebase::abort_rebase(repo, perm, ctx);
        }
        InProgress::ApplyMailbox => (&["am", "--abort"], OpClass::Generic),
        InProgress::Bisect => (&["bisect", "reset"], OpClass::Generic),
    };
    let mut collected = Collected::default();
    run_control(repo, ctx, args, class, &mut collected)?;
    Ok(collected.finish((), RefreshSet::OPERATION_STATE))
}

/// Skip the current step of the operation in progress. A merge has no skip (`Unsupported`).
pub fn skip_operation(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<ContinueOutcome>> {
    check_cancel(ctx)?;
    let Some(state) = current_operation(repo) else {
        return Err(no_operation());
    };
    let (args, class): (&[&str], OpClass) = match state {
        InProgress::Merge => {
            return Err(GitError::Unsupported {
                what: "a merge cannot be skipped",
            });
        }
        InProgress::CherryPick | InProgress::CherryPickSequence => {
            (&["cherry-pick", "--skip"], OpClass::CherryPick)
        }
        InProgress::Revert | InProgress::RevertSequence => (&["revert", "--skip"], OpClass::Revert),
        InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase => {
            let outcome = rebase::skip_rebase(repo, perm, ctx)?;
            return Ok(map_outcome(outcome, ContinueOutcome::Rebase));
        }
        InProgress::ApplyMailbox => (&["am", "--skip"], OpClass::Generic),
        InProgress::Bisect => return Err(no_operation()),
    };
    let mut collected = Collected::default();
    run_control(repo, ctx, args, class, &mut collected)?;
    Ok(collected.finish(ContinueOutcome::Completed, RefreshSet::OPERATION_STATE))
}
