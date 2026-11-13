//! A4-A7: what a snapshot holds, untracked capture and its limits, and no-op detection.

use gitcthulhu_oplog::{
    OperationKind, OplogOptions, Scope, SkipReason, SkippedFile, UntrackedCapture,
    testing::capture_round_trip,
};

use crate::support::{Fixture, b, desc, git_in, has_blob_of, record, record_ok, show_json};

const MAIN_REF: &str = "refs/gitcthulhu/oplog/main";

fn assert_round_trip(fx: &Fixture, scope: Scope, untracked: UntrackedCapture, what: &str) {
    let oplog = fx.oplog();
    let (_tree, equal) =
        capture_round_trip(&oplog, &fx.repo(), scope, &untracked).expect("capture works");
    assert!(equal, "write_tree then read_tree is lossless for {what}");
}

#[test]
fn snapshot_tree_round_trips() {
    let fx = Fixture::empty();
    assert_round_trip(&fx, Scope::Full, UntrackedCapture::None, "an unborn HEAD");
    assert_round_trip(
        &fx,
        Scope::RefsOnly,
        UntrackedCapture::None,
        "an unborn HEAD (refs only)",
    );

    fx.commit_file("a.txt", "a\n", "initial");
    fx.git(&["branch", "feature"]);
    fx.git(&["tag", "-a", "v1", "-m", "annotated"]);
    fx.git(&["symbolic-ref", "refs/heads/alias", "refs/heads/main"]);
    fx.git(&["config", "branch.main.remote", "origin"]);
    fx.git(&["config", "branch.main.merge", "refs/heads/main"]);
    fx.write("a.txt", "staged\n");
    fx.git(&["add", "a.txt"]);
    fx.write("a.txt", "unstaged\n");
    fx.write("new.txt", "untracked\n");
    assert_round_trip(
        &fx,
        Scope::Full,
        UntrackedCapture::All,
        "a symbolic HEAD with changes",
    );

    fx.git(&["stash", "push", "-q", "-m", "saved"]);
    assert_round_trip(&fx, Scope::Full, UntrackedCapture::None, "a present stash");

    fx.git(&["checkout", "-q", "--detach", "HEAD"]);
    assert_round_trip(&fx, Scope::Full, UntrackedCapture::None, "a detached HEAD");
    assert_round_trip(
        &fx,
        Scope::RefsOnly,
        UntrackedCapture::None,
        "a detached HEAD (refs only)",
    );
}

#[test]
fn staged_and_unstaged_versions_are_both_preserved() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    fx.write("a.txt", "staged\n");
    fx.git(&["add", "a.txt"]);
    fx.write("a.txt", "unstaged\n");
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::CreateBranchAndCheckout, "Create branch 'x'"),
        |fx| {
            fx.git(&["checkout", "-q", "-b", "x"]);
        },
    );
    let index = fx.rev(&format!("{MAIN_REF}:before/files/index/a.txt"));
    let worktree = fx.rev(&format!("{MAIN_REF}:before/files/worktree/a.txt"));
    assert_ne!(
        index, worktree,
        "index tree blob differs from worktree tree blob"
    );
    assert_eq!(fx.git(&["cat-file", "blob", &index]), "staged\n");
    assert_eq!(fx.git(&["cat-file", "blob", &worktree]), "unstaged\n");
}

