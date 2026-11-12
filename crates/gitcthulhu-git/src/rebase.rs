//! Rebase, plain and interactive (WP-B7, spec §2.6 and §5).
//!
//! The interactive rebase is planned in memory ([`plan_interactive`]), edited by the GUI and then handed to
//! `git rebase -i`: Git's own todo list is replaced by ours through the editor helper after a check that both name
//! the same commits, and reworded messages are served through the same helper (`GIT_EDITOR`).
//!
//! Stops (conflicts, `edit`, `break`, a failed `exec`) are reported as `Ok(RebaseOutcome::Stopped)` because the
//! operation is half-way and the GUI continues, skips or aborts it.

use std::path::{Path, PathBuf};

use gix::ObjectId;

use crate::{
    GitError, GitErrorKind, GitVersion, InProgress, OpClass, OpContext, OpOutcome, RebaseAction,
    RebaseOptions, RebaseOutcome, RebasePlan, RebaseStep, RebaseStopReason, RefreshSet, RelaPath,
    Repo, RepoExclusive, Result, TodoAction, TodoLine,
    cli::{self, CommandKind, EditorMode, GitCommand},
    merge::{Collected, check_cancel, current_operation, head_commit_id},
};

/// The first Git that knows `--no-update-refs`.
const UPDATE_REFS_VERSION: GitVersion = GitVersion {
    major: 2,
    minor: 38,
    patch: 0,
};

fn other<E: std::error::Error + Send + Sync + 'static>(err: E) -> GitError {
    GitError::Other(anyhow::Error::new(err))
}

/// `<git_dir>/gitcthulhu/rebase`: the plan of the interactive rebase this application started.
fn plan_dir(repo: &Repo) -> PathBuf {
    repo.git_dir().join("gitcthulhu").join("rebase")
}

fn is_rebase(state: Option<InProgress>) -> bool {
    matches!(
        state,
        Some(InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase)
    )
}

fn rebase_in_progress(repo: &Repo) -> bool {
    is_rebase(current_operation(repo))
}

/// Delete a plan directory that outlived its rebase (the user finished it in a terminal).
fn remove_plan_dir(repo: &Repo) {
    // Best effort: a leftover directory is replaced by the next start.
    std::fs::remove_dir_all(plan_dir(repo)).ok();
}

fn cleanup_stale_plan_dir(repo: &Repo) {
    if !rebase_in_progress(repo) {
        remove_plan_dir(repo);
    }
}

fn no_rebase() -> GitError {
    GitError::NoOperationInProgress {
        expected: Some(InProgress::Rebase),
    }
}

// ───────────────────────────── state after a command ─────────────────────────────

/// `rebase-merge/` if present, else `rebase-apply/`.
fn rebase_state_dir(repo: &Repo) -> PathBuf {
    let merge = repo.git_dir().join("rebase-merge");
    if merge.is_dir() {
        merge
    } else {
        repo.git_dir().join("rebase-apply")
    }
}

/// Changes whenever the rebase advances to another step.
fn progress_marker(repo: &Repo) -> Option<Vec<u8>> {
    let dir = rebase_state_dir(repo);
    std::fs::read(dir.join("done"))
        .or_else(|_| std::fs::read(dir.join("next")))
        .ok()
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&text).trim().to_owned())
}

/// Paths with unmerged index entries.
fn conflicted_paths(repo: &Repo) -> Result<Vec<RelaPath>> {
    let index = repo.local().index_or_empty().map_err(other)?;
    let mut paths: Vec<RelaPath> = Vec::new();
    for entry in index.entries() {
        if entry.stage() == gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let path = entry.path(&index);
        if paths
            .last()
            .is_none_or(|last| last.as_slice() != path.as_ref() as &[u8])
        {
            paths.push(path.to_owned());
        }
    }
    Ok(paths)
}

fn resolve(repo: &Repo, rev: &str) -> Option<ObjectId> {
    if rev.is_empty() {
        return None;
    }
    repo.local()
        .rev_parse_single(rev)
        .ok()
        .map(|id| id.detach())
}

