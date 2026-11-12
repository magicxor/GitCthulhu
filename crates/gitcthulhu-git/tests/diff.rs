//! Tests of `diff` (spec §2.5, WP-B2).

mod support;

use gitcthulhu_git::{
    Against, CancelToken, ChangeKind, CommitFilesOptions, DiffContent, DiffOptions, DiffTarget,
    EntryKind, Eol, EolStyle, ExportPatch, FileDiff, GitErrorKind, LineKind, PartialAvailability,
    PartialBlock, PatchTextOptions, TextDiff, WhitespaceMode, diff,
};
use support::{TestEnv, TestRepo};

fn unstaged(path: &str) -> DiffTarget {
    DiffTarget::Unstaged {
        path: path.into(),
        previous_path: None,
    }
}

fn staged(path: &str) -> DiffTarget {
    DiffTarget::Staged {
        path: path.into(),
        previous_path: None,
    }
}

fn file_diff(fixture: &TestRepo, target: &DiffTarget, opts: &DiffOptions) -> FileDiff {
    let repo = fixture.open();
    diff::file_diff(&repo, target, opts, &CancelToken::new()).expect("the diff can be computed")
}

fn text(diff: &FileDiff) -> &TextDiff {
    match &diff.content {
        DiffContent::Text(text) => text,
        other => panic!("expected a text diff, got {other:?}"),
    }
}

/// `" ctx"`, `"+added"`, `"-removed"` for every line of every hunk.
fn render(text: &TextDiff) -> Vec<String> {
    text.hunks
        .iter()
        .flat_map(|hunk| text.hunk_lines(hunk))
        .map(|line| {
            let prefix = match line.kind {
                LineKind::Context => ' ',
                LineKind::Added => '+',
                LineKind::Removed => '-',
            };
            format!("{prefix}{}", text.line_text(line))
        })
        .collect()
}

fn numbered(count: usize) -> String {
    (1..=count).map(|n| format!("line {n}\n")).collect()
}

fn rev(fixture: &TestRepo, spec: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(fixture.git(&["rev-parse", spec]).as_bytes())
        .expect("rev-parse prints an id")
}

#[test]
fn text_modification_lines_and_numbers() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "a\nb\nc\nd\ne\n");
    fixture.commit_all("one");
    fixture.write("f.txt", "a\nb\nC\nd\ne\n");

    let d = file_diff(&fixture, &unstaged("f.txt"), &DiffOptions::default());
    assert_eq!(d.kind, ChangeKind::Modified, "tracked file changed");
    let t = text(&d);
    assert_eq!(t.hunks.len(), 1, "one hunk");
    let header = t.hunks[0].header;
    assert_eq!(
        (
            header.old_start,
            header.old_lines,
            header.new_start,
            header.new_lines
        ),
        (1, 5, 1, 5),
        "header covers the whole short file"
    );
    assert_eq!(
        render(t),
        [" a", " b", "-c", "+C", " d", " e"],
        "line order"
    );
    let lines = t.hunk_lines(&t.hunks[0]);
    assert_eq!(
        (lines[2].old_lineno, lines[2].new_lineno),
        (Some(3), None),
        "removed line has only an old number"
    );
    assert_eq!(
        (lines[3].old_lineno, lines[3].new_lineno),
        (None, Some(3)),
        "added line has only a new number"
    );
    assert_eq!(
        (lines[0].old_lineno, lines[0].new_lineno),
        (Some(1), Some(1)),
        "context has both"
    );
    assert_eq!((t.added, t.removed), (1, 1), "counts");
    assert_eq!(t.partial, PartialAvailability::Allowed, "plain text");
    assert_eq!(
        d.old.expect("old side").id,
        rev(&fixture, ":f.txt"),
        "old side is the index blob"
    );
}

#[test]
fn added_file_all_added() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("keep.txt", "k\n");
    fixture.commit_all("one");
    fixture.write("new.txt", "x\ny\n");
    fixture.git(&["add", "new.txt"]);

    let d = file_diff(&fixture, &staged("new.txt"), &DiffOptions::default());
    assert_eq!(d.kind, ChangeKind::Added, "new file");
    assert!(d.old.is_none(), "no old side");
    let t = text(&d);
    assert_eq!(render(t), ["+x", "+y"], "everything is added");
    let header = t.hunks[0].header;
    assert_eq!(
        (
            header.old_start,
            header.old_lines,
            header.new_start,
            header.new_lines
        ),
        (0, 0, 1, 2),
        "an empty old side starts at the line before"
    );
}

#[test]
fn deleted_file_all_removed() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("gone.txt", "x\ny\n");
    fixture.commit_all("one");
    fixture.remove("gone.txt");

    let d = file_diff(&fixture, &unstaged("gone.txt"), &DiffOptions::default());
    assert_eq!(
        d.kind,
        ChangeKind::Deleted,
        "file removed from the worktree"
    );
    assert!(d.new.is_none(), "no new side");
    assert_eq!(render(text(&d)), ["-x", "-y"], "everything is removed");
}

