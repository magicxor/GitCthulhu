//! Tests of `merge`, `cherry_pick`, `revert`, `reset` and `continue/abort/skip_operation` (spec §2.6, WP-B7).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, ContinueOutcome, FastForward, GitError, GitErrorKind, InProgress, MergeOptions,
    MergeOutcome, NoPrompter, OpContext, OpWarning, OverwriteKind, PickOptions, ResetMode,
    SequenceOutcome, merge,
};
use support::{TestEnv, TestRepo};

fn ctx(t: &TestRepo) -> OpContext {
    t.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn rev(t: &TestRepo, spec: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(t.git(&["rev-parse", spec]).as_bytes()).expect("rev-parse prints an id")
}

fn opts(rev: &str) -> MergeOptions {
    MergeOptions {
        rev: rev.into(),
        ff: FastForward::Auto,
        squash: false,
        message: None,
        autostash: false,
        skip_hooks: false,
    }
}

fn kind<T: std::fmt::Debug>(result: Result<T, GitError>) -> GitErrorKind {
    match result {
        Err(err) => err.kind(),
        Ok(value) => panic!("expected an error, got Ok({value:?})"),
    }
}

/// `main` has `base` and `m.txt`; `feature` (checked out nothing, branch only) has `base` and `f.txt`.
fn diverged(t: &TestRepo) {
    t.write("a.txt", "base\n");
    t.write("b.txt", "b\n");
    t.commit_all("base");
    t.git(&["branch", "feature"]);
    t.git(&["switch", "-q", "feature"]);
    t.write("f.txt", "f\n");
    t.commit_all("feature work");
    t.git(&["switch", "-q", "main"]);
    t.write("m.txt", "m\n");
    t.commit_all("main work");
}

/// `main` and `feature` both changed `a.txt` differently.
fn conflicting(t: &TestRepo) {
    t.write("a.txt", "base\n");
    t.write("b.txt", "b\n");
    t.commit_all("base");
    t.git(&["branch", "feature"]);
    t.git(&["switch", "-q", "feature"]);
    t.write("a.txt", "feature\n");
    t.commit_all("feature change");
    t.git(&["switch", "-q", "main"]);
    t.write("a.txt", "main\n");
    t.commit_all("main change");
}

/// `feature` is ahead of `main` by one commit that changes `a.txt`.
fn ahead(t: &TestRepo) {
    t.write("a.txt", "base\n");
    t.write("b.txt", "b\n");
    t.commit_all("base");
    t.git(&["branch", "feature"]);
    t.git(&["switch", "-q", "feature"]);
    t.write("a.txt", "feature\n");
    t.commit_all("feature change");
    t.git(&["switch", "-q", "main"]);
}

fn parent_count(t: &TestRepo, commit: &str) -> usize {
    t.git(&["rev-list", "--parents", "-n1", commit])
        .split_whitespace()
        .count()
        - 1
}

#[test]
fn merge_fast_forward() {
    let env = TestEnv::new();
    let t = env.repo("r");
    ahead(&t);
    let tip = rev(&t, "feature");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let out = merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("feature"))
        .expect("fast-forward merge succeeds");
    match out.value {
        MergeOutcome::FastForwarded { new_head } => {
            assert_eq!(new_head, tip, "HEAD moved to the tip of feature")
        }
        other => panic!("expected FastForwarded, got {other:?}"),
    }
    assert_eq!(rev(&t, "HEAD"), tip, "HEAD is the feature tip");
    assert_eq!(
        parent_count(&t, "HEAD"),
        1,
        "no merge commit was created by a fast-forward"
    );
    assert!(
        out.refresh
            .contains(gitcthulhu_git::RefreshSet::OPERATION_STATE),
        "a merge refreshes the operation state"
    );
}

#[test]
fn merge_up_to_date() {
    let env = TestEnv::new();
    let t = env.repo("r");
    ahead(&t);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let out = merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("main"))
        .expect("merging the current branch into itself succeeds");
    assert!(
        matches!(out.value, MergeOutcome::UpToDate),
        "nothing to merge: {:?}",
        out.value
    );
}

