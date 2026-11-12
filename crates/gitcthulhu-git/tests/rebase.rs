//! Tests of plain `rebase`, `plan_interactive` and the continue/skip/abort functions (spec §2.6, §5.7, WP-B7).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, GitError, GitErrorKind, InProgress, NoPrompter, OpContext, RebaseOptions,
    RebaseOutcome, RebaseStopReason, rebase,
};
use support::{TestEnv, TestRepo};

fn ctx(t: &TestRepo) -> OpContext {
    t.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn rev(t: &TestRepo, spec: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(t.git(&["rev-parse", spec]).as_bytes()).expect("rev-parse prints an id")
}

fn kind<T: std::fmt::Debug>(result: Result<T, GitError>) -> GitErrorKind {
    match result {
        Err(err) => err.kind(),
        Ok(value) => panic!("expected an error, got Ok({value:?})"),
    }
}

/// `feature` (checked out) has two commits on top of `base`; `main` moved on with `m.txt`.
fn diverged(t: &TestRepo) {
    t.write("a.txt", "base\n");
    t.commit_all("base");
    t.git(&["switch", "-q", "-c", "feature"]);
    t.write("f1.txt", "1\n");
    t.commit_all("feature one");
    t.write("f2.txt", "2\n");
    t.commit_all("feature two");
    t.git(&["switch", "-q", "main"]);
    t.write("m.txt", "m\n");
    t.commit_all("main work");
    t.git(&["switch", "-q", "feature"]);
}

/// `feature` (checked out) and `main` changed `a.txt` differently.
fn conflicting(t: &TestRepo) {
    t.write("a.txt", "base\n");
    t.commit_all("base");
    t.git(&["switch", "-q", "-c", "feature"]);
    t.write("a.txt", "feature\n");
    t.commit_all("feature change");
    t.git(&["switch", "-q", "main"]);
    t.write("a.txt", "main\n");
    t.commit_all("main change");
    t.git(&["switch", "-q", "feature"]);
}

fn rebase_dir_exists(t: &TestRepo) -> bool {
    t.path.join(".git").join("rebase-merge").exists()
        || t.path.join(".git").join("rebase-apply").exists()
}

#[test]
fn plain_rebase_onto_upstream() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    let main_tip = rev(&t, "main");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let out = rebase::rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        "main",
        &RebaseOptions::default(),
    )
    .expect("clean rebase");
    let RebaseOutcome::Completed { new_head } = out.value else {
        panic!("expected Completed, got {:?}", out.value);
    };
    assert_eq!(new_head, rev(&t, "HEAD"), "new_head is HEAD");
    assert_eq!(
        rev(&t, "HEAD~2"),
        main_tip,
        "the two commits sit on top of main"
    );
    assert_eq!(
        t.git(&["log", "--format=%s", "--reverse", "main..HEAD"]),
        "feature one\nfeature two",
        "both commits were replayed in order"
    );
    assert!(!rebase_dir_exists(&t), "no rebase is left in progress");
}

#[test]
fn plain_rebase_conflict_stop_and_continue() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let original = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let out = rebase::rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        "main",
        &RebaseOptions::default(),
    )
    .expect("a conflict is a stop, not an error");
    match out.value {
        RebaseOutcome::Stopped {
            reason,
            at,
            conflicts,
        } => {
            assert_eq!(reason, RebaseStopReason::Conflict, "stopped by a conflict");
            assert_eq!(at, Some(original), "stopped at the commit being replayed");
            assert_eq!(
                conflicts,
                [bstr::BString::from("a.txt")],
                "the conflicted path"
            );
        }
        other => panic!("expected Stopped, got {other:?}"),
    }
    let unresolved = rebase::continue_rebase(&repo, guard.write_permission(), &ctx(&t), None);
    match unresolved {
        Err(GitError::Conflicts {
            operation, paths, ..
        }) => {
            assert!(
                matches!(
                    operation,
                    Some(InProgress::Rebase | InProgress::RebaseInteractive)
                ),
                "the rebase is still in progress: {operation:?}"
            );
            assert_eq!(paths, [bstr::BString::from("a.txt")], "the unresolved path");
        }
        other => {
            panic!("continue with unresolved conflicts is refused with Conflicts, got {other:?}")
        }
    }

    t.write("a.txt", "resolved\n");
    t.git(&["add", "a.txt"]);
    let out = rebase::continue_rebase(&repo, guard.write_permission(), &ctx(&t), None)
        .expect("continue after resolving");
    assert!(
        matches!(out.value, RebaseOutcome::Completed { .. }),
        "completed: {:?}",
        out.value
    );
    assert_eq!(t.read("a.txt"), b"resolved\n", "the resolution is kept");
    assert!(!rebase_dir_exists(&t), "the rebase is over");
}

#[test]
fn plain_rebase_abort() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let original = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    rebase::rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        "main",
        &RebaseOptions::default(),
    )
    .expect("stops with a conflict");
    rebase::abort_rebase(&repo, guard.write_permission(), &ctx(&t)).expect("abort");
    assert_eq!(rev(&t, "HEAD"), original, "HEAD is back where it was");
    assert_eq!(
        t.git(&["branch", "--show-current"]),
        "feature",
        "on the branch again"
    );
    assert!(!rebase_dir_exists(&t), "the rebase is over");
    assert_eq!(
        kind(rebase::abort_rebase(
            &repo,
            guard.write_permission(),
            &ctx(&t)
        )),
        GitErrorKind::NoOperationInProgress,
        "a second abort has nothing to abort"
    );
}

