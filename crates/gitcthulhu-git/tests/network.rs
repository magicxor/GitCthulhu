//! Tests of `network` (spec 2.6, WP-B8): clone, fetch, pull and push against `file://` bare remotes.

mod support;

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gitcthulhu_git::{
    CancelToken, CloneOptions, FastForwardOutcome, FetchOptions, FetchTarget, ForceMode, GitError,
    GitErrorKind, MergeOutcome, MergeOutcomeOrRebase, NoPrompter, OpContext, OpWarning, Progress,
    PullMode, PullOptions, PushOptions, PushRefSpec, PushRefStatus, RebaseOutcome, RefreshSet,
    Result, TagFetch, TagPush, network,
};
use support::{TestEnv, TestRepo};

fn file_url(path: &Path) -> String {
    format!("file:///{}", path.to_string_lossy().replace('\\', "/"))
}

fn full(name: &str) -> gix::refs::FullName {
    name.try_into().expect("valid ref name")
}

fn ctx_of(repo: &TestRepo) -> OpContext {
    repo.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn assert_kind<T: std::fmt::Debug>(
    result: Result<T>,
    expected: GitErrorKind,
    why: &str,
) -> GitError {
    match result {
        Err(err) => {
            assert_eq!(err.kind(), expected, "{why}: got {err:?}");
            err
        }
        Ok(value) => panic!("{why}: expected {expected:?}, got Ok({value:?})"),
    }
}

/// A work repository with one commit on `main`, pushed to the bare remote `origin` with upstream tracking.
struct World {
    env: TestEnv,
    bare: std::path::PathBuf,
    url: String,
    work: TestRepo,
}

fn world() -> World {
    let env = TestEnv::new();
    let bare = env.bare_remote("origin");
    let url = file_url(&bare);
    let work = env.repo("work");
    work.write("a.txt", "one\n");
    work.commit_all("one");
    work.git(&["remote", "add", "origin", &url]);
    work.git(&["push", "-q", "-u", "origin", "main"]);
    World {
        env,
        bare,
        url,
        work,
    }
}

impl World {
    /// A second clone of the remote, made with plain git.
    fn other(&self, name: &str) -> TestRepo {
        let other = self.env.repo(name);
        other.git(&["remote", "add", "origin", &self.url]);
        other.git(&["fetch", "-q", "origin"]);
        other.git(&["reset", "-q", "--hard", "origin/main"]);
        other.git(&["branch", "-q", "-u", "origin/main"]);
        other
    }

    fn remote_head(&self) -> String {
        self.work
            .git(&["ls-remote", "origin", "refs/heads/main"])
            .split_whitespace()
            .next()
            .expect("ls-remote lists main")
            .to_owned()
    }
}

fn spec(src: &str, dst: &str) -> PushRefSpec {
    PushRefSpec {
        src: Some(full(src)),
        dst: full(dst),
        expected_remote: None,
    }
}

fn push_opts(remote: &str, refspecs: Vec<PushRefSpec>, force: ForceMode) -> PushOptions {
    PushOptions {
        remote: remote.into(),
        refspecs,
        force,
        tags: TagPush::None,
        set_upstream: false,
        push_options: vec![],
        skip_hooks: false,
    }
}

fn fetch_opts(target: FetchTarget) -> FetchOptions {
    FetchOptions {
        target,
        prune: None,
        tags: TagFetch::Default,
        refspecs: vec![],
    }
}

fn pull_opts(mode: PullMode) -> PullOptions {
    PullOptions {
        remote: None,
        branch: None,
        mode,
        autostash: false,
    }
}

fn clone_opts(url: &str, dir: &Path) -> CloneOptions {
    CloneOptions {
        url: url.into(),
        directory: dir.to_owned(),
        branch: None,
        depth: None,
        recurse_submodules: false,
        bare: false,
    }
}

// ───────────────────────────── clone ─────────────────────────────

#[test]
fn clone_with_progress_events() {
    let w = world();
    let target = w.env.path("cloned");
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let ctx = OpContext {
        cancel: CancelToken::new(),
        progress: Arc::new(move |ev: Progress| sink.lock().expect("lock").push(ev)),
        prompter: Arc::new(NoPrompter),
        label: "clone".into(),
    };
    let out = network::clone(&w.env.rt, &ctx, &clone_opts(&w.url, &target)).expect("clone works");
    assert!(
        target.join("a.txt").is_file(),
        "the work tree is checked out"
    );
    assert!(
        same_dir(out.value.workdir().expect("not bare"), &target),
        "the returned repo is the clone"
    );
    let events = events.lock().expect("lock");
    assert!(
        events.iter().any(|e| matches!(e, Progress::Command(_))),
        "the command line is reported: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Progress::Message(_) | Progress::Phase { .. })),
        "stderr is reported: {events:?}"
    );
    assert_eq!(out.log.commands.len(), 1, "one command ran");
}

fn same_dir(a: &Path, b: &Path) -> bool {
    support::same_path(a, b)
}

#[test]
fn clone_into_nonempty_dir_rejected() {
    let w = world();
    let target = w.env.path("occupied");
    std::fs::create_dir_all(&target).expect("dir");
    std::fs::write(target.join("x"), "x").expect("file");
    let ctx = OpContext::silent(CancelToken::new());
    let err = assert_kind(
        network::clone(&w.env.rt, &ctx, &clone_opts(&w.url, &target)),
        GitErrorKind::AlreadyExists,
        "target is not empty",
    );
    assert!(
        matches!(
            err,
            GitError::AlreadyExists {
                what: "directory",
                ..
            }
        ),
        "names the directory: {err:?}"
    );
    assert!(
        target.join("x").is_file(),
        "the existing content is untouched"
    );
    assert_kind(
        network::clone(&w.env.rt, &ctx, &clone_opts("  ", &w.env.path("blank-url"))),
        GitErrorKind::InvalidName,
        "blank url",
    );
}

#[test]
fn clone_into_existing_empty_dir_works_and_failure_keeps_it() {
    let w = world();
    let target = w.env.path("empty");
    std::fs::create_dir_all(&target).expect("dir");
    let ctx = OpContext::silent(CancelToken::new());
    network::clone(&w.env.rt, &ctx, &clone_opts(&w.url, &target)).expect("empty dir is fine");
    assert!(target.join("a.txt").is_file(), "cloned into the empty dir");

    let keep = w.env.path("keep");
    std::fs::create_dir_all(&keep).expect("dir");
    let bad = file_url(&w.env.path("does-not-exist.git"));
    assert!(
        network::clone(&w.env.rt, &ctx, &clone_opts(&bad, &keep)).is_err(),
        "a missing remote fails"
    );
    assert!(
        keep.is_dir(),
        "a directory that existed before is not removed"
    );
}

#[test]
fn clone_failure_removes_created_dir() {
    let w = world();
    let target = w.env.path("nested").join("deeper").join("repo");
    let bad = file_url(&w.env.path("does-not-exist.git"));
    let ctx = OpContext::silent(CancelToken::new());
    assert!(
        network::clone(&w.env.rt, &ctx, &clone_opts(&bad, &target)).is_err(),
        "a missing remote fails"
    );
    assert!(!target.exists(), "the created target is removed again");
}

#[test]
fn clone_cancel_removes_created_dir() {
    let w = world();
    // Make the remote slow so that the cancel arrives while the clone runs.
    w.env.git_global(&[
        "config",
        "--global",
        "uploadpack.packObjectsHook",
        "sleep 20; exec git",
    ]);
    let target = w.env.path("cancelled");
    let cancel = CancelToken::new();
    // Cancel only once Git has created the target directory, so the cleanup of a directory Git populated is tested
    // (cancelling before the spawn would leave nothing to clean up).
    let canceller = {
        let token = cancel.clone();
        let target = target.clone();
        std::thread::spawn(move || {
            let created = wait_until(Duration::from_secs(10), || target.is_dir());
            std::thread::sleep(Duration::from_millis(300));
            token.cancel();
            created
        })
    };
    let ctx = OpContext::silent(cancel);
    let started = Instant::now();
    assert_kind(
        network::clone(&w.env.rt, &ctx, &clone_opts(&w.url, &target)),
        GitErrorKind::Cancelled,
        "cancelled clone",
    );
    assert!(
        canceller.join().expect("canceller thread"),
        "git created the target directory before the cancel"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "cancel kills the process tree instead of waiting for the 20 s hook"
    );
    assert!(
        !target.exists(),
        "the directory this call created is removed"
    );
}

/// Poll `cond` every 20 ms until it holds or `timeout` passes.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

#[test]
fn fetch_cancel_returns_cancelled() {
    let w = world();
    let other = w.other("other");
    w.work.write("b.txt", "b\n");
    w.work.commit_all("two");
    w.work.git(&["push", "-q", "origin", "main"]);
    // A slow remote: upload-pack sleeps before packing.
    w.env.git_global(&[
        "config",
        "--global",
        "uploadpack.packObjectsHook",
        "sleep 20; exec git",
    ]);
    let repo = other.open();
    let cancel = CancelToken::new();
    let canceller = {
        let token = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            token.cancel();
        })
    };
    let started = Instant::now();
    assert_kind(
        network::fetch(
            &repo,
            &other.op_ctx(cancel, Arc::new(NoPrompter)),
            &fetch_opts(FetchTarget::Remote("origin".into())),
        ),
        GitErrorKind::Cancelled,
        "cancelled fetch",
    );
    canceller.join().expect("canceller thread");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "cancel kills the process tree instead of waiting for the 20 s hook"
    );
    assert_ne!(
        other.git(&["rev-parse", "origin/main"]),
        w.work.git(&["rev-parse", "main"]),
        "the tracking ref was not updated"
    );
}

