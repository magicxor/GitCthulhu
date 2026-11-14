//! Clone, fetch, pull and push (WP-B8).
//!
//! Every function here runs the `git` executable through [`crate::cli`], so hooks (pre-push, post-checkout,
//! post-merge), credential helpers, LFS and signing behave natively. Repository reads (remotes, branch
//! configuration, HEAD) use `gix`.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gix::{ObjectId, refs::FullName};

use crate::{
    CloneOptions, FastForwardOutcome, FetchOptions, FetchReport, FetchTarget, ForceMode, GitError,
    GitErrorKind, GitRuntime, MergeOutcome, MergeOutcomeOrRebase, OpClass, OpContext, OpOutcome,
    OpWarning, PullMode, PullOptions, PushOptions, PushRefSpec, PushReport, PushedRefUpdate,
    RebaseOutcome, RefreshSet, Repo, RepoExclusive, Result, TagFetch, TagPush,
    cli::{self, CommandKind, GitCommand, GitOutput, porcelain},
    error::{classify, combined_output},
    internal::{errors::gix_error, outcome::Collected, post_state},
};

// ───────────────────────────── helpers ─────────────────────────────

/// Merge the logs of several finished commands into one outcome.
fn outcome<T>(value: T, refresh: RefreshSet, outs: &[GitOutput]) -> OpOutcome<T> {
    let mut collected = Collected::default();
    for out in outs {
        collected.record(out);
    }
    collected.finish(value, refresh)
}

/// The names of all configured remotes.
fn remote_names(local: &gix::Repository) -> Vec<String> {
    local
        .remote_names()
        .iter()
        .map(|name| name.to_string())
        .collect()
}

/// `Err(NoSuchRemote)` unless `name` is configured; also rejects names that Git would take for an option.
fn require_remote(local: &gix::Repository, name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('-') {
        return Err(GitError::InvalidName {
            what: "remote",
            name: name.to_owned(),
        });
    }
    if remote_names(local).iter().any(|r| r == name) {
        Ok(())
    } else {
        Err(GitError::NoSuchRemote {
            name: name.to_owned(),
        })
    }
}

fn config_string(local: &gix::Repository, key: &str) -> Option<String> {
    local
        .config_snapshot()
        .string(key)
        .map(|value| value.to_string())
}

/// The short name of the branch HEAD points to; `None` when detached.
fn head_branch_short(local: &gix::Repository) -> Result<Option<String>> {
    Ok(local
        .head_name()
        .map_err(gix_error)?
        .map(|name| name.shorten().to_string()))
}

fn head_commit(local: &gix::Repository) -> Option<ObjectId> {
    local.head_id().ok().map(|id| id.detach())
}

/// A `FullName` for `name`, or `InvalidName` when Git would not accept it.
fn full_name(name: &str, what: &'static str) -> Result<FullName> {
    FullName::try_from(name).map_err(|_| GitError::InvalidName {
        what,
        name: name.to_owned(),
    })
}

/// A `refs/<kind>/<name>` string; `name` is taken as is when it already starts with `refs/`.
fn qualified(kind: &str, name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_owned()
    } else {
        format!("refs/{kind}/{name}")
    }
}

// ───────────────────────────── clone ─────────────────────────────

