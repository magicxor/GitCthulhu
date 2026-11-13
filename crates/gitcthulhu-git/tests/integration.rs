//! Cross-module flows of the backend (spec §12 integration step): operations of one package are checked through the
//! reads of the others, the way the GUI chains them.

mod support;

use std::{
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

use gitbutler_filemonitor::WatchMode;
use gitcthulhu_git::{
    CancelToken, CheckoutOptions, CommitOptions, DiffContent, DiffOptions, DiffTarget,
    FetchOptions, FetchTarget, ForceMode, GitErrorKind, GraphBuilder, HeadState, LogOptions,
    LogWalker, MergeOutcomeOrRebase, NoPrompter, OpContext, PullMode, PullOptions, PushOptions,
    PushRefSpec, RebaseAction, RebaseOptions, RebaseOutcome, RefListOptions, RefreshSet, Repo,
    RepoWatcher, Selection, SelectionRequest, StatusOptions, TagFetch, TagPush, WatchEvent, branch,
    commit, diff, network, rebase, refs, stage, state, status,
};
use support::{TestEnv, TestRepo};

fn ctx(t: &TestRepo) -> OpContext {
    t.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn oid(hex: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(hex.trim().as_bytes()).expect("git prints hex object ids")
}

fn file_url(path: &Path) -> String {
    format!("file:///{}", path.to_string_lossy().replace('\\', "/"))
}

fn full(name: &str) -> gix::refs::FullName {
    name.try_into().expect("valid ref name")
}

fn st(repo: &Repo) -> gitcthulhu_git::Status {
    status::status(repo, &StatusOptions::default(), &CancelToken::new()).expect("status works")
}

fn do_commit(
    t: &TestRepo,
    repo: &Repo,
    message: &str,
) -> gitcthulhu_git::OpOutcome<gitcthulhu_git::CommitOutcome> {
    let mut guard = repo.lock_exclusive();
    commit::commit(
        repo,
        guard.write_permission(),
        &ctx(t),
        &CommitOptions {
            message: message.to_owned(),
            ..CommitOptions::default()
        },
    )
    .expect("the commit succeeds")
}

/// The whole history as the GUI loads it: `(summaries, graph builder)` after walking in pages of 7.
fn walk_all(repo: &Repo, uncommitted_head: Option<gix::ObjectId>) -> (Vec<String>, GraphBuilder) {
    let cancel = CancelToken::new();
    let mut walker = LogWalker::new(repo, &LogOptions::default(), &cancel).expect("walk starts");
    let mut builder = GraphBuilder::new(walker.first_parent());
    if uncommitted_head.is_some() {
        builder.push_uncommitted(uncommitted_head);
    }
    let mut summaries = Vec::new();
    while !walker.is_exhausted() {
        let page = walker.next_page(7, &cancel).expect("page loads");
        summaries.extend(page.commits.iter().map(|c| c.summary.to_string()));
        builder.push_page(&page, &walker);
    }
    (summaries, builder)
}

fn numbered(changed: &[u32]) -> String {
    (1..=30)
        .map(|n| {
            if changed.contains(&n) {
                format!("line {n} CHANGED\n")
            } else {
                format!("line {n}\n")
            }
        })
        .collect()
}

#[test]
fn status_stage_hunk_commit_log_graph() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("f.txt", numbered(&[]));
    t.write("other.txt", "other\n");
    let first = t.commit_all("first");
    t.write("f.txt", numbered(&[2, 15, 29]));
    t.write("new.txt", "brand new\n");
    let repo = t.open();

    // status sees the modified file and the untracked one, nothing staged
    let s = st(&repo);
    assert!(s.staged.is_empty(), "nothing is staged yet: {:?}", s.staged);
    let paths: Vec<_> = s.unstaged.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(
        paths,
        ["f.txt", "new.txt"],
        "modified and untracked file are listed"
    );

    // diff of the modified file has three hunks
    let target = DiffTarget::Unstaged {
        path: "f.txt".into(),
        previous_path: None,
    };
    let fd = diff::file_diff(&repo, &target, &DiffOptions::default(), &CancelToken::new())
        .expect("diff works");
    let DiffContent::Text(text) = &fd.content else {
        panic!("a text diff was expected, got {:?}", fd.content);
    };
    assert_eq!(
        text.hunks.len(),
        3,
        "three separate hunks: {:?}",
        text.hunks
    );
    let second = text.hunks[1].header;

    // stage the middle hunk only
    {
        let mut guard = repo.lock_exclusive();
        let out = stage::stage_selection(
            &repo,
            guard.write_permission(),
            &ctx(&t),
            &SelectionRequest {
                path: "f.txt".into(),
                previous_path: None,
                base_old: fd.old.map(|s| s.id),
                base_new: fd.new.map(|s| s.id),
                algorithm: None,
                selection: Selection::Hunks(vec![second]),
            },
        )
        .expect("staging a hunk works");
        assert!(
            out.refresh.contains(RefreshSet::STATUS),
            "status must reload"
        );
    }
    assert_eq!(
        t.index_blob("f.txt"),
        numbered(&[15]).into_bytes(),
        "only the middle hunk is in the index (git agrees)"
    );
    let s = st(&repo);
    assert_eq!(s.staged.len(), 1, "f.txt is staged: {:?}", s.staged);
    assert!(
        s.unstaged.iter().any(|e| e.path == "f.txt"),
        "the other two hunks stay unstaged"
    );
    let staged_diff = diff::file_diff(
        &repo,
        &DiffTarget::Staged {
            path: "f.txt".into(),
            previous_path: None,
        },
        &DiffOptions::default(),
        &CancelToken::new(),
    )
    .expect("staged diff works");
    let DiffContent::Text(staged_text) = &staged_diff.content else {
        panic!("a text diff was expected");
    };
    assert_eq!(
        (staged_text.added, staged_text.removed),
        (1, 1),
        "the staged diff is one changed line"
    );

    // commit it
    let one = do_commit(&t, &repo, "change the middle");
    assert!(
        one.refresh
            .contains(RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::BRANCHES),
        "commit refreshes status, log and branches: {:?}",
        one.refresh
    );
    assert_eq!(
        one.value.id,
        oid(&t.git(&["rev-parse", "HEAD"])),
        "the outcome names the new HEAD"
    );
    let s = st(&repo);
    assert!(s.staged.is_empty(), "everything staged was committed");
    assert_eq!(s.unstaged.len(), 2, "two entries remain: {:?}", s.unstaged);

    // stage the rest and commit again
    {
        let mut guard = repo.lock_exclusive();
        stage::stage_all(&repo, guard.write_permission(), &ctx(&t)).expect("stage all works");
    }
    let two = do_commit(&t, &repo, "everything else");
    assert!(
        st(&repo).counts.is_clean(),
        "the tree is clean after the second commit"
    );
    assert_eq!(
        t.git(&["status", "--porcelain"]),
        "",
        "git agrees that the tree is clean"
    );

    // log and graph
    let (summaries, builder) = walk_all(&repo, Some(two.value.id));
    assert_eq!(
        summaries,
        ["everything else", "change the middle", "first"],
        "newest first"
    );
    assert_eq!(
        builder.layout().len(),
        4,
        "three commits and the uncommitted row"
    );
    assert_eq!(
        builder.layout().max_lane(),
        0,
        "linear history uses one lane"
    );
    assert_eq!(builder.open_lanes(), 0, "no lane is left open at the root");
    assert_eq!(
        t.git(&["rev-list", "--count", &format!("{first}..HEAD")]),
        "2",
        "git counts the same commits"
    );
}

#[test]
fn fetch_pull_push_against_bare_remote_with_hooks() {
    let env = TestEnv::new();
    let bare = env.bare_remote("origin");
    let url = file_url(&bare);
    let work = env.repo("work");
    work.write("a.txt", "one\n");
    work.commit_all("one");
    work.git(&["remote", "add", "origin", &url]);
    work.git(&["push", "-q", "-u", "origin", "main"]);

    // a second clone pushes a commit
    let other = env.repo("other");
    other.git(&["remote", "add", "origin", &url]);
    other.git(&["fetch", "-q", "origin"]);
    other.git(&["reset", "-q", "--hard", "origin/main"]);
    other.write("b.txt", "from other\n");
    other.commit_all("from other");
    other.git(&["push", "-q", "origin", "main"]);

    let repo = work.open();
    // fetch through the backend, then look at ahead/behind
    {
        let out = network::fetch(
            &repo,
            &ctx(&work),
            &FetchOptions {
                target: FetchTarget::Remote("origin".into()),
                prune: None,
                tags: TagFetch::Default,
                refspecs: vec![],
            },
        )
        .expect("fetch works");
        assert!(
            out.refresh.contains(RefreshSet::REMOTES | RefreshSet::LOG),
            "fetch refreshes remotes and log: {:?}",
            out.refresh
        );
        assert_eq!(out.value.updated_refs, 1, "one remote-tracking ref moved");
    }
    let local = refs::resolve_commit(&repo, "main").expect("main resolves");
    let remote = refs::resolve_commit(&repo, "origin/main").expect("origin/main resolves");
    let ab = refs::ahead_behind(&repo, &[(local, remote)], 1000, &CancelToken::new())
        .expect("ahead/behind works");
    assert_eq!(
        (ab[0].ahead, ab[0].behind),
        (0, 1),
        "behind by the other clone's commit"
    );

    // a local commit makes it diverge; pull --ff-only must refuse, pull (merge) must merge
    work.write("c.txt", "local\n");
    work.git(&["add", "c.txt"]);
    do_commit(&work, &repo, "local work");
    {
        let mut guard = repo.lock_exclusive();
        let err = network::pull(
            &repo,
            guard.write_permission(),
            &ctx(&work),
            &PullOptions {
                remote: None,
                branch: None,
                mode: PullMode::FfOnly,
                autostash: false,
            },
        )
        .expect_err("a diverged branch cannot fast-forward");
        assert_ne!(
            err.kind(),
            GitErrorKind::Cancelled,
            "the failure is a real error: {err:?}"
        );
    }
    {
        let mut guard = repo.lock_exclusive();
        let out = network::pull(
            &repo,
            guard.write_permission(),
            &ctx(&work),
            &PullOptions {
                remote: None,
                branch: None,
                mode: PullMode::Merge,
                autostash: false,
            },
        )
        .expect("pull with merge works");
        assert!(
            matches!(out.value, MergeOutcomeOrRebase::Merge(_)),
            "a merge happened: {:?}",
            out.value
        );
    }
    assert!(
        work.path.join("b.txt").is_file() && work.path.join("c.txt").is_file(),
        "both lines of work are in the tree"
    );
    assert!(
        state::repo_state(&repo)
            .expect("state")
            .in_progress
            .is_none(),
        "no operation is left in progress"
    );
    let (summaries, builder) = walk_all(&repo, None);
    assert_eq!(
        summaries.len(),
        4,
        "one, from other, local work, merge: {summaries:?}"
    );
    assert_eq!(
        builder.layout().max_lane(),
        1,
        "the merge needs a second lane"
    );
    assert_eq!(builder.open_lanes(), 0, "all lanes closed at the root");

    // push with a pre-push hook: the hook runs natively and may block the push
    work.hook("pre-push", "echo ran >> hook-ran.txt");
    let opts = PushOptions {
        remote: "origin".into(),
        refspecs: vec![PushRefSpec {
            src: Some(full("refs/heads/main")),
            dst: full("refs/heads/main"),
            expected_remote: None,
        }],
        force: ForceMode::No,
        tags: TagPush::None,
        set_upstream: false,
        push_options: vec![],
        skip_hooks: false,
    };
    network::push(&repo, &ctx(&work), &opts).expect("the push works");
    assert!(work.path.join("hook-ran.txt").is_file(), "pre-push ran");
    assert_eq!(
        work.git(&["ls-remote", "origin", "refs/heads/main"])
            .split_whitespace()
            .next()
            .map(str::to_owned),
        Some(work.git(&["rev-parse", "HEAD"])),
        "the remote has our HEAD"
    );

    work.write("d.txt", "d\n");
    work.commit_all("blocked");
    work.hook("pre-push", "echo nope >&2\nexit 1");
    let err = network::push(&repo, &ctx(&work), &opts).expect_err("the hook blocks the push");
    assert_eq!(
        err.kind(),
        GitErrorKind::HookFailed,
        "hook failure is classified: {err:?}"
    );

    // a rejecting pre-receive hook on the remote is reported as such
    work.hook("pre-push", "exit 0");
    std::fs::create_dir_all(bare.join("hooks")).expect("hooks dir");
    std::fs::write(
        bare.join("hooks").join("pre-receive"),
        "#!/bin/sh\necho declined >&2\nexit 1\n",
    )
    .expect("write pre-receive");
    // Git ignores a hook without the executable bit (Unix).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            bare.join("hooks").join("pre-receive"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("pre-receive can be made executable");
    }
    let err = network::push(&repo, &ctx(&work), &opts).expect_err("the remote declines");
    assert_eq!(
        err.kind(),
        GitErrorKind::RemoteRejected,
        "remote rejection is classified: {err:?}"
    );
    std::fs::remove_file(bare.join("hooks").join("pre-receive")).expect("remove pre-receive");
    network::push(&repo, &ctx(&work), &opts).expect("the push works once the remote accepts");
}