#[test]
fn missing_final_newline_flags_both_sides() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "a\nb");
    fixture.commit_all("one");
    fixture.write("f.txt", "a\nc\n");

    let d = file_diff(&fixture, &unstaged("f.txt"), &DiffOptions::default());
    let t = text(&d);
    assert!(t.old_missing_final_newline, "old side ends without newline");
    assert!(!t.new_missing_final_newline, "new side ends with newline");
    let removed = t
        .lines
        .iter()
        .find(|l| l.kind == LineKind::Removed)
        .expect("a removed line");
    assert_eq!(
        removed.eol,
        Eol::None,
        "the removed last line has no terminator"
    );
    assert_eq!(t.line_text(removed), "b", "text is without terminator");
}

#[test]
fn crlf_file_eol_crlf_kept() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "a\r\nb\r\nc\r\n");
    fixture.commit_all("one");
    fixture.write("f.txt", "a\r\nB\r\nc\r\n");

    let d = file_diff(&fixture, &unstaged("f.txt"), &DiffOptions::default());
    let t = text(&d);
    assert_eq!(
        (t.old_eol, t.new_eol),
        (EolStyle::CrLf, EolStyle::CrLf),
        "CRLF files"
    );
    assert!(
        t.lines.iter().all(|l| l.eol == Eol::CrLf),
        "every line keeps its CRLF terminator"
    );
    assert_eq!(
        render(t),
        [" a", "-b", "+B", " c"],
        "text has no carriage returns"
    );
}

/// Open `fixture` with a runtime whose gix opens see `core.autocrlf=true` (the test isolation forces `false` as an
/// override, which beats the repository's own config). The child `git` gets the same value from the local config.
fn open_with_autocrlf(env: &TestEnv, fixture: &TestRepo) -> gitcthulhu_git::Repo {
    fixture.git(&["config", "core.autocrlf", "true"]);
    let rt = env
        .runtime_with(|config| {
            let isolation = config
                .test_isolation
                .as_mut()
                .expect("the fixtures are isolated");
            isolation
                .overrides
                .retain(|(key, _)| key != "core.autocrlf");
            isolation
                .overrides
                .push(("core.autocrlf".into(), "true".into()));
        })
        .expect("runtime");
    gitcthulhu_git::Repo::open(&rt, &fixture.path).expect("the repository can be opened")
}

#[test]
fn autocrlf_true_worktree_diff_is_lf() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "a\nb\n");
    fixture.commit_all("one");
    let repo = open_with_autocrlf(&env, &fixture);
    fixture.write("f.txt", "a\r\nb\r\nc\r\n");

    let d = diff::file_diff(
        &repo,
        &unstaged("f.txt"),
        &DiffOptions::default(),
        &CancelToken::new(),
    )
    .expect("diff");
    let t = text(&d);
    assert_eq!(
        render(t),
        [" a", " b", "+c"],
        "only the new line differs in git form"
    );
    assert_eq!(
        t.new_eol,
        EolStyle::Lf,
        "the worktree side is shown in git (LF) form"
    );
    let expected = fixture.git(&["hash-object", "--path=f.txt", "f.txt"]);
    assert_eq!(
        d.new.expect("new side").id.to_string(),
        expected,
        "the worktree side id is the hash of the converted content"
    );
}

#[test]
fn autocrlf_true_keeps_crlf_already_in_the_index() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    // Committed with autocrlf off: the blob itself has CRLF. Git then never converts this path on `add` (the
    // "CRLF in index" rule), so its worktree content is its git form.
    fixture.write("f.txt", "a\r\nb\r\nc\r\n");
    fixture.commit_all("one");
    let repo = open_with_autocrlf(&env, &fixture);
    fixture.write("f.txt", "a\r\nB\r\nc\r\n");

    let d = diff::file_diff(
        &repo,
        &unstaged("f.txt"),
        &DiffOptions::default(),
        &CancelToken::new(),
    )
    .expect("diff");
    let t = text(&d);
    assert_eq!(
        render(t),
        [" a", "-b", "+B", " c"],
        "only the edited line differs, like `git diff`"
    );
    assert_eq!(t.new_eol, EolStyle::CrLf, "CRLF stays in the git form");
    assert_eq!(
        fixture
            .git(&["diff", "--numstat"])
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>(),
        ["1", "1"],
        "git agrees: one line changed"
    );
    // `git hash-object` does not load the index (so it converts); `git add` is the oracle that matters.
    fixture.git(&["add", "f.txt"]);
    assert_eq!(
        d.new.expect("new side").id,
        rev(&fixture, ":f.txt"),
        "the worktree side id is the blob `git add` stores"
    );
}

#[test]
fn mixed_eol_detected() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("keep.txt", "k\n");
    fixture.commit_all("one");
    fixture.write("f.txt", "a\r\nb\nc\n");
    fixture.git(&["add", "f.txt"]);

    let d = file_diff(&fixture, &staged("f.txt"), &DiffOptions::default());
    assert_eq!(
        text(&d).new_eol,
        EolStyle::Mixed,
        "CRLF and LF lines in one file"
    );
}

