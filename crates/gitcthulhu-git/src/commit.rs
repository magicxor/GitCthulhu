//! Committing and the commit message draft (WP-B6).
//!
//! `commit` runs `git commit` so that hooks, signing and identity behave natively. The message draft lives in
//! `<git_dir>/gitcthulhu/commit-draft`, or in `MERGE_MSG` while a merge, cherry-pick or revert is in progress.

use std::{fs, io, path::PathBuf};

use bstr::ByteSlice;
use gix::{ObjectId, state::InProgress};

use crate::{
    CommandLog, CommitOptions, CommitOutcome, GitError, OpClass, OpContext, OpOutcome, OpWarning,
    RefreshSet, Repo, RepoExclusive, Result, SignMode,
    branch::gix_error,
    cli::{self, CommandKind, GitCommand, GitOutput},
};

/// Wrap the result of one finished git command into an [`OpOutcome`].
///
/// Git ignores the exit status of some post hooks (`post-commit`), so a failed post hook that the executor did not
/// already turn into a warning is reported here from the hook trace.
pub(crate) fn outcome<T>(value: T, refresh: RefreshSet, out: GitOutput) -> OpOutcome<T> {
    let mut warnings = out.warnings;
    for hook in &out.hooks {
        let failed = hook.exit_code != Some(0);
        let already_reported = warnings.iter().any(
            |w| matches!(w, OpWarning::PostHookFailed { hook: name, .. } if *name == hook.name),
        );
        if failed && hook.name.starts_with("post-") && !already_reported {
            // Same shape as the executor's warnings: stderr + stdout of the whole command (§1.11).
            let mut output = out.stderr.clone();
            if !out.stdout.is_empty() {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&String::from_utf8_lossy(&out.stdout));
            }
            warnings.push(OpWarning::PostHookFailed {
                hook: hook.name.clone(),
                exit_code: hook.exit_code,
                output,
            });
        }
    }
    OpOutcome {
        value,
        refresh,
        log: CommandLog {
            commands: vec![out.record],
            hooks: out.hooks,
            removed_stale_locks: out.removed_stale_locks,
        },
        warnings,
    }
}

/// An outcome for an operation that needed no command at all.
pub(crate) fn no_op_outcome<T>(value: T) -> OpOutcome<T> {
    OpOutcome {
        value,
        refresh: RefreshSet::empty(),
        log: CommandLog::default(),
        warnings: Vec::new(),
    }
}

/// `git commit`, hooks included.
///
/// Errors: `EmptyMessage` (empty message without `amend`, before Git is spawned), `BareRepository`, `UnbornHead`
/// (amend without commits), and whatever the executor classifies: `IdentityUnknown`, `HookFailed`, `SigningFailed`,
/// `NothingToCommit`. A failing `post-commit` hook is an [`OpWarning`](crate::OpWarning). On success the persisted
/// draft message is deleted.
pub fn commit(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &CommitOptions,
) -> Result<OpOutcome<CommitOutcome>> {
    let _ = perm;
    ctx.cancel.check()?;
    if repo.workdir().is_none() {
        return Err(GitError::BareRepository);
    }
    let keep_old_message = opts.message.trim().is_empty() && opts.amend;
    if opts.message.trim().is_empty() && !opts.amend {
        return Err(GitError::EmptyMessage);
    }
    let gix_repo = repo.local();
    if opts.amend && gix_repo.head().map_err(gix_error)?.is_unborn() {
        return Err(GitError::UnbornHead);
    }

    let mut cmd = GitCommand::new(repo, CommandKind::Mutation).with_hooks();
    if let Some(identity) = &opts.identity_override {
        cmd = cmd
            .arg("-c")
            .arg(format!("user.name={}", identity.name))
            .arg("-c")
            .arg(format!("user.email={}", identity.email));
    }
    cmd = cmd.arg("commit");
    if keep_old_message {
        cmd = cmd.arg("--no-edit");
    } else {
        cmd = cmd.arg("--file=-").stdin(opts.message.clone().into_bytes());
    }
    if opts.amend {
        cmd = cmd.arg("--amend");
    }
    if opts.allow_empty {
        cmd = cmd.arg("--allow-empty");
    }
    if let Some(author) = &opts.author {
        cmd = cmd.arg(format!("--author={} <{}>", author.name, author.email));
    }
    if opts.signoff {
        cmd = cmd.arg("--signoff");
    }
    for (key, value) in &opts.trailers {
        cmd = cmd.arg("--trailer").arg(format!("{key}: {value}"));
    }
    match opts.sign {
        SignMode::FromConfig => {}
        SignMode::Sign => cmd = cmd.arg("-S"),
        SignMode::DontSign => cmd = cmd.arg("--no-gpg-sign"),
    }
    if opts.skip_hooks {
        cmd = cmd.arg("--no-verify");
    }

    let out = cli::run_checked(repo, cmd, ctx, OpClass::Commit)?;

    let id = head_commit(repo)?;
    let summary = commit_summary(repo, id)?;
    // The commit exists now: a draft that cannot be deleted (a virus scanner or an editor holding the file on
    // Windows) must not turn the successful commit into an error.
    let draft = draft_path(repo);
    if let Err(err) = remove_file_if_exists(&draft) {
        tracing::warn!(
            "could not delete the commit message draft {}: {err}",
            draft.display()
        );
    }
    Ok(outcome(
        CommitOutcome { id, summary },
        RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::REPO_STATE,
        out,
    ))
}

