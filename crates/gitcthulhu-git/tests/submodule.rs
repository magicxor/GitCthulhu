//! Tests of `submodule` (spec §2.5/§2.6, WP-B5). Every fixture relies on `protocol.file.allow=always`, which the
//! shared test configuration sets.

mod support;

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use gitcthulhu_git::{
    CancelToken, GitErrorKind, NoPrompter, OpContext, Progress, RefreshSet, Repo, SubmoduleState,
    submodule,
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

/// A library repository, and a superproject that has it as submodule `sub`.
fn superproject(env: &TestEnv) -> (TestRepo, TestRepo, gix::ObjectId) {
    let lib = env.repo("lib");
    let lib_head = commit(&lib, "lib.txt", "lib one");
    let sup = env.repo("super");
    commit(&sup, "a.txt", "super one");
    sup.git(&["submodule", "add", "-q", &path_str(&lib.path), "sub"]);
    sup.git(&["commit", "-q", "-m", "add submodule"]);
    (lib, sup, lib_head)
}

fn list(repo: &Repo, with_dirty: bool) -> Vec<gitcthulhu_git::SubmoduleInfo> {
    submodule::list(repo, with_dirty, &CancelToken::new()).expect("list")
}

#[test]
fn list_states_uninitialized_initialized_checked_out_missing() {
    let env = TestEnv::new();
    let (lib, sup, lib_head) = superproject(&env);

    let infos = list(&sup.open(), false);
    assert_eq!(infos.len(), 1, "one submodule");
    let sub = &infos[0];
    assert_eq!(sub.name, "sub", "name");
    assert_eq!(sub.path, "sub", "path");
    assert_eq!(sub.state, SubmoduleState::CheckedOut, "just added");
    assert_eq!(sub.index_commit, Some(lib_head), "index commit");
    assert_eq!(sub.head_commit, Some(lib_head), "head commit");
    assert!(
        sub.url.as_deref().is_some_and(|u| u.contains("lib")),
        "url: {:?}",
        sub.url
    );
    assert_eq!(sub.dirty, None, "dirty is opt-in");

    let clone = env.path("clone");
    sup.git(&["clone", "-q", &path_str(&sup.path), &path_str(&clone)]);
    let cloned = Repo::open(&env.rt, &clone).expect("open clone");
    let infos = list(&cloned, false);
    assert_eq!(infos[0].state, SubmoduleState::Uninitialized, "fresh clone");
    assert_eq!(infos[0].head_commit, None, "nothing checked out");

    TestRepo::init_with(&env, &clone, &["submodule", "init"]); // runs `git submodule init` in the clone
    let infos = list(&Repo::open(&env.rt, &clone).expect("reopen"), false);
    assert_eq!(
        infos[0].state,
        SubmoduleState::Initialized,
        "configured, not cloned"
    );

    std::fs::remove_dir_all(sup.path.join("sub")).expect("remove the submodule directory");
    let infos = list(&sup.open(), false);
    assert_eq!(
        infos[0].state,
        SubmoduleState::Missing,
        "in the index, not on disk"
    );
    drop(lib);
}

#[test]
fn dirty_detection_opt_in() {
    let env = TestEnv::new();
    let (_lib, sup, _) = superproject(&env);
    let repo = sup.open();
    assert_eq!(list(&repo, true)[0].dirty, Some(false), "clean");
    std::fs::write(sup.path.join("sub").join("lib.txt"), "changed").expect("modify a file");
    assert_eq!(list(&repo, false)[0].dirty, None, "not computed by default");
    assert_eq!(list(&repo, true)[0].dirty, Some(true), "modified file");
}

#[test]
fn add_validates_input() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    std::fs::create_dir_all(fixture.path.join("busy")).expect("dir");
    std::fs::write(fixture.path.join("busy").join("x"), "x").expect("file");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let try_add = |guard: &mut gitcthulhu_git::RepoExclusiveGuard, url: &str, path: &str| {
        submodule::add(
            &repo,
            guard.write_permission(),
            &ctx(&fixture),
            url,
            &path.into(),
            None,
            None,
        )
        .expect_err("invalid")
        .kind()
    };
    assert_eq!(
        try_add(&mut guard, "  ", "sub"),
        GitErrorKind::InvalidName,
        "blank url"
    );
    assert_eq!(
        try_add(&mut guard, "-evil", "sub"),
        GitErrorKind::InvalidName,
        "option-like url"
    );
    assert_eq!(
        try_add(&mut guard, "https://h/x.git", " "),
        GitErrorKind::InvalidName,
        "blank path"
    );
    assert_eq!(
        try_add(&mut guard, "https://h/x.git", "busy"),
        GitErrorKind::AlreadyExists,
        "non-empty target"
    );
}

#[test]
fn add_and_list() {
    let env = TestEnv::new();
    let lib = env.repo("lib");
    let lib_head = commit(&lib, "lib.txt", "lib one");
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let out = submodule::add(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &path_str(&lib.path),
        &"vendor/lib".into(),
        Some("libname"),
        None,
    )
    .expect("add");
    assert!(
        out.refresh
            .contains(RefreshSet::SUBMODULES | RefreshSet::STATUS | RefreshSet::CONFIG),
        "refresh set"
    );
    let infos = list(&repo, false);
    assert_eq!(infos.len(), 1, "listed");
    assert_eq!(infos[0].name, "libname", "custom name");
    assert_eq!(infos[0].path, "vendor/lib", "nested path");
    assert_eq!(infos[0].state, SubmoduleState::CheckedOut, "checked out");
    assert_eq!(infos[0].head_commit, Some(lib_head), "at the library head");
    assert!(
        fixture.path.join("vendor/lib/lib.txt").is_file(),
        "files present"
    );
    assert!(
        fixture.git(&["status", "--short"]).contains(".gitmodules"),
        ".gitmodules staged"
    );
    // Adding into a now non-empty directory fails before Git runs.
    let err = submodule::add(
        &repo,
        guard.write_permission(),
        &ctx(&fixture),
        &path_str(&lib.path),
        &"vendor/lib".into(),
        None,
        None,
    )
    .expect_err("occupied");
    assert_eq!(err.kind(), GitErrorKind::AlreadyExists, "target exists");
}

