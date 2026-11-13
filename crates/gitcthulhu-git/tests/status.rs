//! Tests of `status::status` and `status::refresh_index` (spec §2.5).

mod support;

use gitcthulhu_git::{
    CancelToken, ChangeKind, ConflictKind, EntryKind, GitError, GitErrorKind, RenameMode, Status,
    StatusEntry, StatusOptions, SubmoduleStatusMode, UntrackedMode,
    status::{refresh_index, status},
};
use support::{TestEnv, TestRepo, symlinks_supported};

fn run(repo: &TestRepo, opts: &StatusOptions) -> Status {
    status(&repo.open(), opts, &CancelToken::new()).expect("status succeeds")
}

fn run_default(repo: &TestRepo) -> Status {
    run(repo, &StatusOptions::default())
}

fn find<'a>(list: &'a [StatusEntry], path: &str) -> &'a StatusEntry {
    list.iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("{path} is in {list:?}"))
}

fn prev(entry: &StatusEntry) -> Option<String> {
    entry
        .previous_path
        .as_ref()
        .map(|p| String::from_utf8_lossy(p).into_owned())
}

fn paths(list: &[StatusEntry]) -> Vec<String> {
    list.iter().map(|e| e.path.to_string()).collect()
}

/// A repository with one commit holding `a.txt`.
fn base(env: &TestEnv) -> TestRepo {
    let repo = env.repo("repo");
    repo.write("a.txt", "one\ntwo\nthree\n");
    repo.commit_all("base");
    repo
}

const LINES: &str = "alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\ngolf\nhotel\nindia\njuliet\n";

#[test]
fn clean_repo_is_empty() {
    let env = TestEnv::new();
    let repo = base(&env);
    let status = run_default(&repo);
    assert!(
        status.staged.is_empty(),
        "nothing staged: {:?}",
        status.staged
    );
    assert!(
        status.unstaged.is_empty(),
        "nothing unstaged: {:?}",
        status.unstaged
    );
    assert!(status.counts.is_clean(), "counts are zero");
    assert!(
        status.fingerprint.head.is_some(),
        "HEAD is part of the fingerprint"
    );
    assert!(
        status.fingerprint.index.is_some(),
        "the index file is part of the fingerprint"
    );
}

#[test]
fn untracked_file_is_unstaged_untracked() {
    let env = TestEnv::new();
    let repo = base(&env);
    repo.write("new.txt", "x");
    let status = run_default(&repo);
    assert!(status.staged.is_empty(), "untracked files are not staged");
    let entry = find(&status.unstaged, "new.txt");
    assert_eq!(entry.kind, ChangeKind::Untracked, "kind");
    assert_eq!(
        entry.new.map(|s| s.kind),
        Some(EntryKind::Blob),
        "new side is a blob"
    );
    assert_eq!(status.counts.unstaged.untracked, 1, "counted");
}

#[test]
fn staged_and_unstaged_same_path_appear_in_both() {
    let env = TestEnv::new();
    let repo = base(&env);
    repo.write("a.txt", "one\ntwo\nthree\nfour\n");
    repo.git(&["add", "a.txt"]);
    repo.write("a.txt", "one\ntwo\nthree\nfour\nfive\n");
    let status = run_default(&repo);
    assert_eq!(
        find(&status.staged, "a.txt").kind,
        ChangeKind::Modified,
        "staged half"
    );
    assert_eq!(
        find(&status.unstaged, "a.txt").kind,
        ChangeKind::Modified,
        "unstaged half"
    );
    let staged = find(&status.staged, "a.txt");
    assert_eq!(
        staged.old.map(|s| s.id.to_string()),
        Some(repo.git(&["rev-parse", "HEAD:a.txt"])),
        "HEAD side"
    );
    assert_eq!(
        staged.new.map(|s| s.id.to_string()),
        Some(repo.git(&["rev-parse", ":a.txt"])),
        "index side"
    );
}

#[test]
fn staged_rename_detected() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    repo.write("old.txt", LINES);
    repo.commit_all("base");
    repo.git(&["mv", "old.txt", "new.txt"]);
    let status = run_default(&repo);
    assert_eq!(status.staged.len(), 1, "one entry: {:?}", status.staged);
    let entry = &status.staged[0];
    assert_eq!(entry.kind, ChangeKind::Renamed, "renamed");
    assert_eq!(entry.path, "new.txt", "destination");
    assert_eq!(prev(entry), Some("old.txt".into()), "source");
    assert!(
        status.unstaged.is_empty(),
        "nothing unstaged: {:?}",
        status.unstaged
    );
    assert_eq!(status.counts.staged.renamed, 1, "counted");
}

