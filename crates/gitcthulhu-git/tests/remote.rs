//! Tests of `remote` (spec §2.6, WP-B5).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, GitErrorKind, NoPrompter, OpContext, RefreshSet, RemoteRef, remote,
};
use support::{TestEnv, TestRepo};

fn commit(repo: &TestRepo, msg: &str) {
    repo.write("a.txt", msg);
    repo.commit_all(msg);
}

fn ctx(fixture: &TestRepo) -> OpContext {
    fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn remote_ref(name: &str, url: &str) -> Option<RemoteRef> {
    Some(RemoteRef {
        name: name.into(),
        url: url.into(),
    })
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn add_with_distinct_push_url() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let out = remote::add_remote(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        "origin",
        "https://example.com/a.git",
        Some("https://example.com/push.git"),
    )
    .expect("add");
    assert!(
        out.refresh
            .contains(RefreshSet::REMOTES | RefreshSet::CONFIG),
        "refresh set"
    );
    assert_eq!(
        fixture.git(&["remote", "get-url", "origin"]),
        "https://example.com/a.git",
        "fetch url"
    );
    assert_eq!(
        fixture.git(&["remote", "get-url", "--push", "origin"]),
        "https://example.com/push.git",
        "push url"
    );
    remote::add_remote(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        "second",
        "https://example.com/b.git",
        Some("https://example.com/b.git"),
    )
    .expect("add with equal push url");
    assert!(
        fixture
            .git_fail(&["config", "--get", "remote.second.pushurl"])
            .is_empty(),
        "an equal push url is not stored separately"
    );
    let err = remote::add_remote(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        "origin",
        "https://example.com/c.git",
        None,
    )
    .expect_err("duplicate");
    assert_eq!(err.kind(), GitErrorKind::AlreadyExists, "remote exists");
}

#[test]
fn blank_url_rejected() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    for (name, url, push) in [
        ("origin", "  ", None),
        ("origin", "", None),
        ("origin", "https://x/y.git", Some(" ")),
        ("origin", "--upload-pack=evil", None),
        ("", "https://x/y.git", None),
        ("bad name", "https://x/y.git", None),
        ("-n", "https://x/y.git", None),
    ] {
        let err = remote::add_remote(
            &repo,
            guard.write_permission(),
            &ctx(&fixture),
            name,
            url,
            push,
        )
        .expect_err("invalid input");
        assert_eq!(
            err.kind(),
            GitErrorKind::InvalidName,
            "{name:?} {url:?} {push:?}"
        );
    }
    assert_eq!(
        fixture.git(&["remote"]),
        "",
        "nothing was created before spawning git"
    );
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn set_urls_and_clear_push_url() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    fixture.git(&["remote", "add", "origin", "https://example.com/a.git"]);
    fixture.git(&[
        "remote",
        "set-url",
        "--push",
        "origin",
        "https://example.com/p.git",
    ]);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);

    remote::set_remote_urls(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        "origin",
        "https://example.com/new.git",
        Some("https://example.com/newpush.git"),
    )
    .expect("set urls");
    assert_eq!(
        fixture.git(&["remote", "get-url", "origin"]),
        "https://example.com/new.git",
        "fetch"
    );
    assert_eq!(
        fixture.git(&["remote", "get-url", "--push", "origin"]),
        "https://example.com/newpush.git",
        "push"
    );

    remote::set_remote_urls(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        "origin",
        "https://example.com/new.git",
        None,
    )
    .expect("clear push url");
    assert_eq!(
        fixture.git(&["remote", "get-url", "--push", "origin"]),
        "https://example.com/new.git",
        "push url falls back to the fetch url"
    );
    // Clearing again, with no pushurl set, is not an error.
    remote::set_remote_urls(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        "origin",
        "https://example.com/new.git",
        None,
    )
    .expect("clear twice");
    let err = remote::set_remote_urls(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        "ghost",
        "https://x/y",
        None,
    )
    .expect_err("unknown remote");
    assert_eq!(err.kind(), GitErrorKind::NotFound, "unknown remote");
}