#[test]
fn conflicted_index_is_captured_with_all_stages() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("a.txt", "feature\n", "feature change");
    fx.git(&["checkout", "-q", "main"]);
    fx.commit_file("a.txt", "main\n", "main change");
    let recorded = record(
        &fx,
        &oplog,
        desc(OperationKind::Merge, "Merge 'feature' into main"),
        |fx| {
            let out = fx.git_output(&["merge", "feature"]);
            assert!(!out.status.success(), "the merge stops on a conflict");
            Err("conflict".into())
        },
    )
    .expect("recording works");
    assert!(
        recorded.is_some(),
        "a merge stopped on conflicts changes the repository"
    );
    snapbox::assert_data_eq!(
        fx.git(&[
            "ls-tree",
            "-r",
            "--name-only",
            &format!("{MAIN_REF}:after/files/index-conflicts")
        ]),
        snapbox::str![[r#"
a.txt/1
a.txt/2
a.txt/3

"#]]
    );
}

#[test]
fn nested_repositories_and_ignored_files_are_never_captured() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    fx.write(".gitignore", "ignored.txt\n");
    fx.git(&["add", ".gitignore"]);
    fx.git(&["commit", "-q", "-m", "ignore"]);
    fx.write("ignored.txt", "ignored\n");
    let nested = fx.path.join("nested");
    std::fs::create_dir_all(&nested).expect("dir can be created");
    crate::support::git_in(&nested, &["init", "-q", "-b", "main"]);
    std::fs::write(nested.join("n.txt"), "nested\n").expect("file can be written");
    crate::support::git_in(&nested, &["add", "n.txt"]);
    crate::support::git_in(
        &nested,
        &[
            "-c",
            "user.name=x",
            "-c",
            "user.email=x@x",
            "commit",
            "-q",
            "-m",
            "nested",
        ],
    );
    fx.write("u.txt", "untracked\n");
    record_ok(
        &fx,
        &oplog,
        gitcthulhu_oplog::OperationDescriptor {
            untracked: UntrackedCapture::All,
            ..desc(OperationKind::Commit, "Commit \"c\"")
        },
        |fx| {
            fx.commit_file(
                "c.txt", "c
", "c",
            )
        },
    );
    snapbox::assert_data_eq!(
        fx.git(&[
            "ls-tree",
            "-r",
            "--name-only",
            &format!("{MAIN_REF}:before/files/worktree")
        ]),
        snapbox::str![[r#"
.gitignore
a.txt
u.txt

"#]]
    );
}

#[cfg(unix)]
#[test]
fn symlinks_are_stored_as_links() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    std::os::unix::fs::symlink("a.txt", fx.path.join("link")).expect("symlink can be created");
    record_ok(
        &fx,
        &oplog,
        gitcthulhu_oplog::OperationDescriptor {
            untracked: UntrackedCapture::All,
            ..desc(OperationKind::Commit, "Commit \"c\"")
        },
        |fx| {
            fx.commit_file(
                "c.txt", "c
", "c",
            )
        },
    );
    let line = fx.git(&[
        "ls-tree",
        &format!("{MAIN_REF}:before/files/worktree"),
        "link",
    ]);
    assert!(
        line.starts_with("120000 "),
        "a symlink has mode 120000: {line}"
    );
}

fn record_with_untracked(
    fx: &Fixture,
    options: OplogOptions,
    untracked: UntrackedCapture,
) -> Vec<SkippedFile> {
    let oplog = fx.oplog_with(options);
    let mut guard = fx.guard();
    let pending = oplog
        .prepare(
            &fx.repo(),
            gitcthulhu_oplog::OperationDescriptor {
                untracked,
                ..desc(OperationKind::Discard, "Discard")
            },
            guard.write_permission(),
        )
        .expect("prepare works")
        .expect("recording is enabled");
    let skipped = pending.skipped_untracked().to_vec();
    fx.commit_file("marker.txt", "x\n", "something changes");
    oplog
        .finish(
            &fx.repo(),
            pending,
            gitcthulhu_oplog::OperationResult::Succeeded,
            guard.write_permission(),
        )
        .expect("finish works")
        .expect("an entry is recorded");
    skipped
}

fn worktree_of_newest_before(fx: &Fixture) -> String {
    fx.git(&[
        "ls-tree",
        "-r",
        "--name-only",
        &format!("{MAIN_REF}:before/files/worktree"),
    ])
}

#[test]
fn untracked_capture_none_paths_all_and_disabled() {
    // None: absent, and no blob of its content was written.
    let fx = Fixture::new();
    fx.write("it.txt", "it-content-none\n");
    fx.write("sibling.txt", "sibling-content-none\n");
    let skipped = record_with_untracked(&fx, OplogOptions::default(), UntrackedCapture::None);
    assert!(skipped.is_empty(), "nothing requested, nothing skipped");
    assert_eq!(
        worktree_of_newest_before(&fx),
        "a.txt\n",
        "no untracked file in the snapshot"
    );
    assert!(
        !has_blob_of(&fx, b"it-content-none\n"),
        "untracked contents never hit the ODB"
    );

    // Paths([it]): present, the sibling is absent.
    let fx = Fixture::new();
    fx.write("it.txt", "it\n");
    fx.write("sibling.txt", "sibling-content-paths\n");
    record_with_untracked(
        &fx,
        OplogOptions::default(),
        UntrackedCapture::Paths(vec![b("it.txt")]),
    );
    assert_eq!(worktree_of_newest_before(&fx), "a.txt\nit.txt\n");
    assert!(
        !has_blob_of(&fx, b"sibling-content-paths\n"),
        "the sibling was not hashed"
    );

    // A directory path selects every untracked file below it.
    let fx = Fixture::new();
    fx.write("dir/x.txt", "x\n");
    fx.write("dir/sub/y.txt", "y\n");
    fx.write("dirt.txt", "not below dir\n");
    record_with_untracked(
        &fx,
        OplogOptions::default(),
        UntrackedCapture::Paths(vec![b("dir")]),
    );
    assert_eq!(
        worktree_of_newest_before(&fx),
        "a.txt\ndir/sub/y.txt\ndir/x.txt\n"
    );

    // All: both.
    let fx = Fixture::new();
    fx.write("it.txt", "it\n");
    fx.write("sibling.txt", "sibling\n");
    record_with_untracked(&fx, OplogOptions::default(), UntrackedCapture::All);
    assert_eq!(
        worktree_of_newest_before(&fx),
        "a.txt\nit.txt\nsibling.txt\n"
    );

    // capture_untracked = false: reported as CaptureDisabled, not captured.
    let fx = Fixture::new();
    fx.write("it.txt", "it-content-disabled\n");
    let skipped = record_with_untracked(
        &fx,
        OplogOptions {
            capture_untracked: false,
            ..OplogOptions::default()
        },
        UntrackedCapture::Paths(vec![b("it.txt")]),
    );
    assert_eq!(
        skipped,
        vec![SkippedFile {
            path: b("it.txt"),
            size: 20,
            reason: SkipReason::CaptureDisabled
        }]
    );
    assert_eq!(worktree_of_newest_before(&fx), "a.txt\n");
    assert!(!has_blob_of(&fx, b"it-content-disabled\n"));
}

#[test]
fn index_flags_are_captured_and_round_trip() {
    let fx = Fixture::new();
    fx.commit_file("b.txt", "b\n", "add b");
    fx.write("new.txt", "intent\n");
    fx.git(&["add", "-N", "new.txt"]);
    fx.git(&["update-index", "--assume-unchanged", "a.txt"]);
    fx.git(&["update-index", "--skip-worktree", "b.txt"]);
    let oplog = fx.oplog();
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::CreateBranchAndCheckout, "Create branch"),
        |fx| {
            fx.git(&["checkout", "-q", "-b", "x"]);
        },
    );
    let state = show_json(&fx, &format!("{MAIN_REF}:before/state.json"));
    // bits: 1 = intent-to-add, 2 = skip-worktree, 4 = assume-unchanged
    assert_eq!(
        state["indexFlags"],
        serde_json::json!([["a.txt", 4], ["b.txt", 2], ["new.txt", 1]]),
        "only entries with one of the three flags are listed"
    );
    assert_round_trip(&fx, Scope::Full, UntrackedCapture::None, "index flags");
}