/// Persist (or with `None` or a blank message delete) the message of the commit being composed.
///
/// While a merge, cherry-pick or revert is in progress the message goes to `MERGE_MSG` instead, which Git itself
/// uses as the message of the concluding commit. `MERGE_MSG` belongs to Git, so a blank message never deletes it
/// (`git merge --continue` / `git commit --no-edit` would otherwise abort with an empty message); the call is then a
/// no-op.
pub fn save_draft_message(repo: &Repo, message: Option<&str>) -> Result<()> {
    let blank = message.is_none_or(|text| text.trim().is_empty());
    let path = if merge_like_state(repo) {
        if blank {
            return Ok(());
        }
        repo.git_dir().join("MERGE_MSG")
    } else {
        draft_path(repo)
    };
    match message {
        Some(text) if !text.trim().is_empty() => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(|err| GitError::io(dir, err))?;
            }
            fs::write(&path, text).map_err(|err| GitError::io(&path, err))
        }
        _ => remove_file_if_exists(&path),
    }
}

/// The persisted draft message, if any.
///
/// While merging, cherry-picking or reverting it is `MERGE_MSG`; in a rebase that stopped at `edit` (for amending)
/// it is `rebase-merge/message`; otherwise the draft file.
pub fn load_draft_message(repo: &Repo) -> Result<Option<String>> {
    let git_dir = repo.git_dir();
    let path = if merge_like_state(repo) {
        git_dir.join("MERGE_MSG")
    } else if rebase_stopped_for_amend(repo) {
        git_dir.join("rebase-merge").join("message")
    } else {
        draft_path(repo)
    };
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(GitError::io(&path, err)),
    }
}

fn draft_path(repo: &Repo) -> PathBuf {
    repo.git_dir().join("gitcthulhu").join("commit-draft")
}

/// A merge, cherry-pick or revert is in progress (Git takes the message from `MERGE_MSG`).
fn merge_like_state(repo: &Repo) -> bool {
    matches!(
        repo.local().state(),
        Some(
            InProgress::Merge
                | InProgress::CherryPick
                | InProgress::CherryPickSequence
                | InProgress::Revert
                | InProgress::RevertSequence
        )
    )
}

/// A rebase stopped at an `edit` (the sequencer writes `rebase-merge/amend` only for that).
fn rebase_stopped_for_amend(repo: &Repo) -> bool {
    let dir = repo.git_dir().join("rebase-merge");
    matches!(
        repo.local().state(),
        Some(InProgress::Rebase | InProgress::RebaseInteractive)
    ) && dir.join("amend").is_file()
        && dir.join("message").is_file()
}

fn remove_file_if_exists(path: &std::path::Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(GitError::io(path, err)),
    }
}

/// HEAD peeled to a commit, read fresh after the command exited.
fn head_commit(repo: &Repo) -> Result<ObjectId> {
    Ok(repo.local().head_id().map_err(gix_error)?.detach())
}

fn commit_summary(repo: &Repo, id: ObjectId) -> Result<String> {
    let gix_repo = repo.local();
    let commit = gix_repo.find_commit(id).map_err(gix_error)?;
    let message = commit.message_raw_sloppy();
    let first_line = message.lines().next().unwrap_or_default();
    Ok(String::from_utf8_lossy(first_line).into_owned())
}