#[test]
fn worktree_rename_detected_with_untracked() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    repo.write("old.txt", LINES);
    repo.commit_all("base");
    std::fs::rename(repo.path.join("old.txt"), repo.path.join("moved.txt"))
        .expect("rename on disk");
    repo.write("other.txt", "unrelated\n");
    let status = run_default(&repo);
    let moved = find(&status.unstaged, "moved.txt");
    assert_eq!(
        moved.kind,
        ChangeKind::Renamed,
        "rename in the worktree: {:?}",
        status.unstaged
    );
    assert_eq!(prev(moved), Some("old.txt".into()), "source");
    assert_eq!(
        find(&status.unstaged, "other.txt").kind,
        ChangeKind::Untracked,
        "unrelated file stays untracked"
    );
    assert!(
        !status.unstaged.iter().any(|e| e.path == "old.txt"),
        "the source is not listed separately: {:?}",
        status.unstaged
    );
}

#[test]
fn worktree_rename_next_to_plain_deletion_and_staged_change() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    repo.write("old.txt", LINES);
    repo.write("gone.txt", "nothing like it\n");
    repo.write("a.txt", "a\n");
    repo.commit_all("base");
    repo.write("a.txt", "a2\n");
    repo.git(&["add", "a.txt"]);
    std::fs::rename(repo.path.join("old.txt"), repo.path.join("moved.txt"))
        .expect("rename on disk");
    repo.remove("gone.txt");
    repo.write("other.txt", "unrelated\n");
    let status = run_default(&repo);
    assert_eq!(
        find(&status.staged, "a.txt").kind,
        ChangeKind::Modified,
        "the staged half is kept: {:?}",
        status.staged
    );
    let moved = find(&status.unstaged, "moved.txt");
    assert_eq!(moved.kind, ChangeKind::Renamed, "{:?}", status.unstaged);
    assert_eq!(prev(moved), Some("old.txt".into()), "source");
    assert_eq!(
        find(&status.unstaged, "gone.txt").kind,
        ChangeKind::Deleted,
        "unmatched deletion"
    );
    assert_eq!(
        find(&status.unstaged, "other.txt").kind,
        ChangeKind::Untracked,
        "unmatched untracked file"
    );
    assert_eq!(
        paths(&status.unstaged),
        ["gone.txt", "moved.txt", "other.txt"],
        "each path once, sorted"
    );
    assert_eq!(status.counts.unstaged.renamed, 1, "counted");
}

#[test]
fn rename_limit_disables_tracking() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    for i in 0..3 {
        repo.write(&format!("f{i}.txt"), format!("file {i}\n{LINES}"));
    }
    repo.commit_all("base");
    for i in 0..3 {
        std::fs::rename(
            repo.path.join(format!("f{i}.txt")),
            repo.path.join(format!("g{i}.txt")),
        )
        .expect("rename on disk");
        // Inexact renames: only these are subject to the limit.
        let mut content = repo.read(&format!("g{i}.txt"));
        content.extend_from_slice(
            b"extra line
",
        );
        repo.write(&format!("g{i}.txt"), content);
    }
    let opts = StatusOptions {
        renames: RenameMode::IndexAndWorktree { limit: 1 },
        ..StatusOptions::default()
    };
    let limited = run(&repo, &opts);
    assert!(
        limited
            .unstaged
            .iter()
            .all(|e| e.kind != ChangeKind::Renamed),
        "a limit below the candidate count disables tracking: {:?}",
        limited.unstaged
    );
    let unlimited = run_default(&repo);
    assert_eq!(
        unlimited
            .unstaged
            .iter()
            .filter(|e| e.kind == ChangeKind::Renamed)
            .count(),
        3,
        "the default limit finds all renames: {:?}",
        unlimited.unstaged
    );
    let off = run(
        &repo,
        &StatusOptions {
            renames: RenameMode::Off,
            ..StatusOptions::default()
        },
    );
    assert!(
        off.unstaged.iter().all(|e| e.kind != ChangeKind::Renamed),
        "Off tracks nothing"
    );
}

#[test]
fn deleted_in_index_vs_worktree() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    repo.write("staged.txt", "s\n");
    repo.write("loose.txt", "l\n");
    repo.commit_all("base");
    repo.git(&["rm", "-q", "staged.txt"]);
    repo.remove("loose.txt");
    let status = run_default(&repo);
    assert_eq!(
        find(&status.staged, "staged.txt").kind,
        ChangeKind::Deleted,
        "git rm is staged"
    );
    assert!(
        !status.unstaged.iter().any(|e| e.path == "staged.txt"),
        "and not unstaged"
    );
    assert_eq!(
        find(&status.unstaged, "loose.txt").kind,
        ChangeKind::Deleted,
        "rm is unstaged"
    );
    assert!(
        !status.staged.iter().any(|e| e.path == "loose.txt"),
        "and not staged"
    );
    assert_eq!(find(&status.unstaged, "loose.txt").new, None, "no new side");
}

/// Commits `files` on main, creates `theirs` from there, lets both sides change, and merges into a conflict.
fn conflict_repo(
    env: &TestEnv,
    name: &str,
    base: &[(&str, &str)],
    ours: impl FnOnce(&TestRepo),
    theirs: impl FnOnce(&TestRepo),
) -> TestRepo {
    let repo = env.repo(name);
    repo.write("keep.txt", "keep\n");
    for (path, content) in base {
        repo.write(path, content);
    }
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "theirs"]);
    theirs(&repo);
    repo.commit_all("theirs");
    repo.git(&["checkout", "-q", "main"]);
    ours(&repo);
    repo.commit_all("ours");
    repo.git_fail(&["merge", "theirs"]);
    repo
}