#[test]
fn windows1252_decoded() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("keep.txt", "k\n");
    fixture.commit_all("one");
    let mut bytes = Vec::new();
    for line in [
        "Le caf\u{e9} \u{e9}tait tr\u{e8}s fr\u{e9}quent\u{e9} par les \u{e9}tudiants.",
        "Il y avait une \u{e9}norme s\u{e9}lection de th\u{e9}s et de g\u{e2}teaux.",
        "Les prix \u{e9}taient mod\u{e9}r\u{e9}s, \u{e0} la fa\u{e7}on de la maison.",
    ] {
        let (encoded, _, _) = encoding_rs::WINDOWS_1252.encode(line);
        bytes.extend_from_slice(&encoded);
        bytes.push(b'\n');
    }
    fixture.write("fr.txt", &bytes);
    fixture.git(&["add", "fr.txt"]);

    let d = file_diff(&fixture, &staged("fr.txt"), &DiffOptions::default());
    let t = text(&d);
    assert_eq!(t.encoding, "windows-1252", "detected from the bytes");
    assert!(
        render(t)[0].contains("caf\u{e9} \u{e9}tait"),
        "decoded: {:?}",
        render(t)[0]
    );
}

#[test]
fn full_file_single_hunk() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", numbered(40));
    fixture.commit_all("one");
    fixture.write("f.txt", numbered(40).replace("line 20\n", "changed\n"));

    let opts = DiffOptions {
        full_file: true,
        ..DiffOptions::default()
    };
    let d = file_diff(&fixture, &unstaged("f.txt"), &opts);
    let t = text(&d);
    assert_eq!(t.hunks.len(), 1, "one hunk");
    assert_eq!(
        t.hunks[0].lines.len(),
        41,
        "all 40 lines plus the added one"
    );
    let header = t.hunks[0].header;
    assert_eq!(
        (header.old_start, header.old_lines),
        (1, 40),
        "whole old file"
    );
    assert_eq!(
        (header.new_start, header.new_lines),
        (1, 40),
        "whole new file"
    );
}

#[test]
fn context_lines_respected() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", numbered(30));
    fixture.commit_all("one");
    fixture.write("f.txt", numbered(30).replace("line 15\n", "changed\n"));

    for (context, expected) in [(0u32, 2usize), (1, 4), (3, 8)] {
        let opts = DiffOptions {
            context_lines: context,
            ..DiffOptions::default()
        };
        let d = file_diff(&fixture, &unstaged("f.txt"), &opts);
        assert_eq!(
            text(&d).lines.len(),
            expected,
            "context {context}: two changed lines plus 2*context"
        );
    }
}

#[test]
fn whitespace_ignore_all_hides_change_and_blocks_partial() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "a b\nc\n");
    fixture.commit_all("one");
    fixture.write("f.txt", "a  b\nc\n");

    let d = file_diff(&fixture, &unstaged("f.txt"), &DiffOptions::default());
    assert_eq!(text(&d).hunks.len(), 1, "whitespace matters by default");
    assert_eq!(
        text(&d).partial,
        PartialAvailability::Allowed,
        "partial allowed"
    );

    let opts = DiffOptions {
        whitespace: WhitespaceMode::IgnoreAll,
        ..DiffOptions::default()
    };
    let d = file_diff(&fixture, &unstaged("f.txt"), &opts);
    let t = text(&d);
    assert!(t.hunks.is_empty(), "the whitespace-only change is hidden");
    assert_eq!(
        t.partial,
        PartialAvailability::WholeFileOnly(PartialBlock::WhitespaceIgnored),
        "ignoring whitespace blocks partial operations"
    );
}

#[test]
fn binary_detected() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("b.dat", [0u8, 1, 2, 3, 0, 5]);
    fixture.commit_all("one");
    fixture.write("b.dat", [0u8, 1, 2, 3, 0, 6, 7]);

    let d = file_diff(&fixture, &unstaged("b.dat"), &DiffOptions::default());
    assert!(
        matches!(d.content, DiffContent::Binary),
        "NUL bytes: {:?}",
        d.content
    );
    assert!(
        !d.new.expect("new side").id.is_null(),
        "the worktree side of a binary file is still hashed"
    );
}

#[test]
fn too_large_detected() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "a\n");
    fixture.commit_all("one");
    fixture.write("f.txt", "x".repeat(100));

    let opts = DiffOptions {
        max_file_bytes: 10,
        ..DiffOptions::default()
    };
    let d = file_diff(&fixture, &unstaged("f.txt"), &opts);
    assert!(
        matches!(d.content, DiffContent::TooLarge { threshold: 10 }),
        "{:?}",
        d.content
    );
    assert!(
        d.new.expect("new side").id.is_null(),
        "too large worktree files are not hashed"
    );
}

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[test]
fn image_png_both_sides() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("i.png", PNG_1X1);
    fixture.commit_all("one");
    let mut changed = PNG_1X1.to_vec();
    changed.push(0);
    fixture.write("i.png", &changed);

    let d = file_diff(&fixture, &unstaged("i.png"), &DiffOptions::default());
    match &d.content {
        DiffContent::Image { old, new } => {
            let old = old.as_ref().expect("old image");
            let new = new.as_ref().expect("new image");
            assert_eq!(old.mime, "image/png", "sniffed");
            assert_eq!(&*old.bytes, PNG_1X1, "old bytes");
            assert_eq!(&*new.bytes, &changed[..], "new bytes");
        }
        other => panic!("expected an image diff, got {other:?}"),
    }

    let tiny = DiffOptions {
        max_image_bytes: 4,
        ..DiffOptions::default()
    };
    let d = file_diff(&fixture, &unstaged("i.png"), &tiny);
    assert!(
        matches!(d.content, DiffContent::Binary),
        "images above the limit are binary"
    );
}