#[test]
fn interactive_rebase_end_to_end() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "0\n");
    let base = t.commit_all("base");
    for (file, msg) in [
        ("f1.txt", "one"),
        ("f2.txt", "two"),
        ("f3.txt", "three"),
        ("f4.txt", "four"),
    ] {
        t.write(file, "x\n");
        t.commit_all(msg);
    }
    let repo = t.open();
    let before = refs::resolve_commit(&repo, "HEAD").expect("HEAD");

    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    assert_eq!(plan.steps.len(), 4, "four commits above the base");
    plan.steps[0].action = RebaseAction::Reword;
    plan.steps[0].new_message = Some("one (reworded)".into());
    plan.steps[2].action = RebaseAction::Drop;
    plan.steps[3].action = RebaseAction::Fixup;
    let out = {
        let mut guard = repo.lock_exclusive();
        rebase::start_interactive(
            &repo,
            guard.write_permission(),
            &ctx(&t),
            &plan,
            &RebaseOptions::default(),
        )
        .expect("the rebase runs")
    };
    assert!(
        out.refresh.contains(
            RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::REPO_STATE
        ),
        "a rebase refreshes everything it can touch: {:?}",
        out.refresh
    );
    let RebaseOutcome::Completed { new_head } = out.value else {
        panic!("the rebase should complete: {:?}", out.value);
    };
    assert_ne!(new_head, before, "history was rewritten");
    assert_eq!(
        new_head,
        oid(&t.git(&["rev-parse", "HEAD"])),
        "HEAD is the reported head"
    );
    assert!(
        state::repo_state(&repo).expect("state").rebase.is_none(),
        "no rebase is left in progress"
    );
    assert!(
        st(&repo).counts.is_clean(),
        "the tree is clean after the rebase"
    );
    assert!(
        !t.path.join("f3.txt").exists(),
        "the dropped commit's file is gone"
    );
    assert!(t.path.join("f4.txt").exists(), "the fixup's change is kept");

    let (summaries, builder) = walk_all(&repo, None);
    assert_eq!(
        summaries,
        ["two", "one (reworded)", "base"],
        "reword, drop and fixup are visible in the log"
    );
    assert_eq!(builder.layout().max_lane(), 0, "still a straight line");
    assert_eq!(builder.open_lanes(), 0, "no dangling lanes");
}