fn assert_conflict(status: &Status, path: &str, kind: ConflictKind) {
    let entry = find(&status.unstaged, path);
    assert_eq!(entry.kind, ChangeKind::Conflicted, "{path} is conflicted");
    let info = entry.conflict.as_ref().expect("conflict info");
    assert_eq!(info.kind, kind, "{path}: kind");
    let expect = match kind {
        ConflictKind::BothModified => [true, true, true],
        ConflictKind::BothAdded => [false, true, true],
        ConflictKind::BothDeleted => [true, false, false],
        ConflictKind::AddedByUs => [false, true, false],
        ConflictKind::AddedByThem => [false, false, true],
        ConflictKind::DeletedByUs => [true, false, true],
        ConflictKind::DeletedByThem => [true, true, false],
    };
    assert_eq!(
        [
            info.base.is_some(),
            info.ours.is_some(),
            info.theirs.is_some()
        ],
        expect,
        "{path}: stages present"
    );
    assert!(
        !status.staged.iter().any(|e| e.path == path),
        "{path}: conflicted paths never appear in staged: {:?}",
        status.staged
    );
}

#[test]
fn conflict_kinds_all_seven() {
    let env = TestEnv::new();

    let repo = conflict_repo(
        &env,
        "both-modified",
        &[("f.txt", "base\n")],
        |r| r.write("f.txt", "ours\n"),
        |r| r.write("f.txt", "theirs\n"),
    );
    let status = run_default(&repo);
    assert_conflict(&status, "f.txt", ConflictKind::BothModified);
    assert_eq!(status.counts.unstaged.conflicted, 1, "counted");

    let repo = conflict_repo(
        &env,
        "both-added",
        &[],
        |r| r.write("f.txt", "ours\n"),
        |r| r.write("f.txt", "theirs\n"),
    );
    assert_conflict(&run_default(&repo), "f.txt", ConflictKind::BothAdded);

    let repo = conflict_repo(
        &env,
        "deleted-by-us",
        &[("f.txt", "base\n")],
        |r| {
            r.git(&["rm", "-q", "f.txt"]);
        },
        |r| r.write("f.txt", "theirs\n"),
    );
    assert_conflict(&run_default(&repo), "f.txt", ConflictKind::DeletedByUs);

    let repo = conflict_repo(
        &env,
        "deleted-by-them",
        &[("f.txt", "base\n")],
        |r| r.write("f.txt", "ours\n"),
        |r| {
            r.git(&["rm", "-q", "f.txt"]);
        },
    );
    assert_conflict(&run_default(&repo), "f.txt", ConflictKind::DeletedByThem);

    // rename/rename(1to2): the source is deleted by both, each destination is added by one side.
    let repo = conflict_repo(
        &env,
        "rename-rename",
        &[("a.txt", LINES)],
        |r| {
            r.git(&["mv", "a.txt", "b.txt"]);
        },
        |r| {
            r.git(&["mv", "a.txt", "c.txt"]);
        },
    );
    let status = run_default(&repo);
    assert_conflict(&status, "a.txt", ConflictKind::BothDeleted);
    assert_conflict(&status, "b.txt", ConflictKind::AddedByUs);
    assert_conflict(&status, "c.txt", ConflictKind::AddedByThem);
    let listed: Vec<_> = status
        .unstaged
        .iter()
        .map(|entry| (entry.path.to_string(), entry.kind))
        .collect();
    assert_eq!(
        listed,
        [
            ("a.txt".to_owned(), ChangeKind::Conflicted),
            ("b.txt".to_owned(), ChangeKind::Conflicted),
            ("c.txt".to_owned(), ChangeKind::Conflicted),
        ],
        "every conflicted path is listed once, never also as untracked (gix walks a theirs-only path as untracked)"
    );
    assert_eq!(status.counts.unstaged.untracked, 0, "nothing is untracked");
}

#[test]
fn intent_to_add_listed() {
    let env = TestEnv::new();
    let repo = base(&env);
    repo.write("n.txt", "new\n");
    repo.git(&["add", "-N", "n.txt"]);
    let status = run_default(&repo);
    assert_eq!(
        find(&status.unstaged, "n.txt").kind,
        ChangeKind::IntentToAdd,
        "listed as intent-to-add"
    );
    assert!(
        !status.staged.iter().any(|e| e.path == "n.txt"),
        "the tree-index half shows nothing: {:?}",
        status.staged
    );
}

/// A repository with a committed submodule `sub` that has one extra commit available.
fn submodule_repo(env: &TestEnv) -> TestRepo {
    let sub = env.repo("sub-origin");
    sub.write("s.txt", "1\n");
    sub.commit_all("sub one");
    let repo = env.repo("super");
    repo.write("a.txt", "a\n");
    repo.commit_all("super base");
    let url = sub.path.to_string_lossy().into_owned();
    repo.git(&["submodule", "add", "-q", &url, "sub"]);
    repo.commit_all("add submodule");
    repo
}