#[test]
fn merge_no_ff_creates_merge_commit() {
    let env = TestEnv::new();
    let t = env.repo("r");
    ahead(&t);
    let main_tip = rev(&t, "main");
    let feature_tip = rev(&t, "feature");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let mut o = opts("feature");
    o.ff = FastForward::NoFf;
    let out = merge::merge(&repo, guard.write_permission(), &ctx(&t), &o)
        .expect("--no-ff merge succeeds");
    let MergeOutcome::Merged { new_head } = out.value else {
        panic!("expected Merged, got {:?}", out.value);
    };
    assert_eq!(rev(&t, "HEAD"), new_head, "new_head is HEAD");
    assert_eq!(
        parent_count(&t, "HEAD"),
        2,
        "a merge commit has two parents"
    );
    assert_eq!(rev(&t, "HEAD^1"), main_tip, "first parent is the old main");
    assert_eq!(rev(&t, "HEAD^2"), feature_tip, "second parent is feature");
}

#[test]
fn merge_ff_only_fails_when_diverged() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    let before = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let mut o = opts("feature");
    o.ff = FastForward::FfOnly;
    let result = merge::merge(&repo, guard.write_permission(), &ctx(&t), &o);
    assert_eq!(
        kind(result),
        GitErrorKind::NotFastForwardable,
        "--ff-only on diverged branches"
    );
    assert_eq!(rev(&t, "HEAD"), before, "HEAD did not move");
}

#[test]
fn merge_conflict_state_and_paths() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("feature"));
    match result {
        Err(GitError::Conflicts {
            operation, paths, ..
        }) => {
            assert_eq!(
                operation,
                Some(InProgress::Merge),
                "the merge is in progress"
            );
            assert_eq!(
                paths,
                [bstr::BString::from("a.txt")],
                "exactly the conflicted path"
            );
        }
        other => panic!("expected Conflicts, got {other:?}"),
    }
    assert!(
        t.path.join(".git").join("MERGE_HEAD").exists(),
        "the repository is in the MERGING state"
    );
}

#[test]
fn merge_autostash_restores_changes() {
    let env = TestEnv::new();
    let t = env.repo("r");
    ahead(&t);
    t.write("b.txt", "local edit\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let mut o = opts("feature");
    o.autostash = true;
    let out = merge::merge(&repo, guard.write_permission(), &ctx(&t), &o)
        .expect("autostash merge succeeds");
    assert!(
        matches!(out.value, MergeOutcome::FastForwarded { .. }),
        "fast-forwarded: {:?}",
        out.value
    );
    assert_eq!(
        t.read("b.txt"),
        b"local edit\n",
        "the local change is back after the merge"
    );
    assert_eq!(
        t.read("a.txt"),
        b"feature\n",
        "the merged change is in the work tree"
    );
}

#[test]
fn merge_autostash_conflict_reported() {
    let env = TestEnv::new();
    let t = env.repo("r");
    ahead(&t);
    t.write("a.txt", "local edit\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let mut o = opts("feature");
    o.autostash = true;
    let out = merge::merge(&repo, guard.write_permission(), &ctx(&t), &o)
        .expect("the merge itself succeeds");
    assert!(
        matches!(out.value, MergeOutcome::AutostashConflict { .. }),
        "re-applying the autostash conflicted: {:?}",
        out.value
    );
}

#[test]
fn merge_squash() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    let before = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let mut o = opts("feature");
    o.squash = true;
    let out =
        merge::merge(&repo, guard.write_permission(), &ctx(&t), &o).expect("squash merge succeeds");
    assert!(
        matches!(out.value, MergeOutcome::Squashed),
        "squashed: {:?}",
        out.value
    );
    assert_eq!(rev(&t, "HEAD"), before, "HEAD did not move");
    assert!(
        t.git(&["diff", "--cached", "--name-only"])
            .contains("f.txt"),
        "the changes of feature are staged"
    );
}

#[test]
fn merge_unrelated_histories() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "base\n");
    t.commit_all("base");
    t.git(&["switch", "-q", "--orphan", "other"]);
    t.write("o.txt", "o\n");
    t.commit_all("orphan");
    t.git(&["switch", "-q", "main"]);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("other"));
    assert_eq!(
        kind(result),
        GitErrorKind::UnrelatedHistories,
        "no common ancestor"
    );
}