#[test]
fn submodule_pointer_summary() {
    let env = TestEnv::new();
    let sub = env.repo("sub");
    sub.write("s.txt", "1\n");
    let first = sub.commit_all("first");
    let main = env.repo("main");
    main.write("m.txt", "m\n");
    main.commit_all("main one");
    let sub_url = sub.path.to_string_lossy().replace('\\', "/");
    main.git(&["submodule", "add", &sub_url, "sm"]);
    main.commit_all("add submodule");

    sub.write("s.txt", "2\n");
    sub.commit_all("second");
    sub.write("s.txt", "3\n");
    let third = sub.commit_all("third");
    main.git(&["-C", "sm", "fetch", "-q", "origin"]);
    main.git(&["-C", "sm", "checkout", "-q", &third.to_string()]);

    let d = file_diff(&main, &unstaged("sm"), &DiffOptions::default());
    match &d.content {
        DiffContent::Submodule {
            old_commit,
            new_commit,
            summary,
            ..
        } => {
            assert_eq!(*old_commit, Some(first), "pointer in the index");
            assert_eq!(*new_commit, Some(third), "checked out commit");
            let summary = summary
                .as_ref()
                .expect("summary from the submodule repository");
            let subjects: Vec<&str> = summary.commits.iter().map(|(_, s)| s.as_str()).collect();
            assert_eq!(subjects, ["third", "second"], "newest first");
            assert!(
                !summary.rewound && !summary.truncated,
                "fast-forward, short"
            );
        }
        other => panic!("expected a submodule diff, got {other:?}"),
    }
}

#[test]
fn typechange_reported() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("t", "target\n");
    fixture.write("link-target", "elsewhere");
    fixture.commit_all("one");
    // Stage a symlink entry without needing symlink support of the file system.
    let blob = fixture.git(&["hash-object", "-w", "link-target"]);
    fixture.git(&["update-index", "--cacheinfo", &format!("120000,{blob},t")]);

    let d = file_diff(&fixture, &staged("t"), &DiffOptions::default());
    assert_eq!(d.kind, ChangeKind::TypeChanged, "file became a link");
    assert!(
        matches!(
            d.content,
            DiffContent::TypeChange {
                old: EntryKind::Blob,
                new: EntryKind::Link
            }
        ),
        "{:?}",
        d.content
    );
}

#[test]
fn mode_only_change() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("run.sh", "echo hi\n");
    fixture.commit_all("one");
    fixture.git(&["update-index", "--chmod=+x", "run.sh"]);

    let d = file_diff(&fixture, &staged("run.sh"), &DiffOptions::default());
    assert!(
        matches!(
            d.content,
            DiffContent::ModeOnly {
                old: EntryKind::Blob,
                new: EntryKind::BlobExecutable
            }
        ),
        "{:?}",
        d.content
    );
}

#[test]
fn empty_file_added() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("keep.txt", "k\n");
    fixture.commit_all("one");
    fixture.write("empty.txt", "");
    fixture.git(&["add", "empty.txt"]);

    let d = file_diff(&fixture, &staged("empty.txt"), &DiffOptions::default());
    assert_eq!(d.kind, ChangeKind::Added, "added");
    assert!(matches!(d.content, DiffContent::Empty), "{:?}", d.content);
}

#[test]
fn staged_rename_with_modification() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("old.txt", numbered(20));
    fixture.commit_all("one");
    fixture.git(&["mv", "old.txt", "new.txt"]);
    fixture.write("new.txt", numbered(20).replace("line 10\n", "changed\n"));
    fixture.git(&["add", "new.txt"]);

    let target = DiffTarget::Staged {
        path: "new.txt".into(),
        previous_path: Some("old.txt".into()),
    };
    let d = file_diff(&fixture, &target, &DiffOptions::default());
    assert_eq!(d.kind, ChangeKind::Renamed, "rename");
    assert_eq!(
        d.previous_path.as_ref().map(|p| p.to_string()),
        Some("old.txt".to_owned()),
        "old path kept"
    );
    let t = text(&d);
    assert_eq!(
        (t.added, t.removed),
        (1, 1),
        "only the modification is shown"
    );
    assert_eq!(
        t.partial,
        PartialAvailability::Allowed,
        "staged renames allow partial unstage"
    );

    let repo = fixture.open();
    let patch = diff::staged_patch_text(&repo, &PatchTextOptions::default(), &CancelToken::new())
        .expect("patch text");
    assert_eq!(patch.files.len(), 1, "the rename is one file");
    assert_eq!(
        patch.files[0].kind,
        ChangeKind::Renamed,
        "detected as rename"
    );
}

#[test]
fn unstaged_conflict_diff_against_ours() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("c.txt", "base\n");
    fixture.commit_all("base");
    fixture.git(&["checkout", "-q", "-b", "other"]);
    fixture.write("c.txt", "theirs\n");
    fixture.commit_all("theirs");
    fixture.git(&["checkout", "-q", "main"]);
    fixture.write("c.txt", "ours\n");
    fixture.commit_all("ours");
    fixture.git_fail(&["merge", "other"]);

    let d = file_diff(&fixture, &unstaged("c.txt"), &DiffOptions::default());
    assert_eq!(d.kind, ChangeKind::Conflicted, "unmerged path");
    let t = text(&d);
    assert_eq!(
        t.partial,
        PartialAvailability::WholeFileOnly(PartialBlock::Conflict),
        "conflicts block partial"
    );
    let rendered = render(t);
    assert!(
        rendered.contains(&" ours".to_owned()),
        "old side is stage 2, so ours is unchanged context: {rendered:?}"
    );
    assert!(
        rendered.iter().any(|l| l.starts_with("+<<<<<<<")),
        "the worktree has conflict markers: {rendered:?}"
    );
}