/// The commit of the last `done` line that names one.
fn last_done_commit(repo: &Repo) -> Option<String> {
    let done = std::fs::read(rebase_state_dir(repo).join("done")).ok()?;
    let done = String::from_utf8_lossy(&done);
    let line = done
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    let mut tokens = line.split_whitespace();
    let command = tokens.next()?;
    if matches!(
        command,
        "exec" | "x" | "break" | "b" | "label" | "l" | "reset" | "t" | "noop"
    ) {
        return None;
    }
    tokens
        .find(|t| !t.starts_with('-') && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
}

/// Why the rebase in progress stopped, derived like Git's own prompt does.
fn stop_reason(repo: &Repo, conflicted: bool) -> RebaseStopReason {
    if conflicted {
        return RebaseStopReason::Conflict;
    }
    let dir = rebase_state_dir(repo);
    if dir.join("amend").exists() {
        return RebaseStopReason::Edit;
    }
    let done = std::fs::read(dir.join("done")).unwrap_or_default();
    let done = String::from_utf8_lossy(&done);
    let command = done
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .and_then(|l| l.split_whitespace().next());
    match command {
        Some("edit" | "e") => RebaseStopReason::Edit,
        Some("break" | "b") => RebaseStopReason::Break,
        Some("exec" | "x") => RebaseStopReason::ExecFailed,
        Some(_) => RebaseStopReason::ResolvedNeedsContinue,
        None if dir.join("next").exists() => RebaseStopReason::ResolvedNeedsContinue,
        None => RebaseStopReason::Unknown,
    }
}

/// The outcome of a rebase command that exited (successfully or stopped): finished, or stopped somewhere.
fn current_outcome(repo: &Repo) -> Result<RebaseOutcome> {
    if !rebase_in_progress(repo) {
        return Ok(RebaseOutcome::Completed {
            new_head: head_commit_id(repo)?,
        });
    }
    let conflicts = conflicted_paths(repo)?;
    let at = read_trimmed(&repo.git_dir().join("REBASE_HEAD"))
        .and_then(|s| resolve(repo, &s))
        .or_else(|| {
            read_trimmed(&rebase_state_dir(repo).join("stopped-sha"))
                .and_then(|s| resolve(repo, &s))
        })
        .or_else(|| last_done_commit(repo).and_then(|s| resolve(repo, &s)));
    Ok(RebaseOutcome::Stopped {
        reason: stop_reason(repo, !conflicts.is_empty()),
        at,
        conflicts,
    })
}

/// Run a rebase command and turn its result into an outcome.
///
/// A failure with the rebase still in progress is a stop, not an error, provided the rebase advanced (`before` is
/// the marker taken before the command, `None` for a start). A `--continue` that made no progress keeps its error
/// (`Conflicts`: the conflicts are not resolved yet).
fn run_rebase(
    repo: &Repo,
    ctx: &OpContext,
    cmd: GitCommand,
    before: Option<Option<Vec<u8>>>,
    collected: &mut Collected,
) -> Result<RebaseOutcome> {
    match cli::run_checked(repo, cmd, ctx, OpClass::Rebase) {
        Ok(out) => {
            collected.add(out);
            current_outcome(repo)
        }
        Err(err) => {
            let stoppable = matches!(
                err.kind(),
                GitErrorKind::Conflicts | GitErrorKind::EmptyPick | GitErrorKind::CommandFailed
            );
            let advanced = match &before {
                None => true,
                Some(marker) => *marker != progress_marker(repo),
            };
            if stoppable && advanced && rebase_in_progress(repo) {
                return current_outcome(repo);
            }
            // `git rebase --continue` refuses unresolved conflicts with "<path>: needs merge / You must edit all
            // merge conflicts", a text the classifier does not know: report it like the other operations do.
            if !advanced && err.kind() == GitErrorKind::CommandFailed && rebase_in_progress(repo) {
                let paths = conflicted_paths(repo)?;
                if !paths.is_empty() {
                    return Err(GitError::Conflicts {
                        operation: current_operation(repo),
                        paths,
                        stderr: err.stderr().unwrap_or_default().to_owned(),
                    });
                }
            }
            Err(err)
        }
    }
}

/// The editor Git gets for a rebase command: our helper when it exists and a plan is present, else `:`.
fn editor_for(repo: &Repo) -> EditorMode {
    let dir = plan_dir(repo);
    if repo.runtime().has_editor_helper() && dir.is_dir() {
        EditorMode::RebaseHelper { plan_dir: dir }
    } else {
        EditorMode::Noop
    }
}

// ───────────────────────────── plain rebase ─────────────────────────────

/// `git rebase <upstream>`.
///
/// Conflicts and other stops are `Ok(Stopped)`; the GUI continues, skips or aborts.
pub fn rebase(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    upstream: &str,
    opts: &RebaseOptions,
) -> Result<OpOutcome<RebaseOutcome>> {
    let _ = perm;
    check_cancel(ctx)?;
    if upstream.trim().is_empty() || upstream.starts_with('-') {
        return Err(GitError::InvalidName {
            what: "rebase upstream",
            name: upstream.to_owned(),
        });
    }
    cleanup_stale_plan_dir(repo);

    let mut cmd = GitCommand::new(repo, CommandKind::Mutation)
        .arg("rebase")
        .with_hooks()
        .editor(EditorMode::Noop);
    if let Some(onto) = opts.onto {
        cmd = cmd.arg("--onto").arg(onto.to_string());
    }
    if opts.autostash {
        cmd = cmd.arg("--autostash");
    }
    if opts.rebase_merges {
        cmd = cmd.arg("--rebase-merges");
    }
    if opts.skip_hooks {
        cmd = cmd.arg("--no-verify");
    }
    cmd = cmd.arg(upstream);

    let mut collected = Collected::default();
    let value = run_rebase(repo, ctx, cmd, None, &mut collected)?;
    Ok(collected.finish(value, RefreshSet::OPERATION_STATE))
}

// ───────────────────────────── planning ─────────────────────────────

/// The plan of an interactive rebase onto `base`; read-only.
///
/// `base = None` rebases everything (`--root`). Merge commits in the range are not listed (Git linearises them) and
/// counted in `merges_dropped`.
pub fn plan_interactive(repo: &Repo, base: Option<ObjectId>) -> Result<RebasePlan> {
    let head = head_commit_id(repo)?;
    let local = repo.local();
    if let Some(base) = base {
        let invalid = || GitError::InvalidName {
            what: "rebase base (not an ancestor of HEAD)",
            name: base.to_string(),
        };
        if local.find_commit(base).is_err() {
            return Err(invalid());
        }
        let merge_base = local.merge_base(head, base).map_err(|_| invalid())?;
        if merge_base.map(|id| id.detach()) != Some(base) {
            return Err(invalid());
        }
    }

    let mut steps = Vec::new();
    let mut merges_dropped = 0;
    // gix's topo walk still yields a tip that is also an end, so `base == HEAD` (nothing to rebase) is not walked.
    let walk = if base == Some(head) {
        None
    } else {
        let topo = gix::traverse::commit::topo::Builder::from_iters(
            &local.objects,
            [head],
            base.map(|b| [b]),
        )
        .build()
        .map_err(|err| GitError::Other(anyhow::anyhow!("{err}")))?;
        Some(topo)
    };
    for info in walk.into_iter().flatten() {
        let info = info.map_err(|err| GitError::Other(anyhow::anyhow!("{err}")))?;
        if info.parent_ids.len() > 1 {
            merges_dropped += 1;
            continue;
        }
        let commit = local.find_commit(info.id).map_err(other)?;
        let message = String::from_utf8_lossy(commit.message_raw_sloppy().as_ref()).into_owned();
        let summary = message.lines().next().unwrap_or_default().trim().to_owned();
        steps.push(RebaseStep {
            action: RebaseAction::Pick,
            commit: info.id,
            summary,
            message,
            new_message: None,
        });
    }
    steps.reverse();
    Ok(RebasePlan {
        head,
        head_ref: local.head_name().map_err(other)?,
        base,
        steps,
        merges_dropped,
    })
}

// ───────────────────────────── interactive rebase ─────────────────────────────

fn action_word(action: RebaseAction) -> &'static str {
    match action {
        RebaseAction::Pick => "pick",
        RebaseAction::Reword => "reword",
        RebaseAction::Edit => "edit",
        RebaseAction::Squash => "squash",
        RebaseAction::Fixup => "fixup",
        RebaseAction::Drop => "drop",
    }
}