#[test]
fn merge_hook_failure_pre_merge_commit() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    t.hook("pre-merge-commit", "echo 'blocked by hook' >&2\nexit 1");
    let before = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("feature"));
    match result {
        Err(GitError::HookFailed { hook, output, .. }) => {
            assert_eq!(hook, "pre-merge-commit", "the failing hook is named");
            assert!(
                output.contains("blocked by hook"),
                "hook output is surfaced: {output}"
            );
        }
        other => panic!("expected HookFailed, got {other:?}"),
    }
    assert_eq!(rev(&t, "HEAD"), before, "no merge commit was created");
}

#[test]
fn merge_hook_skipped_with_no_verify() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    t.hook("pre-merge-commit", "exit 1");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let mut o = opts("feature");
    o.skip_hooks = true;
    merge::merge(&repo, guard.write_permission(), &ctx(&t), &o)
        .expect("--no-verify skips pre-merge-commit");
    assert_eq!(parent_count(&t, "HEAD"), 2, "the merge commit exists");
}

#[test]
fn merge_custom_message_multiline() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let mut o = opts("feature");
    o.message = Some("Merge feature\n\nSecond paragraph\nwith two lines".into());
    merge::merge(&repo, guard.write_permission(), &ctx(&t), &o).expect("merge succeeds");
    assert_eq!(
        t.git(&["log", "-1", "--format=%B"]),
        "Merge feature\n\nSecond paragraph\nwith two lines",
        "the custom multi-line message is the commit message"
    );
    assert!(
        !t.path
            .join(".git")
            .join("gitcthulhu")
            .join("merge-msg")
            .exists(),
        "the message file is removed afterwards"
    );
}

#[test]
fn merge_post_merge_failure_is_warning() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    t.hook("post-merge", "echo 'post failed' >&2\nexit 1");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let out = merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("feature"))
        .expect("a failing post-merge hook does not fail the merge");
    assert_eq!(parent_count(&t, "HEAD"), 2, "the merge happened");
    assert!(
        out.warnings
            .iter()
            .any(|w| matches!(w, OpWarning::PostHookFailed { hook, .. } if hook == "post-merge")),
        "the failing post hook is reported as a warning: {:?}",
        out.warnings
    );
}

#[test]
fn cherry_pick_clean() {
    let env = TestEnv::new();
    let t = env.repo("r");
    ahead(&t);
    let pick = rev(&t, "feature");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let out = merge::cherry_pick(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[pick],
        PickOptions::default(),
    )
    .expect("clean cherry-pick");
    let SequenceOutcome::Completed { new_head } = out.value;
    assert_eq!(new_head, rev(&t, "HEAD"), "new_head is HEAD");
    assert_ne!(new_head, pick, "a new commit was created");
    assert_eq!(t.read("a.txt"), b"feature\n", "the change was applied");
    assert_eq!(
        t.git(&["log", "-1", "--format=%s"]),
        "feature change",
        "the message is kept"
    );
}