#[test]
fn submodule_pointer_change_head_only() {
    let env = TestEnv::new();
    let repo = submodule_repo(&env);
    let sub = TestRepo::init_with(&env, &repo.path.join("sub"), &["status"]);
    sub.write("s.txt", "2\n");
    sub.commit_all("sub two");
    let status = run_default(&repo);
    let entry = find(&status.unstaged, "sub");
    assert_eq!(entry.kind, ChangeKind::Modified, "pointer moved");
    let change = entry.submodule.as_ref().expect("submodule details");
    assert_ne!(
        change.old_commit, change.new_commit,
        "old and new commit differ"
    );
    assert_eq!(
        entry.new.map(|s| s.kind),
        Some(EntryKind::Submodule),
        "kind is submodule"
    );
    repo.git(&["add", "sub"]);
    let status = run_default(&repo);
    assert_eq!(
        find(&status.staged, "sub").kind,
        ChangeKind::Modified,
        "staged pointer change"
    );
    assert!(
        status.unstaged.is_empty(),
        "nothing left unstaged: {:?}",
        status.unstaged
    );
}

#[test]
fn submodule_dirty_only_with_head_and_dirty() {
    let env = TestEnv::new();
    let repo = submodule_repo(&env);
    std::fs::write(repo.path.join("sub").join("s.txt"), "dirty\n").expect("modify the submodule");
    let head_only = run_default(&repo);
    assert!(
        head_only.unstaged.is_empty(),
        "HeadOnly ignores a dirty submodule: {:?}",
        head_only.unstaged
    );
    let dirty = run(
        &repo,
        &StatusOptions {
            submodules: SubmoduleStatusMode::HeadAndDirty,
            ..StatusOptions::default()
        },
    );
    let entry = find(&dirty.unstaged, "sub");
    assert_eq!(
        entry.kind,
        ChangeKind::Modified,
        "dirty submodule is modified"
    );
    assert_eq!(
        entry.submodule.as_ref().and_then(|s| s.dirty),
        Some(true),
        "dirty flag"
    );
    let ignored = run(
        &repo,
        &StatusOptions {
            submodules: SubmoduleStatusMode::Ignore,
            ..StatusOptions::default()
        },
    );
    assert!(
        ignored.unstaged.is_empty(),
        "Ignore: {:?}",
        ignored.unstaged
    );
}

#[test]
fn submodule_moved_pointer_with_clean_worktree_is_not_dirty() {
    let env = TestEnv::new();
    let repo = submodule_repo(&env);
    let sub = TestRepo::init_with(&env, &repo.path.join("sub"), &["status"]);
    sub.write("s.txt", "2\n");
    sub.commit_all("sub two");
    let status = run(
        &repo,
        &StatusOptions {
            submodules: SubmoduleStatusMode::HeadAndDirty,
            ..StatusOptions::default()
        },
    );
    let entry = find(&status.unstaged, "sub");
    assert_eq!(entry.kind, ChangeKind::Modified, "pointer moved");
    let change = entry.submodule.as_ref().expect("submodule details");
    assert_ne!(change.old_commit, change.new_commit, "the commit moved");
    assert_ne!(
        change.dirty,
        Some(true),
        "a moved HEAD alone does not make the submodule's work tree dirty: {change:?}"
    );
}

/// `submodule.<name>.ignore = all` (committed in `.gitmodules`): Git lists nothing for the submodule and `git add -A`
/// skips it, so it is not reported as modified in any mode; a staged pointer change still shows.
#[test]
fn submodule_configured_ignore_all_is_not_reported() {
    let env = TestEnv::new();
    let repo = submodule_repo(&env);
    repo.git(&["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"]);
    repo.commit_all("ignore the submodule");
    let sub = TestRepo::init_with(&env, &repo.path.join("sub"), &["status"]);
    sub.write("s.txt", "2\n");
    sub.commit_all("sub two");
    std::fs::write(repo.path.join("sub").join("s.txt"), "dirty\n").expect("modify the submodule");
    assert!(
        repo.git(&["status", "--porcelain=v2"]).is_empty(),
        "Git reports nothing"
    );
    for mode in [
        SubmoduleStatusMode::HeadOnly,
        SubmoduleStatusMode::HeadAndDirty,
    ] {
        let status = run(
            &repo,
            &StatusOptions {
                submodules: mode,
                ..StatusOptions::default()
            },
        );
        assert!(
            status.unstaged.is_empty(),
            "{mode:?}: the ignored submodule is not listed: {:?}",
            status.unstaged
        );
    }
    repo.git(&["add", "-f", "sub"]);
    let status = run_default(&repo);
    assert_eq!(
        find(&status.staged, "sub").kind,
        ChangeKind::Modified,
        "a staged pointer change is shown, as in git status"
    );
}

