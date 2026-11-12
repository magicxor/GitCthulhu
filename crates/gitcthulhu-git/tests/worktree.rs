//! Tests of `worktree` (spec §2.5/§2.6, WP-B5, D0.7).

mod support;

use std::{path::Path, sync::Arc};

use gitcthulhu_git::{
    CancelToken, GitErrorKind, HeadState, NoPrompter, OpContext, RefreshSet, Repo, RepoKind,
    StartPoint, WorktreeAddOptions, WorktreeTarget, worktree,
};
use support::{TestEnv, TestRepo, same_path};

fn commit(repo: &TestRepo, file: &str, msg: &str) -> gix::ObjectId {
    repo.write(file, msg);
    repo.commit_all(msg)
}

fn ctx(fixture: &TestRepo) -> OpContext {
    fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn full(name: &str) -> gix::refs::FullName {
    name.try_into().expect("valid ref name")
}

fn opts(path: &Path, target: WorktreeTarget) -> WorktreeAddOptions {
    WorktreeAddOptions {
        path: path.to_owned(),
        target,
        lock_reason: None,
    }
}

#[test]
fn list_main_and_linked() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let head = commit(&fixture, "a.txt", "one");
    fixture.git(&["branch", "feat"]);
    let wt = env.path("wt-feat");
    fixture.git(&["worktree", "add", "-q", &path_str(&wt), "feat"]);
    let detached = env.path("wt-detached");
    fixture.git(&["worktree", "add", "-q", "--detach", &path_str(&detached)]);
    fixture.git(&[
        "worktree",
        "lock",
        "--reason",
        "usb stick",
        &path_str(&detached),
    ]);

    let entries = worktree::list(&fixture.open()).expect("list");
    assert_eq!(entries.len(), 3, "main + two linked: {entries:?}");
    let main = &entries[0];
    assert!(main.id.is_none(), "main first, no id");
    assert!(same_path(&main.path, &fixture.path), "main path");
    assert!(main.is_current, "the open repository is the main worktree");
    assert!(
        !main.bare && !main.prunable && main.locked.is_none(),
        "plain main entry"
    );
    assert_eq!(
        main.head,
        Some(HeadState::Branch {
            name: full("refs/heads/main"),
            target: head
        }),
        "main head"
    );

    let feat = entries
        .iter()
        .find(|e| e.path == wt || same_path(&e.path, &wt))
        .expect("feat worktree listed");
    assert_eq!(
        feat.id.as_deref(),
        Some("wt-feat"),
        "id is the directory name"
    );
    assert!(!feat.is_current, "not the current one");
    assert!(
        matches!(&feat.head, Some(HeadState::Branch { name, .. }) if *name == full("refs/heads/feat")),
        "feat head: {:?}",
        feat.head
    );
    let det = entries
        .iter()
        .find(|e| same_path(&e.path, &detached))
        .expect("detached worktree listed");
    assert!(
        matches!(det.head, Some(HeadState::Detached { target }) if target == head),
        "detached head"
    );
    assert_eq!(det.locked.as_deref(), Some("usb stick"), "lock reason");

    let from_linked =
        worktree::list(&Repo::open(&env.rt, &wt).expect("open linked")).expect("list");
    let current: Vec<_> = from_linked.iter().filter(|e| e.is_current).collect();
    assert_eq!(current.len(), 1, "exactly one current entry");
    assert_eq!(
        current[0].id.as_deref(),
        Some("wt-feat"),
        "the linked one is current"
    );
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn add_existing_branch() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    fixture.git(&["branch", "feat"]);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let wt = env.path("wt");
    let out = worktree::add(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &opts(&wt, WorktreeTarget::ExistingBranch(full("refs/heads/feat"))),
    )
    .expect("add");
    assert!(
        out.refresh
            .contains(RefreshSet::WORKTREES | RefreshSet::BRANCHES),
        "refresh"
    );
    assert!(
        same_path(&out.value.path, &wt),
        "returned entry is the new worktree"
    );
    assert!(
        matches!(&out.value.head, Some(HeadState::Branch { name, .. }) if *name == full("refs/heads/feat")),
        "feat is checked out"
    );
    assert!(wt.join("a.txt").is_file(), "files are checked out");
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn add_new_branch_from_commit() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let first = commit(&fixture, "a.txt", "one");
    commit(&fixture, "a.txt", "two");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let wt = env.path("wt");
    let mut o = opts(
        &wt,
        WorktreeTarget::NewBranch {
            name: "topic".into(),
            start: StartPoint::Commit(first),
        },
    );
    o.lock_reason = Some("busy".into());
    let out = worktree::add(&repo, guard.write_permission(), &ctx(&fixture), &o).expect("add");
    assert_eq!(
        fixture.git(&["rev-parse", "topic"]),
        first.to_string(),
        "branch starts at the commit"
    );
    assert_eq!(
        out.value.locked.as_deref(),
        Some("busy"),
        "locked with reason"
    );
    assert!(
        matches!(&out.value.head, Some(HeadState::Branch { name, target }) if *name == full("refs/heads/topic") && *target == first),
        "new branch is checked out"
    );

    let wt2 = env.path("wt2");
    let o = opts(
        &wt2,
        WorktreeTarget::NewBranch {
            name: "from-ref".into(),
            start: StartPoint::Ref(full("refs/heads/main")),
        },
    );
    worktree::add(&repo, guard.write_permission(), &ctx(&fixture), &o).expect("add from ref");
    assert_eq!(
        fixture.git(&["rev-parse", "from-ref"]),
        fixture.git(&["rev-parse", "main"]),
        "branch from a ref"
    );

    let o = opts(
        &env.path("wt3"),
        WorktreeTarget::NewBranch {
            name: "topic".into(),
            start: StartPoint::Head,
        },
    );
    let err =
        worktree::add(&repo, guard.write_permission(), &ctx(&fixture), &o).expect_err("exists");
    assert_eq!(err.kind(), GitErrorKind::AlreadyExists, "branch exists");
    let o = opts(
        &env.path("wt4"),
        WorktreeTarget::NewBranch {
            name: "bad name".into(),
            start: StartPoint::Head,
        },
    );
    let err =
        worktree::add(&repo, guard.write_permission(), &ctx(&fixture), &o).expect_err("invalid");
    assert_eq!(err.kind(), GitErrorKind::InvalidName, "invalid branch name");
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn add_detached() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let first = commit(&fixture, "a.txt", "one");
    commit(&fixture, "a.txt", "two");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let wt = env.path("wt");
    let mut o = opts(&wt, WorktreeTarget::Detached(first));
    o.lock_reason = Some(String::new());
    let out = worktree::add(&repo, guard.write_permission(), &ctx(&fixture), &o).expect("add");
    assert_eq!(
        out.value.head,
        Some(HeadState::Detached { target: first }),
        "detached at the commit"
    );
    // Plain `--lock` (no `--reason ""`): Git records its own text ("added with --lock") as the reason.
    assert!(
        out.value.locked.is_some(),
        "an empty reason still locks: {:?}",
        out.value.locked
    );
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn add_head_branch_of_bare_main_repository() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let bare = env.path("bare.git");
    fixture.git(&["clone", "-q", "--bare", ".", &path_str(&bare)]);
    let repo = Repo::open(&env.rt, &bare).expect("open the bare repository");
    let entries = worktree::list(&repo).expect("list");
    assert!(
        entries[0].bare && entries[0].id.is_none(),
        "bare main entry: {entries:?}"
    );
    let mut guard = repo.lock_exclusive();
    let wt = env.path("wt-main");
    let out = worktree::add(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &opts(&wt, WorktreeTarget::ExistingBranch(full("refs/heads/main"))),
    )
    .expect("a bare repository's HEAD branch is not checked out anywhere");
    assert!(
        matches!(&out.value.head, Some(HeadState::Branch { name, .. }) if *name == full("refs/heads/main")),
        "main is checked out in the new worktree: {:?}",
        out.value.head
    );
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn add_refuses_existing_path_before_branch_creation() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);

    let file = env.path("a-file");
    std::fs::write(&file, "x").expect("file");
    let dir = env.path("busy-dir");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("keep.txt"), "x").expect("file in dir");
    for target in [&file, &dir] {
        let o = opts(
            target,
            WorktreeTarget::NewBranch {
                name: "never".into(),
                start: StartPoint::Head,
            },
        );
        let err = worktree::add(&repo, guard.write_permission(), &ctx(&fixture), &o)
            .expect_err("occupied");
        assert_eq!(
            err.kind(),
            GitErrorKind::AlreadyExists,
            "{}",
            target.display()
        );
    }
    assert_eq!(
        fixture.git(&["branch", "--list", "never"]),
        "",
        "the branch was not created"
    );
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn add_into_existing_empty_dir() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let wt = env.path("empty");
    std::fs::create_dir_all(&wt).expect("dir");
    let o = opts(
        &wt,
        WorktreeTarget::NewBranch {
            name: "x".into(),
            start: StartPoint::Head,
        },
    );
    worktree::add(&repo, guard.write_permission(), &ctx(&fixture), &o)
        .expect("empty directory is fine");
    assert!(wt.join("a.txt").is_file(), "checked out into the directory");
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn add_branch_checked_out_elsewhere() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    fixture.git(&["branch", "feat"]);
    let first = env.path("first");
    fixture.git(&["worktree", "add", "-q", &path_str(&first), "feat"]);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let err = worktree::add(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &opts(
            &env.path("second"),
            WorktreeTarget::ExistingBranch(full("refs/heads/feat")),
        ),
    )
    .expect_err("feat is taken");
    match err {
        gitcthulhu_git::GitError::BranchCheckedOutElsewhere { branch, worktree } => {
            assert_eq!(branch, "feat", "branch name");
            assert!(
                same_path(&worktree, &first),
                "worktree path: {}",
                worktree.display()
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let err = worktree::add(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &opts(
            &env.path("third"),
            WorktreeTarget::ExistingBranch(full("refs/heads/main")),
        ),
    )
    .expect_err("main is checked out in the open repository");
    assert_eq!(
        err.kind(),
        GitErrorKind::BranchCheckedOutElsewhere,
        "the current worktree counts too"
    );
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn remove_dirty_requires_force() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let wt = env.path("wt");
    fixture.git(&["worktree", "add", "-q", "-b", "feat", &path_str(&wt)]);
    std::fs::write(wt.join("a.txt"), "dirty").expect("dirty the worktree");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);

    let err = worktree::remove(&repo, guard.write_permission(), &ctx(&fixture), &wt, false)
        .expect_err("dirty without force");
    assert_eq!(
        err.kind(),
        GitErrorKind::UncommittedChanges,
        "git refuses, reported so the GUI can offer force: {err:?}"
    );
    assert!(wt.is_dir(), "the worktree is still there");
    worktree::remove(&repo, guard.write_permission(), &ctx(&fixture), &wt, true).expect("forced");
    assert!(!wt.exists(), "the worktree is gone");
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn remove_locked_needs_double_force() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let wt = env.path("wt");
    fixture.git(&["worktree", "add", "-q", "-b", "feat", &path_str(&wt)]);
    fixture.git(&["worktree", "lock", &path_str(&wt)]);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    worktree::remove(&repo, guard.write_permission(), &ctx(&fixture), &wt, false)
        .expect_err("locked");
    assert!(wt.is_dir(), "still there");
    worktree::remove(&repo, guard.write_permission(), &ctx(&fixture), &wt, true)
        .expect("force removes a locked worktree (passed twice)");
    assert!(!wt.exists(), "removed");
}

#[test]
fn remove_main_or_current_is_unsupported() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let wt = env.path("wt");
    fixture.git(&["worktree", "add", "-q", "-b", "feat", &path_str(&wt)]);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let err = worktree::remove(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &fixture.path,
        true,
    )
    .expect_err("main worktree");
    assert_eq!(
        err.kind(),
        GitErrorKind::Unsupported,
        "the main worktree cannot be removed"
    );

    let linked = Repo::open(&env.rt, &wt).expect("open linked");
    // Same common directory, so the main repository's guard is the permission for both.
    let err = worktree::remove(&linked, guard.write_permission(), &ctx(&fixture), &wt, true)
        .expect_err("current worktree");
    assert_eq!(
        err.kind(),
        GitErrorKind::Unsupported,
        "the worktree of the open repository"
    );
    let err = worktree::remove(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &env.path("nowhere"),
        false,
    )
    .expect_err("unknown path");
    assert_eq!(err.kind(), GitErrorKind::NotFound, "unknown worktree");
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn remove_returns_worktree_id() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let wt = env.path("my-wt");
    fixture.git(&["worktree", "add", "-q", "-b", "feat", &path_str(&wt)]);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let out = worktree::remove(&repo, guard.write_permission(), &ctx(&fixture), &wt, false)
        .expect("remove");
    assert_eq!(
        out.value, "my-wt",
        "id = directory name under <common>/worktrees/"
    );
    assert!(
        out.refresh
            .contains(RefreshSet::WORKTREES | RefreshSet::BRANCHES),
        "refresh"
    );
    assert_eq!(
        worktree::list(&repo).expect("list").len(),
        1,
        "only main is left"
    );
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn prune_gone_worktree_returns_id() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let gone = env.path("gone-wt");
    let kept = env.path("kept-wt");
    fixture.git(&["worktree", "add", "-q", "-b", "g", &path_str(&gone)]);
    fixture.git(&["worktree", "add", "-q", "-b", "k", &path_str(&kept)]);
    std::fs::remove_dir_all(&gone).expect("delete the worktree directory behind git's back");
    let repo = fixture.open();
    let entries = worktree::list(&repo).expect("list");
    assert!(
        entries
            .iter()
            .any(|e| e.id.as_deref() == Some("gone-wt") && e.prunable),
        "the missing directory is prunable: {entries:?}"
    );
    let mut guard = fixture.exclusive(&repo);
    let out = worktree::prune(&repo, guard.write_permission(), &ctx(&fixture)).expect("prune");
    assert_eq!(out.value, ["gone-wt"], "only the gone worktree is pruned");
    assert_eq!(worktree::list(&repo).expect("list").len(), 2, "main + kept");
}

