//! Tests of `branch::*` (spec §2.6, WP-B6).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, CheckoutOptions, GitError, GitErrorKind, NoPrompter, OpOutcome, OpWarning, Repo,
    StartPoint, branch,
};
use gix::refs::FullName;
use support::{TestEnv, TestRepo};

fn assert_kind<T: std::fmt::Debug>(result: Result<T, GitError>, expected: GitErrorKind, why: &str) {
    match result {
        Err(err) => assert_eq!(err.kind(), expected, "{why}: got {err:?}"),
        Ok(value) => panic!("{why}: expected {expected:?}, got Ok({value:?})"),
    }
}

fn full(name: &str) -> FullName {
    FullName::try_from(name).expect("a valid full ref name")
}

/// Runs `op` with the exclusive permission and a silent context.
fn with_perm<T>(
    fixture: &TestRepo,
    repo: &Repo,
    op: impl FnOnce(&mut gitcthulhu_git::RepoExclusive, &gitcthulhu_git::OpContext) -> T,
) -> T {
    let ctx = fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter));
    let mut guard = fixture.exclusive(repo);
    op(guard.write_permission(), &ctx)
}

/// `main` with `a.txt`, plus branch `other` where `a.txt` differs.
fn two_branches(env: &TestEnv) -> TestRepo {
    let fixture = env.repo("repo");
    fixture.write("a.txt", "1\n2\n3\n");
    fixture.write("b.txt", "b\n");
    fixture.commit_all("initial");
    fixture.git(&["switch", "-q", "-c", "other"]);
    fixture.write("a.txt", "one\n2\n3\n");
    fixture.commit_all("other changes a");
    fixture.git(&["switch", "-q", "main"]);
    fixture
}

/// `two_branches` plus a bare remote `origin` that has `main`, `other` and `feature/x`; fetched.
fn with_remote(env: &TestEnv) -> TestRepo {
    let fixture = two_branches(env);
    let remote = env.bare_remote("origin");
    fixture.git(&["remote", "add", "origin", &remote.to_string_lossy()]);
    fixture.git(&["branch", "feature/x", "other"]);
    fixture.git(&["push", "-q", "origin", "main", "other", "feature/x"]);
    fixture.git(&["branch", "-D", "feature/x"]);
    fixture.git(&["fetch", "-q", "origin"]);
    fixture
}

fn head_ref(fixture: &TestRepo) -> String {
    fixture.git(&["symbolic-ref", "-q", "HEAD"])
}

#[test]
fn checkout_branch_switches_and_runs_post_checkout() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.hook("post-checkout", "echo \"$3\" > .git/post-checkout-ran");
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_branch(
            &repo,
            perm,
            ctx,
            &full("refs/heads/other"),
            CheckoutOptions::default(),
        )
    })
    .expect("a clean checkout works");
    assert_eq!(head_ref(&fixture), "refs/heads/other", "HEAD moved");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "one\n2\n3\n",
        "the work tree follows"
    );
    assert_eq!(
        fixture.read(".git/post-checkout-ran").trim_ascii(),
        b"1",
        "post-checkout ran with the branch flag"
    );
    assert!(
        outcome
            .refresh
            .contains(gitcthulhu_git::RefreshSet::BRANCHES | gitcthulhu_git::RefreshSet::STATUS),
        "refresh covers branches and status: {:?}",
        outcome.refresh
    );

    let again = with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_branch(
            &repo,
            perm,
            ctx,
            &full("refs/heads/other"),
            CheckoutOptions::default(),
        )
    })
    .expect("checking out the current branch is fine");
    assert!(
        again.refresh.is_empty() && again.log.commands.is_empty(),
        "the current branch is a no-op"
    );
}