/// `git clone`.
///
/// Validates before spawning (blank url or name, non-empty target), creates missing parent directories and, when the
/// clone fails or is cancelled, removes the target directory if and only if this call created it.
pub fn clone(
    rt: &Arc<GitRuntime>,
    ctx: &OpContext,
    opts: &CloneOptions,
) -> Result<OpOutcome<Repo>> {
    ctx.cancel.check()?;
    if opts.url.trim().is_empty() {
        return Err(GitError::InvalidName {
            what: "url",
            name: opts.url.clone(),
        });
    }
    let target = &opts.directory;
    let dir_name = target
        .file_name()
        .filter(|name| !name.is_empty() && !name.to_string_lossy().trim().is_empty())
        .ok_or_else(|| GitError::InvalidName {
            what: "directory",
            name: target.display().to_string(),
        })?
        .to_owned();
    let existed = target.exists();
    if existed && !is_empty_dir(target)? {
        return Err(GitError::AlreadyExists {
            what: "directory",
            name: target.display().to_string(),
        });
    }

    let parent: PathBuf = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_owned(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(|e| GitError::io(&parent, e))?;

    let mut cmd = GitCommand::global(parent, CommandKind::Network)
        .arg("clone")
        .arg("--progress")
        .with_progress()
        .with_hooks();
    if opts.recurse_submodules {
        cmd = cmd.arg("--recurse-submodules");
    }
    if let Some(branch) = &opts.branch {
        cmd = cmd.arg(format!("--branch={branch}"));
    }
    if let Some(depth) = opts.depth {
        cmd = cmd.arg(format!("--depth={depth}"));
    }
    if opts.bare {
        cmd = cmd.arg("--bare");
    }
    cmd = cmd.arg("--").arg(&opts.url).arg(dir_name);

    let result = cli::run_checked_global(rt, cmd, ctx, OpClass::Clone)
        .and_then(|out| Repo::open(rt, target).map(|repo| (repo, out)));
    match result {
        Ok((repo, out)) => Ok(outcome(repo, RefreshSet::empty(), &[out])),
        Err(err) => {
            if !existed {
                remove_dir_with_retries(target);
            }
            Err(err)
        }
    }
}

fn is_empty_dir(path: &Path) -> Result<bool> {
    if !path.is_dir() {
        // A file in the way is as much "already there" as a populated directory.
        return Ok(false);
    }
    let mut entries = std::fs::read_dir(path).map_err(|e| GitError::io(path, e))?;
    Ok(entries.next().is_none())
}

/// Best-effort removal: on Windows a just-killed process may hold handles for a few more milliseconds.
fn remove_dir_with_retries(path: &Path) {
    for attempt in 0..10 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(err) => {
                tracing::debug!(?err, attempt, path = %path.display(), "removing the failed clone target");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    tracing::warn!(path = %path.display(), "could not remove the directory of a failed clone");
}

// ───────────────────────────── fetch ─────────────────────────────

/// No `perm`: writes only refs/remotes/*, FETCH_HEAD (and refs named by explicit `refspecs`) under Git's own locks
/// (§1.5).
///
/// `FetchTarget::All` fetches every configured remote, one process each; failures are collected per remote and the
/// call fails only if every attempted remote failed. A cancelled credential prompt stops the loop.
pub fn fetch(repo: &Repo, ctx: &OpContext, opts: &FetchOptions) -> Result<OpOutcome<FetchReport>> {
    ctx.cancel.check()?;
    let local = repo.local();
    let remotes = match &opts.target {
        FetchTarget::Remote(name) => {
            require_remote(&local, name)?;
            vec![name.clone()]
        }
        FetchTarget::All => remote_names(&local),
    };
    drop(local);
    if let Some(bad) = opts
        .refspecs
        .iter()
        .find(|s| s.is_empty() || s.starts_with('-'))
    {
        return Err(GitError::InvalidName {
            what: "refspec",
            name: bad.clone(),
        });
    }

    let total = remotes.len();
    let mut outs = Vec::new();
    let mut per_remote: Vec<(String, Result<(), String>)> = Vec::new();
    let mut first_error: Option<GitError> = None;
    let mut failed = 0usize;
    let mut updated_refs = 0usize;
    for (index, remote) in remotes.iter().enumerate() {
        ctx.cancel.check()?;
        let mut remote_ctx = ctx.clone();
        remote_ctx.label = match &opts.target {
            FetchTarget::All => format!("Fetching {remote} ({}/{total})", index + 1),
            FetchTarget::Remote(_) => format!("Fetching {remote}"),
        };
        let cmd = fetch_command(repo, remote, opts);
        match cli::run_checked(repo, cmd, &remote_ctx, OpClass::Fetch) {
            Ok(out) => {
                updated_refs += count_updated_refs(&out.stderr);
                per_remote.push((remote.clone(), Ok(())));
                outs.push(out);
            }
            Err(GitError::Cancelled) => return Err(GitError::Cancelled),
            Err(err) => {
                failed += 1;
                per_remote.push((remote.clone(), Err(error_summary(&err))));
                let stop = matches!(err, GitError::AuthCancelled { .. });
                if first_error.is_none() {
                    first_error = Some(err);
                }
                if stop {
                    break;
                }
            }
        }
    }
    if failed > 0
        && failed == per_remote.len()
        && let Some(err) = first_error
    {
        return Err(err);
    }
    Ok(outcome(
        FetchReport {
            per_remote,
            updated_refs,
        },
        RefreshSet::REMOTES | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::TAGS,
        &outs,
    ))
}

fn fetch_command(repo: &Repo, remote: &str, opts: &FetchOptions) -> GitCommand {
    let mut cmd = GitCommand::new(repo, CommandKind::Network)
        .arg("fetch")
        .arg("--progress")
        .with_progress()
        .with_hooks();
    match opts.prune {
        Some(true) => cmd = cmd.arg("--prune"),
        Some(false) => cmd = cmd.arg("--no-prune"),
        None => {}
    }
    match opts.tags {
        TagFetch::Default => {}
        TagFetch::All => cmd = cmd.arg("--tags"),
        TagFetch::None => cmd = cmd.arg("--no-tags"),
    }
    cmd.arg("--").arg(remote).args(&opts.refspecs)
}

/// Ref update lines of `git fetch` look like ` * [new branch]  main -> origin/main`; up-to-date (`=`) and rejected
/// (`!`) lines are not updates, nor are refs only written to `FETCH_HEAD` or text the remote side printed.
fn count_updated_refs(stderr: &str) -> usize {
    stderr
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| {
            line.contains(" -> ")
                && !line.starts_with(['=', '!'])
                && !line.starts_with("remote:")
                && !line.ends_with("-> FETCH_HEAD")
        })
        .count()
}

/// The message stored per remote in the fetch report.
fn error_summary(err: &GitError) -> String {
    match err.stderr().map(str::trim).filter(|s| !s.is_empty()) {
        Some(stderr) => stderr.to_owned(),
        None => err.to_string(),
    }
}

// ───────────────────────────── pull ─────────────────────────────

/// `git pull`.
///
/// Without an explicit remote/branch the current branch must have an upstream (`NoUpstream` otherwise). An
/// unconfigured divergent pull fails with `PullStrategyRequired`; the GUI asks and retries with `Merge`/`Rebase`.
/// Conflicts are `Err(Conflicts)` with the merge or rebase left in progress.
pub fn pull(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    opts: &PullOptions,
) -> Result<OpOutcome<MergeOutcomeOrRebase>> {
    let _ = perm;
    ctx.cancel.check()?;
    let local = repo.local();
    let branch = head_branch_short(&local)?;
    let before = head_commit(&local);

    let remote = match (&opts.remote, &opts.branch) {
        (Some(remote), _) => Some(remote.clone()),
        (None, Some(_)) => Some(
            branch
                .as_deref()
                .and_then(|b| config_string(&local, &format!("branch.{b}.remote")))
                .ok_or_else(|| no_upstream(branch.clone()))?,
        ),
        (None, None) => {
            let has_upstream = branch.as_deref().is_some_and(|b| {
                config_string(&local, &format!("branch.{b}.remote")).is_some()
                    && config_string(&local, &format!("branch.{b}.merge")).is_some()
            });
            if !has_upstream {
                return Err(no_upstream(branch));
            }
            None
        }
    };
    // `.` is the local repository (an upstream that is a local branch), which Git pulls from without a remote.
    if let Some(remote) = remote.as_deref().filter(|r| *r != ".") {
        require_remote(&local, remote)?;
    }
    if let Some(branch) = &opts.branch
        && (branch.is_empty() || branch.starts_with('-'))
    {
        return Err(GitError::InvalidName {
            what: "branch",
            name: branch.clone(),
        });
    }
    let rebases = match opts.mode {
        PullMode::Rebase => true,
        PullMode::Merge | PullMode::FfOnly => false,
        PullMode::FromConfig => config_rebases(&local, branch.as_deref()),
    };
    drop(local);

    let mut cmd = GitCommand::new(repo, CommandKind::Network)
        .args(["pull", "--progress", "--no-edit"])
        .with_progress()
        .with_hooks();
    match opts.mode {
        PullMode::FromConfig => {}
        PullMode::Merge => cmd = cmd.arg("--no-rebase"),
        PullMode::Rebase => cmd = cmd.arg("--rebase"),
        PullMode::FfOnly => cmd = cmd.arg("--ff-only"),
    }
    if opts.autostash {
        cmd = cmd.arg("--autostash");
    }
    if let Some(remote) = &remote {
        cmd = cmd.arg("--").arg(remote);
        if let Some(branch) = &opts.branch {
            cmd = cmd.arg(branch);
        }
    }

    let out = cli::run_checked(repo, cmd, ctx, OpClass::Pull)?;
    // Refs are read fresh from disk; a pull changes no configuration, so no `reload` is needed.
    let after = head_commit(&repo.local());
    let value = if rebases {
        let new_head = after.ok_or(GitError::UnbornHead)?;
        MergeOutcomeOrRebase::Rebase(RebaseOutcome::Completed { new_head })
    } else {
        MergeOutcomeOrRebase::Merge(merge_outcome(&out, before, after)?)
    };
    let mut collected = Collected::default();
    collected.record(&out);
    // A rebase has no outcome variant for it: the pull completed, but the local changes need attention.
    if rebases && out.autostash_conflicted() {
        collected.warn(OpWarning::AutostashConflict {
            output: combined_output(&out),
        });
    }
    Ok(collected.finish(value, RefreshSet::OPERATION_STATE | RefreshSet::REMOTES))
}

/// Pulls the upstream of the local branch `branch` without checking it out: `git fetch -- <remote>
/// <merge>:<branch>`. Git moves the branch only when that is a fast-forward and refuses a branch that a worktree has
/// checked out (that error is returned as is); the fetch also updates the upstream's remote-tracking ref.
///
/// When the branch cannot be fast-forwarded the call still succeeds: the upstream was fetched, and the outcome tells
/// whether the branch is merely ahead of it or has diverged (an upstream without a remote-tracking ref, which Git
/// cannot record, counts as diverged). `NoUpstream` when the branch has none.
pub fn fetch_fast_forward(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    branch: &FullName,
) -> Result<OpOutcome<FastForwardOutcome>> {
    let _ = perm;
    ctx.cancel.check()?;
    let local = repo.local();
    let short = branch.shorten().to_string();
    let upstream = crate::refs::upstream_of(&local, branch, &short)?
        .ok_or_else(|| no_upstream(Some(short.clone())))?;
    if upstream.remote != "." {
        require_remote(&local, &upstream.remote)?;
    }
    let tip = |local: &gix::Repository, name: &FullName| -> Result<Option<ObjectId>> {
        Ok(local
            .try_find_reference(name.as_ref())
            .map_err(gix_error)?
            .and_then(|mut r| r.peel_to_id().ok().map(|id| id.detach())))
    };
    let before = tip(&local, branch)?.ok_or_else(|| GitError::RefNotFound {
        name: branch.to_string(),
        stderr: String::new(),
    })?;
    drop(local);

    let refspec = format!("{}:{}", upstream.merge, branch);
    // `fetch.output=compact` would print the rejected destination as `team/*`: the rejection is parsed by name.
    let cmd = GitCommand::new(repo, CommandKind::Network)
        .arg("-c")
        .arg("fetch.output=full")
        .arg("fetch")
        .arg("--progress")
        .with_progress()
        .with_hooks()
        .arg("--")
        .arg(&upstream.remote)
        .arg(refspec);
    let refresh = RefreshSet::REMOTES | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::TAGS;
    // Unchecked, like `run_push`: a rejection of the branch alone is an outcome, and the command's log and hook
    // warnings belong to it.
    let out = cli::run(repo.runtime(), cmd, ctx)?;
    if out.success() {
        let after = tip(&repo.local(), branch)?.unwrap_or(before);
        let value = if after == before {
            FastForwardOutcome::UpToDate
        } else {
            FastForwardOutcome::FastForwarded {
                from: before,
                to: after,
            }
        };
        return Ok(outcome(value, refresh, &[out]));
    }
    let err = classify(OpClass::Fetch, &out, &post_state::read(repo)?);
    let stopped_early = matches!(
        err.kind(),
        GitErrorKind::Cancelled | GitErrorKind::AuthCancelled | GitErrorKind::HookFailed
    );
    if stopped_early || !only_branch_rejected(&out.stderr, &short) {
        return Err(err);
    }
    let local = repo.local();
    let upstream_tip = match &upstream.tracking {
        Some(tracking) => tip(&local, tracking)?,
        None => None,
    };
    let ahead = match upstream_tip {
        // Unrelated histories have no merge base: they diverged.
        Some(upstream_tip) => {
            local
                .merge_base(upstream_tip, before)
                .ok()
                .flatten()
                .map(|base| base.detach())
                == Some(upstream_tip)
        }
        None => false,
    };
    let value = if ahead {
        FastForwardOutcome::Ahead
    } else {
        FastForwardOutcome::Diverged
    };
    Ok(outcome(value, refresh, &[out]))
}

/// Whether the only thing that failed in a fetch is the non-fast-forward update of the local branch `branch`, which
/// `git fetch` reports as ` ! [rejected]  main -> main  (non-fast-forward)`. Any other rejected ref (a tag that would
/// be clobbered, say) or error line keeps the fetch a failure.
fn only_branch_rejected(stderr: &str, branch: &str) -> bool {
    let mut branch_rejected = false;
    for line in stderr.split(['\r', '\n']).map(str::trim) {
        if line.starts_with("error:") || line.starts_with("fatal:") {
            return false;
        }
        if !line.starts_with('!') {
            continue;
        }
        let ours = line.contains("[rejected]")
            && line.contains("(non-fast-forward)")
            && line
                .split_once(" -> ")
                // Git pads with ASCII spaces, which a refname cannot hold (other whitespace, like U+00A0, it can).
                .is_some_and(|(_, dst)| dst.split_ascii_whitespace().next() == Some(branch));
        if !ours {
            return false;
        }
        branch_rejected = true;
    }
    branch_rejected
}

fn no_upstream(branch: Option<String>) -> GitError {
    GitError::NoUpstream {
        branch,
        stderr: String::new(),
    }
}

/// Whether `branch.<b>.rebase` (or, when unset, `pull.rebase`) makes a plain `git pull` rebase.
fn config_rebases(local: &gix::Repository, branch: Option<&str>) -> bool {
    let snapshot = local.config_snapshot();
    // Booleans follow Git's rules (a bare `rebase` key is true); any other value (`merges`, `interactive`,
    // the old `preserve`) is a rebase variant.
    let setting = |key: &str| -> Option<bool> {
        match snapshot.try_boolean(key) {
            Ok(value) => value,
            Err(_) => Some(true),
        }
    };
    branch
        .and_then(|b| setting(&format!("branch.{b}.rebase")))
        .or_else(|| setting("pull.rebase"))
        .unwrap_or(false)
}

/// Same parse as `merge::merge` (spec 2.6): stdout decides.
fn merge_outcome(
    out: &GitOutput,
    before: Option<ObjectId>,
    after: Option<ObjectId>,
) -> Result<MergeOutcome> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let up_to_date = stdout
        .lines()
        .any(|l| matches!(l.trim(), "Already up to date." | "Already up-to-date."));
    if up_to_date || before == after && before.is_some() {
        return Ok(MergeOutcome::UpToDate);
    }
    let new_head = after.ok_or(GitError::UnbornHead)?;
    if out.autostash_conflicted() {
        return Ok(MergeOutcome::AutostashConflict { new_head });
    }
    if before.is_none() || stdout.lines().any(|l| l.trim() == "Fast-forward") {
        Ok(MergeOutcome::FastForwarded { new_head })
    } else {
        Ok(MergeOutcome::Merged { new_head })
    }
}