#[test]
fn typechange_file_to_symlink() {
    if !symlinks_supported() {
        eprintln!("skipped: symbolic links are not available");
        return;
    }
    let env = TestEnv::new();
    let repo = base(&env);
    repo.git(&["config", "core.symlinks", "true"]);
    std::fs::remove_file(repo.path.join("a.txt")).expect("remove file");
    #[cfg(unix)]
    std::os::unix::fs::symlink("target", repo.path.join("a.txt")).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("target", repo.path.join("a.txt")).expect("symlink");
    let status = run_default(&repo);
    let entry = find(&status.unstaged, "a.txt");
    assert_eq!(
        entry.kind,
        ChangeKind::TypeChanged,
        "type change: {entry:?}"
    );
    assert_eq!(entry.old.map(|s| s.kind), Some(EntryKind::Blob), "old kind");
    assert_eq!(entry.new.map(|s| s.kind), Some(EntryKind::Link), "new kind");
}

#[test]
fn collapsed_untracked_directories() {
    let env = TestEnv::new();
    let repo = base(&env);
    repo.write("dir/one.txt", "1");
    repo.write("dir/two.txt", "2");
    repo.write("top.txt", "t");
    let collapsed = run(
        &repo,
        &StatusOptions {
            untracked: UntrackedMode::Collapsed,
            ..StatusOptions::default()
        },
    );
    assert_eq!(
        paths(&collapsed.unstaged),
        ["dir/", "top.txt"],
        "directory is one entry"
    );
    assert_eq!(
        find(&collapsed.unstaged, "dir/").new.map(|s| s.kind),
        Some(EntryKind::Tree),
        "kind tree"
    );
    let files = run_default(&repo);
    assert_eq!(
        paths(&files.unstaged),
        ["dir/one.txt", "dir/two.txt", "top.txt"],
        "each file"
    );
    let none = run(
        &repo,
        &StatusOptions {
            untracked: UntrackedMode::None,
            ..StatusOptions::default()
        },
    );
    assert!(none.unstaged.is_empty(), "no walk: {:?}", none.unstaged);
}

#[test]
fn unborn_head_everything_staged_is_added() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    repo.write("a.txt", "a");
    repo.write("b/c.txt", "c");
    repo.git(&["add", "-A"]);
    let status = run_default(&repo);
    assert_eq!(
        paths(&status.staged),
        ["a.txt", "b/c.txt"],
        "sorted by path"
    );
    assert!(
        status.staged.iter().all(|e| e.kind == ChangeKind::Added),
        "all added"
    );
    assert_eq!(status.counts.staged.added, 2, "counted");
    assert_eq!(status.fingerprint.head, None, "no HEAD commit");
}

#[test]
fn cancel_returns_cancelled() {
    let env = TestEnv::new();
    let repo = base(&env);
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = status(&repo.open(), &StatusOptions::default(), &cancel).expect_err("cancelled");
    assert_eq!(err.kind(), GitErrorKind::Cancelled, "got {err:?}");
}

#[test]
fn bare_repository_is_rejected() {
    let env = TestEnv::new();
    let path = env.bare_remote("bare.git");
    let repo = gitcthulhu_git::Repo::open(&env.rt, &path).expect("bare repository opens");
    let err = status(&repo, &StatusOptions::default(), &CancelToken::new()).expect_err("bare");
    assert!(matches!(err, GitError::BareRepository), "got {err:?}");
}

#[test]
fn crlf_autocrlf_true_no_phantom_changes() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    repo.git(&["config", "core.autocrlf", "true"]);
    repo.write("crlf.txt", "one\r\ntwo\r\n");
    repo.write("lf.txt", "one\ntwo\n");
    repo.commit_all("base");
    // Re-checkout so the work tree has what autocrlf produces, then touch the stat data.
    std::fs::remove_file(repo.path.join("crlf.txt")).expect("remove");
    repo.git(&["checkout", "--", "crlf.txt"]);
    // The fixture runtime pins `core.autocrlf=false` for gix opens (config overrides win over the repository's
    // file), so open with a runtime that agrees with Git here.
    let rt = env
        .runtime_with(|config| {
            let isolation = config
                .test_isolation
                .as_mut()
                .expect("fixture runtimes are isolated");
            for (key, value) in &mut isolation.overrides {
                if key == "core.autocrlf" {
                    *value = "true".into();
                }
            }
        })
        .expect("runtime");
    let opened = gitcthulhu_git::Repo::open(&rt, &repo.path).expect("open");
    let status = status(&opened, &StatusOptions::default(), &CancelToken::new()).expect("status");
    assert!(
        status.staged.is_empty(),
        "nothing staged: {:?}",
        status.staged
    );
    assert!(
        status.unstaged.is_empty(),
        "no phantom changes: {:?}",
        status.unstaged
    );
}

fn index_state(repo: &TestRepo) -> (Vec<u8>, std::time::SystemTime) {
    let path = repo.path.join(".git").join("index");
    (
        std::fs::read(&path).expect("index readable"),
        std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("mtime"),
    )
}