/// The todo list of a plan: one `<action> <sha> <summary>` line per step, LF terminated. Drops stay as explicit
/// lines so `rebase.missingCommitsCheck=error` does not fire. `actions` are the ones Git gets ([`prepare_plan`]).
/// An empty plan is a `noop` (an empty todo makes Git fail with "nothing to do").
fn render_plan_todo(plan: &RebasePlan, actions: &[RebaseAction]) -> String {
    let mut todo = String::new();
    for (step, action) in plan.steps.iter().zip(actions) {
        let summary = step.summary.lines().next().unwrap_or_default();
        todo.push_str(&format!(
            "{} {} {}\n",
            action_word(*action),
            step.commit,
            summary
        ));
    }
    if todo.is_empty() {
        todo.push_str("noop\n");
    }
    todo
}

/// The first step that is not a drop must keep a commit that a squash/fixup can meld into.
fn validate_plan(plan: &RebasePlan) -> Result<()> {
    let first = plan
        .steps
        .iter()
        .find(|s| s.action != RebaseAction::Drop)
        .map(|s| s.action);
    if matches!(first, Some(RebaseAction::Squash | RebaseAction::Fixup)) {
        return Err(GitError::InvalidName {
            what: "rebase plan (squash/fixup without a preceding commit)",
            name: String::new(),
        });
    }
    Ok(())
}

