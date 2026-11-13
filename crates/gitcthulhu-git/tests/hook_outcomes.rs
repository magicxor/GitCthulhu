//! Hook failures around operations other than `commit` (spec §1.11, decision D0.6): failing post hooks surface as
//! `OpWarning::PostHookFailed`, a vetoing `reference-transaction` hook as `HookFailed`, whichever module ran Git.

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, CheckoutOptions, FastForward, GitError, MergeOptions, NoPrompter, OpContext,
    OpWarning, Repo, RepoExclusive, StartPoint, branch, merge, stash, tag,
};
use gix::refs::FullName;
use support::{TestEnv, TestRepo};

fn with_perm<T>(
    fixture: &TestRepo,
    repo: &Repo,
    op: impl FnOnce(&mut RepoExclusive, &OpContext) -> T,
) -> T {
    let ctx = fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter));
    let mut guard = fixture.exclusive(repo);
    op(guard.write_permission(), &ctx)
}

fn two_branches(env: &TestEnv) -> TestRepo {
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("initial");
    fixture.git(&["branch", "other"]);
    fixture
}

fn full(name: &str) -> FullName {
    FullName::try_from(name).expect("a valid full ref name")
}

fn hook_warnings(warnings: &[OpWarning]) -> Vec<(&str, Option<i32>)> {
    warnings
        .iter()
        .map(|warning| match warning {
            OpWarning::PostHookFailed {
                hook, exit_code, ..
            } => (hook.as_str(), *exit_code),
            other => panic!("unexpected warning {other:?}"),
        })
        .collect()
}

#[test]
fn failing_post_checkout_is_a_warning_and_the_switch_happened() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.hook("post-checkout", "echo 'post-checkout broke' >&2\nexit 7");
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
    .expect("Git exits 1 after switching; that is not an error");
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "other",
        "the branch was switched"
    );
    assert_eq!(
        hook_warnings(&outcome.warnings),
        [("post-checkout", Some(7))],
        "the failed post hook is reported once"
    );
    match &outcome.warnings[0] {
        OpWarning::PostHookFailed { output, .. } => assert!(
            output.contains("post-checkout broke"),
            "the hook's output is carried: {output}"
        ),
        other => panic!("expected PostHookFailed, got {other:?}"),
    }
}

#[test]
fn vetoing_reference_transaction_hook_is_a_hook_failure() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.hook(
        "reference-transaction",
        "echo 'refs are frozen' >&2\nexit 3",
    );
    let repo = fixture.open();
    let result = with_perm(&fixture, &repo, |perm, ctx| {
        branch::create_branch(&repo, perm, ctx, "new", &StartPoint::Head, false)
    });
    match result {
        Err(GitError::HookFailed {
            hook,
            exit_code,
            output,
        }) => {
            assert_eq!(hook, "reference-transaction", "the hook is named");
            assert_eq!(exit_code, Some(3), "its exit code is reported");
            assert!(output.contains("refs are frozen"), "output: {output}");
        }
        other => panic!("expected HookFailed, got {other:?}"),
    }
    assert!(
        fixture.git_fail(&["rev-parse", "--verify", "new"]).len() > 1,
        "the branch was not created"
    );
}

#[test]
fn reference_transaction_failing_after_the_update_is_a_warning() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    // Git ignores the exit code of the `committed` phase: the update happened.
    fixture.hook(
        "reference-transaction",
        "[ \"$1\" = committed ] && exit 4\nexit 0",
    );
    let repo = fixture.open();
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        branch::create_branch(&repo, perm, ctx, "new", &StartPoint::Head, false)
    })
    .expect("a late reference-transaction failure does not fail the operation");
    assert_eq!(
        hook_warnings(&outcome.warnings),
        [("reference-transaction", Some(4))],
        "reported as a post hook failure"
    );
    fixture.git(&["rev-parse", "--verify", "new"]);
}

#[test]
fn late_reference_transaction_failure_does_not_hide_a_conflict() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.write("a.txt", "main\n");
    fixture.commit_all("main side");
    fixture.git(&["switch", "-q", "other"]);
    fixture.write("a.txt", "other\n");
    fixture.commit_all("other side");
    fixture.git(&["switch", "-q", "main"]);
    // `git merge` updates ORIG_HEAD (phase `committed` fails, which Git ignores), then stops at the conflict and
    // exits 1 for that reason alone.
    fixture.hook(
        "reference-transaction",
        "[ \"$1\" = committed ] && exit 4\nexit 0",
    );
    let repo = fixture.open();
    let result = with_perm(&fixture, &repo, |perm, ctx| {
        merge::merge(
            &repo,
            perm,
            ctx,
            &MergeOptions {
                rev: "other".into(),
                ff: FastForward::Auto,
                squash: false,
                message: None,
                autostash: false,
                skip_hooks: false,
            },
        )
    });
    match result {
        Err(GitError::Conflicts { paths, .. }) => assert_eq!(
            paths,
            [gitcthulhu_git::RelaPath::from("a.txt")],
            "the conflict is reported"
        ),
        other => panic!("expected Conflicts, got {other:?}"),
    }
}

#[test]
fn tag_delete_and_stash_drop_run_with_hook_attribution() {
    let env = TestEnv::new();
    let fixture = two_branches(&env);
    fixture.git(&["tag", "v1"]);
    fixture.write("a.txt", "dirty\n");
    fixture.git(&["stash", "push", "-q"]);
    let stash_id: gix::ObjectId = fixture
        .git(&["rev-parse", "refs/stash"])
        .parse()
        .expect("an object id");
    fixture.hook("reference-transaction", "exit 5");
    let repo = fixture.open();

    let tag_result = with_perm(&fixture, &repo, |perm, ctx| {
        tag::delete_tag(&repo, perm, ctx, "v1")
    });
    assert!(
        matches!(&tag_result, Err(GitError::HookFailed { hook, .. }) if hook == "reference-transaction"),
        "deleting a tag runs hooks traced: {tag_result:?}"
    );
    let drop_result = with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_drop(&repo, perm, ctx, stash_id)
    });
    assert!(
        matches!(&drop_result, Err(GitError::HookFailed { hook, .. }) if hook == "reference-transaction"),
        "dropping a stash runs hooks traced: {drop_result:?}"
    );
}