#[test]
fn clone_recurse_submodules() {
    let w = world();
    let sub = w.env.repo("sub");
    sub.write("s.txt", "sub\n");
    sub.commit_all("sub one");
    w.work
        .git(&["submodule", "add", "-q", &file_url(&sub.path), "vendor"]);
    w.work.commit_all("add submodule");
    w.work.git(&["push", "-q", "origin", "main"]);

    let target = w.env.path("with-sub");
    let ctx = OpContext::silent(CancelToken::new());
    let mut opts = clone_opts(&w.url, &target);
    opts.recurse_submodules = true;
    network::clone(&w.env.rt, &ctx, &opts).expect("clone with submodules");
    assert!(
        target.join("vendor").join("s.txt").is_file(),
        "the submodule is checked out"
    );
}

#[test]
fn clone_bare_branch_and_depth() {
    let w = world();
    w.work.git(&["branch", "dev"]);
    w.work.git(&["push", "-q", "origin", "dev"]);
    let ctx = OpContext::silent(CancelToken::new());

    let bare = w.env.path("bare-clone.git");
    let mut opts = clone_opts(&w.url, &bare);
    opts.bare = true;
    let out = network::clone(&w.env.rt, &ctx, &opts).expect("bare clone");
    assert!(
        out.value.workdir().is_none(),
        "a bare clone has no work tree"
    );

    let dev = w.env.path("dev-clone");
    let mut opts = clone_opts(&w.url, &dev);
    opts.branch = Some("dev".into());
    opts.depth = Some(1);
    network::clone(&w.env.rt, &ctx, &opts).expect("branch clone");
    let head = std::fs::read_to_string(dev.join(".git").join("HEAD")).expect("HEAD");
    assert!(
        head.contains("refs/heads/dev"),
        "dev is checked out: {head}"
    );
}

// ───────────────────────────── fetch ─────────────────────────────