#[test]
fn checkout_would_overwrite_lists_paths() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.write("a.txt", "1\n2\nlocal\n");
    let repo = fixture.open();
    let result = with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_branch(
            &repo,
            perm,
            ctx,
            &full("refs/heads/other"),
            CheckoutOptions::default(),
        )
    });
    match result {
        Err(GitError::WouldOverwrite { paths, .. }) => assert!(
            paths.iter().any(|p| p == "a.txt"),
            "the blocking path is listed: {paths:?}"
        ),
        other => panic!("expected WouldOverwrite, got {other:?}"),
    }
    assert_eq!(head_ref(&fixture), "refs/heads/main", "HEAD did not move");
}

#[test]
fn checkout_carry_changes() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.write("a.txt", "1\n2\nlocal\n");
    let repo = fixture.open();
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_branch(
            &repo,
            perm,
            ctx,
            &full("refs/heads/other"),
            CheckoutOptions {
                carry_changes: true,
                force: false,
            },
        )
    })
    .expect("-m merges the local change into the target");
    assert_eq!(head_ref(&fixture), "refs/heads/other", "HEAD moved");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "one\n2\nlocal\n",
        "the local change was carried over"
    );
}

#[test]
fn checkout_force_discards_changes() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.write("a.txt", "garbage\n");
    let repo = fixture.open();
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_branch(
            &repo,
            perm,
            ctx,
            &full("refs/heads/other"),
            CheckoutOptions {
                carry_changes: false,
                force: true,
            },
        )
    })
    .expect("--discard-changes switches regardless");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "one\n2\n3\n",
        "local changes are gone"
    );
}

#[test]
fn checkout_branch_checked_out_elsewhere() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    let wt = env.path("wt");
    fixture.git(&["worktree", "add", "-q", &wt.to_string_lossy(), "other"]);
    let repo = fixture.open();
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            branch::checkout_branch(
                &repo,
                perm,
                ctx,
                &full("refs/heads/other"),
                CheckoutOptions::default(),
            )
        }),
        GitErrorKind::BranchCheckedOutElsewhere,
        "the branch lives in another worktree",
    );
}

#[test]
fn checkout_missing_branch_is_not_found() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    let repo = fixture.open();
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            branch::checkout_branch(
                &repo,
                perm,
                ctx,
                &full("refs/heads/nope"),
                CheckoutOptions::default(),
            )
        }),
        GitErrorKind::NotFound,
        "no such branch",
    );
}

#[test]
fn checkout_remote_creates_tracking() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_remote_branch(
            &repo,
            perm,
            ctx,
            &full("refs/remotes/origin/feature/x"),
            None,
            CheckoutOptions::default(),
        )
    })
    .expect("a tracking branch can be created");
    assert_eq!(
        outcome.value.as_bstr(),
        "refs/heads/feature/x",
        "the local name is the branch part"
    );
    assert_eq!(
        head_ref(&fixture),
        "refs/heads/feature/x",
        "it is checked out"
    );
    assert_eq!(
        fixture.git(&["config", "branch.feature/x.remote"]),
        "origin",
        "tracks the remote"
    );
    assert_eq!(
        fixture.git(&["config", "branch.feature/x.merge"]),
        "refs/heads/feature/x",
        "tracks the branch"
    );
    assert!(
        outcome.refresh.contains(gitcthulhu_git::RefreshSet::CONFIG),
        "config changed"
    );
}

#[test]
fn checkout_remote_with_custom_local_name() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_remote_branch(
            &repo,
            perm,
            ctx,
            &full("refs/remotes/origin/other"),
            Some("my-other"),
            CheckoutOptions::default(),
        )
    })
    .expect("a custom local name works");
    assert_eq!(
        outcome.value.as_bstr(),
        "refs/heads/my-other",
        "custom name"
    );
    assert_eq!(
        fixture.git(&["config", "branch.my-other.merge"]),
        "refs/heads/other",
        "tracks the remote branch"
    );
}