/// Rewrites `t0.txt` .. `t<count-1>.txt` with the same content, then moves the mtime of every worktree file ten seconds
/// before the one it had, so the stat data in the index is stale, deterministically: each new mtime is in another
/// second than the recorded one (gix compares whole seconds), and every file is far older than any index write that
/// follows. A file whose mtime falls into the same second as the index write stays "racily clean" for gix, so a
/// refresh would not make it clean; that is why the files that are not rewritten (`keep.txt`, conflicted files) are
/// moved back as well.
fn touch_all(repo: &TestRepo, count: usize) {
    fn mtime(path: &std::path::Path) -> std::time::SystemTime {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .expect("the file has an mtime")
    }
    fn backdate(
        dir: &std::path::Path,
        recorded: &std::collections::HashMap<std::path::PathBuf, std::time::SystemTime>,
    ) {
        for entry in std::fs::read_dir(dir).expect("the directory can be listed") {
            let path = entry.expect("readable directory entry").path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).expect("the entry has metadata");
            if meta.is_dir() {
                backdate(&path, recorded);
            } else if meta.is_file() {
                let before = recorded.get(&path).copied().unwrap_or_else(|| mtime(&path));
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .and_then(|file| file.set_modified(before - std::time::Duration::from_secs(10)))
                    .expect("the mtime of the file can be set");
            }
        }
    }
    let mut recorded = std::collections::HashMap::new();
    for i in 0..count {
        let rela = format!("t{i}.txt");
        let path = repo.path.join(&rela);
        recorded.insert(path.clone(), mtime(&path));
        let content = repo.read(&rela);
        repo.write(&rela, content);
    }
    backdate(&repo.path, &recorded);
}

fn touched_repo(env: &TestEnv, count: usize) -> TestRepo {
    let repo = env.repo("repo");
    for i in 0..count {
        repo.write(&format!("t{i}.txt"), format!("content {i}\n"));
    }
    repo.commit_all("base");
    touch_all(&repo, count);
    repo
}

#[test]
fn status_never_writes_index() {
    let env = TestEnv::new();
    let repo = touched_repo(&env, 20);
    let before = index_state(&repo);
    let status = run_default(&repo);
    assert!(
        status.unstaged.is_empty(),
        "content is unchanged: {:?}",
        status.unstaged
    );
    assert!(
        status.index_needs_refresh,
        "stat-dirty entries are reported"
    );
    let after = index_state(&repo);
    assert_eq!(before.0, after.0, "index bytes unchanged");
    assert_eq!(before.1, after.1, "index mtime unchanged");
}

#[test]
fn touched_files_report_needs_refresh_then_refresh_clears_it() {
    let env = TestEnv::new();
    let fixture = touched_repo(&env, 20);
    let repo = fixture.open();
    let cancel = CancelToken::new();
    assert!(
        status(&repo, &StatusOptions::default(), &cancel)
            .expect("status")
            .index_needs_refresh,
        "touched files need a refresh"
    );
    let mut guard = repo.lock_exclusive();
    let stamp = refresh_index(&repo, guard.write_permission(), &cancel)
        .expect("refresh")
        .expect("a stamp");
    drop(guard);
    let after = status(&repo, &StatusOptions::default(), &cancel).expect("status");
    assert!(!after.index_needs_refresh, "refreshed");
    assert_eq!(
        after.fingerprint.index,
        Some(stamp),
        "the returned stamp is the index file's"
    );
}

#[test]
fn refresh_index_keeps_concurrent_staging() {
    let env = TestEnv::new();
    let fixture = touched_repo(&env, 200);
    let repo = fixture.open();
    let cancel = CancelToken::new();
    let status = status(&repo, &StatusOptions::default(), &cancel).expect("status");
    assert!(status.index_needs_refresh, "200 touched files");
    fixture.write("new.txt", "new\n");
    fixture.git(&["add", "new.txt"]);
    let mut guard = repo.lock_exclusive();
    refresh_index(&repo, guard.write_permission(), &cancel).expect("refresh");
    drop(guard);
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        "new.txt",
        "the file staged during the status stays staged"
    );
}

#[test]
fn refresh_index_with_conflicts_succeeds() {
    let env = TestEnv::new();
    let fixture = conflict_repo(
        &env,
        "conflicted",
        &[("f.txt", "base\n"), ("t0.txt", "content 0\n")],
        |r| r.write("f.txt", "ours\n"),
        |r| r.write("f.txt", "theirs\n"),
    );
    touch_all(&fixture, 1);
    let repo = fixture.open();
    let cancel = CancelToken::new();
    let mut guard = repo.lock_exclusive();
    let stamp = refresh_index(&repo, guard.write_permission(), &cancel)
        .expect("unmerged entries do not make the refresh fail");
    drop(guard);
    assert!(stamp.is_some(), "the index was refreshed and written");
    let after = status(&repo, &StatusOptions::default(), &cancel).expect("status");
    assert_conflict(&after, "f.txt", ConflictKind::BothModified);
    assert!(!after.index_needs_refresh, "refreshed despite the conflict");
}