#[test]
fn commit_root_against_empty() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "a\n");
    fixture.write("dir/b.txt", "b\nb\n");
    let root = fixture.commit_all("root");
    let repo = fixture.open();

    let opts = CommitFilesOptions {
        line_stats: true,
        stash: false,
    };
    let details = diff::commit_details(&repo, root, opts).expect("details");
    assert!(details.info.parents.is_empty(), "root commit");
    assert_eq!(&*details.info.summary, "root", "summary");
    assert!(!details.signed, "unsigned");
    let paths: Vec<_> = details.files.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(paths, ["a.txt", "dir/b.txt"], "sorted by path");
    assert!(
        details.files.iter().all(|f| f.kind == ChangeKind::Added),
        "all added"
    );
    assert_eq!(details.files[1].lines_added, Some(2), "line stats");

    let d = diff::file_diff(
        &repo,
        &details.files[1].target,
        &DiffOptions::default(),
        &CancelToken::new(),
    )
    .expect("diff of the commit file");
    assert_eq!(render(text(&d)), ["+b", "+b"], "against the empty tree");

    let again = diff::commit_details(&repo, root, opts).expect("cached details");
    assert_eq!(again.files.len(), 2, "cache hit returns the same files");
}

#[test]
fn merge_commit_against_first_parent() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("base.txt", "base\n");
    fixture.commit_all("base");
    fixture.git(&["checkout", "-q", "-b", "side"]);
    fixture.write("side.txt", "side\n");
    fixture.commit_all("side");
    fixture.git(&["checkout", "-q", "main"]);
    fixture.write("main.txt", "main\n");
    fixture.commit_all("main");
    fixture.git(&["merge", "--no-ff", "-q", "-m", "merge", "side"]);
    let merge = rev(&fixture, "HEAD");
    let repo = fixture.open();

    let details =
        diff::commit_details(&repo, merge, CommitFilesOptions::default()).expect("details");
    assert_eq!(details.info.parents.len(), 2, "a merge");
    let paths: Vec<_> = details.files.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(
        paths,
        ["side.txt"],
        "what the merge brought into the first parent"
    );

    let second = diff::commit_files(
        &repo,
        merge,
        Against::Parent(1),
        CommitFilesOptions::default(),
    )
    .expect("against the second parent");
    let paths: Vec<_> = second.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(paths, ["main.txt"], "against parent 2");
}

#[test]
fn stash_files_include_untracked() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("t.txt", "one\n");
    fixture.commit_all("one");
    fixture.write("t.txt", "two\n");
    fixture.write("untracked.txt", "u\n");
    fixture.git(&["stash", "push", "-u", "-q"]);
    let stash = rev(&fixture, "refs/stash");
    let untracked_commit = rev(&fixture, "refs/stash^3");
    let repo = fixture.open();

    let without = diff::commit_files(
        &repo,
        stash,
        Against::FirstParent,
        CommitFilesOptions::default(),
    )
    .expect("files");
    assert_eq!(without.len(), 1, "tracked changes only");
    let files = diff::commit_files(
        &repo,
        stash,
        Against::FirstParent,
        CommitFilesOptions {
            line_stats: false,
            stash: true,
        },
    )
    .expect("files with untracked");
    let extra = files
        .iter()
        .find(|f| f.path == "untracked.txt")
        .expect("untracked file listed");
    assert_eq!(
        extra.kind,
        ChangeKind::Added,
        "untracked files are additions"
    );
    assert!(
        matches!(
            &extra.target,
            DiffTarget::Commit { commit, against: Against::Empty, .. } if *commit == untracked_commit
        ),
        "target points into the third parent: {:?}",
        extra.target
    );
}

#[test]
fn inline_highlights_mark_changed_words() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "the quick brown fox\n");
    fixture.commit_all("one");
    fixture.write("f.txt", "the quick red fox\n");

    let d = file_diff(&fixture, &unstaged("f.txt"), &DiffOptions::default());
    let t = text(&d);
    let spans_of = |kind: LineKind| {
        let line = t.lines.iter().find(|l| l.kind == kind).expect("line");
        let text_of_line = t.line_text(line);
        t.inline_spans(line)
            .iter()
            .map(|r| text_of_line[r.start as usize..r.end as usize].to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(spans_of(LineKind::Removed), ["brown"], "removed side");
    assert_eq!(spans_of(LineKind::Added), ["red"], "added side");

    let off = DiffOptions {
        inline_highlights: false,
        ..DiffOptions::default()
    };
    let d = file_diff(&fixture, &unstaged("f.txt"), &off);
    assert!(
        text(&d)
            .lines
            .iter()
            .all(|l| text(&d).inline_spans(l).is_empty()),
        "disabled"
    );
}

#[test]
fn lfs_pointer_shown_as_text() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write(
        ".gitattributes",
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    );
    fixture.write(
        "big.bin",
        "version https://git-lfs.github.com/spec/v1\noid sha256:aaa\nsize 12\n",
    );
    fixture.commit_all("one");
    fixture.write(
        "big.bin",
        "version https://git-lfs.github.com/spec/v1\noid sha256:bbb\nsize 13\n",
    );

    let d = file_diff(&fixture, &unstaged("big.bin"), &DiffOptions::default());
    let t = text(&d);
    assert_eq!(
        t.partial,
        PartialAvailability::WholeFileOnly(PartialBlock::Lfs),
        "pointer text, not content"
    );
    assert_eq!((t.added, t.removed), (2, 2), "pointer lines changed");
}