#[test]
fn checkout_remote_reuses_existing_tracking_branch() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    fixture.git(&["branch", "--track", "mine", "refs/remotes/origin/feature/x"]);
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_remote_branch(
            &repo,
            perm,
            ctx,
            &full("refs/remotes/origin/feature/x"),
            None,
            CheckoutOptions::default(),
        )
    })
    .expect("the existing tracking branch is checked out");
    assert_eq!(
        outcome.value.as_bstr(),
        "refs/heads/mine",
        "existing branch"
    );
    assert_eq!(head_ref(&fixture), "refs/heads/mine", "checked out");
    assert!(
        fixture.git(&["branch", "--list", "feature/x"]).is_empty(),
        "no second branch was created"
    );
}

#[test]
fn checkout_remote_name_collision() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    // A local `feature/x` that tracks nothing.
    fixture.git(&["branch", "feature/x", "main"]);
    let repo = fixture.open();
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            branch::checkout_remote_branch(
                &repo,
                perm,
                ctx,
                &full("refs/remotes/origin/feature/x"),
                None,
                CheckoutOptions::default(),
            )
        }),
        GitErrorKind::AlreadyExists,
        "a local branch of that name tracks something else",
    );
}

#[test]
fn checkout_remote_missing_ref_is_not_found() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    let repo = fixture.open();
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            branch::checkout_remote_branch(
                &repo,
                perm,
                ctx,
                &full("refs/remotes/origin/vanished"),
                None,
                CheckoutOptions::default(),
            )
        }),
        GitErrorKind::NotFound,
        "the remote-tracking ref does not exist",
    );
    assert!(
        fixture.git(&["branch", "--list", "vanished"]).is_empty(),
        "no local branch was created"
    );
}

#[test]
fn checkout_remote_with_ambiguous_local_name() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    // `origin/feature/x` is now ambiguous: a local branch has that very name.
    fixture.git(&["branch", "origin/feature/x", "main"]);
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_remote_branch(
            &repo,
            perm,
            ctx,
            &full("refs/remotes/origin/feature/x"),
            None,
            CheckoutOptions::default(),
        )
    })
    .expect("the full ref name makes the start point unambiguous");
    assert_eq!(
        outcome.value.as_bstr(),
        "refs/heads/feature/x",
        "new branch"
    );
    assert_eq!(
        fixture.git(&["rev-parse", "HEAD"]),
        fixture.git(&["rev-parse", "refs/remotes/origin/feature/x"]),
        "it starts at the remote-tracking branch, not at the local namesake"
    );
    assert_eq!(
        fixture.git(&["config", "branch.feature/x.remote"]),
        "origin",
        "tracking is set"
    );
}

#[test]
fn checkout_detached() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    let target = fixture.git(&["rev-parse", "other"]);
    let id = gix::ObjectId::from_hex(target.as_bytes()).expect("hex id");
    let repo = fixture.open();
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_detached(&repo, perm, ctx, id, CheckoutOptions::default())
    })
    .expect("detaching works");
    assert!(
        fixture.git_fail(&["symbolic-ref", "-q", "HEAD"]).is_empty(),
        "HEAD is detached"
    );
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), target, "at the commit");
}

#[test]
fn create_branch_without_checkout() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::create_branch(&repo, perm, ctx, "topic/a", &StartPoint::Head, false)
    })
    .expect("a branch can be created");
    assert_eq!(
        outcome.value.as_bstr(),
        "refs/heads/topic/a",
        "full name returned"
    );
    assert_eq!(head_ref(&fixture), "refs/heads/main", "HEAD did not move");
    assert_eq!(
        fixture.git(&["rev-parse", "topic/a"]),
        fixture.git(&["rev-parse", "main"]),
        "starts at HEAD"
    );

    let other = fixture.git(&["rev-parse", "other"]);
    let id = gix::ObjectId::from_hex(other.as_bytes()).expect("hex id");
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::create_branch(&repo, perm, ctx, "at-commit", &StartPoint::Commit(id), true)
    })
    .expect("branch at a commit with checkout");
    assert_eq!(head_ref(&fixture), "refs/heads/at-commit", "checked out");
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), other, "at that commit");
    assert!(
        fixture
            .git_fail(&["config", "branch.at-commit.remote"])
            .is_empty(),
        "a commit start point never sets an upstream"
    );
}

