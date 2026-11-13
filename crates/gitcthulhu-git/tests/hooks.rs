//! Client-side hooks around `commit::commit` (spec §2.6 and decision D0.6, WP-B6).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{CancelToken, CommitOptions, GitError, NoPrompter, OpWarning, Repo, commit};
use support::{TestEnv, TestRepo};

fn run_commit(
    fixture: &TestRepo,
    repo: &Repo,
    opts: &CommitOptions,
) -> Result<gitcthulhu_git::OpOutcome<gitcthulhu_git::CommitOutcome>, GitError> {
    let ctx = fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter));
    let mut guard = fixture.exclusive(repo);
    commit::commit(repo, guard.write_permission(), &ctx, opts)
}

fn options(message: &str) -> CommitOptions {
    CommitOptions {
        message: message.to_owned(),
        ..CommitOptions::default()
    }
}

fn with_staged_change(env: &TestEnv) -> TestRepo {
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("initial");
    fixture.write("a.txt", "one\ntwo\n");
    fixture.git(&["add", "a.txt"]);
    fixture
}

/// A hook script at `path` (parent directories are created), executable on Unix.
fn write_hook(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("hooks have a parent")).expect("dir is created");
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("hook is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("hook is made executable");
    }
}

fn expect_hook_failure(result: Result<impl std::fmt::Debug, GitError>, name: &str) -> String {
    match result {
        Err(GitError::HookFailed {
            hook,
            exit_code,
            output,
        }) => {
            assert_eq!(hook, name, "the failing hook is named");
            assert_eq!(exit_code, Some(1), "its exit code is reported");
            output
        }
        other => panic!("expected HookFailed({name}), got {other:?}"),
    }
}

#[test]
fn pre_commit_failure_reports_hook_name_and_output() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    fixture.hook(
        "pre-commit",
        "echo 'lint failed: trailing space' >&2\nexit 1",
    );
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let repo = fixture.open();
    let output = expect_hook_failure(
        run_commit(&fixture, &repo, &options("blocked")),
        "pre-commit",
    );
    assert!(
        output.contains("lint failed: trailing space"),
        "the hook's output is carried: {output}"
    );
    assert_eq!(
        fixture.git(&["rev-parse", "HEAD"]),
        head,
        "no commit was created"
    );
}

#[test]
fn commit_msg_hook_can_rewrite_message() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    fixture.hook("commit-msg", "echo 'Ticket: ABC-1' >> \"$1\"");
    let repo = fixture.open();
    run_commit(&fixture, &repo, &options("original")).expect("commit succeeds");
    let message = fixture.git(&["log", "-1", "--format=%B"]);
    assert!(
        message.contains("original") && message.contains("Ticket: ABC-1"),
        "the hook's edit is part of the commit: {message}"
    );
}

#[test]
fn prepare_commit_msg_runs() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    fixture.hook(
        "prepare-commit-msg",
        "echo \"$2\" > .git/prepare-source\necho '[prepared]' >> \"$1\"",
    );
    let repo = fixture.open();
    let outcome = run_commit(&fixture, &repo, &options("subject")).expect("commit succeeds");
    assert_eq!(
        fixture.read(".git/prepare-source").trim_ascii(),
        b"message",
        "Git passes the message source"
    );
    assert!(
        fixture
            .git(&["log", "-1", "--format=%B"])
            .contains("[prepared]"),
        "the hook edited the message"
    );
    assert!(
        outcome
            .log
            .hooks
            .iter()
            .any(|hook| hook.name == "prepare-commit-msg"),
        "the hook run is logged: {:?}",
        outcome.log.hooks
    );
}

#[test]
fn post_commit_runs() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    fixture.hook("post-commit", "git rev-parse HEAD > .git/post-commit-head");
    let repo = fixture.open();
    let outcome = run_commit(&fixture, &repo, &options("done")).expect("commit succeeds");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read(".git/post-commit-head")).trim(),
        outcome.value.id.to_string(),
        "the hook ran after the commit existed"
    );
    assert!(
        outcome.warnings.is_empty(),
        "no warnings: {:?}",
        outcome.warnings
    );
}

#[test]
fn post_commit_failure_is_warning() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    fixture.hook("post-commit", "echo 'notify failed' >&2\nexit 1");
    let repo = fixture.open();
    let outcome = run_commit(&fixture, &repo, &options("done anyway"))
        .expect("a failing post-commit does not fail the commit");
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%s"]),
        "done anyway",
        "the commit exists"
    );
    match outcome.warnings.as_slice() {
        [
            OpWarning::PostHookFailed {
                hook,
                exit_code,
                output,
            },
        ] => {
            assert_eq!(hook, "post-commit", "the failing hook is named");
            assert_eq!(*exit_code, Some(1), "its exit code is reported");
            assert!(
                output.contains("notify failed"),
                "the hook's output is carried: {output}"
            );
        }
        other => panic!("expected exactly one PostHookFailed warning, got {other:?}"),
    }
}

#[test]
fn skip_hooks_passes_no_verify() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    fixture.hook("pre-commit", "exit 1");
    fixture.hook("commit-msg", "exit 1");
    let repo = fixture.open();
    expect_hook_failure(
        run_commit(&fixture, &repo, &options("checked")),
        "pre-commit",
    );
    let outcome = run_commit(
        &fixture,
        &repo,
        &CommitOptions {
            skip_hooks: true,
            ..options("unchecked")
        },
    )
    .expect("skip_hooks bypasses pre-commit and commit-msg");
    assert!(
        outcome
            .log
            .commands
            .iter()
            .any(|record| record.args.contains("--no-verify")),
        "--no-verify is passed: {:?}",
        outcome.log.commands
    );
}

#[test]
fn core_hooks_path_respected() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let hooks_dir = env.path("shared-hooks");
    write_hook(
        &hooks_dir.join("pre-commit"),
        "echo 'from shared hooks' >&2\nexit 1",
    );
    fixture.git(&["config", "core.hooksPath", &hooks_dir.to_string_lossy()]);
    // A hook in the default directory must be ignored when core.hooksPath is set.
    fixture.hook("pre-commit", "exit 0");
    let repo = fixture.open();
    let output = expect_hook_failure(run_commit(&fixture, &repo, &options("x")), "pre-commit");
    assert!(
        output.contains("from shared hooks"),
        "the hook of core.hooksPath ran: {output}"
    );
}

#[test]
fn husky_style_hooks_path() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    // husky: core.hooksPath=.husky/_ ; every stub delegates to the user's script one level up.
    write_hook(
        &fixture.path.join(".husky").join("_").join("pre-commit"),
        "sh \"$(dirname \"$0\")/../pre-commit\"",
    );
    write_hook(
        &fixture.path.join(".husky").join("pre-commit"),
        "echo 'husky says no' >&2\nexit 1",
    );
    fixture.git(&["config", "core.hooksPath", ".husky/_"]);
    let repo = fixture.open();
    let output = expect_hook_failure(run_commit(&fixture, &repo, &options("x")), "pre-commit");
    assert!(
        output.contains("husky says no"),
        "the script behind the husky stub ran: {output}"
    );
}