#[test]
fn cherry_pick_conflict_then_continue_with_message() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let pick = rev(&t, "feature");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::cherry_pick(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[pick],
        PickOptions::default(),
    );
    match result {
        Err(GitError::Conflicts {
            operation, paths, ..
        }) => {
            assert!(
                matches!(
                    operation,
                    Some(InProgress::CherryPick | InProgress::CherryPickSequence)
                ),
                "a cherry-pick is in progress: {operation:?}"
            );
            assert_eq!(paths, [bstr::BString::from("a.txt")], "the conflicted path");
        }
        other => panic!("expected Conflicts, got {other:?}"),
    }
    let unresolved = merge::continue_operation(&repo, guard.write_permission(), &ctx(&t), None);
    assert_eq!(
        kind(unresolved),
        GitErrorKind::Conflicts,
        "continuing with unresolved conflicts is refused"
    );

    t.write("a.txt", "resolved\n");
    t.git(&["add", "a.txt"]);
    let out = merge::continue_operation(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        Some("Custom message\n\nwith body"),
    )
    .expect("continue after resolving");
    assert!(
        matches!(out.value, ContinueOutcome::Completed),
        "completed: {:?}",
        out.value
    );
    assert_eq!(
        t.git(&["log", "-1", "--format=%B"]),
        "Custom message\n\nwith body",
        "the message passed to continue is the commit message"
    );
    assert!(
        !t.path.join(".git").join("CHERRY_PICK_HEAD").exists(),
        "the cherry-pick is finished"
    );
}

#[test]
fn cherry_pick_merge_requires_mainline() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    t.git(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]);
    let merge_commit = rev(&t, "HEAD");
    t.git(&["switch", "-q", "-c", "other", "HEAD~1"]);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::cherry_pick(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[merge_commit],
        PickOptions::default(),
    );
    assert_eq!(
        kind(result),
        GitErrorKind::Unsupported,
        "a merge commit needs a mainline parent"
    );
    merge::cherry_pick(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[merge_commit],
        PickOptions {
            mainline: Some(1),
            no_commit: false,
        },
    )
    .expect("with -m 1 the pick works");
}

#[test]
fn cherry_pick_empty_is_empty_pick() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "base\n");
    t.commit_all("base");
    t.git(&["switch", "-q", "-c", "feature"]);
    t.write("a.txt", "same\n");
    let pick = t.commit_all("feature sets same");
    t.git(&["switch", "-q", "main"]);
    t.write("a.txt", "same\n");
    t.commit_all("main sets same");
    let before = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::cherry_pick(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[pick],
        PickOptions::default(),
    );
    assert_eq!(
        kind(result),
        GitErrorKind::EmptyPick,
        "the pick would be empty"
    );
    merge::skip_operation(&repo, guard.write_permission(), &ctx(&t))
        .expect("skipping the empty pick");
    assert_eq!(rev(&t, "HEAD"), before, "skipping leaves HEAD alone");
    assert!(
        !t.path.join(".git").join("CHERRY_PICK_HEAD").exists(),
        "the operation is over"
    );
}

#[test]
fn cherry_pick_skip() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let pick = rev(&t, "feature");
    let before = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::cherry_pick(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[pick],
        PickOptions::default(),
    );
    assert_eq!(kind(result), GitErrorKind::Conflicts, "the pick conflicts");
    let out = merge::skip_operation(&repo, guard.write_permission(), &ctx(&t))
        .expect("skip the conflicting pick");
    assert!(
        matches!(out.value, ContinueOutcome::Completed),
        "completed: {:?}",
        out.value
    );
    assert_eq!(rev(&t, "HEAD"), before, "nothing was applied");
    assert_eq!(t.read("a.txt"), b"main\n", "the work tree is clean again");
}

#[test]
fn revert_clean() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "base\n");
    t.commit_all("base");
    t.write("n.txt", "new\n");
    let added = t.commit_all("add n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let out = merge::revert(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[added],
        PickOptions::default(),
    )
    .expect("clean revert");
    let SequenceOutcome::Completed { new_head } = out.value;
    assert_eq!(new_head, rev(&t, "HEAD"), "new_head is HEAD");
    assert!(!t.path.join("n.txt").exists(), "the added file is gone");
    assert!(
        t.git(&["log", "-1", "--format=%s"])
            .starts_with("Revert \"add n\""),
        "Git's revert message"
    );
}