/// The actions Git gets for the plan's steps, and the messages the editor helper serves, keyed by the full id of
/// the step during which Git asks for the message.
///
/// A group is a leading step plus the squash/fixup steps melded into it (drops in between are no-ops, as for Git).
/// Its message is the last `new_message` set in the group. Git asks for a message only
/// - at the last step of a group that contains a `squash` (the combined message), and
/// - at a `reword` step.
///
/// A group without a squash keeps its leading commit's message, so its message is served at the leading step, which
/// becomes a `reword` if it was a `pick`; otherwise Git would never ask and the message would be lost. An `edit`
/// leader is left alone: the GUI amends the commit at the stop.
fn prepare_plan(plan: &RebasePlan) -> (Vec<RebaseAction>, Vec<(ObjectId, String)>) {
    let mut actions: Vec<RebaseAction> = plan.steps.iter().map(|s| s.action).collect();
    let kept: Vec<usize> = (0..plan.steps.len())
        .filter(|&i| plan.steps[i].action != RebaseAction::Drop)
        .collect();
    let mut messages = Vec::new();
    let mut start = 0;
    while start < kept.len() {
        let mut end = start + 1;
        while end < kept.len()
            && matches!(
                plan.steps[kept[end]].action,
                RebaseAction::Squash | RebaseAction::Fixup
            )
        {
            end += 1;
        }
        let group = &kept[start..end];
        start = end;
        let Some(message) = group
            .iter()
            .rev()
            .find_map(|&i| plan.steps[i].new_message.as_ref())
        else {
            continue;
        };
        let has_squash = group[1..]
            .iter()
            .any(|&i| plan.steps[i].action == RebaseAction::Squash);
        if has_squash {
            let last = group[group.len() - 1];
            messages.push((plan.steps[last].commit, message.clone()));
            continue;
        }
        let leader = group[0];
        if actions[leader] == RebaseAction::Pick {
            actions[leader] = RebaseAction::Reword;
        }
        if actions[leader] == RebaseAction::Reword {
            messages.push((plan.steps[leader].commit, message.clone()));
        }
    }
    (actions, messages)
}

