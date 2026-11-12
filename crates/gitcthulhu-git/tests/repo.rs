//! Tests of `GitRuntime` and `Repo` (spec §2.2).

mod support;

use std::{sync::mpsc, time::Duration};

use gitcthulhu_git::{GitError, GitErrorKind, GitVersion, Repo, RepoKind};
use support::{TestEnv, same_path};

fn assert_kind<T: std::fmt::Debug>(result: Result<T, GitError>, expected: GitErrorKind, why: &str) {
    match result {
        Err(err) => assert_eq!(err.kind(), expected, "{why}: got {err:?}"),
        Ok(value) => panic!("{why}: expected {expected:?}, got Ok({value:?})"),
    }
}

#[test]
fn open_from_subdirectory_finds_root() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a/b/file.txt", "x");
    fixture.commit_all("one");

    let repo = Repo::open(&env.rt, &fixture.path.join("a").join("b"))
        .expect("a sub-directory opens its work tree");
    assert_eq!(repo.kind(), &RepoKind::Main, "an ordinary repository");
    let workdir = repo.workdir().expect("not bare");
    assert!(
        same_path(workdir, &fixture.path),
        "workdir is the root, not the sub-directory: {}",
        workdir.display()
    );
    assert!(
        same_path(repo.git_dir(), &fixture.path.join(".git")),
        "git dir is <root>/.git: {}",
        repo.git_dir().display()
    );
    assert_eq!(
        repo.git_dir(),
        repo.common_dir(),
        "main worktree: per-worktree and common git dir coincide"
    );
}

#[test]
fn open_non_repo_is_not_a_repository() {
    let env = TestEnv::new();
    let plain = env.path("plain");
    std::fs::create_dir_all(&plain).expect("directory can be created");
    assert_kind(
        Repo::open(&env.rt, &plain),
        GitErrorKind::NotARepository,
        "an ordinary directory",
    );

    let file = plain.join("file.txt");
    std::fs::write(&file, "x").expect("file can be written");
    assert_kind(
        Repo::open(&env.rt, &file),
        GitErrorKind::NotARepository,
        "a file is not a directory",
    );

    assert_kind(
        Repo::open(&env.rt, &env.path("missing")),
        GitErrorKind::NotFound,
        "a path that does not exist",
    );
}

#[test]
fn open_bare_repository_reports_bare() {
    let env = TestEnv::new();
    let bare = env.bare_remote("remote");
    let repo = Repo::open(&env.rt, &bare).expect("a bare repository opens");
    assert_eq!(repo.kind(), &RepoKind::Bare, "bare kind");
    assert!(
        repo.workdir().is_none(),
        "a bare repository has no work tree"
    );
}

#[test]
fn open_linked_worktree_reports_kind_and_paths() {
    let env = TestEnv::new();
    let main = env.repo("main-repo");
    main.write("f.txt", "x");
    main.commit_all("one");
    let linked = env.path("linked");
    main.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "side",
        linked.to_str().expect("utf-8 path"),
    ]);

    let repo = Repo::open(&env.rt, &linked).expect("a linked worktree opens");
    assert_eq!(
        repo.kind(),
        &RepoKind::LinkedWorktree {
            id: "linked".into()
        },
        "the id is the directory name under worktrees/"
    );
    assert!(
        same_path(repo.workdir().expect("not bare"), &linked),
        "workdir is the linked checkout"
    );
    assert!(
        same_path(repo.common_dir(), &main.path.join(".git")),
        "common dir is the main repository's git dir: {}",
        repo.common_dir().display()
    );
    assert!(
        same_path(
            repo.git_dir(),
            &main.path.join(".git").join("worktrees").join("linked")
        ),
        "git dir is the per-worktree directory: {}",
        repo.git_dir().display()
    );
    assert_ne!(
        repo.git_dir(),
        repo.common_dir(),
        "the two differ for linked worktrees"
    );
    assert!(
        !repo
            .common_dir()
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)),
        "the common dir is normalised (gix joins the `../..` of the `commondir` file): {}",
        repo.common_dir().display()
    );

    let main_repo = main.open();
    assert_eq!(
        repo.common_dir(),
        main_repo.common_dir(),
        "main and linked worktree report the identical common dir, the key of the in-process lock"
    );
}

#[test]
fn linked_worktree_shares_the_exclusive_lock_with_main() {
    let env = TestEnv::new();
    let main = env.repo("main-repo");
    main.write("f.txt", "x");
    main.commit_all("one");
    let linked = env.path("linked");
    main.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "side",
        linked.to_str().expect("utf-8 path"),
    ]);
    let main_repo = main.open();
    let linked_repo = Repo::open(&env.rt, &linked).expect("a linked worktree opens");

    let guard = main_repo.lock_exclusive();
    assert!(
        linked_repo
            .try_lock_exclusive(Duration::from_millis(100))
            .is_none(),
        "refs are shared by all worktrees, so a tab on the linked worktree must wait for the main one"
    );
    drop(guard);
    assert!(
        linked_repo
            .try_lock_exclusive(Duration::from_secs(5))
            .is_some(),
        "free again once the main worktree's guard is dropped"
    );
}

