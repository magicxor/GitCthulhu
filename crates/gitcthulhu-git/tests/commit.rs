//! Tests of `commit::commit` and the draft message (spec §2.6, WP-B6).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, CommitOptions, GitError, GitErrorKind, Identity, NoPrompter, OpContext, Repo,
    SignMode, commit,
};
use support::{TestEnv, TestRepo};

fn ctx(fixture: &TestRepo) -> OpContext {
    fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn do_commit(
    fixture: &TestRepo,
    repo: &Repo,
    opts: &CommitOptions,
) -> Result<gitcthulhu_git::OpOutcome<gitcthulhu_git::CommitOutcome>, GitError> {
    let mut guard = fixture.exclusive(repo);
    commit::commit(repo, guard.write_permission(), &ctx(fixture), opts)
}

fn options(message: &str) -> CommitOptions {
    CommitOptions {
        message: message.to_owned(),
        ..CommitOptions::default()
    }
}

fn assert_kind<T: std::fmt::Debug>(result: Result<T, GitError>, expected: GitErrorKind, why: &str) {
    match result {
        Err(err) => assert_eq!(err.kind(), expected, "{why}: got {err:?}"),
        Ok(value) => panic!("{why}: expected {expected:?}, got Ok({value:?})"),
    }
}

/// A repository with one commit and a staged change to `a.txt`.
fn with_staged_change(env: &TestEnv) -> TestRepo {
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("initial");
    fixture.write("a.txt", "one\ntwo\n");
    fixture.git(&["add", "a.txt"]);
    fixture
}

#[test]
fn commit_creates_commit_with_message() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    let outcome = do_commit(&fixture, &repo, &options("Add two\n\nBody line\n"))
        .expect("a staged change can be committed");
    assert_eq!(
        outcome.value.id.to_string(),
        fixture.git(&["rev-parse", "HEAD"]),
        "the outcome names the new HEAD"
    );
    assert_eq!(
        outcome.value.summary, "Add two",
        "summary is the first line"
    );
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%B"]).trim_end(),
        "Add two\n\nBody line",
        "message is stored as given"
    );
    assert!(
        outcome.refresh.contains(gitcthulhu_git::RefreshSet::STATUS)
            && outcome.refresh.contains(gitcthulhu_git::RefreshSet::LOG),
        "status and log must be reloaded: {:?}",
        outcome.refresh
    );
    assert!(
        !outcome.log.commands.is_empty(),
        "the executed command is logged"
    );
}

#[test]
fn commit_preserves_hash_lines() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    do_commit(
        &fixture,
        &repo,
        &options("Fix #12\n\n# not a comment\nSee #13"),
    )
    .expect("commit succeeds");
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%B"]).trim_end(),
        "Fix #12\n\n# not a comment\nSee #13",
        "lines starting with # are kept (no editor cleanup)"
    );
}

#[test]
fn amend_replaces_message() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    let first = do_commit(&fixture, &repo, &options("first")).expect("commit succeeds");
    let amended = do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            amend: true,
            ..options("second")
        },
    )
    .expect("amend succeeds");
    assert_ne!(
        first.value.id, amended.value.id,
        "amend creates a new commit"
    );
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%s"]),
        "second",
        "message was replaced"
    );
    assert_eq!(
        fixture.git(&["rev-list", "--count", "HEAD"]),
        "2",
        "amend does not add a commit"
    );
}

#[test]
fn amend_empty_message_keeps_old() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    do_commit(&fixture, &repo, &options("keep me")).expect("commit succeeds");
    fixture.write("b.txt", "b\n");
    fixture.git(&["add", "b.txt"]);
    let amended = do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            amend: true,
            ..options("")
        },
    )
    .expect("amend without message succeeds");
    assert_eq!(amended.value.summary, "keep me", "old message is kept");
    assert_eq!(
        fixture
            .git(&["show", "--name-only", "--format=", "HEAD"])
            .lines()
            .count(),
        2,
        "the amended commit contains the new file as well"
    );
}

#[test]
fn author_override() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            author: Some(Identity {
                name: "Other Author".into(),
                email: "other@example.org".into(),
            }),
            ..options("by someone else")
        },
    )
    .expect("commit succeeds");
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%an <%ae>|%cn <%ce>"]),
        "Other Author <other@example.org>|Test User <test@example.com>",
        "only the author changes"
    );
}