fn write_plan_dir(repo: &Repo, plan: &RebasePlan) -> Result<PathBuf> {
    let dir = plan_dir(repo);
    let io = |path: &Path| {
        let path = path.to_owned();
        move |err| GitError::io(&path, err)
    };
    std::fs::remove_dir_all(&dir).ok();
    let messages = dir.join("messages");
    std::fs::create_dir_all(&messages).map_err(io(&messages))?;
    let write = |name: &str, content: &str| {
        let path = dir.join(name);
        std::fs::write(&path, content.as_bytes()).map_err(io(&path))
    };
    let (actions, messages_by_step) = prepare_plan(plan);
    write("todo", &render_plan_todo(plan, &actions))?;
    let expected: String = plan
        .steps
        .iter()
        .map(|s| format!("{}\n", s.commit))
        .collect();
    write("expected", &expected)?;
    for (key, message) in messages_by_step {
        let path = messages.join(key.to_string());
        let mut text = message;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        std::fs::write(&path, text.as_bytes()).map_err(io(&path))?;
    }
    // `plan.json` also marks the rebase as started by us (`RebaseState::started_by_us`), so it is written in every
    // build; without the `serde` feature it carries only the ids.
    #[cfg(feature = "serde")]
    let json = serde_json::to_vec_pretty(plan).map_err(other)?;
    #[cfg(not(feature = "serde"))]
    let json = serde_json::to_vec_pretty(&serde_json::json!({
        "head": plan.head.to_string(),
        "base": plan.base.map(|id| id.to_string()),
        "steps": plan.steps.iter().map(|s| s.commit.to_string()).collect::<Vec<_>>(),
    }))
    .map_err(other)?;
    let path = dir.join("plan.json");
    std::fs::write(&path, json).map_err(io(&path))?;
    Ok(dir)
}

/// Run `git rebase -i` with the edited `plan`.
///
/// Errors: `OperationInProgress`, `RebasePlanStale` (HEAD moved, or Git would rebase a different set of commits;
/// the rebase is aborted then), `Unsupported` (editor helper missing), `InvalidName` (squash/fixup first).
pub fn start_interactive(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    plan: &RebasePlan,
    opts: &RebaseOptions,
) -> Result<OpOutcome<RebaseOutcome>> {
    let _ = perm;
    check_cancel(ctx)?;
    if let Some(kind) = current_operation(repo) {
        return Err(GitError::OperationInProgress {
            kind,
            stderr: String::new(),
        });
    }
    // A different branch at the same commit counts as moved: the rebase would rewrite the wrong branch.
    if head_commit_id(repo)? != plan.head
        || repo.local().head_name().map_err(other)? != plan.head_ref
    {
        return Err(GitError::RebasePlanStale {
            detail: "HEAD moved".to_owned(),
        });
    }
    if !repo.runtime().has_editor_helper() {
        return Err(GitError::Unsupported {
            what: "editor helper missing",
        });
    }
    validate_plan(plan)?;

    let dir = write_plan_dir(repo, plan)?;
    let mut cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(["rebase", "-i", "--no-autosquash"])
        .with_hooks()
        .editor(EditorMode::RebaseHelper {
            plan_dir: dir.clone(),
        });
    if opts.autostash {
        cmd = cmd.arg("--autostash");
    }
    if opts.skip_hooks {
        cmd = cmd.arg("--no-verify");
    }
    if repo.runtime().version() >= UPDATE_REFS_VERSION {
        cmd = cmd.arg("--no-update-refs");
    }
    if let Some(onto) = opts.onto {
        cmd = cmd.arg("--onto").arg(onto.to_string());
    }
    cmd = match plan.base {
        Some(base) => cmd.arg(base.to_string()),
        None => cmd.arg("--root"),
    };

    let mut collected = Collected::default();
    let result = run_rebase(repo, ctx, cmd, None, &mut collected);

    if dir.join("mismatch").exists() {
        if rebase_in_progress(repo) {
            let abort = GitCommand::new(repo, CommandKind::Mutation).args(["rebase", "--abort"]);
            // The plan is already refused; a failing abort must not hide that.
            cli::run_checked(repo, abort, ctx, OpClass::Rebase).ok();
        }
        remove_plan_dir(repo);
        return Err(GitError::RebasePlanStale {
            detail: "Git would rebase a different set of commits".to_owned(),
        });
    }
    match result {
        Ok(outcome) => {
            if matches!(outcome, RebaseOutcome::Completed { .. }) {
                remove_plan_dir(repo);
            }
            Ok(collected.finish(outcome, RefreshSet::OPERATION_STATE))
        }
        Err(err) => {
            cleanup_stale_plan_dir(repo);
            Err(err)
        }
    }
}