// ───────────────────────────── push ─────────────────────────────

/// The push family takes no `perm` (§1.5). Rejections: Err(NonFastForward | LeaseRejected | RemoteRejected) carrying
/// the full `PushReport` (refs that did update included).
pub fn push(repo: &Repo, ctx: &OpContext, opts: &PushOptions) -> Result<OpOutcome<PushReport>> {
    ctx.cancel.check()?;
    let local = repo.local();
    require_remote(&local, &opts.remote)?;
    if opts.refspecs.is_empty() && opts.tags == TagPush::None {
        return Err(GitError::Unsupported {
            what: "push without refspecs",
        });
    }
    let mut args: Vec<OsString> = vec!["push".into(), "--progress".into(), "--porcelain".into()];
    if opts.set_upstream {
        args.push("-u".into());
    }
    args.extend(force_args(
        &local,
        &opts.remote,
        opts.force,
        &opts.refspecs,
    )?);
    match opts.tags {
        TagPush::None => {}
        TagPush::Follow => args.push("--follow-tags".into()),
        TagPush::All => args.push("--tags".into()),
    }
    for option in &opts.push_options {
        args.push("-o".into());
        args.push(option.into());
    }
    if opts.skip_hooks {
        args.push("--no-verify".into());
    }
    args.push("--".into());
    args.push((&opts.remote).into());
    for spec in &opts.refspecs {
        args.push(refspec_arg(spec).into());
    }
    drop(local);

    let mut refresh = RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::LOG;
    if opts.tags != TagPush::None
        || opts
            .refspecs
            .iter()
            .any(|s| s.dst.as_bstr().starts_with(b"refs/tags/"))
    {
        refresh |= RefreshSet::TAGS;
    }
    let cmd = GitCommand::new(repo, CommandKind::Network)
        .args(args)
        .with_progress()
        .with_hooks();
    let result = run_push(repo, ctx, cmd, &opts.remote, refresh);
    if opts.set_upstream {
        // `-u` wrote `branch.<b>.remote/merge` for every ref that was pushed (also when others were rejected);
        // the configuration snapshot of `repo` must see it, or the next pull/push would miss the upstream (§1.4).
        // A failed reload never hides the push's own error.
        if let Err(err) = repo.reload() {
            if result.is_ok() {
                return Err(err);
            }
            tracing::warn!(?err, "reloading the repository after a failed push -u");
        }
    }
    result
}