#[test]
fn identity_override_sets_author_and_committer() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            identity_override: Some(Identity {
                name: "One Shot".into(),
                email: "shot@example.org".into(),
            }),
            ..options("one shot identity")
        },
    )
    .expect("commit succeeds");
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%an <%ae>|%cn <%ce>"]),
        "One Shot <shot@example.org>|One Shot <shot@example.org>",
        "author and committer both use the override"
    );
    assert_eq!(
        fixture.git(&["config", "user.name"]),
        "Test User",
        "the configuration is untouched"
    );
}

#[test]
fn signoff_and_trailers() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            signoff: true,
            trailers: vec![("Reviewed-by".into(), "Alice <alice@example.org>".into())],
            ..options("with trailers")
        },
    )
    .expect("commit succeeds");
    let message = fixture.git(&["log", "-1", "--format=%B"]);
    assert!(
        message.contains("Signed-off-by: Test User <test@example.com>"),
        "signoff trailer present: {message}"
    );
    assert!(
        message.contains("Reviewed-by: Alice <alice@example.org>"),
        "custom trailer present: {message}"
    );
}

#[test]
fn empty_message_rejected_before_git() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let repo = fixture.open();
    assert_kind(
        do_commit(&fixture, &repo, &options("  \n")),
        GitErrorKind::EmptyMessage,
        "blank message without amend",
    );
    assert_eq!(
        fixture.git(&["rev-parse", "HEAD"]),
        head,
        "nothing was committed"
    );
}

#[test]
fn nothing_to_commit_classified() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("initial");
    let repo = fixture.open();
    assert_kind(
        do_commit(&fixture, &repo, &options("nothing")),
        GitErrorKind::NothingToCommit,
        "clean index",
    );
    do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            allow_empty: true,
            ..options("empty on purpose")
        },
    )
    .expect("allow_empty commits without changes");
}

#[test]
fn identity_unknown_classified() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    // No user configuration anywhere, and Git must not guess one from the host.
    env.git_global(&["config", "--global", "--unset", "user.name"]);
    env.git_global(&["config", "--global", "--unset", "user.email"]);
    env.git_global(&["config", "--global", "user.useConfigOnly", "true"]);
    let runtime = env
        .runtime_with(|config| {
            if let Some(isolation) = config.test_isolation.as_mut() {
                isolation
                    .overrides
                    .retain(|(key, _)| !key.starts_with("user."));
            }
        })
        .expect("a runtime without identity");
    let repo = Repo::open(&runtime, &fixture.path).expect("repository opens");
    assert_kind(
        do_commit(&fixture, &repo, &options("who am i")),
        GitErrorKind::IdentityUnknown,
        "no identity configured",
    );
    do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            identity_override: Some(Identity {
                name: "Retry".into(),
                email: "retry@example.org".into(),
            }),
            ..options("who am i")
        },
    )
    .expect("the retry with an identity override works");
}

#[test]
fn merge_commit_concludes_merge() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("initial");
    fixture.git(&["switch", "-q", "-c", "topic"]);
    fixture.write("b.txt", "topic\n");
    fixture.commit_all("topic work");
    fixture.git(&["switch", "-q", "main"]);
    fixture.write("c.txt", "main\n");
    fixture.commit_all("main work");
    fixture.git(&["merge", "--no-commit", "--no-ff", "topic"]);
    let repo = fixture.open();
    let outcome = do_commit(&fixture, &repo, &options("Merge topic")).expect("merge concludes");
    assert_eq!(
        fixture
            .git(&["rev-list", "--parents", "-n1", "HEAD"])
            .split_whitespace()
            .count(),
        3,
        "the commit has two parents"
    );
    assert_eq!(outcome.value.summary, "Merge topic", "message is used");
    assert!(
        !fixture.path.join(".git").join("MERGE_HEAD").exists(),
        "the merge is no longer in progress"
    );
}