/// `--progress` is passed on every supported Git (2.34.1 accepts it) and the clone's progress reaches the sink.
#[test]
fn add_reports_clone_progress() {
    let env = TestEnv::new();
    let lib = env.repo("lib");
    commit(&lib, "lib.txt", "lib one");
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    let repo = fixture.open();
    let mut guard = fixture.exclusive(&repo);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let ctx = OpContext {
        cancel: CancelToken::new(),
        progress: Arc::new(move |ev: Progress| sink.lock().expect("lock").push(ev)),
        prompter: Arc::new(NoPrompter),
        label: "submodule add".into(),
    };
    // A file:// URL: a plain path clones locally (hard links) and reports no transfer phases.
    let url = format!(
        "file:///{}",
        path_str(&lib.path)
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    submodule::add(
        &repo,
        guard.write_permission(),
        &ctx,
        &url,
        &"sub".into(),
        None,
        None,
    )
    .expect("add");
    assert!(
        fixture.path.join("sub/lib.txt").is_file(),
        "the submodule is checked out"
    );
    let events = events.lock().expect("lock");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Progress::Command(line) if line.contains("--progress"))),
        "--progress is passed on Git {}: {events:?}",
        env.rt.version()
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Progress::Phase { phase, .. } if phase == "Receiving objects")),
        "the clone's transfer phases are reported: {events:?}"
    );
}

#[test]
fn init_update_checks_out() {
    let env = TestEnv::new();
    let (_lib, sup, lib_head) = superproject(&env);
    let clone = env.path("clone");
    sup.git(&["clone", "-q", &path_str(&sup.path), &path_str(&clone)]);
    let repo = Repo::open(&env.rt, &clone).expect("open clone");
    let mut guard = repo.lock_exclusive();
    submodule::init_update(&repo, guard.write_permission(), &ctx(&sup), &[], false)
        .expect("init+update");
    let infos = list(&repo, false);
    assert_eq!(infos[0].state, SubmoduleState::CheckedOut, "checked out");
    assert_eq!(infos[0].head_commit, Some(lib_head), "recorded commit");
    assert!(clone.join("sub/lib.txt").is_file(), "files exist");

    // `update` moves a checked-out submodule back to the recorded commit.
    let before = infos[0].index_commit;
    submodule::update(
        &repo,
        guard.write_permission(),
        &ctx(&sup),
        &["sub".into()],
        true,
    )
    .expect("update");
    assert_eq!(
        list(&repo, false)[0].head_commit,
        before,
        "still at the recorded commit"
    );
}

#[test]
fn sync_updates_url() {
    let env = TestEnv::new();
    let (_lib, sup, _) = superproject(&env);
    let repo = sup.open();
    sup.git(&[
        "config",
        "-f",
        ".gitmodules",
        "submodule.sub.url",
        "https://example.com/moved.git",
    ]);
    assert_ne!(
        sup.git(&["config", "submodule.sub.url"]),
        "https://example.com/moved.git",
        "precondition: .git/config still has the old url"
    );
    let mut guard = sup.exclusive(&repo);
    let out =
        submodule::sync(&repo, guard.write_permission(), &ctx(&sup), &[], false).expect("sync");
    assert!(out.refresh.contains(RefreshSet::CONFIG), "CONFIG refresh");
    assert_eq!(
        sup.git(&["config", "submodule.sub.url"]),
        "https://example.com/moved.git",
        "url synchronised into .git/config"
    );
}

#[test]
fn deinit_then_list_uninitialized() {
    let env = TestEnv::new();
    let (_lib, sup, _) = superproject(&env);
    let repo = sup.open();
    let mut guard = sup.exclusive(&repo);
    submodule::deinit(
        &repo,
        guard.write_permission(),
        &ctx(&sup),
        &["sub".into()],
        false,
    )
    .expect("deinit");
    let infos = list(&repo, false);
    assert_eq!(infos.len(), 1, "still declared in .gitmodules");
    assert_eq!(
        infos[0].state,
        SubmoduleState::Uninitialized,
        "deinit removes the configuration"
    );
}

#[test]
fn remove_cleans_modules_dir() {
    let env = TestEnv::new();
    let (_lib, sup, _) = superproject(&env);
    let repo = sup.open();
    assert!(
        sup.path.join(".git/modules/sub").is_dir(),
        "precondition: the module repository exists"
    );
    let mut guard = sup.exclusive(&repo);
    submodule::remove(&repo, guard.write_permission(), &ctx(&sup), &"sub".into()).expect("remove");
    assert!(
        !sup.path.join(".git/modules/sub").exists(),
        "modules directory deleted"
    );
    assert!(!sup.path.join("sub").exists(), "worktree directory deleted");
    assert!(
        !String::from_utf8_lossy(&sup.read(".gitmodules")).contains("sub"),
        ".gitmodules entry removed by git rm"
    );
    assert!(list(&repo, false).is_empty(), "no submodules left");
    let err = submodule::remove(&repo, guard.write_permission(), &ctx(&sup), &"sub".into())
        .expect_err("gone");
    assert_eq!(err.kind(), GitErrorKind::NotFound, "unknown submodule");
    assert!(
        same_path(repo.common_dir(), &sup.path.join(".git")),
        "sanity"
    );
}