#[test]
#[ignore = "needs WP-B0b (executor)"]
fn lock_unlock_with_reason() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let wt = env.path("wt");
    fixture.git(&["worktree", "add", "-q", "-b", "feat", &path_str(&wt)]);
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let find = |repo: &Repo| {
        worktree::list(repo)
            .expect("list")
            .into_iter()
            .find(|e| same_path(&e.path, &wt))
            .expect("listed")
    };
    worktree::lock(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &wt,
        Some("on a stick"),
    )
    .expect("lock");
    assert_eq!(
        find(&repo).locked.as_deref(),
        Some("on a stick"),
        "locked with reason"
    );
    worktree::unlock(&repo, guard.write_permission(), &ctx(&fixture), &wt).expect("unlock");
    assert_eq!(find(&repo).locked, None, "unlocked");
    worktree::lock(&repo, guard.write_permission(), &ctx(&fixture), &wt, None)
        .expect("lock without reason");
    assert_eq!(
        find(&repo).locked.as_deref(),
        Some(""),
        "locked without reason is Some(\"\")"
    );
    let err = worktree::lock(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &fixture.path,
        None,
    )
    .expect_err("main");
    assert_eq!(
        err.kind(),
        GitErrorKind::Unsupported,
        "the main worktree cannot be locked"
    );
}

#[test]
fn open_linked_worktree_as_repo() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let wt = env.path("wt-open");
    fixture.git(&["worktree", "add", "-q", "-b", "feat", &path_str(&wt)]);
    let repo = Repo::open(&env.rt, &wt).expect("open the linked worktree");
    assert_eq!(
        repo.kind(),
        &RepoKind::LinkedWorktree {
            id: "wt-open".into()
        },
        "Repo::open reports LinkedWorktree"
    );
    assert!(
        same_path(repo.common_dir(), &fixture.path.join(".git")),
        "common dir is the main one"
    );
}