#[test]
fn pathspec_with_glob_chars_is_literal() {
    let env = TestEnv::new();
    let repo = base(&env);
    repo.write("[12].txt", "bracket");
    repo.write("1.txt", "one");
    #[cfg(unix)]
    {
        repo.write("star*.txt", "star");
        repo.write("starx.txt", "x");
    }
    let status = run(
        &repo,
        &StatusOptions {
            pathspec: vec!["[12].txt".into()],
            ..StatusOptions::default()
        },
    );
    assert_eq!(
        paths(&status.unstaged),
        ["[12].txt"],
        "only the literal name matches"
    );
    #[cfg(unix)]
    {
        let status = run(
            &repo,
            &StatusOptions {
                pathspec: vec!["star*.txt".into()],
                ..StatusOptions::default()
            },
        );
        assert_eq!(
            paths(&status.unstaged),
            ["star*.txt"],
            "a star is not a glob"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_path_roundtrips() {
    use std::os::unix::ffi::OsStrExt;
    let env = TestEnv::new();
    let repo = base(&env);
    let name = std::ffi::OsStr::from_bytes(b"bad-\xff-name.txt");
    std::fs::write(repo.path.join(name), "x").expect("write non-UTF-8 name");
    let status = run_default(&repo);
    assert_eq!(status.unstaged.len(), 1, "one entry: {:?}", status.unstaged);
    assert_eq!(
        status.unstaged[0].path.as_slice(),
        b"bad-\xff-name.txt",
        "bytes preserved"
    );
}

/// `index.sparse` (Scalar's default): sparse directory entries, which gix cannot diff against a tree. The status
/// comes from `git status --porcelain=v2` there and equals gix's status of the same state with a full index.
#[test]
fn sparse_index_status_equals_full_index_status() {
    let env = TestEnv::new();
    // The same changes in a cone-mode sparse checkout, with a sparse or a full index.
    let changed = |name: &str, sparse_index: bool| {
        let repo = env.repo(name);
        repo.write("in/x/a.txt", LINES);
        repo.write("in/x/gone.txt", "gone\n");
        repo.write("in/staged-gone.txt", "staged gone\n");
        repo.write("in/move-me.txt", LINES);
        repo.write("in/both.txt", "base\n");
        repo.write("out/y/b.txt", "outside the cone\n");
        repo.commit_all("base");
        let index_mode = if sparse_index {
            "--sparse-index"
        } else {
            "--no-sparse-index"
        };
        repo.git(&["sparse-checkout", "init", "--cone", index_mode]);
        repo.git(&["sparse-checkout", "set", "in"]);
        assert_eq!(
            repo.index_is_sparse(),
            sparse_index,
            "a sparse directory entry is in the index exactly with --sparse-index"
        );
        repo.write("in/x/a.txt", format!("{LINES}kilo\n"));
        repo.remove("in/x/gone.txt");
        repo.git(&["rm", "-q", "in/staged-gone.txt"]);
        repo.git(&["mv", "in/move-me.txt", "in/moved.txt"]);
        repo.write("in/added.txt", "added\n");
        repo.git(&["add", "in/added.txt"]);
        repo.write("in/both.txt", "staged\n");
        repo.git(&["add", "in/both.txt"]);
        repo.write("in/both.txt", "staged and changed again\n");
        repo.write("in/ita.txt", "intent\n");
        repo.git(&["add", "-N", "in/ita.txt"]);
        repo.write("in/new/untracked.txt", "u\n");
        repo.write("in/space name.txt", "u\n");
        repo
    };
    let expect = |status: &Status| {
        let line = |e: &StatusEntry| format!("{:?} {} <- {:?}", e.kind, e.path, prev(e));
        (
            status.staged.iter().map(line).collect::<Vec<_>>(),
            status.unstaged.iter().map(line).collect::<Vec<_>>(),
        )
    };

    let sparse = run_default(&changed("sparse", true));
    assert_eq!(
        expect(&sparse),
        (
            vec![
                "Added in/added.txt <- None".to_owned(),
                "Modified in/both.txt <- None".to_owned(),
                "Renamed in/moved.txt <- Some(\"in/move-me.txt\")".to_owned(),
                "Deleted in/staged-gone.txt <- None".to_owned(),
            ],
            vec![
                "Modified in/both.txt <- None".to_owned(),
                "IntentToAdd in/ita.txt <- None".to_owned(),
                "Untracked in/new/untracked.txt <- None".to_owned(),
                "Untracked in/space name.txt <- None".to_owned(),
                "Modified in/x/a.txt <- None".to_owned(),
                "Deleted in/x/gone.txt <- None".to_owned(),
            ]
        ),
        "the sparse index's status lists every change in the cone"
    );

    // The same state with a full index, computed by gix: every field (kinds, sides, ids) must agree.
    let full = run_default(&changed("full", false));
    assert_eq!(sparse.staged, full.staged, "staged entries equal gix's");
    assert_eq!(
        sparse.unstaged, full.unstaged,
        "unstaged entries equal gix's"
    );
}

/// A conflict in a sparse index is reported with its stages.
#[test]
fn sparse_index_status_reports_conflicts() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    repo.write("in/c.txt", "base\n");
    repo.write("out/o.txt", "outside\n");
    repo.commit_all("base");
    repo.git(&["switch", "-q", "-c", "side"]);
    repo.write("in/c.txt", "side\n");
    repo.commit_all("side");
    repo.git(&["switch", "-q", "main"]);
    repo.write("in/c.txt", "main\n");
    repo.commit_all("main");
    repo.git(&["sparse-checkout", "init", "--cone", "--sparse-index"]);
    repo.git(&["sparse-checkout", "set", "in"]);
    let _ = repo.git_fail(&["merge", "side"]);
    let status = run_default(&repo);
    assert!(status.staged.is_empty(), "{:?}", status.staged);
    let entry = find(&status.unstaged, "in/c.txt");
    assert_eq!(entry.kind, ChangeKind::Conflicted, "conflicted");
    let conflict = entry.conflict.as_ref().expect("conflict details");
    assert_eq!(conflict.kind, ConflictKind::BothModified, "both modified");
    assert!(
        conflict.base.is_some() && conflict.ours.is_some() && conflict.theirs.is_some(),
        "all three stages: {conflict:?}"
    );
}

/// 100k tracked files, 1k untracked: `status` must stay within budget (spec §9: <= 400 ms, Linux <= 150 ms).
/// Budgets are asserted only with `GITCTHULHU_PERF_ASSERT=1`.
#[test]
#[ignore = "performance test; run in the release job"]
fn perf_status_100k_files() {
    let env = TestEnv::new();
    let repo = env.repo("big");
    let mut stream = Vec::new();
    stream.extend_from_slice(
        b"commit refs/heads/main\ncommitter T <t@example.com> 1000000000 +0000\ndata 4\nbig\n",
    );
    for i in 0..100_000 {
        let content = format!("file {i}\n");
        stream.extend_from_slice(
            format!(
                "M 100644 inline d{}/f{i}.txt\ndata {}\n{content}\n",
                i % 500,
                content.len()
            )
            .as_bytes(),
        );
    }
    repo.fast_import(&stream);
    repo.git(&["reset", "-q", "--hard"]);
    for i in 0..1000 {
        repo.write(&format!("untracked/u{i}.txt"), "u");
    }
    // The GUI refreshes stat-dirty (and racily clean) entries through `refresh_index` after the first status; do
    // the same here so the measured run is the steady state and not a full re-hash.
    std::thread::sleep(std::time::Duration::from_millis(50));
    repo.git(&[
        "update-index",
        "-q",
        "--ignore-submodules",
        "--unmerged",
        "--refresh",
    ]);
    let opened = repo.open();
    let cancel = CancelToken::new();
    // Warm run (index stat data, file system cache), then the measured one.
    status(&opened, &StatusOptions::default(), &cancel).expect("warm-up status");
    let started = std::time::Instant::now();
    let result = status(&opened, &StatusOptions::default(), &cancel).expect("status");
    let elapsed = started.elapsed();
    eprintln!("status of 100k files: {elapsed:?}");
    // Informational: a deleted tracked file next to untracked files makes `status` repeat the index-worktree half
    // with worktree rename tracking (and hash every untracked file).
    repo.remove("d0/f0.txt");
    let started = std::time::Instant::now();
    let with_deletion = status(&opened, &StatusOptions::default(), &cancel).expect("status");
    eprintln!(
        "status of 100k files with a worktree deletion: {:?}",
        started.elapsed()
    );
    assert_eq!(
        with_deletion.counts.unstaged.deleted, 1,
        "the deletion is found"
    );
    // The GUI's options (staged renames only, as `git status`): the deletion costs no second pass.
    let index_only = StatusOptions {
        renames: RenameMode::IndexOnly,
        ..StatusOptions::default()
    };
    let started = std::time::Instant::now();
    let gui_with_deletion = status(&opened, &index_only, &cancel).expect("status");
    let gui_elapsed = started.elapsed();
    eprintln!(
        "status of 100k files with a worktree deletion, staged renames only: {gui_elapsed:?}"
    );
    assert_eq!(
        gui_with_deletion.counts.unstaged.deleted, 1,
        "the deletion is found"
    );
    assert_eq!(
        result.counts.unstaged.untracked, 1000,
        "untracked files found"
    );
    if std::env::var_os("GITCTHULHU_PERF_ASSERT").is_some() {
        let budget = if cfg!(windows) { 400 } else { 150 };
        assert!(
            elapsed.as_millis() <= budget,
            "status took {elapsed:?}, budget {budget} ms"
        );
        assert!(
            gui_elapsed.as_millis() <= budget,
            "status with a deletion (staged renames only) took {gui_elapsed:?}, budget {budget} ms"
        );
    }
}