#[test]
fn untracked_limits_skip_large_files_and_admit_the_smallest_first() {
    let fx = Fixture::new();
    let options = OplogOptions {
        untracked_file_limit: 1024,
        untracked_total_limit: 4 * 1024,
        ..OplogOptions::default()
    };
    let big = vec![b'B'; 2048];
    fx.write("big.bin", &big);
    let medium: Vec<Vec<u8>> = (0..3).map(|i| vec![b'0' + i; 1000]).collect();
    for (i, content) in medium.iter().enumerate() {
        fx.write(&format!("m{i}.bin"), content);
    }
    let small = vec![b's'; 100];
    fx.write("small.bin", &small);
    let overflow = vec![b'o'; 1010];
    fx.write("overflow.bin", &overflow);

    let oplog = fx.oplog_with(options.clone());
    let preview = oplog
        .preview_skipped(&fx.repo(), &[b("big.bin"), b("small.bin"), b("a.txt")])
        .expect("preview works");
    assert_eq!(
        preview,
        vec![SkippedFile {
            path: b("big.bin"),
            size: 2048,
            reason: SkipReason::TooLarge
        }],
        "only the too large file is reported; tracked files are ignored"
    );
    assert!(!has_blob_of(&fx, &big), "the preview hashes nothing");

    let skipped = record_with_untracked(&fx, options, UntrackedCapture::All);
    assert_eq!(
        skipped,
        vec![
            SkippedFile {
                path: b("big.bin"),
                size: 2048,
                reason: SkipReason::TooLarge
            },
            SkippedFile {
                path: b("overflow.bin"),
                size: 1010,
                reason: SkipReason::OverBudget
            },
        ],
        "100 + 3 * 1000 fit into 4 KiB, the 1010 bytes after them don't; sorted by path"
    );
    assert_eq!(
        worktree_of_newest_before(&fx),
        "a.txt\nm0.bin\nm1.bin\nm2.bin\nsmall.bin\n"
    );
    assert!(!has_blob_of(&fx, &big), "no blob of a skipped file");
    assert!(!has_blob_of(&fx, &overflow), "no blob of a skipped file");
    assert!(has_blob_of(&fx, &small), "admitted files are stored");
}