#[test]
fn open_submodule_reports_kind() {
    let env = TestEnv::new();
    let inner = env.repo("inner");
    inner.write("lib.txt", "lib");
    inner.commit_all("inner one");
    let outer = env.repo("outer");
    outer.write("top.txt", "top");
    outer.commit_all("outer one");
    outer.git(&[
        "submodule",
        "add",
        "-q",
        inner.path.to_str().expect("utf-8 path"),
        "sub",
    ]);

    let repo = Repo::open(&env.rt, &outer.path.join("sub"))
        .expect("a submodule directory opens the submodule");
    match repo.kind() {
        RepoKind::Submodule { superproject } => {
            let superproject = superproject
                .as_deref()
                .expect("the superproject is found through .git/modules");
            assert!(
                same_path(superproject, &outer.path),
                "superproject is the outer work tree: {}",
                superproject.display()
            );
        }
        other => panic!("expected a submodule, got {other:?}"),
    }
    assert!(
        same_path(repo.workdir().expect("not bare"), &outer.path.join("sub")),
        "workdir is the submodule checkout"
    );
}

#[test]
fn open_nested_submodule_reports_its_immediate_superproject() {
    let env = TestEnv::new();
    let leaf = env.repo("leaf");
    leaf.write("leaf.txt", "leaf");
    leaf.commit_all("leaf one");
    let middle = env.repo("middle");
    middle.write("middle.txt", "middle");
    middle.commit_all("middle one");
    middle.git(&[
        "submodule",
        "add",
        "-q",
        leaf.path.to_str().expect("utf-8 path"),
        "leaf",
    ]);
    middle.commit_all("add leaf");
    let outer = env.repo("outer");
    outer.write("top.txt", "top");
    outer.commit_all("outer one");
    outer.git(&[
        "submodule",
        "add",
        "-q",
        middle.path.to_str().expect("utf-8 path"),
        "middle",
    ]);
    outer.git(&["submodule", "update", "-q", "--init", "--recursive"]);

    let nested = outer.path.join("middle").join("leaf");
    let repo = Repo::open(&env.rt, &nested).expect("a nested submodule opens");
    match repo.kind() {
        RepoKind::Submodule { superproject } => {
            let superproject = superproject.as_deref().expect("the superproject is found");
            assert!(
                same_path(superproject, &outer.path.join("middle")),
                "the superproject of a nested submodule is the intermediate submodule, not the top-level repository: {}",
                superproject.display()
            );
        }
        other => panic!("expected a submodule, got {other:?}"),
    }
}

#[test]
fn open_reftable_repo_is_unsupported() {
    let env = TestEnv::new();
    if env.rt.version()
        < (GitVersion {
            major: 2,
            minor: 45,
            patch: 0,
        })
    {
        eprintln!(
            "skipped: reftable needs Git 2.45, found {}",
            env.rt.version()
        );
        return;
    }
    let path = env.path("reftable");
    std::fs::create_dir_all(&path).expect("directory can be created");
    let fixture = support::TestRepo::init_with(
        &env,
        &path,
        &["init", "-q", "--ref-format=reftable", "-b", "main"],
    );
    fixture.write("f.txt", "x");
    fixture.commit_all("one");

    assert_kind(
        Repo::open(&env.rt, &path),
        GitErrorKind::Unsupported,
        "reftable repositories show no refs to gix and must be refused",
    );
}

#[test]
fn init_creates_branch_main() {
    let env = TestEnv::new();
    let path = env.path("fresh");
    let repo = Repo::init(&env.rt, &path, Some("trunk")).expect("init works");
    let head = std::fs::read_to_string(repo.git_dir().join("HEAD")).expect("HEAD exists");
    assert_eq!(
        head.trim(),
        "ref: refs/heads/trunk",
        "the initial branch is the requested one"
    );

    let default_path = env.path("fresh-default");
    let default =
        Repo::init(&env.rt, &default_path, None).expect("init works without a branch name");
    let head = std::fs::read_to_string(default.git_dir().join("HEAD")).expect("HEAD exists");
    assert_eq!(
        head.trim(),
        "ref: refs/heads/main",
        "init.defaultBranch of the isolated global config applies"
    );
}