#[test]
fn fetch_prune_removes_deleted_branch() {
    let w = world();
    w.work.git(&["push", "-q", "origin", "main:feat"]);
    let other = w.other("other");
    other.git(&["fetch", "-q", "origin"]);
    assert!(
        other.git(&["branch", "-r"]).contains("origin/feat"),
        "the tracking ref exists before"
    );
    w.work.git(&["push", "-q", "origin", "--delete", "feat"]);

    let repo = other.open();
    let mut opts = fetch_opts(FetchTarget::Remote("origin".into()));
    opts.prune = Some(true);
    let out = network::fetch(&repo, &ctx_of(&other), &opts).expect("fetch works");
    assert!(
        !other.git(&["branch", "-r"]).contains("origin/feat"),
        "pruned"
    );
    assert_eq!(out.value.updated_refs, 1, "the prune line counts");
    assert!(
        out.refresh.contains(RefreshSet::REMOTES | RefreshSet::TAGS),
        "refresh covers remotes and tags"
    );
    assert_eq!(out.value.per_remote.len(), 1, "one remote");
}

#[test]
fn fetch_all_reports_per_remote_errors() {
    let w = world();
    let other = w.other("other");
    other.git(&[
        "remote",
        "add",
        "broken",
        &file_url(&w.env.path("missing.git")),
    ]);
    w.work.write("b.txt", "b\n");
    w.work.commit_all("two");
    w.work.git(&["push", "-q", "origin", "main"]);

    let repo = other.open();
    let out = network::fetch(&repo, &ctx_of(&other), &fetch_opts(FetchTarget::All))
        .expect("one remote works, so the call works");
    let by_name = |name: &str| {
        &out.value
            .per_remote
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is reported"))
            .1
    };
    assert!(by_name("origin").is_ok(), "origin fetched");
    assert!(by_name("broken").is_err(), "broken failed");
    assert!(out.value.updated_refs >= 1, "origin/main advanced");

    other.git(&["remote", "remove", "origin"]);
    let repo = other.open();
    assert!(
        network::fetch(&repo, &ctx_of(&other), &fetch_opts(FetchTarget::All)).is_err(),
        "all remotes failed: Err"
    );
}

#[test]
fn fetch_unknown_remote_is_no_such_remote() {
    let w = world();
    let other = w.other("other");
    let repo = other.open();
    assert_kind(
        network::fetch(
            &repo,
            &ctx_of(&other),
            &fetch_opts(FetchTarget::Remote("nope".into())),
        ),
        GitErrorKind::NoSuchRemote,
        "unknown remote",
    );
}

#[test]
fn fetch_tags_none() {
    let w = world();
    let other = w.other("other");
    let repo = other.open();
    w.work.write("b.txt", "b\n");
    w.work.commit_all("two");
    w.work.git(&["tag", "v1"]);
    w.work.git(&["push", "-q", "origin", "main", "v1"]);
    let mut opts = fetch_opts(FetchTarget::Remote("origin".into()));
    opts.tags = TagFetch::None;
    network::fetch(&repo, &ctx_of(&other), &opts).expect("fetch --no-tags");
    assert_eq!(other.git(&["tag"]), "", "no tag was fetched");
    opts.tags = TagFetch::All;
    network::fetch(&repo, &ctx_of(&other), &opts).expect("fetch --tags");
    assert_eq!(other.git(&["tag"]), "v1", "the tag arrives with --tags");
}

// ───────────────────────────── pull ─────────────────────────────

/// `work` and `other` both have one new commit on top of the shared one, in different files.
fn diverged(w: &World, same_file: bool) -> TestRepo {
    let other = w.other("other");
    other.write(if same_file { "a.txt" } else { "o.txt" }, "from other\n");
    other.commit_all("other");
    other.git(&["push", "-q", "origin", "main"]);
    w.work.write("a.txt", "from work\n");
    w.work.commit_all("work");
    w.work.git(&["fetch", "-q", "origin"]);
    other
}

#[test]
fn pull_merge() {
    let w = world();
    diverged(&w, false);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let out = network::pull(
        &repo,
        guard.write_permission(),
        &ctx_of(&w.work),
        &pull_opts(PullMode::Merge),
    )
    .expect("clean merge");
    assert!(
        matches!(
            out.value,
            MergeOutcomeOrRebase::Merge(MergeOutcome::Merged { .. })
        ),
        "a merge commit: {:?}",
        out.value
    );
    assert!(
        w.work.path.join("o.txt").is_file(),
        "the other side's file arrived"
    );

    let up_to_date = network::pull(
        &repo,
        guard.write_permission(),
        &ctx_of(&w.work),
        &pull_opts(PullMode::Merge),
    )
    .expect("pull again");
    assert!(
        matches!(
            up_to_date.value,
            MergeOutcomeOrRebase::Merge(MergeOutcome::UpToDate)
        ),
        "nothing new: {:?}",
        up_to_date.value
    );
}

#[test]
fn pull_fast_forwards() {
    let w = world();
    let other = w.other("other");
    other.write("o.txt", "o\n");
    other.commit_all("other");
    other.git(&["push", "-q", "origin", "main"]);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let out = network::pull(
        &repo,
        guard.write_permission(),
        &ctx_of(&w.work),
        &pull_opts(PullMode::FfOnly),
    )
    .expect("fast-forward");
    assert!(
        matches!(
            out.value,
            MergeOutcomeOrRebase::Merge(MergeOutcome::FastForwarded { .. })
        ),
        "fast-forward: {:?}",
        out.value
    );
}

// ---- fetch_fast_forward: a pull of a branch that is not checked out ----

/// `world()` plus a branch `feature` pushed with upstream tracking (HEAD stays on `main`), and a second clone that has
/// `feature` checked out.
fn feature_world() -> (World, TestRepo) {
    let w = world();
    w.work.git(&["branch", "-q", "feature"]);
    w.work.git(&["push", "-q", "-u", "origin", "feature"]);
    let other = w.other("other");
    other.git(&["checkout", "-q", "-b", "feature", "origin/feature"]);
    (w, other)
}