#[test]
fn staged_patch_text_budget_and_truncation_marker() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("keep.txt", "k\n");
    fixture.commit_all("one");
    for name in ["a.txt", "b.txt", "c.txt"] {
        fixture.write(name, numbered(100));
    }
    fixture.write("Cargo.lock", numbered(50));
    fixture.git(&["add", "-A"]);
    let repo = fixture.open();

    let full = diff::staged_patch_text(&repo, &PatchTextOptions::default(), &CancelToken::new())
        .expect("patch text");
    assert_eq!(full.files.len(), 4, "four staged files");
    let a = full
        .files
        .iter()
        .find(|f| f.path == "a.txt")
        .expect("a.txt");
    assert_eq!(
        (a.added, a.removed, a.binary),
        (100, 0, false),
        "stats of a.txt"
    );
    assert!(full.text.contains("File: a.txt (added)"), "body of a.txt");
    assert!(
        !full.text.contains("File: Cargo.lock"),
        "skipped by glob: header only"
    );
    assert!(
        full.text.contains("- Cargo.lock (added) +50 -0"),
        "but listed in the header"
    );
    assert_eq!(
        full.truncated_files,
        0,
        "everything fits: {} chars",
        full.text.len()
    );
    assert!(
        !full.text.contains("[diff truncated"),
        "no marker without truncation"
    );

    let tight = PatchTextOptions {
        budget_chars: 200,
        per_file_min_chars: 40,
        skip_globs: vec!["*.lock".into()],
    };
    let cut = diff::staged_patch_text(&repo, &tight, &CancelToken::new()).expect("patch text");
    assert!(cut.truncated_files > 0, "files were cut: {}", cut.text);
    assert!(
        cut.text.trim_end().ends_with(&format!(
            "[diff truncated: {} more files]",
            cut.truncated_files
        )),
        "marker at the end: {}",
        cut.text
    );
}

#[test]
fn staged_patch_text_cancel() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("keep.txt", "k\n");
    fixture.commit_all("one");
    fixture.write("a.txt", "a\n");
    fixture.git(&["add", "-A"]);
    let repo = fixture.open();

    let cancel = CancelToken::new();
    cancel.cancel();
    let err = diff::staged_patch_text(&repo, &PatchTextOptions::default(), &cancel)
        .expect_err("cancelled");
    assert_eq!(err.kind(), GitErrorKind::Cancelled, "{err:?}");
}

#[test]
fn export_patch_matches_git_diff() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "1\n2\n3\n");
    fixture.write("b.bin", [0u8, 1, 2]);
    fixture.commit_all("one");
    fixture.write("a.txt", "1\nx\n3\n");
    fixture.write("b.bin", [0u8, 9, 9, 9]);
    let repo = fixture.open();
    let cancel = CancelToken::new();

    let unstaged_patch =
        diff::export_patch(&repo, &ExportPatch::Unstaged(vec![]), &cancel).expect("unstaged patch");
    let expected = fixture.git_bytes(&[
        "diff",
        "--binary",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
    ]);
    assert_eq!(
        unstaged_patch.as_slice(),
        &expected[..],
        "unstaged, all paths"
    );

    fixture.git(&["add", "-A"]);
    let staged_patch =
        diff::export_patch(&repo, &ExportPatch::Staged(vec!["a.txt".into()]), &cancel)
            .expect("staged patch");
    let expected = fixture.git_bytes(&[
        "diff",
        "--binary",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--cached",
        "--",
        "a.txt",
    ]);
    assert_eq!(staged_patch.as_slice(), &expected[..], "staged, one path");

    let commit = fixture.commit_all("two");
    let patch =
        diff::export_patch(&repo, &ExportPatch::Commit(commit), &cancel).expect("commit patch");
    let expected = fixture.git_bytes(&[
        "format-patch",
        "-1",
        "--stdout",
        "--binary",
        &commit.to_string(),
    ]);
    assert_eq!(
        patch.as_slice(),
        &expected[..],
        "commit patch equals format-patch"
    );

    cancel.cancel();
    let err =
        diff::export_patch(&repo, &ExportPatch::Commit(commit), &cancel).expect_err("cancelled");
    assert_eq!(err.kind(), GitErrorKind::Cancelled, "{err:?}");
}

#[test]
fn export_patch_glob_char_path_literal() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    for name in ["[12].txt", "1.txt", "2.txt"] {
        fixture.write(name, "old\n");
    }
    fixture.commit_all("one");
    for name in ["[12].txt", "1.txt", "2.txt"] {
        fixture.write(name, "new\n");
    }
    let repo = fixture.open();

    let patch = diff::export_patch(
        &repo,
        &ExportPatch::Unstaged(vec!["[12].txt".into()]),
        &CancelToken::new(),
    )
    .expect("patch");
    let patch = patch.to_string();
    assert!(patch.contains("b/[12].txt"), "the literal name: {patch}");
    assert!(
        !patch.contains("b/1.txt") && !patch.contains("b/2.txt"),
        "the glob did not match other files: {patch}"
    );
}