#[test]
fn draft_roundtrip_and_cleared_after_commit() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    assert_eq!(
        commit::load_draft_message(&repo).expect("load works"),
        None,
        "no draft yet"
    );
    commit::save_draft_message(&repo, Some("half written\n\nmessage")).expect("save works");
    assert_eq!(
        commit::load_draft_message(&repo).expect("load works"),
        Some("half written\n\nmessage".to_owned()),
        "draft round trips"
    );
    assert!(
        fixture
            .path
            .join(".git")
            .join("gitcthulhu")
            .join("commit-draft")
            .is_file(),
        "draft lives in <git_dir>/gitcthulhu/commit-draft"
    );
    let editmsg = fixture.path.join(".git").join("COMMIT_EDITMSG");
    assert!(
        !editmsg.exists()
            || !String::from_utf8_lossy(
                &std::fs::read(&editmsg).expect("COMMIT_EDITMSG is readable")
            )
            .contains("half written"),
        "COMMIT_EDITMSG (Git's editor file) is not used for drafts"
    );
    do_commit(&fixture, &repo, &options("final")).expect("commit succeeds");
    assert_eq!(
        commit::load_draft_message(&repo).expect("load works"),
        None,
        "the draft is deleted after a successful commit"
    );
    commit::save_draft_message(&repo, Some("again")).expect("save works");
    commit::save_draft_message(&repo, Some("  \n")).expect("blank save works");
    assert_eq!(
        commit::load_draft_message(&repo).expect("load works"),
        None,
        "a blank message deletes the draft"
    );
    commit::save_draft_message(&repo, None).expect("deleting a missing draft is fine");
}

#[test]
fn draft_in_merge_state_writes_merge_msg() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("initial");
    fixture.git(&["switch", "-q", "-c", "topic"]);
    fixture.write("b.txt", "topic\n");
    fixture.commit_all("topic work");
    fixture.git(&["switch", "-q", "main"]);
    fixture.write("c.txt", "main\n");
    fixture.commit_all("main work");
    fixture.git(&["merge", "--no-commit", "--no-ff", "topic"]);
    let repo = fixture.open();
    commit::save_draft_message(&repo, Some("my merge message")).expect("save works");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read(".git/MERGE_MSG")),
        "my merge message",
        "Git's own MERGE_MSG carries the draft"
    );
    assert!(
        !fixture
            .path
            .join(".git")
            .join("gitcthulhu")
            .join("commit-draft")
            .exists(),
        "no separate draft file while merging"
    );
    assert_eq!(
        commit::load_draft_message(&repo).expect("load works"),
        Some("my merge message".to_owned()),
        "load reads MERGE_MSG"
    );

    commit::save_draft_message(&repo, Some(" \n")).expect("a blank save works");
    commit::save_draft_message(&repo, None).expect("clearing works");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read(".git/MERGE_MSG")),
        "my merge message",
        "a blank draft never deletes Git's own MERGE_MSG"
    );
    let outcome =
        do_commit(&fixture, &repo, &options("Merge topic into main")).expect("merge concludes");
    assert_eq!(
        outcome.value.summary, "Merge topic into main",
        "the message passed to commit wins over MERGE_MSG"
    );
    assert_eq!(
        commit::load_draft_message(&repo).expect("load works"),
        None,
        "after the merge the draft file is used again, and it is empty"
    );
}

/// The sequence editor for `draft_in_rebase_edit_reads_rebase_message`: turns every `pick` into `edit`, using only
/// shell built-ins (no `sed`, which may be missing from the PATH Git gives the editor).
const PICK_TO_EDIT: &str = r#"#!/bin/sh
out=""
while IFS= read -r line; do
  case "$line" in
    pick\ *) line="edit ${line#pick }" ;;
  esac
  out="$out$line
"
done < "$1"
printf '%s' "$out" > "$1"
"#;

#[test]
fn draft_in_rebase_edit_reads_rebase_message() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("initial");
    fixture.write("a.txt", "two\n");
    fixture.commit_all("to be edited\n\nwith a body");
    fixture.write(".git/pick-to-edit.sh", PICK_TO_EDIT);
    fixture.git(&[
        "-c",
        "sequence.editor=sh .git/pick-to-edit.sh",
        "rebase",
        "-q",
        "-i",
        "HEAD~1",
    ]);
    assert!(
        fixture.path.join(".git/rebase-merge/amend").is_file(),
        "the rebase stopped at the edit step"
    );
    let repo = fixture.open();
    commit::save_draft_message(&repo, Some("unrelated draft")).expect("save works");
    let draft = commit::load_draft_message(&repo)
        .expect("load works")
        .expect("the stopped commit's message is offered");
    assert!(
        draft.starts_with("to be edited\n\nwith a body"),
        "load reads rebase-merge/message while stopped for amending: {draft:?}"
    );
}