/// Make the message of the stopped step available to the editor helper.
fn stage_message(repo: &Repo, message: &str) -> Result<()> {
    let Some(sha) = last_done_commit(repo).and_then(|s| resolve(repo, &s)) else {
        return Ok(());
    };
    let dir = plan_dir(repo).join("messages");
    std::fs::create_dir_all(&dir).map_err(|err| GitError::io(&dir, err))?;
    let path = dir.join(sha.to_string());
    let mut text = message.to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    std::fs::write(&path, text.as_bytes()).map_err(|err| GitError::io(&path, err))
}

fn control(
    repo: &Repo,
    ctx: &OpContext,
    flag: &str,
    message: Option<&str>,
) -> Result<OpOutcome<RebaseOutcome>> {
    check_cancel(ctx)?;
    if !rebase_in_progress(repo) {
        remove_plan_dir(repo);
        return Err(no_rebase());
    }
    if let Some(message) = message {
        stage_message(repo, message)?;
    }
    let before = progress_marker(repo);
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(["rebase", flag])
        .with_hooks()
        .editor(editor_for(repo));
    let mut collected = Collected::default();
    let value = run_rebase(repo, ctx, cmd, Some(before), &mut collected)?;
    if matches!(value, RebaseOutcome::Completed { .. }) {
        remove_plan_dir(repo);
    }
    Ok(collected.finish(value, RefreshSet::OPERATION_STATE))
}

/// `git rebase --continue`.
///
/// Never amends (the GUI calls `commit::commit` with `amend` at an `edit` stop first). `message` is used when Git
/// asks for a message of the stopped step (conflict on `reword`/`squash`/`fixup -c`, or any resolved conflict).
/// Unresolved conflicts: `Err(Conflicts)`.
pub fn continue_rebase(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    message: Option<&str>,
) -> Result<OpOutcome<RebaseOutcome>> {
    let _ = perm;
    control(repo, ctx, "--continue", message)
}