#[test]
fn worktree_side_id_equals_git_hash_object() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write(".gitattributes", "*.txt text\n");
    fixture.write("f.txt", "a\nb\n");
    fixture.commit_all("one");
    fixture.write("f.txt", "a\r\nb\r\nc\r\n");

    let d = file_diff(&fixture, &unstaged("f.txt"), &DiffOptions::default());
    let expected = fixture.git(&["hash-object", "--path=f.txt", "f.txt"]);
    let new = d.new.expect("new side");
    assert_eq!(new.id.to_string(), expected, "hash of the git-form content");
    assert!(new.from_worktree, "flagged as worktree side");
}

#[test]
fn hunk_grouping_matches_git_merge_rule() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", numbered(40));
    fixture.commit_all("one");

    // Changes at line 10 and at 10 + 1 + gap.
    for (gap, hunks) in [(6usize, 1usize), (7, 2)] {
        let second = 10 + 1 + gap;
        let content: String = (1..=40)
            .map(|n| {
                if n == 10 || n == second {
                    format!("changed {n}\n")
                } else {
                    format!("line {n}\n")
                }
            })
            .collect();
        fixture.write("f.txt", content);
        let d = file_diff(&fixture, &unstaged("f.txt"), &DiffOptions::default());
        assert_eq!(text(&d).hunks.len(), hunks, "gap of {gap} unchanged lines");
        let git = fixture.git(&["diff", "-U3", "--no-color"]);
        assert_eq!(
            git.lines().filter(|l| l.starts_with("@@")).count(),
            hunks,
            "git agrees for gap {gap}"
        );
    }
}

#[test]
fn tree_files_compares_two_commits() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "1\n2\n");
    let first = fixture.commit_all("one");
    fixture.write("a.txt", "1\n2\n3\n");
    fixture.write("b.txt", "b\n");
    let second = fixture.commit_all("two");
    let repo = fixture.open();

    let files = diff::tree_files(&repo, Some(first), second, true).expect("files");
    let summary: Vec<_> = files
        .iter()
        .map(|f| (f.path.to_string(), f.kind, f.lines_added, f.lines_removed))
        .collect();
    assert_eq!(
        summary,
        [
            ("a.txt".to_owned(), ChangeKind::Modified, Some(1), Some(0)),
            ("b.txt".to_owned(), ChangeKind::Added, Some(1), Some(0)),
        ],
        "changes with line stats"
    );
    let from_empty = diff::tree_files(&repo, None, first, false).expect("files");
    assert_eq!(from_empty.len(), 1, "everything of the first commit");
    assert_eq!(from_empty[0].lines_added, None, "no stats unless requested");
}

#[test]
fn read_blob_respects_limit() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "hello world\n");
    fixture.commit_all("one");
    let repo = fixture.open();
    let id = rev(&fixture, "HEAD:f.txt");

    let bytes = diff::read_blob(&repo, id, 1024).expect("blob");
    assert_eq!(&*bytes, b"hello world\n", "content");
    let err = diff::read_blob(&repo, id, 4).expect_err("too large");
    assert_eq!(err.kind(), GitErrorKind::Unsupported, "{err:?}");
    let missing = gix::ObjectId::from_hex(b"1111111111111111111111111111111111111111").expect("id");
    let err = diff::read_blob(&repo, missing, 1024).expect_err("missing");
    assert_eq!(err.kind(), GitErrorKind::NotFound, "{err:?}");
}

#[test]
fn untracked_file_diff_is_all_added() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("keep.txt", "k\n");
    fixture.commit_all("one");
    fixture.write("new.txt", "x\n");

    let d = file_diff(&fixture, &unstaged("new.txt"), &DiffOptions::default());
    assert_eq!(d.kind, ChangeKind::Untracked, "not in the index");
    assert!(d.old.is_none(), "no old side");
    assert_eq!(render(text(&d)), ["+x"], "added");
    let repo = fixture.open();
    let err = diff::file_diff(
        &repo,
        &unstaged("nothing.txt"),
        &DiffOptions::default(),
        &CancelToken::new(),
    )
    .expect_err("unknown path");
    assert_eq!(err.kind(), GitErrorKind::NotFound, "{err:?}");
}

#[test]
fn missing_final_newline_on_both_sides() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("f.txt", "a\nb");
    fixture.commit_all("one");
    fixture.write("f.txt", "a\nc");

    let d = file_diff(&fixture, &unstaged("f.txt"), &DiffOptions::default());
    let t = text(&d);
    assert!(
        t.old_missing_final_newline && t.new_missing_final_newline,
        "neither side ends with a newline"
    );
    assert_eq!(render(t), [" a", "-b", "+c"], "the last line changed");
    assert!(
        t.lines
            .iter()
            .filter(|l| l.kind != LineKind::Context)
            .all(|l| l.eol == Eol::None),
        "both changed last lines have no terminator"
    );
}