#[test]
fn unicode_message_roundtrip() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let repo = fixture.open();
    let message = "Zażółć gęślą jaźń \u{2014} 日本語 \u{1F389}\n\nCzęść druga: ünï";
    let outcome = do_commit(&fixture, &repo, &options(message)).expect("commit succeeds");
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%B"]).trim_end(),
        message,
        "the message is stored as UTF-8"
    );
    assert_eq!(
        outcome.value.summary, "Zażółć gęślą jaźń \u{2014} 日本語 \u{1F389}",
        "summary decodes"
    );
}

#[test]
fn amend_on_unborn_head_is_refused() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let repo = fixture.open();
    assert_kind(
        do_commit(
            &fixture,
            &repo,
            &CommitOptions {
                amend: true,
                ..options("x")
            },
        ),
        GitErrorKind::UnbornHead,
        "amend without commits",
    );
}

/// Whether `ssh-keygen` is available; the signing tests are skipped without it.
fn ssh_keygen_available() -> bool {
    std::process::Command::new("ssh-keygen")
        .arg("-?")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn generate_key(dir: &std::path::Path, passphrase: &str) -> std::path::PathBuf {
    let key = dir.join("signing_key");
    let status = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", passphrase, "-f"])
        .arg(&key)
        .stdin(std::process::Stdio::null())
        .status()
        .expect("ssh-keygen runs");
    assert!(status.success(), "ssh-keygen generates a key");
    key
}

#[test]
fn ssh_signing_with_unencrypted_key() {
    if !ssh_keygen_available() {
        eprintln!("skipped: ssh-keygen is not installed");
        return;
    }
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let key = generate_key(env.tmp.path(), "");
    fixture.git(&["config", "gpg.format", "ssh"]);
    fixture.git(&[
        "config",
        "user.signingkey",
        &key.with_extension("pub").to_string_lossy(),
    ]);
    let repo = fixture.open();
    do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            sign: SignMode::Sign,
            ..options("signed")
        },
    )
    .expect("an SSH signed commit can be created");
    assert!(
        fixture
            .git(&["cat-file", "commit", "HEAD"])
            .contains("gpgsig"),
        "the commit object carries a signature"
    );
}

#[test]
fn ssh_signing_passphrase_prompts_via_askpass() {
    if !ssh_keygen_available() {
        eprintln!("skipped: ssh-keygen is not installed");
        return;
    }
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    let key = generate_key(env.tmp.path(), "s3cret");
    fixture.git(&["config", "gpg.format", "ssh"]);
    fixture.git(&["config", "user.signingkey", &key.to_string_lossy()]);
    let repo = fixture.open();
    let prompter = Arc::new(support::TestPrompter::new([Some("s3cret")]));
    let ctx = fixture.op_ctx(CancelToken::new(), prompter.clone());
    let mut guard = fixture.exclusive(&repo);
    commit::commit(
        &repo,
        guard.write_permission(),
        &ctx,
        &CommitOptions {
            sign: SignMode::Sign,
            ..options("signed with passphrase")
        },
    )
    .expect("the passphrase prompt is answered by the test prompter");
    assert_eq!(prompter.requests().len(), 1, "exactly one prompt was shown");
    assert!(
        fixture
            .git(&["cat-file", "commit", "HEAD"])
            .contains("gpgsig"),
        "the commit object carries a signature"
    );
}

#[test]
fn signing_failure_classified() {
    let env = TestEnv::new();
    let fixture = with_staged_change(&env);
    fixture.git(&["config", "gpg.program", "false"]);
    let repo = fixture.open();
    assert_kind(
        do_commit(
            &fixture,
            &repo,
            &CommitOptions {
                sign: SignMode::Sign,
                ..options("cannot sign")
            },
        ),
        GitErrorKind::SigningFailed,
        "a failing signing program",
    );
    do_commit(
        &fixture,
        &repo,
        &CommitOptions {
            sign: SignMode::DontSign,
            ..options("unsigned")
        },
    )
    .expect("DontSign bypasses the broken program");
}