#[test]
fn interactive_rebase_conflict_stop_resolve_continue() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "base\n");
    let base = t.commit_all("base");
    t.write("a.txt", "one\n");
    t.commit_all("set one");
    t.write("a.txt", "two\n");
    t.commit_all("set two");
    t.write("b.txt", "b\n");
    t.commit_all("add b");
    let repo = t.open();

    // swapping the first two commits conflicts on a.txt
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps.swap(0, 1);
    let out = {
        let mut guard = repo.lock_exclusive();
        rebase::start_interactive(
            &repo,
            guard.write_permission(),
            &ctx(&t),
            &plan,
            &RebaseOptions::default(),
        )
        .expect("the rebase starts")
    };
    let RebaseOutcome::Stopped { conflicts, .. } = out.value else {
        panic!("the rebase should stop on a conflict: {:?}", out.value);
    };
    assert_eq!(
        conflicts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["a.txt"],
        "a.txt conflicts"
    );
    let rs = state::repo_state(&repo).expect("state");
    assert!(
        rs.rebase.is_some(),
        "the repository state reports the rebase: {rs:?}"
    );
    assert!(
        state::has_conflicts(&repo).expect("conflict check"),
        "conflicts are visible"
    );
    let s = st(&repo);
    assert!(
        s.unstaged.iter().any(|e| e.path == "a.txt"),
        "status lists the conflicted file among the unstaged entries"
    );

    // resolve with git and continue through the backend
    t.write("a.txt", "resolved\n");
    t.git(&["add", "a.txt"]);
    let out = {
        let mut guard = repo.lock_exclusive();
        match rebase::continue_rebase(
            &repo,
            guard.write_permission(),
            &ctx(&t),
            Some("resolved first"),
        ) {
            Ok(out) => out,
            Err(err) => panic!("continue failed: {err:?}"),
        }
    };
    // the second pick conflicts as well (it sets the same file again): resolve once more if needed
    let mut outcome = out.value;
    let mut guard_rounds = 0;
    while let RebaseOutcome::Stopped { .. } = outcome {
        guard_rounds += 1;
        assert!(guard_rounds < 4, "the rebase keeps stopping: {outcome:?}");
        t.write("a.txt", "resolved again\n");
        t.git(&["add", "a.txt"]);
        let mut guard = repo.lock_exclusive();
        outcome =
            rebase::continue_rebase(&repo, guard.write_permission(), &ctx(&t), Some("resolved"))
                .expect("continue works")
                .value;
    }
    assert!(
        matches!(outcome, RebaseOutcome::Completed { .. }),
        "the rebase completes: {outcome:?}"
    );
    assert!(
        state::repo_state(&repo)
            .expect("state")
            .in_progress
            .is_none(),
        "nothing is left in progress"
    );
    assert!(st(&repo).counts.is_clean(), "the tree is clean");
    let (summaries, _) = walk_all(&repo, None);
    assert_eq!(summaries.len(), 4, "no commit was lost: {summaries:?}");
    assert_eq!(summaries[0], "add b", "the last commit is still on top");
}