fn pushed_fixture(env: &TestEnv) -> TestRepo {
    let bare = env.bare_remote("origin");
    let fixture = env.repo("repo");
    commit(&fixture, "one");
    fixture.git(&["remote", "add", "origin", &bare.to_string_lossy()]);
    fixture.git(&["push", "-q", "-u", "origin", "main"]);
    fixture
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn rename_moves_refs_and_upstreams() {
    let env = TestEnv::new();
    let fixture = pushed_fixture(&env);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let out = remote::rename_remote(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        "origin",
        "upstream2",
    )
    .expect("rename");
    assert!(
        out.refresh.contains(RefreshSet::BRANCHES | RefreshSet::LOG),
        "refresh set"
    );
    assert!(
        !fixture
            .git(&["for-each-ref", "refs/remotes/upstream2/main"])
            .is_empty(),
        "tracking refs moved"
    );
    assert_eq!(
        fixture.git(&["config", "branch.main.remote"]),
        "upstream2",
        "branch upstream follows"
    );
    let err = remote::rename_remote(&repo, guard.write_permission(), &ctx(&fixture), "nope", "x")
        .expect_err("unknown");
    assert_eq!(err.kind(), GitErrorKind::NotFound, "unknown remote");
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn remove_cleans_refs_and_upstreams() {
    let env = TestEnv::new();
    let fixture = pushed_fixture(&env);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    remote::remove_remote(&repo, guard.write_permission(), &ctx(&fixture), "origin")
        .expect("remove");
    assert_eq!(
        fixture.git(&["for-each-ref", "refs/remotes"]),
        "",
        "tracking refs deleted"
    );
    assert_eq!(fixture.git(&["remote"]), "", "remote gone");
    assert!(
        fixture
            .git_fail(&["config", "branch.main.remote"])
            .is_empty(),
        "branch.main.remote unset"
    );
}

#[test]
fn forge_remotes_rules_table() {
    // 1: no remotes.
    let env = TestEnv::new();
    let f = env.repo("none");
    commit(&f, "one");
    let r = remote::forge_remotes(&f.open()).expect("forge remotes");
    assert_eq!((r.base, r.push), (None, None), "no remotes");

    // 2: a single remote that is not called origin.
    let f = env.repo("single");
    commit(&f, "one");
    f.git(&["remote", "add", "solo", "https://h/solo.git"]);
    let r = remote::forge_remotes(&f.open()).expect("forge remotes");
    assert_eq!(
        r.base,
        remote_ref("solo", "https://h/solo.git"),
        "the only remote is the base"
    );
    assert_eq!(r.push, r.base, "push defaults to the base");

    // 3: origin and upstream: upstream is the base (fork workflow), remote.pushDefault decides the push side.
    let f = env.repo("fork");
    commit(&f, "one");
    f.git(&["remote", "add", "origin", "https://h/me.git"]);
    f.git(&["remote", "add", "upstream", "https://h/up.git"]);
    let r = remote::forge_remotes(&f.open()).expect("forge remotes");
    assert_eq!(
        r.base,
        remote_ref("upstream", "https://h/up.git"),
        "`upstream` beats origin"
    );
    assert_eq!(r.push, r.base, "push follows the base by default");
    f.git(&["config", "remote.pushDefault", "origin"]);
    let r = remote::forge_remotes(&f.open()).expect("forge remotes");
    assert_eq!(
        r.push,
        remote_ref("origin", "https://h/me.git"),
        "remote.pushDefault"
    );
    f.git(&["config", "remote.pushDefault", "ghost"]);
    let r = remote::forge_remotes(&f.open()).expect("forge remotes");
    assert_eq!(r.push, r.base, "an unknown pushDefault is ignored");

    // 4: origin among others.
    let f = env.repo("origin");
    commit(&f, "one");
    f.git(&["remote", "add", "zzz", "https://h/z.git"]);
    f.git(&["remote", "add", "origin", "https://h/o.git"]);
    let r = remote::forge_remotes(&f.open()).expect("forge remotes");
    assert_eq!(
        r.base,
        remote_ref("origin", "https://h/o.git"),
        "origin is the default"
    );

    // 5: the upstream of the current branch decides first; pushRemote decides the push side.
    f.git(&["config", "branch.main.remote", "zzz"]);
    f.git(&["config", "branch.main.merge", "refs/heads/main"]);
    f.git(&["config", "branch.main.pushRemote", "origin"]);
    let r = remote::forge_remotes(&f.open()).expect("forge remotes");
    assert_eq!(
        r.base,
        remote_ref("zzz", "https://h/z.git"),
        "branch upstream remote"
    );
    assert_eq!(
        r.push,
        remote_ref("origin", "https://h/o.git"),
        "branch.<b>.pushRemote"
    );

    // 6: insteadOf rewrites are applied.
    let f = env.repo("rewrite");
    commit(&f, "one");
    f.git(&["remote", "add", "origin", "gh:me/x.git"]);
    f.git(&["config", "url.https://github.com/.insteadOf", "gh:"]);
    let r = remote::forge_remotes(&f.open()).expect("forge remotes");
    assert_eq!(
        r.base,
        remote_ref("origin", "https://github.com/me/x.git"),
        "insteadOf applied"
    );
}

#[test]
fn default_branch_from_symref() {
    let env = TestEnv::new();
    let f = env.repo("repo");
    commit(&f, "one");
    let head = f.git(&["rev-parse", "HEAD"]);
    f.git(&["remote", "add", "origin", "https://h/o.git"]);
    f.git(&["remote", "add", "other", "https://h/p.git"]);
    f.git(&["remote", "add", "bare", "https://h/q.git"]);
    f.git(&["update-ref", "refs/remotes/origin/develop", &head]);
    f.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/develop",
    ]);
    f.git(&["update-ref", "refs/remotes/other/master", &head]);
    let repo = f.open();
    assert_eq!(
        remote::default_branch(&repo, "origin")
            .expect("default")
            .as_deref(),
        Some("develop"),
        "symref target"
    );
    assert_eq!(
        remote::default_branch(&repo, "other")
            .expect("default")
            .as_deref(),
        Some("master"),
        "falls back to master"
    );
    assert_eq!(
        remote::default_branch(&repo, "bare").expect("default"),
        None,
        "unknown"
    );
}