#[test]
fn revert_conflict_abort_restores() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "1\n");
    t.commit_all("one");
    t.write("a.txt", "2\n");
    let two = t.commit_all("two");
    t.write("a.txt", "3\n");
    t.commit_all("three");
    let before = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::revert(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[two],
        PickOptions::default(),
    );
    match result {
        Err(GitError::Conflicts { operation, .. }) => assert!(
            matches!(
                operation,
                Some(InProgress::Revert | InProgress::RevertSequence)
            ),
            "a revert is in progress: {operation:?}"
        ),
        other => panic!("expected Conflicts, got {other:?}"),
    }
    merge::abort_operation(&repo, guard.write_permission(), &ctx(&t)).expect("abort the revert");
    assert_eq!(rev(&t, "HEAD"), before, "HEAD is unchanged");
    assert_eq!(t.read("a.txt"), b"3\n", "the work tree is restored");
    assert!(
        t.git(&["status", "--porcelain"]).is_empty(),
        "nothing is left over"
    );
}

#[test]
fn reset_soft_mixed_hard_keep() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "1\n");
    t.write("b.txt", "b\n");
    let c1 = t.commit_all("one");
    t.write("a.txt", "2\n");
    t.commit_all("two");
    t.write("a.txt", "3\n");
    let c3 = t.commit_all("three");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();

    merge::reset(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        c1,
        ResetMode::Soft,
    )
    .expect("soft reset");
    assert_eq!(rev(&t, "HEAD"), c1, "soft: HEAD moved");
    assert_eq!(
        t.index_blob("a.txt"),
        b"3\n",
        "soft: the index keeps the old tip"
    );
    t.git(&["reset", "-q", "--hard", &c3.to_string()]);

    merge::reset(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        c1,
        ResetMode::Mixed,
    )
    .expect("mixed reset");
    assert_eq!(t.index_blob("a.txt"), b"1\n", "mixed: the index follows");
    assert_eq!(
        t.read("a.txt"),
        b"3\n",
        "mixed: the work tree keeps its content"
    );
    t.git(&["reset", "-q", "--hard", &c3.to_string()]);

    merge::reset(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        c1,
        ResetMode::Hard,
    )
    .expect("hard reset");
    assert_eq!(t.read("a.txt"), b"1\n", "hard: the work tree follows");
    t.git(&["reset", "-q", "--hard", &c3.to_string()]);

    // Keep: an unrelated local change survives, a conflicting one refuses the reset.
    t.write("b.txt", "local\n");
    merge::reset(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        c1,
        ResetMode::Keep,
    )
    .expect("keep with an unrelated local change");
    assert_eq!(rev(&t, "HEAD"), c1, "keep: HEAD moved");
    assert_eq!(t.read("a.txt"), b"1\n", "keep: tracked files follow");
    assert_eq!(
        t.read("b.txt"),
        b"local\n",
        "keep: the local change survives"
    );
    t.git(&["checkout", "-q", "--", "b.txt"]);
    t.git(&["reset", "-q", "--hard", &c3.to_string()]);
    t.write("a.txt", "local conflicting\n");
    let result = merge::reset(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        c1,
        ResetMode::Keep,
    );
    match result {
        Err(GitError::WouldOverwrite { kind, paths, .. }) => {
            assert_eq!(
                kind,
                OverwriteKind::LocalChanges,
                "a local change is in the way"
            );
            assert_eq!(paths, [bstr::BString::from("a.txt")], "the path in the way");
        }
        other => panic!("expected WouldOverwrite, got {other:?}"),
    }
    assert_eq!(rev(&t, "HEAD"), c3, "the refused reset did not move HEAD");
    assert_eq!(
        t.read("a.txt"),
        b"local conflicting\n",
        "the local change is untouched"
    );
}