#[test]
fn preview_skipped_expands_directories_and_respects_capture_untracked() {
    let fx = Fixture::new();
    fx.write(".gitignore", "*.log\n");
    fx.git(&["add", ".gitignore"]);
    fx.git(&["commit", "-q", "-m", "ignore"]);
    fx.write("dir/big.bin", vec![b'x'; 2048]);
    fx.write("dir/ok.txt", "ok\n");
    fx.write("dir/huge.log", vec![b'x'; 4096]);
    let oplog = fx.oplog_with(OplogOptions {
        untracked_file_limit: 1024,
        ..OplogOptions::default()
    });
    assert_eq!(
        oplog
            .preview_skipped(&fx.repo(), &[b("dir")])
            .expect("preview works"),
        vec![SkippedFile {
            path: b("dir/big.bin"),
            size: 2048,
            reason: SkipReason::TooLarge
        }],
        "ignored files below the directory are not reported"
    );
    let oplog = fx.oplog_with(OplogOptions {
        capture_untracked: false,
        ..OplogOptions::default()
    });
    let reasons: Vec<_> = oplog
        .preview_skipped(&fx.repo(), &[b("dir/")])
        .expect("preview works")
        .into_iter()
        .map(|s| (s.path.to_string(), s.reason))
        .collect();
    assert_eq!(
        reasons,
        vec![
            ("dir/big.bin".to_owned(), SkipReason::CaptureDisabled),
            ("dir/ok.txt".to_owned(), SkipReason::CaptureDisabled),
        ]
    );
}

#[test]
fn preview_skipped_reports_nested_repositories() {
    let fx = Fixture::new();
    fx.write("nested/n.txt", "n\n");
    git_in(&fx.path.join("nested"), &["init", "-q"]);
    fx.write("dir/inner/.git", "gitdir: ../../elsewhere\n");
    fx.write("dir/ok.txt", "ok\n");
    let oplog = fx.oplog();
    let reasons = |paths: &[_]| -> Vec<(String, SkipReason)> {
        oplog
            .preview_skipped(&fx.repo(), paths)
            .expect("preview works")
            .into_iter()
            .map(|s| (s.path.to_string(), s.reason))
            .collect()
    };
    // No snapshot holds a nested repository, so deleting it must be confirmed even when every file fits.
    assert_eq!(
        reasons(&[b("nested"), b("dir/")]),
        vec![
            ("dir/inner".to_owned(), SkipReason::NestedRepository),
            ("nested".to_owned(), SkipReason::NestedRepository),
        ]
    );
    assert_eq!(
        reasons(&[b("")]),
        vec![
            ("dir/inner".to_owned(), SkipReason::NestedRepository),
            ("nested".to_owned(), SkipReason::NestedRepository),
        ],
        "the worktree root expands to the repositories below it"
    );
}

#[test]
fn finish_records_nothing_for_operations_without_effect() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    fx.git(&["branch", "feature"]);
    let merged = record(
        &fx,
        &oplog,
        desc(OperationKind::Merge, "Merge 'feature' into main"),
        |fx| {
            let head = fx.rev("HEAD");
            fx.git(&["merge", "feature"]);
            assert_eq!(fx.rev("HEAD"), head, "feature is already merged");
            Ok(())
        },
    )
    .expect("recording works");
    assert_eq!(merged, None, "an already up to date merge is a no-op");

    let hook = fx.path.join(".git").join("hooks").join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho rejected >&2\nexit 1\n")
        .expect("the hook can be written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("the hook can be made executable");
    }
    fx.write("b.txt", "b\n");
    fx.git(&["add", "b.txt"]);
    let committed = record(
        &fx,
        &oplog,
        desc(OperationKind::Commit, "Commit \"b\""),
        |fx| {
            let out = fx.git_output(&["commit", "-q", "-m", "b"]);
            assert!(
                !out.status.success(),
                "the pre-commit hook rejects the commit"
            );
            Err("pre-commit hook failed".into())
        },
    )
    .expect("recording works");
    assert_eq!(committed, None, "a failed commit changes nothing");
    assert!(
        crate::support::all_entries(&fx, &oplog).is_empty(),
        "nothing was appended"
    );
}