#[test]
fn symlink_checked_out_as_file_is_not_a_type_change() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.git(&["config", "core.symlinks", "false"]);
    // What Git does with `core.symlinks=false`: the index entry stays a link, the worktree has a plain file with
    // the target.
    fixture.write("link", "target");
    let blob = fixture.git(&["hash-object", "-w", "link"]);
    fixture.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("120000,{blob},link"),
    ]);
    fixture.git(&["commit", "-q", "-m", "link"]);
    fixture.write("link", "other");

    let d = file_diff(&fixture, &unstaged("link"), &DiffOptions::default());
    assert_eq!(d.kind, ChangeKind::Modified, "the link target changed");
    assert_eq!(
        d.new.expect("new side").kind,
        EntryKind::Link,
        "still a link"
    );
    let t = text(&d);
    assert_eq!(render(t), ["-target", "+other"], "target text diff");
    assert_eq!(
        t.partial,
        PartialAvailability::WholeFileOnly(PartialBlock::Symlink),
        "links are staged as a whole"
    );
}

#[test]
fn tree_files_accepts_tree_ids_and_targets_resolve() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "1\n");
    fixture.commit_all("one");
    fixture.write("a.txt", "2\n");
    fixture.commit_all("two");
    let (old_tree, new_tree) = (rev(&fixture, "HEAD~1^{tree}"), rev(&fixture, "HEAD^{tree}"));
    let repo = fixture.open();

    let files = diff::tree_files(&repo, Some(old_tree), new_tree, false).expect("files");
    assert_eq!(files.len(), 1, "one changed file");
    let d = diff::file_diff(
        &repo,
        &files[0].target,
        &DiffOptions::default(),
        &CancelToken::new(),
    )
    .expect("the target of a tree comparison can be diffed");
    assert_eq!(render(text(&d)), ["-1", "+2"], "tree against tree");
}

#[test]
fn staged_patch_text_describes_non_text_changes() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("keep.txt", "k\n");
    fixture.commit_all("one");
    fixture.write("empty.txt", "");
    fixture.write("b.dat", [0u8, 1, 2]);
    fixture.git(&["add", "-A"]);
    let repo = fixture.open();

    let patch = diff::staged_patch_text(&repo, &PatchTextOptions::default(), &CancelToken::new())
        .expect("patch text");
    let binary: Vec<(String, bool)> = patch
        .files
        .iter()
        .map(|f| (f.path.to_string(), f.binary))
        .collect();
    assert_eq!(
        binary,
        [("b.dat".to_owned(), true), ("empty.txt".to_owned(), false)],
        "only real binary content is flagged binary"
    );
    assert!(
        patch
            .text
            .contains("File: empty.txt (added)\nEmpty file.\n"),
        "an empty file is described, not called binary: {}",
        patch.text
    );
    assert!(
        patch
            .text
            .contains("File: b.dat (added)\nBinary or too large to show.\n"),
        "binary placeholder: {}",
        patch.text
    );
}

#[test]
fn export_patch_chunks_long_path_lists() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    // 300 pathspecs of ~100 bytes each: well above one 24 000-byte chunk.
    let names: Vec<String> = (0..300)
        .map(|n| format!("{}-{n:04}.txt", "x".repeat(80)))
        .collect();
    for name in &names {
        fixture.write(name, "old\n");
    }
    fixture.commit_all("one");
    for name in &names {
        fixture.write(name, "new\n");
    }
    let repo = fixture.open();

    // Reversed on purpose: the output still follows path order.
    let paths = names.iter().rev().map(|n| n.as_str().into()).collect();
    let patch = diff::export_patch(&repo, &ExportPatch::Unstaged(paths), &CancelToken::new())
        .expect("patch");
    let expected = fixture.git_bytes(&[
        "diff",
        "--binary",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
    ]);
    assert_eq!(
        patch.as_slice(),
        &expected[..],
        "chunked output equals one `git diff` over everything"
    );
}

/// Spec §9: a 50k-line file diffs within 120 ms including inline highlights (release build, reference machine).
/// Run with `--ignored`; the budget is asserted only with `GITCTHULHU_PERF_ASSERT=1`.
#[test]
#[ignore = "performance test"]
fn perf_file_diff_50k_lines() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let old: String = (0..50_000)
        .map(|n| format!("fn item_{n}() {{ let value = {n}; }}\n"))
        .collect();
    fixture.write("big.rs", &old);
    fixture.commit_all("one");
    let new: String = (0..50_000)
        .map(|n| {
            if n % 10 == 0 {
                format!("fn item_{n}() {{ let value = {}; }}\n", n + 1)
            } else {
                format!("fn item_{n}() {{ let value = {n}; }}\n")
            }
        })
        .collect();
    fixture.write("big.rs", &new);
    let repo = fixture.open();

    let started = std::time::Instant::now();
    let d = diff::file_diff(
        &repo,
        &unstaged("big.rs"),
        &DiffOptions::default(),
        &CancelToken::new(),
    )
    .expect("diff");
    let elapsed = started.elapsed();
    assert_eq!(
        (text(&d).added, text(&d).removed),
        (5_000, 5_000),
        "every tenth line changed"
    );
    println!("file_diff of 50k lines: {elapsed:?}");
    if std::env::var_os("GITCTHULHU_PERF_ASSERT").is_some_and(|v| v == "1") {
        assert!(
            elapsed <= std::time::Duration::from_millis(120),
            "budget 120 ms, took {elapsed:?}"
        );
    }
}