#[test]
fn init_runs_template_hooks() {
    let env = TestEnv::new();
    let template = env.path("template");
    std::fs::create_dir_all(template.join("hooks")).expect("template can be created");
    std::fs::write(
        template.join("hooks").join("pre-commit"),
        "#!/bin/sh\nexit 0\n",
    )
    .expect("hook template");
    env.git_global(&[
        "config",
        "--file",
        env.global_config.to_str().expect("utf-8 path"),
        "init.templateDir",
        &template.to_string_lossy().replace('\\', "/"),
    ]);

    let repo = Repo::init(&env.rt, &env.path("with-template"), None).expect("init works");
    assert!(
        repo.git_dir().join("hooks").join("pre-commit").is_file(),
        "git init copied the hook from init.templateDir"
    );
}

#[test]
fn reload_sees_new_remote() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "x");
    fixture.commit_all("one");
    let repo = fixture.open();
    assert!(
        repo.local().remote_names().is_empty(),
        "no remotes configured yet"
    );

    fixture.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);
    assert!(
        repo.local().remote_names().is_empty(),
        "the config snapshot is stale until reload"
    );
    repo.reload().expect("reload works");
    let names: Vec<String> = repo
        .local()
        .remote_names()
        .iter()
        .map(|n| n.to_string())
        .collect();
    assert_eq!(names, ["origin"], "reload re-reads the configuration");
}

#[test]
fn git_too_old_is_reported() {
    let env = TestEnv::new();
    let fake = env.path(if cfg!(windows) {
        "fake-git.cmd"
    } else {
        "fake-git"
    });
    #[cfg(windows)]
    std::fs::write(&fake, "@echo git version 2.30.0\r\n").expect("fake git can be written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(&fake, "#!/bin/sh\necho 'git version 2.30.0'\n")
            .expect("fake git can be written");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .expect("fake git can be made executable");
    }

    match env.runtime_with(|config| config.git_path = Some(fake.clone())) {
        Err(GitError::GitTooOld { found, required }) => {
            assert_eq!(
                found,
                GitVersion {
                    major: 2,
                    minor: 30,
                    patch: 0
                },
                "the fake's version"
            );
            assert_eq!(required, GitVersion::MIN, "the documented minimum");
        }
        other => panic!("expected GitTooOld, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn git_not_found_is_reported() {
    let env = TestEnv::new();
    let result =
        env.runtime_with(|config| config.git_path = Some(env.path("does-not-exist").join("git")));
    match result {
        Err(GitError::GitNotFound { searched }) => {
            assert!(
                searched.contains("does-not-exist"),
                "the searched path is named: {searched}"
            );
        }
        other => panic!("expected GitNotFound, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn missing_helper_reported_in_status() {
    let env = TestEnv::new();
    let status = env.rt.helper_status();
    assert!(
        status.askpass.is_ok() && env.rt.has_askpass(),
        "the test askpass helper is built"
    );
    assert!(
        status.editor.is_ok() && env.rt.has_editor_helper(),
        "the test editor helper is built"
    );

    let missing = env.path("no-askpass");
    let rt = env
        .runtime_with(|config| config.askpass_exe = Some(missing.clone()))
        .expect("a missing helper does not fail the runtime");
    assert!(!rt.has_askpass(), "askpass is reported missing");
    assert_eq!(
        rt.helper_status().askpass,
        Err(missing),
        "the expected path is reported for the missing helper"
    );
    assert!(rt.has_editor_helper(), "the editor helper is unaffected");
}

#[test]
fn try_lock_exclusive_times_out_while_held() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "x");
    fixture.commit_all("one");
    let repo = fixture.open();

    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = {
        let repo = repo.clone();
        std::thread::spawn(move || {
            // The guard is `!Send`: take and drop it on this thread.
            let guard = repo.lock_exclusive();
            locked_tx.send(()).expect("the main thread listens");
            release_rx.recv().expect("the main thread releases");
            drop(guard);
        })
    };
    locked_rx.recv().expect("the holder got the lock");

    let started = std::time::Instant::now();
    assert!(
        repo.try_lock_exclusive(Duration::from_millis(200))
            .is_none(),
        "the lock is held by the other thread"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "the attempt waited for the timeout"
    );

    release_tx.send(()).expect("the holder listens");
    holder.join().expect("the holder finishes");
    assert!(
        repo.try_lock_exclusive(Duration::from_secs(5)).is_some(),
        "after the holder let go the lock is available"
    );
}

#[test]
fn runtime_reports_version_and_git_path() {
    let env = TestEnv::new();
    assert!(
        env.rt.version() >= GitVersion::MIN,
        "the runtime enforces the minimum version"
    );
    assert!(
        env.rt.git_path().file_name().is_some(),
        "the git path names an executable: {}",
        env.rt.git_path().display()
    );
}
