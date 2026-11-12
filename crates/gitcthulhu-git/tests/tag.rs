//! Tests of `tag::*` (spec §2.6, WP-B6).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, GitError, GitErrorKind, NoPrompter, Repo, SignMode, TagOptions, tag,
};
use support::{TestEnv, TestRepo};

fn assert_kind<T: std::fmt::Debug>(result: Result<T, GitError>, expected: GitErrorKind, why: &str) {
    match result {
        Err(err) => assert_eq!(err.kind(), expected, "{why}: got {err:?}"),
        Ok(value) => panic!("{why}: expected {expected:?}, got Ok({value:?})"),
    }
}

fn two_commits(env: &TestEnv) -> (TestRepo, gix::ObjectId, gix::ObjectId) {
    let fixture = env.repo("repo");
    fixture.write("a.txt", "1\n");
    let first = fixture.commit_all("first");
    fixture.write("a.txt", "2\n");
    let second = fixture.commit_all("second");
    (fixture, first, second)
}

fn create(
    fixture: &TestRepo,
    repo: &Repo,
    name: &str,
    target: gix::ObjectId,
    opts: &TagOptions,
) -> Result<gitcthulhu_git::OpOutcome<gix::refs::FullName>, GitError> {
    let ctx = fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter));
    let mut guard = fixture.exclusive(repo);
    tag::create_tag(repo, guard.write_permission(), &ctx, name, target, opts)
}

#[test]
fn lightweight_tag() {
    let env = TestEnv::new();
    let (fixture, first, _) = two_commits(&env);
    let repo = fixture.open();
    let outcome = create(&fixture, &repo, "v1", first, &TagOptions::default())
        .expect("a lightweight tag can be created");
    assert_eq!(
        outcome.value.as_bstr(),
        "refs/tags/v1",
        "full name returned"
    );
    assert_eq!(
        fixture.git(&["cat-file", "-t", "v1"]),
        "commit",
        "no tag object"
    );
    assert_eq!(
        fixture.git(&["rev-parse", "v1"]),
        first.to_string(),
        "points at the target, not at HEAD"
    );
    assert!(
        outcome.refresh.contains(gitcthulhu_git::RefreshSet::TAGS),
        "tags are refreshed"
    );
}

#[test]
fn lightweight_tag_ignores_tag_gpgsign() {
    let env = TestEnv::new();
    let (fixture, first, _) = two_commits(&env);
    // `tag.gpgSign` would turn a plain `git tag` into a signed annotated tag (and fail with "no tag message?").
    fixture.git(&["config", "tag.gpgSign", "true"]);
    fixture.git(&["config", "gpg.program", "false"]);
    let repo = fixture.open();
    create(&fixture, &repo, "light", first, &TagOptions::default())
        .expect("a lightweight tag is created despite tag.gpgSign");
    assert_eq!(
        fixture.git(&["cat-file", "-t", "light"]),
        "commit",
        "still lightweight, unsigned"
    );
    assert_kind(
        create(
            &fixture,
            &repo,
            "annotated",
            first,
            &TagOptions {
                message: Some("signed by config".into()),
                ..TagOptions::default()
            },
        ),
        GitErrorKind::SigningFailed,
        "annotated tags still honour tag.gpgSign (here: a failing signing program)",
    );
}

#[test]
fn annotated_tag_with_message() {
    let env = TestEnv::new();
    let (fixture, _, second) = two_commits(&env);
    let repo = fixture.open();
    create(
        &fixture,
        &repo,
        "v2",
        second,
        &TagOptions {
            message: Some("Release 2\n\nNotes # kept".into()),
            ..TagOptions::default()
        },
    )
    .expect("an annotated tag can be created");
    assert_eq!(
        fixture.git(&["cat-file", "-t", "v2"]),
        "tag",
        "a tag object"
    );
    assert_eq!(
        fixture
            .git(&["tag", "-l", "--format=%(contents)", "v2"])
            .trim_end(),
        "Release 2\n\nNotes # kept",
        "the message is stored"
    );
    assert_eq!(
        fixture.git(&["rev-parse", "v2^{commit}"]),
        second.to_string(),
        "peels to the target"
    );
}