fn refspec_arg(spec: &PushRefSpec) -> String {
    match &spec.src {
        Some(src) => format!("{}:{}", src.as_bstr(), spec.dst.as_bstr()),
        None => format!(":{}", spec.dst.as_bstr()),
    }
}

/// The arguments that force the push: `--force`, or the per-refspec leases of `ForceMode::WithLease`.
///
/// Per refspec: an explicit `expected_remote` gives `--force-with-lease=<dst>:<sha>`; otherwise an existing
/// tracking ref `refs/remotes/<remote>/<short dst>` gives `--force-with-lease=<dst>` (and, once, for the whole
/// command, `--force-if-includes` unless the refspec is a deletion); otherwise `--force-with-lease=<dst>:` expects
/// the ref to be absent.
fn force_args(
    local: &gix::Repository,
    remote: &str,
    force: ForceMode,
    refspecs: &[PushRefSpec],
) -> Result<Vec<OsString>> {
    match force {
        ForceMode::No => Ok(Vec::new()),
        ForceMode::Force => Ok(vec!["--force".into()]),
        ForceMode::WithLease => {
            let mut args: Vec<OsString> = Vec::new();
            let mut if_includes = false;
            for spec in refspecs {
                let dst = spec.dst.as_bstr().to_string();
                if let Some(expected) = spec.expected_remote {
                    args.push(format!("--force-with-lease={dst}:{expected}").into());
                    continue;
                }
                let tracking = dst
                    .strip_prefix("refs/heads/")
                    .map(|short| format!("refs/remotes/{remote}/{short}"));
                let has_tracking_ref = match tracking {
                    Some(name) => local
                        .try_find_reference(name.as_str())
                        .map_err(gix_error)?
                        .is_some(),
                    None => false,
                };
                if has_tracking_ref {
                    args.push(format!("--force-with-lease={dst}").into());
                    if spec.src.is_some() {
                        if_includes = true;
                    }
                } else {
                    args.push(format!("--force-with-lease={dst}:").into());
                }
            }
            if if_includes {
                args.push("--force-if-includes".into());
            }
            Ok(args)
        }
    }
}