#[test]
fn create_branch_from_remote_sets_upstream() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    let repo = fixture.open();
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::create_branch(
            &repo,
            perm,
            ctx,
            "local-other",
            &StartPoint::Ref(full("refs/remotes/origin/other")),
            false,
        )
    })
    .expect("branch from a remote-tracking ref");
    assert_eq!(
        fixture.git(&["config", "branch.local-other.merge"]),
        "refs/heads/other",
        "upstream set without checkout"
    );
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::create_branch(
            &repo,
            perm,
            ctx,
            "local-x",
            &StartPoint::Ref(full("refs/remotes/origin/feature/x")),
            true,
        )
    })
    .expect("branch from a remote-tracking ref with checkout");
    assert_eq!(head_ref(&fixture), "refs/heads/local-x", "checked out");
    assert_eq!(
        fixture.git(&["config", "branch.local-x.remote"]),
        "origin",
        "upstream set with checkout"
    );

    fixture.git(&["config", "branch.autoSetupMerge", "false"]);
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::create_branch(
            &repo,
            perm,
            ctx,
            "no-upstream",
            &StartPoint::Ref(full("refs/remotes/origin/other")),
            true,
        )
    })
    .expect("branch from a remote-tracking ref with autoSetupMerge=false");
    assert!(
        fixture
            .git_fail(&["config", "branch.no-upstream.remote"])
            .is_empty(),
        "branch.autoSetupMerge=false is honoured (Git decides)"
    );
}

#[test]
fn create_invalid_name_rejected_without_git() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    let repo = fixture.open();
    for bad in [
        "", "a b", "x..y", "-x", "HEAD", "@", "a~1", "end.lock", "/lead", "tail/", "a:b", " pad",
    ] {
        let result = with_perm(&fixture, &repo, |perm, ctx| {
            branch::create_branch(&repo, perm, ctx, bad, &StartPoint::Head, false)
        });
        assert_kind(result, GitErrorKind::InvalidName, &format!("name {bad:?}"));
    }
    assert_kind(
        branch::validate_branch_name(&repo, "main"),
        GitErrorKind::AlreadyExists,
        "an existing branch",
    );
    assert_kind(
        branch::validate_branch_name(&repo, "main/sub"),
        GitErrorKind::AlreadyExists,
        "a branch name below an existing branch",
    );
    let valid = branch::validate_branch_name(&repo, "feature/new").expect("a fresh name is fine");
    assert_eq!(valid.as_bstr(), "refs/heads/feature/new", "full name");
    assert_eq!(
        fixture.git(&["branch", "--list"]).lines().count(),
        2,
        "no branch was created"
    );
}

#[test]
fn rename_keeps_reflog_and_upstream() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    fixture.git(&["branch", "--track", "old", "refs/remotes/origin/other"]);
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::rename_branch(&repo, perm, ctx, &full("refs/heads/old"), "renamed")
    })
    .expect("rename works");
    assert_eq!(
        outcome.value.as_bstr(),
        "refs/heads/renamed",
        "new full name"
    );
    assert!(
        fixture.git(&["branch", "--list", "old"]).is_empty(),
        "old name is gone"
    );
    assert_eq!(
        fixture.git(&["config", "branch.renamed.merge"]),
        "refs/heads/other",
        "the upstream moved with the branch"
    );
    assert!(
        fixture
            .git(&["reflog", "show", "renamed"])
            .contains("branch: Created from"),
        "the reflog moved with the branch"
    );

    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            branch::rename_branch(&repo, perm, ctx, &full("refs/heads/renamed"), "main")
        }),
        GitErrorKind::AlreadyExists,
        "renaming onto an existing branch",
    );
}