#[test]
fn reset_keep_refuses_to_overwrite_untracked_file() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "1\n");
    let c1 = t.commit_all("one");
    t.write("new.txt", "tracked\n");
    let c2 = t.commit_all("add new");
    t.git(&["reset", "-q", "--hard", &c1.to_string()]);
    t.write("new.txt", "untracked\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = merge::reset(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        c2,
        ResetMode::Keep,
    );
    match result {
        Err(GitError::WouldOverwrite { kind, paths, .. }) => {
            assert_eq!(
                kind,
                OverwriteKind::Untracked,
                "an untracked file is in the way"
            );
            assert_eq!(
                paths,
                [bstr::BString::from("new.txt")],
                "the path in the way"
            );
        }
        other => panic!("expected WouldOverwrite, got {other:?}"),
    }
    assert_eq!(
        t.read("new.txt"),
        b"untracked\n",
        "the untracked file is untouched"
    );
}

#[test]
fn revert_merge_requires_mainline() {
    let env = TestEnv::new();
    let t = env.repo("r");
    diverged(&t);
    t.git(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]);
    let merge_commit = rev(&t, "HEAD");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    match merge::revert(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[merge_commit],
        PickOptions::default(),
    ) {
        Err(GitError::Unsupported { what }) => assert!(
            what.starts_with("revert"),
            "the message names the revert: {what}"
        ),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    merge::revert(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &[merge_commit],
        PickOptions {
            mainline: Some(1),
            no_commit: false,
        },
    )
    .expect("with -m 1 the revert works");
    assert!(
        !t.path.join("f.txt").exists(),
        "the merged-in file is reverted"
    );
}

#[test]
fn continue_without_operation_errors() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "1\n");
    t.commit_all("one");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    assert_eq!(
        kind(merge::continue_operation(
            &repo,
            guard.write_permission(),
            &ctx(&t),
            None
        )),
        GitErrorKind::NoOperationInProgress,
        "continue"
    );
    assert_eq!(
        kind(merge::abort_operation(
            &repo,
            guard.write_permission(),
            &ctx(&t)
        )),
        GitErrorKind::NoOperationInProgress,
        "abort"
    );
    assert_eq!(
        kind(merge::skip_operation(
            &repo,
            guard.write_permission(),
            &ctx(&t)
        )),
        GitErrorKind::NoOperationInProgress,
        "skip"
    );
}

#[test]
fn merge_cannot_be_skipped() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("feature"))
        .expect_err("conflict");
    assert_eq!(
        kind(merge::skip_operation(
            &repo,
            guard.write_permission(),
            &ctx(&t)
        )),
        GitErrorKind::Unsupported,
        "a merge has no skip"
    );
}

#[test]
fn merge_continue_with_message() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("feature"))
        .expect_err("conflict");
    t.write("a.txt", "resolved\n");
    t.git(&["add", "a.txt"]);
    merge::continue_operation(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        Some("Resolved merge\n\nbody"),
    )
    .expect("continue the merge");
    assert_eq!(parent_count(&t, "HEAD"), 2, "the merge commit exists");
    assert_eq!(
        t.git(&["log", "-1", "--format=%B"]),
        "Resolved merge\n\nbody",
        "MERGE_MSG was replaced by the message"
    );
}

#[test]
fn abort_merge_keeps_unrelated_local_changes() {
    let env = TestEnv::new();
    let t = env.repo("r");
    conflicting(&t);
    let before = rev(&t, "HEAD");
    t.write("b.txt", "local edit\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    merge::merge(&repo, guard.write_permission(), &ctx(&t), &opts("feature"))
        .expect_err("conflict");
    merge::abort_operation(&repo, guard.write_permission(), &ctx(&t)).expect("abort the merge");
    assert_eq!(rev(&t, "HEAD"), before, "HEAD is unchanged");
    assert_eq!(
        t.read("a.txt"),
        b"main\n",
        "the conflicted file is restored"
    );
    assert_eq!(
        t.read("b.txt"),
        b"local edit\n",
        "the unrelated local change survives the abort"
    );
    assert!(
        !t.path.join(".git").join("MERGE_HEAD").exists(),
        "the merge is over"
    );
}