/// Run a push command, parse the porcelain output and classify rejections.
///
/// Order (§1.9): cancellation (from `cli::run`), a refused credential prompt and a failing pre-push hook come
/// first; then any `!` line of the porcelain output (rows 11-13); only then the generic classifier. A `!` line is a
/// rejection even if Git exited with 0.
fn run_push(
    repo: &Repo,
    ctx: &OpContext,
    cmd: GitCommand,
    remote: &str,
    refresh: RefreshSet,
) -> Result<OpOutcome<PushReport>> {
    let out = cli::run(repo.runtime(), cmd, ctx)?;
    let parsed = porcelain::parse(&out.stdout);
    let report = build_report(repo, remote, &parsed);
    let classified = if out.exit_code == Some(0) {
        None
    } else {
        let err = classify(OpClass::Push, &out, &post_state::read(repo)?);
        if matches!(
            err.kind(),
            GitErrorKind::Cancelled | GitErrorKind::AuthCancelled | GitErrorKind::HookFailed
        ) {
            return Err(err);
        }
        Some(err)
    };
    if let Some(err) = porcelain::rejection(&parsed, report.clone(), out.stderr.clone()) {
        return Err(err);
    }
    match classified {
        Some(err) => Err(err),
        None => Ok(outcome(report, refresh, &[out])),
    }
}