struct Watched {
    _watcher: RepoWatcher,
    events: Receiver<WatchEvent>,
}

impl Watched {
    fn new(repo: &Repo) -> Self {
        let (tx, events) = mpsc::channel();
        let watcher = RepoWatcher::start_with_mode(repo, WatchMode::Auto, move |event| {
            tx.send(event).ok();
        })
        .expect("the watcher starts");
        Watched {
            _watcher: watcher,
            events,
        }
    }

    fn expect(&self, wanted: RefreshSet, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut union = RefreshSet::empty();
        while !union.contains(wanted) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(WatchEvent::Changed { refresh, .. }) => union |= refresh,
                Ok(WatchEvent::Error { message }) => {
                    panic!("{what}: the watcher failed: {message}")
                }
                Err(_) => panic!("{what}: expected {wanted:?} within 8 s, got {union:?}"),
            }
        }
    }

    fn settle(&self) {
        while self
            .events
            .recv_timeout(Duration::from_millis(1500))
            .is_ok()
        {}
    }
}

/// Runs `test` and, if it panics, once more (file system notifications are timing sensitive).
fn retried(test: impl Fn()) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&test)) {
        eprintln!("the test failed, trying once more: {panic:?}");
        test();
    }
}

#[test]
fn watcher_reports_backend_operations() {
    retried(|| {
        let env = TestEnv::new();
        let t = env.repo("r");
        t.write("a.txt", "one\n");
        t.commit_all("first");
        let repo = t.open();
        let watched = Watched::new(&repo);
        watched.settle();

        // stage through the backend
        t.write("a.txt", "two\n");
        watched.expect(RefreshSet::STATUS, "an edit");
        watched.settle();
        {
            let mut guard = repo.lock_exclusive();
            stage::stage_paths(&repo, guard.write_permission(), &ctx(&t), &["a.txt".into()])
                .expect("staging works");
        }
        watched.expect(RefreshSet::STATUS, "staging through the backend");
        watched.settle();

        // commit through the backend
        do_commit(&t, &repo, "second");
        watched.expect(
            RefreshSet::STATUS | RefreshSet::LOG,
            "a commit through the backend",
        );
        watched.settle();

        // create and switch a branch through the backend
        {
            let mut guard = repo.lock_exclusive();
            branch::create_branch(
                &repo,
                guard.write_permission(),
                &ctx(&t),
                "topic",
                &gitcthulhu_git::StartPoint::Head,
                false,
            )
            .expect("branch is created");
            branch::checkout_branch(
                &repo,
                guard.write_permission(),
                &ctx(&t),
                &full("refs/heads/topic"),
                CheckoutOptions::default(),
            )
            .expect("checkout works");
        }
        watched.expect(
            RefreshSet::BRANCHES,
            "branch operations through the backend",
        );
        let list = refs::list(
            &repo,
            RefListOptions {
                remotes: true,
                tags: true,
                stashes: true,
            },
        )
        .expect("refs list");
        assert!(
            matches!(&list.head, HeadState::Branch { name, .. } if name.as_bstr() == "refs/heads/topic"),
            "HEAD is on the new branch: {:?}",
            list.head
        );
    });
}