#[test]
fn autostash() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    t.write("a.txt", "dirty\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let without = rebase::rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        "main",
        &RebaseOptions::default(),
    );
    assert_eq!(
        kind(without),
        GitErrorKind::UncommittedChanges,
        "Git refuses to rebase with local changes"
    );
    let out = rebase::rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        "main",
        &RebaseOptions {
            autostash: true,
            ..RebaseOptions::default()
        },
    )
    .expect("autostash rebase");
    assert!(
        matches!(out.value, RebaseOutcome::Completed { .. }),
        "completed: {:?}",
        out.value
    );
    assert_eq!(t.read("a.txt"), b"dirty\n", "the local change is back");
}

#[test]
fn pre_rebase_hook_failure() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    t.hook("pre-rebase", "echo 'no rebasing today' >&2\nexit 1");
    let before = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = rebase::rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        "main",
        &RebaseOptions::default(),
    );
    match result {
        Err(GitError::HookFailed { hook, output, .. }) => {
            assert_eq!(hook, "pre-rebase", "the failing hook is named");
            assert!(
                output.contains("no rebasing today"),
                "hook output: {output}"
            );
        }
        other => panic!("expected HookFailed, got {other:?}"),
    }
    assert_eq!(rev(&t, "HEAD"), before, "nothing changed");

    let out = rebase::rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        "main",
        &RebaseOptions {
            skip_hooks: true,
            ..RebaseOptions::default()
        },
    )
    .expect("--no-verify skips pre-rebase");
    assert!(
        matches!(out.value, RebaseOutcome::Completed { .. }),
        "completed: {:?}",
        out.value
    );
}

#[test]
fn plan_lists_commits_oldest_first() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "0\n");
    let base = t.commit_all("base");
    t.write("a.txt", "1\n");
    let c1 = t.commit_all("first\n\nbody one");
    t.write("a.txt", "2\n");
    let c2 = t.commit_all("second");
    let repo = t.open();
    let plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    assert_eq!(plan.head, c2, "HEAD recorded for the stale check");
    assert_eq!(
        plan.head_ref.as_ref().map(|r| r.as_bstr().to_string()),
        Some("refs/heads/main".to_owned()),
        "branch recorded"
    );
    assert_eq!(plan.base, Some(base), "base recorded");
    assert_eq!(plan.merges_dropped, 0, "no merges");
    let ids: Vec<_> = plan.steps.iter().map(|s| s.commit).collect();
    assert_eq!(ids, [c1, c2], "oldest first");
    assert_eq!(plan.steps[0].summary, "first", "summary is the first line");
    assert!(
        plan.steps[0].message.starts_with("first\n\nbody one"),
        "full message kept: {:?}",
        plan.steps[0].message
    );
    assert!(
        plan.steps
            .iter()
            .all(|s| s.action == gitcthulhu_git::RebaseAction::Pick && s.new_message.is_none()),
        "every step starts as an unchanged pick"
    );
}

#[test]
fn plan_drops_merges_and_counts() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "0\n");
    let base = t.commit_all("base");
    t.git(&["switch", "-q", "-c", "side"]);
    t.write("s.txt", "s\n");
    let side = t.commit_all("side work");
    t.git(&["switch", "-q", "main"]);
    t.write("m.txt", "m\n");
    let main_work = t.commit_all("main work");
    t.git(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    t.write("z.txt", "z\n");
    let last = t.commit_all("after merge");
    let repo = t.open();
    let plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    assert_eq!(plan.merges_dropped, 1, "the merge commit is counted");
    let ids: Vec<_> = plan.steps.iter().map(|s| s.commit).collect();
    assert_eq!(ids.len(), 3, "the merge is not listed: {ids:?}");
    assert!(
        ids.contains(&side) && ids.contains(&main_work) && ids.last() == Some(&last),
        "commits of both sides are linearised with the newest last: {ids:?}"
    );
}

#[test]
fn plan_root() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "0\n");
    let c0 = t.commit_all("root");
    t.write("a.txt", "1\n");
    let c1 = t.commit_all("next");
    let repo = t.open();
    let plan = rebase::plan_interactive(&repo, None).expect("plan from the root");
    assert_eq!(plan.base, None, "--root");
    let ids: Vec<_> = plan.steps.iter().map(|s| s.commit).collect();
    assert_eq!(ids, [c0, c1], "every commit, oldest first");
}

#[test]
fn plan_rejects_base_that_is_not_an_ancestor() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    let main_tip = rev(&t, "main");
    let repo = t.open();
    assert_eq!(
        kind(rebase::plan_interactive(&repo, Some(main_tip))),
        GitErrorKind::InvalidName,
        "main is not an ancestor of feature"
    );
}

#[test]
fn skip_rebase_drops_the_conflicting_commit() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let main_tip = rev(&t, "main");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    rebase::rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        "main",
        &RebaseOptions::default(),
    )
    .expect("stops with a conflict");
    let out = rebase::skip_rebase(&repo, guard.write_permission(), &ctx(&t)).expect("skip");
    assert!(
        matches!(out.value, RebaseOutcome::Completed { .. }),
        "completed: {:?}",
        out.value
    );
    assert_eq!(rev(&t, "HEAD"), main_tip, "nothing of feature is left");
}