/// `git rebase --skip`.
pub fn skip_rebase(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<RebaseOutcome>> {
    let _ = perm;
    control(repo, ctx, "--skip", None)
}

/// `git rebase --abort`, then delete the plan directory.
pub fn abort_rebase(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    check_cancel(ctx)?;
    if !rebase_in_progress(repo) {
        remove_plan_dir(repo);
        return Err(no_rebase());
    }
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args(["rebase", "--abort"])
        .with_hooks();
    let mut collected = Collected::default();
    collected.add(cli::run_checked(repo, cmd, ctx, OpClass::Rebase)?);
    remove_plan_dir(repo);
    Ok(collected.finish((), RefreshSet::OPERATION_STATE))
}

// ───────────────────────────── editing the remaining todo ─────────────────────────────

/// Todo commands Git knows, long and abbreviated.
const TODO_COMMANDS: &[&str] = &[
    "pick",
    "p",
    "reword",
    "r",
    "edit",
    "e",
    "squash",
    "s",
    "fixup",
    "f",
    "exec",
    "x",
    "break",
    "b",
    "drop",
    "d",
    "label",
    "l",
    "reset",
    "t",
    "merge",
    "m",
    "update-ref",
    "u",
    "noop",
];

fn invalid_todo_line(line: &str) -> GitError {
    GitError::InvalidName {
        what: "rebase todo line",
        name: line.to_owned(),
    }
}

/// Whether `text` is one todo line Git accepts: a comment, or a known command with the argument it needs
/// (`fixup -C|-c` needs a commit after the flag). A line break would smuggle in unchecked lines.
fn is_valid_todo_line(text: &str) -> bool {
    if text.contains(['\n', '\r']) {
        return false;
    }
    let text = text.trim();
    if text.starts_with('#') {
        return true;
    }
    let mut tokens = text.split_whitespace();
    let Some(command) = tokens.next() else {
        return false;
    };
    if !TODO_COMMANDS.contains(&command) {
        return false;
    }
    if matches!(command, "break" | "b" | "noop") {
        return true;
    }
    match tokens.next() {
        None => false,
        Some("-C" | "-c") if matches!(command, "fixup" | "f" | "merge" | "m") => {
            tokens.next().is_some()
        }
        Some(_) => true,
    }
}

/// One todo line as written to the file.
fn render_todo_line(line: &TodoLine) -> Result<String> {
    let text = render_todo_text(line)?;
    if is_valid_todo_line(&text) {
        Ok(text)
    } else {
        Err(invalid_todo_line(&text))
    }
}

fn render_todo_text(line: &TodoLine) -> Result<String> {
    let raw = line.raw.trim_end();
    if !raw.trim().is_empty() {
        return Ok(raw.to_owned());
    }
    let (word, flag) = match &line.action {
        TodoAction::Pick => ("pick", None),
        TodoAction::Reword => ("reword", None),
        TodoAction::Edit => ("edit", None),
        TodoAction::Squash => ("squash", None),
        TodoAction::Fixup(crate::FixupFlag::None) => ("fixup", None),
        TodoAction::Fixup(crate::FixupFlag::UseMessage) => ("fixup", Some("-C")),
        TodoAction::Fixup(crate::FixupFlag::EditMessage) => ("fixup", Some("-c")),
        TodoAction::Exec => ("exec", None),
        TodoAction::Break => ("break", None),
        TodoAction::Drop => ("drop", None),
        TodoAction::Label => ("label", None),
        TodoAction::Reset => ("reset", None),
        TodoAction::Merge => ("merge", None),
        TodoAction::UpdateRef => ("update-ref", None),
        TodoAction::Noop => ("noop", None),
        TodoAction::Comment | TodoAction::Unknown(_) => return Err(invalid_todo_line(&line.rest)),
    };
    let mut text = word.to_owned();
    if let Some(flag) = flag {
        text.push(' ');
        text.push_str(flag);
    }
    if let Some(commit) = line.commit {
        text.push(' ');
        text.push_str(&commit.to_string());
    } else if let Some(abbrev) = &line.abbrev {
        text.push(' ');
        text.push_str(abbrev);
    } else if matches!(
        line.action,
        TodoAction::Pick
            | TodoAction::Reword
            | TodoAction::Edit
            | TodoAction::Squash
            | TodoAction::Fixup(_)
            | TodoAction::Drop
    ) {
        // The summary after it would be taken for the commit.
        return Err(invalid_todo_line(&line.rest));
    }
    if !line.rest.is_empty() {
        text.push(' ');
        text.push_str(&line.rest);
    }
    Ok(text)
}

/// Milestone 2: replace the remaining todo of a stopped interactive rebase (`git rebase --edit-todo` equivalent).
///
/// Every line is validated before anything is written; the file is replaced atomically.
pub fn edit_todo(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    todo: &[TodoLine],
) -> Result<OpOutcome<()>> {
    let _ = perm;
    check_cancel(ctx)?;
    let dir = repo.git_dir().join("rebase-merge");
    let target = dir.join("git-rebase-todo");
    if !dir.is_dir() {
        return Err(no_rebase());
    }
    let mut text = String::new();
    for line in todo {
        text.push_str(&render_todo_line(line)?);
        text.push('\n');
    }
    let tmp = dir.join("git-rebase-todo.gitcthulhu-tmp");
    std::fs::write(&tmp, text.as_bytes()).map_err(|err| GitError::io(&tmp, err))?;
    if let Err(err) = std::fs::rename(&tmp, &target) {
        std::fs::remove_file(&tmp).ok();
        return Err(GitError::io(&target, err));
    }
    Ok(Collected::default().finish((), RefreshSet::REPO_STATE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(action: RebaseAction, id: &str, new_message: Option<&str>) -> RebaseStep {
        RebaseStep {
            action,
            commit: ObjectId::from_hex(id.as_bytes()).expect("valid id"),
            summary: "s".into(),
            message: "m".into(),
            new_message: new_message.map(Into::into),
        }
    }

    fn plan(steps: Vec<RebaseStep>) -> RebasePlan {
        RebasePlan {
            head: ObjectId::null(gix::hash::Kind::Sha1),
            head_ref: None,
            base: None,
            steps,
            merges_dropped: 0,
        }
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn squash_group_message_is_keyed_by_last_step() {
        let p = plan(vec![
            step(RebaseAction::Pick, A, Some("combined")),
            step(RebaseAction::Squash, B, None),
            step(RebaseAction::Drop, C, None),
        ]);
        let (actions, messages) = prepare_plan(&p);
        assert_eq!(messages.len(), 1, "one group, one message");
        assert_eq!(
            messages[0].0.to_string(),
            B,
            "keyed by the last executed step of the group (a trailing drop does not count)"
        );
        assert_eq!(
            actions,
            [RebaseAction::Pick, RebaseAction::Squash, RebaseAction::Drop],
            "a squash group asks for its message by itself"
        );
    }

    #[test]
    fn fixup_group_message_is_served_at_the_leading_step() {
        let reword = plan(vec![
            step(RebaseAction::Reword, A, Some("reworded")),
            step(RebaseAction::Drop, C, None),
            step(RebaseAction::Fixup, B, None),
        ]);
        let (actions, messages) = prepare_plan(&reword);
        assert_eq!(
            messages
                .iter()
                .map(|(id, m)| (id.to_string(), m.as_str()))
                .collect::<Vec<_>>(),
            [(A.to_owned(), "reworded")],
            "Git asks at the reword step; the final fixup keeps that message without asking"
        );
        assert_eq!(
            actions,
            [
                RebaseAction::Reword,
                RebaseAction::Drop,
                RebaseAction::Fixup
            ],
            "actions unchanged"
        );

        let pick = plan(vec![
            step(RebaseAction::Pick, A, None),
            step(RebaseAction::Fixup, B, Some("new message")),
        ]);
        let (actions, messages) = prepare_plan(&pick);
        assert_eq!(
            actions,
            [RebaseAction::Reword, RebaseAction::Fixup],
            "the pick becomes a reword so that Git asks for the message at all"
        );
        assert_eq!(messages.len(), 1, "one message");
        assert_eq!(messages[0].0.to_string(), A, "served at the leading step");
    }

    #[test]
    fn empty_plan_is_a_noop() {
        let p = plan(Vec::new());
        assert_eq!(
            render_plan_todo(&p, &[]),
            "noop\n",
            "an empty todo would make Git fail with 'nothing to do'"
        );
    }

    #[test]
    fn todo_lines_are_validated() {
        for valid in [
            "pick abc123 subject",
            "fixup -C abc123 subject",
            "exec make test",
            "break",
            "noop",
            "# a comment",
            "update-ref refs/heads/x",
        ] {
            assert!(
                is_valid_todo_line(valid),
                "{valid:?} is a todo line Git accepts"
            );
        }
        for invalid in [
            "",
            "pick",
            "fixup -C",
            "exec",
            "frobnicate abc123",
            "pick abc123 one\npick def456 two",
            "pick abc123 x\r",
        ] {
            assert!(!is_valid_todo_line(invalid), "{invalid:?} is refused");
        }
    }

    #[test]
    fn leading_squash_is_rejected() {
        let p = plan(vec![
            step(RebaseAction::Drop, A, None),
            step(RebaseAction::Fixup, B, None),
        ]);
        assert!(
            validate_plan(&p).is_err(),
            "a squash/fixup without a commit before it"
        );
    }

    #[test]
    fn todo_renders_drops_explicitly() {
        let p = plan(vec![
            step(RebaseAction::Pick, A, None),
            step(RebaseAction::Drop, B, None),
        ]);
        assert_eq!(
            render_plan_todo(&p, &prepare_plan(&p).0),
            format!("pick {A} s\ndrop {B} s\n"),
            "one line per step, drops kept"
        );
    }
}