#[test]
fn annotated_empty_message() {
    let env = TestEnv::new();
    let (fixture, _, second) = two_commits(&env);
    let repo = fixture.open();
    create(
        &fixture,
        &repo,
        "v3",
        second,
        &TagOptions {
            message: Some(String::new()),
            ..TagOptions::default()
        },
    )
    .expect("an annotated tag may have an empty message");
    assert_eq!(
        fixture.git(&["cat-file", "-t", "v3"]),
        "tag",
        "a tag object"
    );
}

#[test]
fn tag_exists_classified() {
    let env = TestEnv::new();
    let (fixture, first, second) = two_commits(&env);
    let repo = fixture.open();
    create(&fixture, &repo, "v1", first, &TagOptions::default()).expect("first creation works");
    assert_kind(
        create(&fixture, &repo, "v1", second, &TagOptions::default()),
        GitErrorKind::AlreadyExists,
        "the tag exists",
    );
    assert_kind(
        create(&fixture, &repo, "bad name", second, &TagOptions::default()),
        GitErrorKind::InvalidName,
        "invalid tag name",
    );
    assert_kind(
        tag::validate_tag_name(&repo, "v1"),
        GitErrorKind::AlreadyExists,
        "validate sees the existing tag",
    );
    tag::validate_tag_name(&repo, "v9").expect("a fresh name validates");
}

#[test]
fn force_moves_tag() {
    let env = TestEnv::new();
    let (fixture, first, second) = two_commits(&env);
    let repo = fixture.open();
    create(&fixture, &repo, "moving", first, &TagOptions::default()).expect("creation works");
    create(
        &fixture,
        &repo,
        "moving",
        second,
        &TagOptions {
            force: true,
            ..TagOptions::default()
        },
    )
    .expect("force moves the tag");
    assert_eq!(
        fixture.git(&["rev-parse", "moving"]),
        second.to_string(),
        "the tag points at the new target"
    );
}

#[test]
fn delete_tag() {
    let env = TestEnv::new();
    let (fixture, first, _) = two_commits(&env);
    let repo = fixture.open();
    create(&fixture, &repo, "gone", first, &TagOptions::default()).expect("creation works");
    let ctx = fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter));
    let mut guard = fixture.exclusive(&repo);
    tag::delete_tag(&repo, guard.write_permission(), &ctx, "gone").expect("deletion works");
    assert!(fixture.git(&["tag", "-l"]).is_empty(), "no tags left");
    assert_kind(
        tag::delete_tag(&repo, guard.write_permission(), &ctx, "gone"),
        GitErrorKind::NotFound,
        "deleting a missing tag",
    );
}

#[test]
fn ssh_signed_tag() {
    if std::process::Command::new("ssh-keygen")
        .arg("-?")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipped: ssh-keygen is not installed");
        return;
    }
    let env = TestEnv::new();
    let (fixture, _, second) = two_commits(&env);
    let key = env.tmp.path().join("tag_key");
    let status = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .stdin(std::process::Stdio::null())
        .status()
        .expect("ssh-keygen runs");
    assert!(status.success(), "a key is generated");
    fixture.git(&["config", "gpg.format", "ssh"]);
    fixture.git(&[
        "config",
        "user.signingkey",
        &key.with_extension("pub").to_string_lossy(),
    ]);
    let repo = fixture.open();
    create(
        &fixture,
        &repo,
        "signed",
        second,
        &TagOptions {
            message: Some("signed release".into()),
            sign: SignMode::Sign,
            force: false,
        },
    )
    .expect("an SSH signed tag can be created");
    assert!(
        fixture
            .git(&["cat-file", "tag", "signed"])
            .contains("BEGIN SSH SIGNATURE"),
        "the tag object carries an SSH signature"
    );
}