/// Turn the parsed porcelain lines into a report; abbreviated ids are resolved against the local repository
/// (the pushed commit always exists locally; an overwritten old value may not).
fn build_report(repo: &Repo, remote: &str, parsed: &porcelain::PorcelainPush) -> PushReport {
    let local = repo.local();
    let resolve = |spec: &str| -> Option<ObjectId> {
        local.rev_parse_single(spec).ok().map(|id| id.detach())
    };
    let updates = parsed
        .lines
        .iter()
        .filter_map(|line| {
            let remote_ref = FullName::try_from(line.to.as_str()).ok()?;
            let local_ref = FullName::try_from(line.from.as_str()).ok();
            let status = line.status();
            let (old, new) = match status {
                crate::PushRefStatus::Rejected => (None, None),
                crate::PushRefStatus::Deleted => (None, None),
                _ => {
                    let range = line.abbreviated_range();
                    let new = resolve(&line.from).or_else(|| range.and_then(|(_, n)| resolve(n)));
                    let old = match status {
                        crate::PushRefStatus::UpToDate => new,
                        _ => range.and_then(|(o, _)| resolve(o)),
                    };
                    (old, new)
                }
            };
            Some(PushedRefUpdate {
                local: local_ref,
                remote_ref,
                old,
                new,
                status,
                summary: match &line.reason {
                    Some(reason) => format!("{} ({reason})", line.summary),
                    None => line.summary.clone(),
                },
            })
        })
        .collect();
    PushReport {
        remote: remote.to_owned(),
        url: parsed.url.clone(),
        updates,
    }
}