fn commit_and_push_feature(other: &TestRepo) {
    other.write("o.txt", "from other\n");
    other.commit_all("other");
    other.git(&["push", "-q", "origin", "feature"]);
}

/// A commit on the work repository's `feature` while HEAD stays on `main`.
fn commit_on_local_feature(w: &World) {
    w.work.git(&["checkout", "-q", "feature"]);
    w.work.write("w.txt", "from work\n");
    w.work.commit_all("work");
    w.work.git(&["checkout", "-q", "main"]);
}

fn rev(repo: &TestRepo, name: &str) -> String {
    repo.git(&["rev-parse", name]).trim().to_owned()
}

fn fetch_fast_forward(
    w: &World,
    branch: &str,
) -> Result<gitcthulhu_git::OpOutcome<FastForwardOutcome>> {
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    network::fetch_fast_forward(
        &repo,
        guard.write_permission(),
        &ctx_of(&w.work),
        &full(branch),
    )
}

#[test]
fn fetch_fast_forward_moves_a_branch_that_is_not_checked_out() {
    let (w, other) = feature_world();
    commit_and_push_feature(&other);
    let (main_before, feature_before) = (rev(&w.work, "main"), rev(&w.work, "feature"));

    let out = fetch_fast_forward(&w, "refs/heads/feature").expect("fast-forward");

    let remote_feature = rev(&other, "feature");
    match out.value {
        FastForwardOutcome::FastForwarded { from, to } => {
            assert_eq!(from.to_string(), feature_before, "from is the old tip");
            assert_eq!(to.to_string(), remote_feature, "to is the remote's tip");
        }
        value => panic!("expected a fast-forward, got {value:?}"),
    }
    assert_eq!(
        rev(&w.work, "feature"),
        remote_feature,
        "the branch moved to the remote's tip"
    );
    assert_eq!(
        rev(&w.work, "origin/feature"),
        remote_feature,
        "the remote-tracking ref was updated too"
    );
    assert_eq!(
        w.work.git(&["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main",
        "nothing was checked out"
    );
    assert_eq!(
        rev(&w.work, "main"),
        main_before,
        "the current branch did not move"
    );
    assert_eq!(
        w.work.git(&["status", "--porcelain"]),
        "",
        "the worktree was not touched"
    );
    assert!(
        out.refresh
            .contains(RefreshSet::BRANCHES | RefreshSet::REMOTES | RefreshSet::LOG),
        "the branch list, remotes and log change: {:?}",
        out.refresh
    );
}

#[test]
fn fetch_fast_forward_up_to_date() {
    let (w, _other) = feature_world();
    let out = fetch_fast_forward(&w, "refs/heads/feature").expect("nothing to do");
    assert_eq!(
        out.value,
        FastForwardOutcome::UpToDate,
        "the remote has nothing new"
    );
}

#[test]
fn fetch_fast_forward_ahead_leaves_the_branch() {
    let (w, _other) = feature_world();
    commit_on_local_feature(&w);
    let before = rev(&w.work, "feature");
    let out = fetch_fast_forward(&w, "refs/heads/feature").expect("ahead is not an error");
    assert_eq!(
        out.value,
        FastForwardOutcome::Ahead,
        "only the local side has commits"
    );
    assert_eq!(
        rev(&w.work, "feature"),
        before,
        "the branch did not move back"
    );
}

#[test]
fn fetch_fast_forward_diverged_fetches_but_leaves_the_branch() {
    let (w, other) = feature_world();
    commit_and_push_feature(&other);
    commit_on_local_feature(&w);
    let before = rev(&w.work, "feature");
    let out = fetch_fast_forward(&w, "refs/heads/feature").expect("divergence is not an error");
    assert_eq!(
        out.value,
        FastForwardOutcome::Diverged,
        "both sides have commits"
    );
    assert_eq!(
        out.log.commands.len(),
        1,
        "the rejected fetch's command stays in the outcome's log"
    );
    assert_eq!(
        rev(&w.work, "feature"),
        before,
        "the branch stayed where it was"
    );
    assert_eq!(
        rev(&w.work, "origin/feature"),
        rev(&other, "feature"),
        "the remote's commits were fetched all the same"
    );
}

#[test]
fn fetch_fast_forward_diverged_with_compact_output_and_another_local_name() {
    let (w, other) = feature_world();
    // A local branch named differently from its remote branch: compact output would print `team/*`.
    w.work.git(&["config", "fetch.output", "compact"]);
    w.work
        .git(&["branch", "-q", "--track", "team/feature", "origin/feature"]);
    w.work.git(&["checkout", "-q", "team/feature"]);
    w.work.write("w.txt", "from work\n");
    w.work.commit_all("work");
    w.work.git(&["checkout", "-q", "main"]);
    commit_and_push_feature(&other);
    let before = rev(&w.work, "team/feature");
    let out = fetch_fast_forward(&w, "refs/heads/team/feature")
        .expect("the rejection is recognised whatever fetch.output says");
    assert_eq!(
        out.value,
        FastForwardOutcome::Diverged,
        "both sides have commits"
    );
    assert_eq!(
        rev(&w.work, "team/feature"),
        before,
        "the branch stayed where it was"
    );
}

#[test]
fn fetch_fast_forward_keeps_other_failures() {
    let (w, other) = feature_world();
    // The remote moves a tag the work repository has, and every fetch of origin fetches all tags.
    w.work.git(&["tag", "v1"]);
    w.work.git(&["config", "remote.origin.tagOpt", "--tags"]);
    commit_and_push_feature(&other);
    other.git(&["tag", "v1"]);
    other.git(&["push", "-q", "origin", "v1"]);
    commit_on_local_feature(&w);
    let result = fetch_fast_forward(&w, "refs/heads/feature");
    let err = result.expect_err("a rejected tag is a failure even when the branch only diverged");
    assert!(
        err.stderr()
            .is_some_and(|stderr| stderr.contains("would clobber existing tag")),
        "the tag's rejection is reported: {err:?}"
    );
}

#[test]
fn fetch_fast_forward_refuses_the_checked_out_branch() {
    let w = world();
    let other = w.other("other");
    other.write(
        "o.txt", "o
",
    );
    other.commit_all("other");
    other.git(&["push", "-q", "origin", "main"]);
    let before = rev(&w.work, "main");
    let result = fetch_fast_forward(&w, "refs/heads/main");
    assert!(
        result.is_err(),
        "Git refuses to fetch into the checked-out branch: {result:?}"
    );
    assert_eq!(
        rev(&w.work, "main"),
        before,
        "the checked-out branch did not move"
    );
}

#[test]
fn fetch_fast_forward_without_upstream_is_no_upstream() {
    let w = world();
    w.work.git(&["branch", "-q", "lonely"]);
    assert_kind(
        fetch_fast_forward(&w, "refs/heads/lonely"),
        GitErrorKind::NoUpstream,
        "a branch without an upstream has nothing to pull",
    );
}

#[test]
fn pull_rebase_conflict_state() {
    let w = world();
    diverged(&w, true);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let err = assert_kind(
        network::pull(
            &repo,
            guard.write_permission(),
            &ctx_of(&w.work),
            &pull_opts(PullMode::Rebase),
        ),
        GitErrorKind::Conflicts,
        "rebase conflicts",
    );
    assert!(
        err.refresh_on_failure().contains(RefreshSet::REPO_STATE),
        "the GUI reloads the operation state"
    );
    assert!(
        w.work.path.join(".git").join("rebase-merge").is_dir()
            || w.work.path.join(".git").join("rebase-apply").is_dir(),
        "the rebase is left in progress"
    );
}

#[test]
fn pull_rebase_clean() {
    let w = world();
    diverged(&w, false);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let out = network::pull(
        &repo,
        guard.write_permission(),
        &ctx_of(&w.work),
        &pull_opts(PullMode::Rebase),
    )
    .expect("clean rebase");
    assert!(
        matches!(out.value, MergeOutcomeOrRebase::Rebase(_)),
        "rebase outcome: {:?}",
        out.value
    );
    assert_eq!(
        w.work.git(&["rev-list", "--merges", "--count", "HEAD"]),
        "0",
        "history is linear"
    );
}

#[test]
fn pull_rebase_autostash_conflict_is_a_warning() {
    let w = world();
    let other = w.other("other");
    other.write("a.txt", "from other\n");
    other.commit_all("other");
    other.git(&["push", "-q", "origin", "main"]);
    // An uncommitted change to the same line: re-applying the autostash after the rebase conflicts.
    w.work.write("a.txt", "dirty\n");
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let mut opts = pull_opts(PullMode::Rebase);
    opts.autostash = true;
    let out = network::pull(&repo, guard.write_permission(), &ctx_of(&w.work), &opts)
        .expect("the pull itself succeeds");
    assert!(
        matches!(
            out.value,
            MergeOutcomeOrRebase::Rebase(RebaseOutcome::Completed { .. })
        ),
        "the rebase completed: {:?}",
        out.value
    );
    assert!(
        matches!(&out.warnings[..], [OpWarning::AutostashConflict { output }] if !output.is_empty()),
        "the conflicting autostash is reported: {:?}",
        out.warnings
    );
    assert_eq!(
        w.work.git(&["stash", "list"]).lines().count(),
        1,
        "the local changes are still in the stash"
    );
}

#[test]
fn pull_ff_only_diverged() {
    let w = world();
    diverged(&w, false);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    assert_kind(
        network::pull(
            &repo,
            guard.write_permission(),
            &ctx_of(&w.work),
            &pull_opts(PullMode::FfOnly),
        ),
        GitErrorKind::NotFastForwardable,
        "--ff-only on diverged history",
    );
}

#[test]
fn pull_unconfigured_divergent_is_pull_strategy_required() {
    let w = world();
    diverged(&w, false);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let head_before = w.work.git(&["rev-parse", "HEAD"]);
    assert_kind(
        network::pull(
            &repo,
            guard.write_permission(),
            &ctx_of(&w.work),
            &pull_opts(PullMode::FromConfig),
        ),
        GitErrorKind::PullStrategyRequired,
        "no pull.rebase / pull.ff configured",
    );
    assert_eq!(
        w.work.git(&["rev-parse", "HEAD"]),
        head_before,
        "nothing changed locally"
    );
}

#[test]
fn pull_from_config_rebase() {
    let w = world();
    diverged(&w, false);
    w.work.git(&["config", "pull.rebase", "true"]);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let out = network::pull(
        &repo,
        guard.write_permission(),
        &ctx_of(&w.work),
        &pull_opts(PullMode::FromConfig),
    )
    .expect("pull.rebase=true rebases");
    assert!(
        matches!(out.value, MergeOutcomeOrRebase::Rebase(_)),
        "rebase outcome: {:?}",
        out.value
    );
}

#[test]
fn pull_bare_rebase_key_rebases() {
    let w = world();
    diverged(&w, false);
    // `[pull] rebase` without a value is `true` for Git.
    let config = w.work.path.join(".git").join("config");
    let mut text = std::fs::read_to_string(&config).expect("config");
    text.push_str("[pull]\n\trebase\n");
    std::fs::write(&config, text).expect("config");
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let out = network::pull(
        &repo,
        guard.write_permission(),
        &ctx_of(&w.work),
        &pull_opts(PullMode::FromConfig),
    )
    .expect("a bare pull.rebase rebases");
    assert!(
        matches!(out.value, MergeOutcomeOrRebase::Rebase(_)),
        "rebase outcome: {:?}",
        out.value
    );
    assert_eq!(
        w.work.git(&["rev-list", "--merges", "--count", "HEAD"]),
        "0",
        "Git rebased, too"
    );
}

#[test]
fn pull_branch_from_local_upstream() {
    let w = world();
    w.work
        .git(&["switch", "-q", "-c", "topic", "--track", "main"]);
    w.work.git(&["switch", "-q", "main"]);
    w.work.write("m.txt", "m\n");
    w.work.commit_all("on main");
    w.work.git(&["switch", "-q", "topic"]);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let mut opts = pull_opts(PullMode::FfOnly);
    opts.branch = Some("main".into());
    let out = network::pull(&repo, guard.write_permission(), &ctx_of(&w.work), &opts)
        .expect("branch.topic.remote = . is the local repository, not an unknown remote");
    assert!(
        matches!(
            out.value,
            MergeOutcomeOrRebase::Merge(MergeOutcome::FastForwarded { .. })
        ),
        "fast-forward to local main: {:?}",
        out.value
    );
    assert_eq!(
        w.work.git(&["rev-parse", "topic"]),
        w.work.git(&["rev-parse", "main"]),
        "topic caught up with main"
    );
}

#[test]
fn pull_without_upstream_is_no_upstream() {
    let w = world();
    w.work.git(&["switch", "-q", "-c", "lonely"]);
    let repo = w.work.open();
    let mut guard = w.work.exclusive(&repo);
    let err = assert_kind(
        network::pull(
            &repo,
            guard.write_permission(),
            &ctx_of(&w.work),
            &pull_opts(PullMode::FromConfig),
        ),
        GitErrorKind::NoUpstream,
        "branch without upstream",
    );
    assert!(
        matches!(&err, GitError::NoUpstream { branch: Some(b), .. } if b == "lonely"),
        "names the branch: {err:?}"
    );
}

// ───────────────────────────── push ─────────────────────────────

#[test]
fn push_new_branch_sets_upstream() {
    let w = world();
    w.work.git(&["switch", "-q", "-c", "feat"]);
    w.work.write("f.txt", "f\n");
    w.work.commit_all("feat");
    let repo = w.work.open();
    let mut opts = push_opts(
        "origin",
        vec![spec("refs/heads/feat", "refs/heads/feat")],
        ForceMode::No,
    );
    opts.set_upstream = true;
    let out = network::push(&repo, &ctx_of(&w.work), &opts).expect("push works");
    let update = &out.value.updates[0];
    assert_eq!(
        update.status,
        PushRefStatus::New,
        "a new branch: {update:?}"
    );
    assert_eq!(
        update.new.map(|id| id.to_string()),
        Some(w.work.git(&["rev-parse", "feat"])),
        "the pushed commit is reported"
    );
    assert_eq!(out.value.remote, "origin", "the remote");
    assert!(out.value.url.is_some(), "the url is reported");
    assert_eq!(
        w.work.git(&["config", "branch.feat.remote"]),
        "origin",
        "-u configured the upstream"
    );
    assert!(
        out.refresh.contains(RefreshSet::BRANCHES),
        "branches refresh"
    );
}

#[test]
fn push_non_fast_forward_classified() {
    let w = world();
    diverged(&w, false);
    let repo = w.work.open();
    let opts = push_opts(
        "origin",
        vec![spec("refs/heads/main", "refs/heads/main")],
        ForceMode::No,
    );
    let err = assert_kind(
        network::push(&repo, &ctx_of(&w.work), &opts),
        GitErrorKind::NonFastForward,
        "behind the remote",
    );
    let GitError::NonFastForward { report, refs, .. } = err else {
        unreachable!("kind was checked")
    };
    assert_eq!(refs, ["refs/heads/main"], "the rejected ref");
    assert_eq!(
        report.updates[0].status,
        PushRefStatus::Rejected,
        "the report carries the rejection"
    );
}

/// `work` amended its tip while `other` pushed a new commit that `work` has not fetched.
fn stale_lease_world() -> (World, TestRepo) {
    let w = world();
    let other = w.other("other");
    other.write("o.txt", "o\n");
    other.commit_all("other");
    other.git(&["push", "-q", "origin", "main"]);
    w.work.write("a.txt", "amended\n");
    w.work.git(&["add", "-A"]);
    w.work
        .git(&["commit", "-q", "--amend", "-m", "one amended"]);
    (w, other)
}

#[test]
fn push_force_with_lease_rejected_on_stale() {
    let (w, _other) = stale_lease_world();
    let repo = w.work.open();
    let opts = push_opts(
        "origin",
        vec![spec("refs/heads/main", "refs/heads/main")],
        ForceMode::WithLease,
    );
    assert_kind(
        network::push(&repo, &ctx_of(&w.work), &opts),
        GitErrorKind::LeaseRejected,
        "the tracking ref is stale",
    );
    assert_ne!(
        w.remote_head(),
        w.work.git(&["rev-parse", "HEAD"]),
        "the remote is untouched"
    );
}

#[test]
fn push_force_with_lease_succeeds() {
    let w = world();
    w.work.write("a.txt", "amended\n");
    w.work.git(&["add", "-A"]);
    w.work
        .git(&["commit", "-q", "--amend", "-m", "one amended"]);
    let repo = w.work.open();
    let opts = push_opts(
        "origin",
        vec![spec("refs/heads/main", "refs/heads/main")],
        ForceMode::WithLease,
    );
    let out = network::push(&repo, &ctx_of(&w.work), &opts).expect("lease holds");
    assert_eq!(
        out.value.updates[0].status,
        PushRefStatus::Forced,
        "forced update"
    );
    assert_eq!(
        w.remote_head(),
        w.work.git(&["rev-parse", "HEAD"]),
        "the remote has the new tip"
    );
}

#[test]
fn force_push_after_background_fetch_rejected_by_force_if_includes() {
    let (w, _other) = stale_lease_world();
    // Gitnuro-style background fetch: the tracking ref is fresh, but the new commit was never integrated.
    w.work.git(&["fetch", "-q", "origin"]);
    let repo = w.work.open();
    let opts = push_opts(
        "origin",
        vec![spec("refs/heads/main", "refs/heads/main")],
        ForceMode::WithLease,
    );
    let remote_before = w.remote_head();
    let err = assert_kind(
        network::push(&repo, &ctx_of(&w.work), &opts),
        GitErrorKind::LeaseRejected,
        "fetched but not integrated commits must not be overwritten",
    );
    assert!(
        err.to_string().contains("force-with-lease"),
        "the message explains the lease: {err}"
    );
    assert_eq!(
        w.remote_head(),
        remote_before,
        "the other side's commit is still on the remote"
    );
}

#[test]
fn force_push_with_explicit_expect() {
    let (w, _other) = stale_lease_world();
    let repo = w.work.open();
    let remote_tip = gix::ObjectId::from_hex(w.remote_head().as_bytes()).expect("hex");
    let mut wrong = spec("refs/heads/main", "refs/heads/main");
    wrong.expected_remote = Some(
        gix::ObjectId::from_hex(w.work.git(&["rev-parse", "HEAD~0"]).as_bytes()).expect("hex"),
    );
    assert_kind(
        network::push(
            &repo,
            &ctx_of(&w.work),
            &push_opts("origin", vec![wrong], ForceMode::WithLease),
        ),
        GitErrorKind::LeaseRejected,
        "a wrong expectation is rejected",
    );
    let mut right = spec("refs/heads/main", "refs/heads/main");
    right.expected_remote = Some(remote_tip);
    let out = network::push(
        &repo,
        &ctx_of(&w.work),
        &push_opts("origin", vec![right], ForceMode::WithLease),
    )
    .expect("the expectation matches, despite the stale tracking ref");
    assert_eq!(out.value.updates[0].status, PushRefStatus::Forced, "forced");
}

#[test]
fn push_partial_rejection_reports_updated_refs() {
    let w = world();
    diverged(&w, false);
    w.work.git(&["branch", "ff"]);
    w.work.git(&["push", "-q", "origin", "ff"]);
    w.work.git(&["switch", "-q", "ff"]);
    w.work.write("ff.txt", "x\n");
    w.work.commit_all("ff");
    w.work.git(&["switch", "-q", "main"]);
    let repo = w.work.open();
    let opts = push_opts(
        "origin",
        vec![
            spec("refs/heads/ff", "refs/heads/ff"),
            spec("refs/heads/main", "refs/heads/main"),
        ],
        ForceMode::No,
    );
    let err = assert_kind(
        network::push(&repo, &ctx_of(&w.work), &opts),
        GitErrorKind::NonFastForward,
        "main is rejected",
    );
    let GitError::NonFastForward { report, .. } = &err else {
        unreachable!("kind was checked")
    };
    let status_of = |name: &str| {
        report
            .updates
            .iter()
            .find(|u| u.remote_ref.as_bstr() == name)
            .unwrap_or_else(|| panic!("{name} is in the report: {report:?}"))
            .status
    };
    assert_eq!(
        status_of("refs/heads/ff"),
        PushRefStatus::FastForward,
        "ff went through"
    );
    let ff = report
        .updates
        .iter()
        .find(|u| u.remote_ref.as_bstr() == "refs/heads/ff")
        .expect("ff is in the report");
    assert_eq!(
        (
            ff.old.map(|id| id.to_string()),
            ff.new.map(|id| id.to_string())
        ),
        (
            Some(w.work.git(&["rev-parse", "ff~1"])),
            Some(w.work.git(&["rev-parse", "ff"]))
        ),
        "the fast-forward carries full old and new ids"
    );
    assert_eq!(
        w.work
            .git(&["ls-remote", "origin", "refs/heads/ff"])
            .split_whitespace()
            .next(),
        Some(w.work.git(&["rev-parse", "ff"]).as_str()),
        "the remote really has the new ff tip"
    );
    assert_eq!(
        status_of("refs/heads/main"),
        PushRefStatus::Rejected,
        "main was rejected"
    );
    assert!(
        err.refresh_on_failure().contains(RefreshSet::REMOTES),
        "the partial success is reloaded"
    );
}

#[test]
fn remote_rejected_pre_receive_classified() {
    let w = world();
    let hook = w.bare.join("hooks").join("pre-receive");
    std::fs::write(&hook, "#!/bin/sh\necho 'nope' >&2\nexit 1\n").expect("hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    w.work.write("b.txt", "b\n");
    w.work.commit_all("two");
    let repo = w.work.open();
    let err = assert_kind(
        network::push(
            &repo,
            &ctx_of(&w.work),
            &push_opts(
                "origin",
                vec![spec("refs/heads/main", "refs/heads/main")],
                ForceMode::No,
            ),
        ),
        GitErrorKind::RemoteRejected,
        "pre-receive declined",
    );
    assert!(
        matches!(&err, GitError::RemoteRejected { reason, .. } if reason == "pre-receive hook declined"),
        "the reason is Git's: {err:?}"
    );
}

#[test]
fn push_pre_push_hook_failure_reported() {
    let w = world();
    w.work.hook("pre-push", "echo blocked-by-hook >&2\nexit 1");
    w.work.write("b.txt", "b\n");
    w.work.commit_all("two");
    let repo = w.work.open();
    let err = assert_kind(
        network::push(
            &repo,
            &ctx_of(&w.work),
            &push_opts(
                "origin",
                vec![spec("refs/heads/main", "refs/heads/main")],
                ForceMode::No,
            ),
        ),
        GitErrorKind::HookFailed,
        "pre-push failed",
    );
    assert!(
        matches!(&err, GitError::HookFailed { hook, output, .. } if hook == "pre-push" && output.contains("blocked-by-hook")),
        "names the hook and shows its output: {err:?}"
    );
}

#[test]
fn push_pre_push_hook_runs_without_no_verify() {
    let w = world();
    w.work.hook("pre-push", "echo ran > hook-ran.txt");
    w.work.write("b.txt", "b\n");
    w.work.commit_all("two");
    let repo = w.work.open();
    let out = network::push(
        &repo,
        &ctx_of(&w.work),
        &push_opts(
            "origin",
            vec![spec("refs/heads/main", "refs/heads/main")],
            ForceMode::No,
        ),
    )
    .expect("push");
    assert!(
        w.work.path.join("hook-ran.txt").is_file(),
        "the pre-push hook ran (D0.6: no --no-verify)"
    );
    assert!(
        out.log
            .commands
            .iter()
            .all(|c| !c.args.contains("--no-verify")),
        "--no-verify was not passed"
    );
}

#[test]
fn push_skip_hooks_passes_no_verify() {
    let w = world();
    w.work.hook("pre-push", "exit 1");
    w.work.write("b.txt", "b\n");
    w.work.commit_all("two");
    let repo = w.work.open();
    let mut opts = push_opts(
        "origin",
        vec![spec("refs/heads/main", "refs/heads/main")],
        ForceMode::No,
    );
    opts.skip_hooks = true;
    network::push(&repo, &ctx_of(&w.work), &opts)
        .expect("the explicit skip option bypasses the hook");
}

#[test]
fn push_follow_tags() {
    let w = world();
    w.work.write("b.txt", "b\n");
    w.work.commit_all("two");
    w.work.git(&["tag", "-a", "-m", "release", "v2"]);
    w.work.git(&["tag", "light"]);
    let repo = w.work.open();
    let mut opts = push_opts(
        "origin",
        vec![spec("refs/heads/main", "refs/heads/main")],
        ForceMode::No,
    );
    opts.tags = TagPush::Follow;
    let out = network::push(&repo, &ctx_of(&w.work), &opts).expect("push");
    let remote_tags = w.work.git(&["ls-remote", "--tags", "origin"]);
    assert!(
        remote_tags.contains("refs/tags/v2"),
        "annotated tag followed: {remote_tags}"
    );
    assert!(
        !remote_tags.contains("refs/tags/light"),
        "lightweight tag not pushed: {remote_tags}"
    );
    assert!(out.refresh.contains(RefreshSet::TAGS), "tags refresh");
}

#[test]
fn delete_remote_branch_and_tag() {
    let w = world();
    w.work.git(&["branch", "gone"]);
    w.work.git(&["tag", "t1"]);
    w.work.git(&["push", "-q", "origin", "gone", "t1"]);
    let repo = w.work.open();
    let ctx = ctx_of(&w.work);

    let out = network::delete_remote_branch(&repo, &ctx, "origin", "gone").expect("delete branch");
    assert_eq!(
        out.value.updates[0].status,
        PushRefStatus::Deleted,
        "deleted"
    );
    assert!(
        !w.work
            .git(&["ls-remote", "origin"])
            .contains("refs/heads/gone"),
        "the branch is gone from the remote"
    );
    let out = network::delete_remote_tag(&repo, &ctx, "origin", "t1").expect("delete tag");
    assert_eq!(
        out.value.updates[0].status,
        PushRefStatus::Deleted,
        "deleted"
    );
    assert!(
        !w.work
            .git(&["ls-remote", "--tags", "origin"])
            .contains("t1"),
        "the tag is gone from the remote"
    );

    w.work.git(&["tag", "t2"]);
    let out = network::push_tag(&repo, &ctx, "origin", "t2").expect("push tag");
    assert_eq!(out.value.updates[0].status, PushRefStatus::New, "new tag");
    assert!(
        w.work
            .git(&["ls-remote", "--tags", "origin"])
            .contains("refs/tags/t2"),
        "the tag arrived"
    );
}

#[test]
fn push_current_branch_without_upstream_uses_origin() {
    let w = world();
    w.work.git(&["switch", "-q", "-c", "topic"]);
    w.work.write("t.txt", "t\n");
    w.work.commit_all("topic");
    let repo = w.work.open();
    let out =
        network::push_current_branch(&repo, &ctx_of(&w.work), ForceMode::No, TagPush::None, false)
            .expect("push");
    assert_eq!(out.value.remote, "origin", "default remote");
    assert_eq!(
        out.value.updates[0].status,
        PushRefStatus::New,
        "new branch"
    );
    assert_eq!(
        w.work.git(&["config", "branch.topic.merge"]),
        "refs/heads/topic",
        "upstream was set"
    );

    // The same `Repo` handle sees the upstream that `-u` configured: a pull without arguments works.
    let mut guard = w.work.exclusive(&repo);
    let pulled = network::pull(
        &repo,
        guard.write_permission(),
        &ctx_of(&w.work),
        &pull_opts(PullMode::FfOnly),
    )
    .expect("the upstream is known without reopening the repository");
    assert!(
        matches!(
            pulled.value,
            MergeOutcomeOrRebase::Merge(MergeOutcome::UpToDate)
        ),
        "nothing to pull: {:?}",
        pulled.value
    );
    drop(guard);

    // With an upstream now, a further push (same handle) is a fast-forward to it, without -u.
    w.work.write("t.txt", "t2\n");
    w.work.commit_all("topic 2");
    let out =
        network::push_current_branch(&repo, &ctx_of(&w.work), ForceMode::No, TagPush::None, false)
            .expect("push again");
    assert_eq!(
        out.value.updates[0].status,
        PushRefStatus::FastForward,
        "fast-forward"
    );
    assert!(
        out.log
            .commands
            .iter()
            .all(|c| !c.args.split_whitespace().any(|a| a == "-u")),
        "the upstream is not set again: {:?}",
        out.log.commands
    );

    w.work.git(&["switch", "-q", "--detach"]);
    let repo = w.work.open();
    assert_kind(
        network::push_current_branch(&repo, &ctx_of(&w.work), ForceMode::No, TagPush::None, false),
        GitErrorKind::Unsupported,
        "detached HEAD",
    );
}