#[test]
fn delete_unmerged_requires_force() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    let repo = fixture.open();
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            branch::delete_branch(&repo, perm, ctx, &full("refs/heads/other"), false)
        }),
        GitErrorKind::NotFullyMerged,
        "other has commits main does not",
    );
    assert!(
        !fixture.git(&["branch", "--list", "other"]).is_empty(),
        "the branch survived"
    );
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::delete_branch(&repo, perm, ctx, &full("refs/heads/other"), true)
    })
    .expect("force deletes");
    assert!(
        fixture.git(&["branch", "--list", "other"]).is_empty(),
        "the branch is gone"
    );
}

#[test]
fn delete_merged_branch_without_force() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.git(&["branch", "merged", "main"]);
    let repo = fixture.open();
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::delete_branch(&repo, perm, ctx, &full("refs/heads/merged"), false)
    })
    .expect("a merged branch is deleted safely");
    assert!(
        fixture.git(&["branch", "--list", "merged"]).is_empty(),
        "the branch is gone"
    );
}

#[test]
fn delete_current_refused() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    let repo = fixture.open();
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            branch::delete_branch(&repo, perm, ctx, &full("refs/heads/main"), true)
        }),
        GitErrorKind::BranchCheckedOutElsewhere,
        "the current branch cannot be deleted",
    );
    let wt = env.path("wt");
    fixture.git(&["worktree", "add", "-q", &wt.to_string_lossy(), "other"]);
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            branch::delete_branch(&repo, perm, ctx, &full("refs/heads/other"), true)
        }),
        GitErrorKind::BranchCheckedOutElsewhere,
        "a branch checked out in another worktree cannot be deleted",
    );
}

#[test]
fn set_and_unset_upstream() {
    let env = TestEnv::new();
    let fixture = with_remote(&env);
    let repo = fixture.open();
    with_perm(&fixture, &repo, |perm, ctx| {
        branch::set_upstream(
            &repo,
            perm,
            ctx,
            &full("refs/heads/main"),
            Some(&full("refs/remotes/origin/other")),
        )
    })
    .expect("set upstream works");
    assert_eq!(
        fixture.git(&["config", "branch.main.merge"]),
        "refs/heads/other",
        "merge ref set"
    );
    assert_eq!(
        fixture.git(&["config", "branch.main.remote"]),
        "origin",
        "remote set"
    );
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::set_upstream(&repo, perm, ctx, &full("refs/heads/main"), None)
    })
    .expect("unset upstream works");
    assert!(
        fixture
            .git_fail(&["config", "branch.main.merge"])
            .is_empty(),
        "merge ref removed"
    );
    assert!(
        !outcome.log.commands.is_empty(),
        "a command was needed to unset"
    );
}

#[test]
fn unset_upstream_when_none_is_ok() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    let repo = fixture.open();
    let outcome: OpOutcome<()> = with_perm(&fixture, &repo, |perm, ctx| {
        branch::set_upstream(&repo, perm, ctx, &full("refs/heads/main"), None)
    })
    .expect("nothing to unset is not an error");
    assert!(
        outcome.log.commands.is_empty() && outcome.refresh.is_empty(),
        "no command ran and nothing needs a refresh"
    );
}

#[test]
fn checkout_post_checkout_failure_is_warning() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.hook("post-checkout", "echo 'hook broke' >&2\nexit 1");
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::checkout_branch(
            &repo,
            perm,
            ctx,
            &full("refs/heads/other"),
            CheckoutOptions::default(),
        )
    })
    .expect("a failing post-checkout does not fail the checkout");
    assert_eq!(
        head_ref(&fixture),
        "refs/heads/other",
        "the switch happened"
    );
    assert!(
        matches!(
            outcome.warnings.as_slice(),
            [OpWarning::PostHookFailed { hook, .. }] if hook == "post-checkout"
        ),
        "the failure is reported as a warning: {:?}",
        outcome.warnings
    );
}