/// Gitnuro "Push" button: current branch to its upstream (or `<default remote>/<same name>` with -u when none).
pub fn push_current_branch(
    repo: &Repo,
    ctx: &OpContext,
    force: ForceMode,
    tags: TagPush,
    skip_hooks: bool,
) -> Result<OpOutcome<PushReport>> {
    let local = repo.local();
    let head = local
        .head_name()
        .map_err(gix_error)?
        .ok_or(GitError::Unsupported {
            what: "push from detached HEAD",
        })?;
    if head_commit(&local).is_none() {
        return Err(GitError::UnbornHead);
    }
    let short = head.shorten().to_string();
    let upstream_remote =
        config_string(&local, &format!("branch.{short}.remote")).filter(|remote| remote != ".");
    let upstream_merge = config_string(&local, &format!("branch.{short}.merge"));

    let (remote, dst, set_upstream) = match (upstream_remote, upstream_merge) {
        (Some(remote), Some(merge)) => (remote, full_name(&merge, "branch")?, false),
        _ => {
            let remote = default_push_remote(&local, &short)?;
            (remote, head.clone(), true)
        }
    };
    drop(local);
    let opts = PushOptions {
        remote,
        refspecs: vec![PushRefSpec {
            src: Some(head),
            dst,
            expected_remote: None,
        }],
        force,
        tags,
        set_upstream,
        push_options: Vec::new(),
        skip_hooks,
    };
    push(repo, ctx, &opts)
}

/// `branch.<b>.pushRemote`, `remote.pushDefault`, `origin`, or the only remote.
fn default_push_remote(local: &gix::Repository, branch: &str) -> Result<String> {
    let remotes = remote_names(local);
    let configured = config_string(local, &format!("branch.{branch}.pushRemote"))
        .or_else(|| config_string(local, "remote.pushDefault"));
    if let Some(name) = configured {
        require_remote(local, &name)?;
        return Ok(name);
    }
    if remotes.iter().any(|r| r == "origin") {
        return Ok("origin".to_owned());
    }
    match remotes.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(GitError::NoSuchRemote {
            name: "origin".to_owned(),
        }),
        _ => Err(no_upstream(Some(branch.to_owned()))),
    }
}