#[test]
fn unsupported_repositories_are_refused() {
    let fx = Fixture::new();
    fx.git(&["config", "core.sparseCheckout", "true"]);
    assert!(
        matches!(
            gitcthulhu_oplog::Oplog::open(&fx.repo(), OplogOptions::default()),
            Err(gitcthulhu_oplog::OplogError::Unsupported(
                gitcthulhu_oplog::UnsupportedReason::SparseCheckout
            ))
        ),
        "sparse checkouts don't round-trip in v1"
    );

    let fx = Fixture::new();
    let tmp = fx.tmp().to_owned();
    crate::support::git_in(&tmp, &["init", "-q", "--bare", "bare.git"]);
    let bare = but_testsupport::open_repo(&tmp.join("bare.git")).expect("bare repo opens");
    assert!(
        matches!(
            gitcthulhu_oplog::Oplog::open(&bare, OplogOptions::default()),
            Err(gitcthulhu_oplog::OplogError::Unsupported(
                gitcthulhu_oplog::UnsupportedReason::BareRepository
            ))
        ),
        "a bare repository has no worktree to snapshot"
    );
}

#[test]
fn a_repository_of_another_worktree_is_rejected() {
    let fx = Fixture::new();
    let other = Fixture::new();
    let oplog = fx.oplog();
    assert!(
        matches!(
            oplog.state(&other.repo()),
            Err(gitcthulhu_oplog::OplogError::WrongRepository)
        ),
        "the handle checks the git dir"
    );
}

#[test]
fn the_worktree_root_and_dot_slash_paths_are_directory_requests() {
    let fx = Fixture::new();
    fx.write(".gitignore", "build/\n");
    fx.git(&["add", ".gitignore"]);
    fx.git(&["commit", "-q", "-m", "ignore"]);
    fx.write("top.bin", vec![b't'; 2048]);
    fx.write("dir/big.bin", vec![b'x'; 2048]);
    fx.write("build/out.bin", vec![b'o'; 2048]);
    let oplog = fx.oplog_with(OplogOptions {
        untracked_file_limit: 1024,
        ..OplogOptions::default()
    });
    let preview = |paths: &[&str]| -> Vec<String> {
        oplog
            .preview_skipped(&fx.repo(), &paths.iter().map(|p| b(p)).collect::<Vec<_>>())
            .expect("preview works")
            .into_iter()
            .map(|s| s.path.to_string())
            .collect()
    };
    assert_eq!(
        preview(&["."]),
        vec!["dir/big.bin".to_owned(), "top.bin".to_owned()],
        "'.' is the worktree root; the ignored directory is skipped"
    );
    assert_eq!(
        preview(&["./dir/"]),
        vec!["dir/big.bin".to_owned()],
        "a leading ./ and a trailing slash are ignored"
    );
    assert_eq!(
        preview(&["build/out.bin"]),
        Vec::<String>::new(),
        "a file below an ignored directory is not untracked"
    );

    let skipped = record_with_untracked(
        &fx,
        OplogOptions {
            untracked_file_limit: 1024,
            ..OplogOptions::default()
        },
        UntrackedCapture::Paths(vec![b(".")]),
    );
    let skipped: Vec<_> = skipped.into_iter().map(|s| s.path.to_string()).collect();
    assert_eq!(
        skipped,
        vec!["dir/big.bin".to_owned(), "top.bin".to_owned()],
        "capture and preview agree on what '.' requests"
    );
}

#[cfg(windows)]
#[test]
fn a_differently_cased_path_of_the_same_worktree_is_accepted() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    let upper = std::path::PathBuf::from(fx.path.to_str().expect("UTF-8 temp path").to_uppercase());
    let repo =
        but_testsupport::open_repo(&upper).expect("the repository opens via the upper-case path");
    assert!(
        oplog.state(&repo).is_ok(),
        "Windows paths are case-insensitive, so this is the same worktree"
    );
}