/// A push of explicit refspecs without lease logic: used by the delete and tag helpers.
fn simple_push(
    repo: &Repo,
    ctx: &OpContext,
    remote: &str,
    delete: bool,
    refspecs: Vec<String>,
    refresh: RefreshSet,
) -> Result<OpOutcome<PushReport>> {
    ctx.cancel.check()?;
    require_remote(&repo.local(), remote)?;
    let mut cmd = GitCommand::new(repo, CommandKind::Network)
        .args(["push", "--progress", "--porcelain"])
        .with_progress()
        .with_hooks();
    if delete {
        cmd = cmd.arg("--delete");
    }
    let cmd = cmd.arg("--").arg(remote).args(refspecs);
    run_push(repo, ctx, cmd, remote, refresh)
}

/// Delete a branch on the remote.
pub fn delete_remote_branch(
    repo: &Repo,
    ctx: &OpContext,
    remote: &str,
    branch: &str,
) -> Result<OpOutcome<PushReport>> {
    full_name(&qualified("heads", branch), "branch")?;
    simple_push(
        repo,
        ctx,
        remote,
        true,
        vec![qualified("heads", branch)],
        RefreshSet::REMOTES | RefreshSet::LOG,
    )
}

/// Push one tag.
pub fn push_tag(
    repo: &Repo,
    ctx: &OpContext,
    remote: &str,
    tag: &str,
) -> Result<OpOutcome<PushReport>> {
    let name = qualified("tags", tag);
    full_name(&name, "tag")?;
    simple_push(
        repo,
        ctx,
        remote,
        false,
        vec![format!("{name}:{name}")],
        RefreshSet::REMOTES | RefreshSet::TAGS,
    )
}

/// Delete a tag on the remote.
pub fn delete_remote_tag(
    repo: &Repo,
    ctx: &OpContext,
    remote: &str,
    tag: &str,
) -> Result<OpOutcome<PushReport>> {
    let name = qualified("tags", tag);
    full_name(&name, "tag")?;
    simple_push(
        repo,
        ctx,
        remote,
        true,
        vec![name],
        RefreshSet::REMOTES | RefreshSet::TAGS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updated_refs_counts_only_real_updates() {
        let stderr = "From file:///r\n * [new branch]      main       -> origin/main\n   abc..def  dev -> origin/dev\n = [up to date]      x -> origin/x\n ! [rejected]        t -> t (would clobber existing tag)\n - [deleted]         (none)     -> origin/old\n";
        assert_eq!(count_updated_refs(stderr), 3, "new, update and prune count");
        let not_updates =
            "remote: hint: a -> b        \r\n * branch            main       -> FETCH_HEAD\n";
        assert_eq!(
            count_updated_refs(not_updates),
            0,
            "remote text and FETCH_HEAD-only lines are not ref updates"
        );
    }

    #[test]
    fn only_branch_rejected_needs_the_branch_alone() {
        let nbsp = "feature\u{a0}x";
        let rejected = format!(
            "From file:///r\n ! [rejected]        {nbsp}    -> {nbsp}  (non-fast-forward)\n   abc..def  {nbsp} -> origin/{nbsp}\n"
        );
        assert!(
            only_branch_rejected(&rejected, nbsp),
            "a refname may hold U+00A0: only ASCII spaces end the destination"
        );
        assert!(
            !only_branch_rejected(&rejected, "feature"),
            "another branch's rejection is not ours"
        );
        let with_tag =
            format!("{rejected} ! [rejected]        v1  -> v1  (would clobber existing tag)\n");
        assert!(
            !only_branch_rejected(&with_tag, nbsp),
            "a rejected tag keeps the fetch a failure"
        );
        assert!(
            !only_branch_rejected("fatal: couldn't find remote ref refs/heads/x\n", "x"),
            "no rejection line, no branch rejection"
        );
    }

    #[test]
    fn qualified_names() {
        assert_eq!(qualified("heads", "x"), "refs/heads/x", "short name");
        assert_eq!(qualified("tags", "refs/tags/x"), "refs/tags/x", "full name");
    }
}
