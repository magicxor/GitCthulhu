//! Differential test of the in-process staging engine against Git (spec 20 §4.1, §4.8).
//!
//! For every fixture pair in `tests/fixtures/oracle/` (LF, CRLF, missing final newlines, Latin-1, repeated lines,
//! adjacent hunks) and every hunk *i* of our own diff, hunk *i* is rendered as a git patch from the raw bytes and
//! applied with `git apply --cached --whitespace=nowarn`; the resulting index blob must equal the one
//! `stage::stage_selection(Hunks([i]))` writes. Staging every hunk (and every line) must equal `git add`.
//!
//! The hunks come from `diff::file_diff`; display and staging share one line engine (`internal::linediff`), so the
//! displayed line numbers select exactly the lines of Git's patch. `tricky_inputs_stage_exactly_the_displayed_hunks`
//! pins that for CRLF, mixed line endings, missing final newlines, whitespace-only changes and every algorithm;
//! `oracle_discard_each_hunk_equals_git_apply_reverse` pins the discard direction against `git apply -R`.

mod support;

use std::sync::Arc;

use bstr::ByteSlice as _;
use gitcthulhu_git::{
    CancelToken, DiffAlgorithm, DiffContent, DiffOptions, DiffTarget, HunkHeader, LineKind,
    LineRef, NoPrompter, PartialAvailability, Repo, Selection, SelectionRequest, diff, stage,
};
use support::{TestEnv, TestRepo};

const FIXTURES: &[&str] = &[
    "lf_simple",
    "lf_adjacent_hunks",
    "lf_repeated_lines",
    "lf_insert_start",
    "lf_append_end",
    "lf_delete_blocks",
    "crlf_simple",
    "crlf_no_eol",
    "no_eol_old",
    "no_eol_new",
    "no_eol_both",
    "latin1",
];

const CONTEXT_LINES: u32 = 3;

fn fixture(name: &str) -> (Vec<u8>, Vec<u8>) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("oracle");
    let read = |ext: &str| {
        std::fs::read(dir.join(format!("{name}.{ext}"))).expect("the fixture pair exists")
    };
    (read("old"), read("new"))
}

/// One displayed line: its kind and 1-based line numbers.
#[derive(Debug, Clone, Copy)]
struct Line {
    kind: LineKind,
    old: Option<u32>,
    new: Option<u32>,
}

#[derive(Debug, Clone)]
struct Hunk {
    header: HunkHeader,
    lines: Vec<Line>,
}

/// The hunks of the unstaged diff of `f` and the diff's side ids.
struct OurDiff {
    hunks: Vec<Hunk>,
    base_old: Option<gix::ObjectId>,
    base_new: Option<gix::ObjectId>,
}

fn our_diff(repo: &Repo) -> OurDiff {
    our_diff_with(repo, None)
}

/// [`our_diff`] with an explicit line algorithm (`DiffOptions::algorithm`).
fn our_diff_with(repo: &Repo, algorithm: Option<DiffAlgorithm>) -> OurDiff {
    let target = DiffTarget::Unstaged {
        path: "f".into(),
        previous_path: None,
    };
    let opts = DiffOptions {
        context_lines: CONTEXT_LINES,
        algorithm,
        ..DiffOptions::default()
    };
    match diff::file_diff(repo, &target, &opts, &CancelToken::new()) {
        Ok(file_diff) => {
            let DiffContent::Text(text) = &file_diff.content else {
                panic!("fixtures are text, got {:?}", file_diff.content);
            };
            assert_eq!(
                text.partial,
                PartialAvailability::Allowed,
                "the diff offers partial staging for plain text"
            );
            OurDiff {
                hunks: text
                    .hunks
                    .iter()
                    .map(|hunk| Hunk {
                        header: hunk.header,
                        lines: text
                            .hunk_lines(hunk)
                            .iter()
                            .map(|line| Line {
                                kind: line.kind,
                                old: line.old_lineno,
                                new: line.new_lineno,
                            })
                            .collect(),
                    })
                    .collect(),
                base_old: file_diff.old.map(|side| side.id),
                base_new: file_diff.new.map(|side| side.id),
            }
        }
        Err(err) => panic!("file_diff failed: {err:?}"),
    }
}

fn oid(hex: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(hex.trim().as_bytes()).expect("git prints hex object ids")
}

fn index_id(t: &TestRepo) -> gix::ObjectId {
    oid(&t.git(&["rev-parse", ":f"]))
}

/// The lines of `bytes`, each with its terminator.
fn raw_lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|b| *b == b'\n').collect()
}

/// Hunk `hunk` as a git patch built from the raw bytes, with `\ No newline at end of file` after a line without
/// terminator.
fn render_patch(hunk: &Hunk, old: &[u8], new: &[u8]) -> Vec<u8> {
    let (old_lines, new_lines) = (raw_lines(old), raw_lines(new));
    let h = hunk.header;
    let mut out = format!(
        "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -{},{} +{},{} @@\n",
        h.old_start, h.old_lines, h.new_start, h.new_lines
    )
    .into_bytes();
    for line in &hunk.lines {
        let (prefix, bytes) = match line.kind {
            LineKind::Context => (
                b' ',
                old_lines[line.old.expect("context has an old line") as usize - 1],
            ),
            LineKind::Removed => (
                b'-',
                old_lines[line.old.expect("removed has an old line") as usize - 1],
            ),
            LineKind::Added => (
                b'+',
                new_lines[line.new.expect("added has a new line") as usize - 1],
            ),
        };
        out.push(prefix);
        out.extend_from_slice(bytes);
        if !bytes.ends_with(b"\n") {
            out.extend_from_slice(b"\n\\ No newline at end of file\n");
        }
    }
    out
}

fn setup(env: &TestEnv, name: &str) -> (TestRepo, Vec<u8>, Vec<u8>) {
    let (old, new) = fixture(name);
    let t = env.repo(name);
    t.write("f", &old);
    t.commit_all("base");
    t.write("f", &new);
    (t, old, new)
}

fn stage(t: &TestRepo, repo: &Repo, diff: &OurDiff, selection: Selection) {
    stage_with(t, repo, diff, None, selection);
}

/// [`stage`] of a selection made on a diff computed with `algorithm`.
fn stage_with(
    t: &TestRepo,
    repo: &Repo,
    diff: &OurDiff,
    algorithm: Option<DiffAlgorithm>,
    selection: Selection,
) {
    let req = SelectionRequest {
        path: "f".into(),
        previous_path: None,
        base_old: diff.base_old,
        base_new: diff.base_new,
        algorithm,
        selection,
    };
    let mut guard = repo.lock_exclusive();
    stage::stage_selection(
        repo,
        guard.write_permission(),
        &t.op_ctx(CancelToken::new(), Arc::new(NoPrompter)),
        &req,
    )
    .expect("the selection can be staged");
}

#[test]
fn oracle_each_hunk_equals_git_apply_cached() {
    let env = TestEnv::new();
    let mut compared = 0;
    for name in FIXTURES {
        let (t, old, new) = setup(&env, name);
        let repo = t.open();
        let diff = our_diff(&repo);
        assert!(!diff.hunks.is_empty(), "{name}: the fixture has changes");
        for (i, hunk) in diff.hunks.iter().enumerate() {
            let patch = env.path(&format!("{name}-{i}.patch"));
            std::fs::write(&patch, render_patch(hunk, &old, &new)).expect("patch can be written");
            t.git(&["reset", "-q"]);
            t.git(&[
                "apply",
                "--cached",
                "--whitespace=nowarn",
                patch.to_str().expect("temp paths are UTF-8"),
            ]);
            let expected = index_id(&t);
            t.git(&["reset", "-q"]);

            stage(&t, &repo, &diff, Selection::Hunks(vec![hunk.header]));
            assert_eq!(
                index_id(&t),
                expected,
                "{name}, hunk {i} {:?}: the index edit writes the same blob as git apply --cached",
                hunk.header
            );
            compared += 1;
        }
        t.git(&["reset", "-q"]);
    }
    assert!(
        compared >= FIXTURES.len(),
        "every fixture contributed hunks"
    );
}

#[test]
fn oracle_full_stage_equals_git_add() {
    let env = TestEnv::new();
    for name in FIXTURES {
        let (t, _old, _new) = setup(&env, name);
        let repo = t.open();
        let diff = our_diff(&repo);
        t.git(&["add", "f"]);
        let expected = index_id(&t);
        t.git(&["reset", "-q"]);

        stage(
            &t,
            &repo,
            &diff,
            Selection::Hunks(diff.hunks.iter().map(|h| h.header).collect()),
        );
        assert_eq!(
            index_id(&t),
            expected,
            "{name}: staging every hunk equals git add"
        );
        t.git(&["reset", "-q"]);

        let every_line = diff
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter_map(|line| match line.kind {
                LineKind::Added => line.new.map(|new_lineno| LineRef::Added { new_lineno }),
                LineKind::Removed => line.old.map(|old_lineno| LineRef::Removed { old_lineno }),
                LineKind::Context => None,
            })
            .collect();
        stage(&t, &repo, &diff, Selection::Lines(every_line));
        assert_eq!(
            index_id(&t),
            expected,
            "{name}: staging every line equals git add"
        );
    }
}

/// Every added and removed line of `hunk`, by its displayed line number.
fn changed_lines(hunk: &Hunk) -> Selection {
    Selection::Lines(
        hunk.lines
            .iter()
            .filter_map(|line| match line.kind {
                LineKind::Added => line.new.map(|new_lineno| LineRef::Added { new_lineno }),
                LineKind::Removed => line.old.map(|old_lineno| LineRef::Removed { old_lineno }),
                LineKind::Context => None,
            })
            .collect(),
    )
}

/// An input pair with the algorithm its diff is displayed with.
type TrickyInput = (
    &'static str,
    &'static [u8],
    &'static [u8],
    Option<DiffAlgorithm>,
);

/// Inputs where a second line engine would most likely drift from the displayed diff: line endings, missing final
/// newlines, whitespace-only edits, slider-prone blank lines and every algorithm. Written as exact bytes, so no
/// checkout conversion touches them.
fn tricky_inputs() -> Vec<TrickyInput> {
    const REORDERED_OLD: &[u8] = b"{\n  a\n}\n{\n  b\n}\n{\n  c\n}\n";
    const REORDERED_NEW: &[u8] = b"{\n  b\n}\n{\n  a\n}\n{\n  c\n}\n{\n  d\n}\n";
    vec![
        (
            "crlf_lines_changed",
            b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\r\nnine\r\nten\r\n",
            b"one\r\nTWO\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\r\nNINE\r\nten\r\n",
            None,
        ),
        (
            "mixed_eol",
            b"a\r\nb\nc\r\nd\ne\r\nf\ng\r\nh\n",
            b"a\r\nB\nc\r\nd\ne\nf\nG\r\nh\n",
            None,
        ),
        (
            "eol_only_changes",
            b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\r\nj\r\n",
            b"a\r\nb\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\nj\r\n",
            None,
        ),
        (
            "crlf_no_final_newline_both",
            b"a\r\nb\r\nc",
            b"a\r\nB\r\nc\r\nd",
            None,
        ),
        (
            "whitespace_only",
            b"fn f() {\n    x();\n\ty();\n}\n\nfn g() {\n  z(); \n}\n",
            b"fn f() {\n\tx();\n    y();\n}\n\nfn g() {\n  z();\n}\n",
            None,
        ),
        (
            "blank_line_slider",
            b"a\n\nb\n\nc\n",
            b"a\n\nb\n\nX\n\nb\n\nc\n",
            None,
        ),
        (
            "reordered_myers",
            REORDERED_OLD,
            REORDERED_NEW,
            Some(DiffAlgorithm::Myers),
        ),
        (
            "reordered_histogram",
            REORDERED_OLD,
            REORDERED_NEW,
            Some(DiffAlgorithm::Histogram),
        ),
        (
            "reordered_minimal",
            REORDERED_OLD,
            REORDERED_NEW,
            Some(DiffAlgorithm::MyersMinimal),
        ),
    ]
}

#[test]
fn tricky_inputs_stage_exactly_the_displayed_hunks() {
    let env = TestEnv::new();
    let mut reordered_lines = Vec::new();
    for (name, old, new, algorithm) in tricky_inputs() {
        let t = env.repo(name);
        t.write("f", old);
        t.commit_all("base");
        t.write("f", new);
        let repo = t.open();
        let diff = our_diff_with(&repo, algorithm);
        assert!(!diff.hunks.is_empty(), "{name}: the input has changes");
        if name.starts_with("reordered_") {
            let numbers: Vec<(Option<u32>, Option<u32>)> = diff
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .map(|line| (line.old, line.new))
                .collect();
            reordered_lines.push(numbers);
        }
        for (i, hunk) in diff.hunks.iter().enumerate() {
            let patch = env.path(&format!("{name}-{i}.patch"));
            std::fs::write(&patch, render_patch(hunk, old, new)).expect("patch can be written");
            t.git(&[
                "apply",
                "--cached",
                "--whitespace=nowarn",
                patch.to_str().expect("temp paths are UTF-8"),
            ]);
            let expected = index_id(&t);
            t.git(&["reset", "-q"]);

            stage_with(
                &t,
                &repo,
                &diff,
                algorithm,
                Selection::Hunks(vec![hunk.header]),
            );
            assert_eq!(
                index_id(&t),
                expected,
                "{name}, hunk {i} {:?}: staging the displayed hunk equals git apply --cached",
                hunk.header
            );
            t.git(&["reset", "-q"]);

            stage_with(&t, &repo, &diff, algorithm, changed_lines(hunk));
            assert_eq!(
                index_id(&t),
                expected,
                "{name}, hunk {i}: staging the hunk's displayed line numbers selects the same lines"
            );
            t.git(&["reset", "-q"]);
        }
    }
    assert_ne!(
        reordered_lines[0], reordered_lines[1],
        "the reordered input tells Myers and Histogram apart, so SelectionRequest::algorithm is exercised"
    );
}

/// Discard of hunk `header` of `diff`, selected on a diff computed with `algorithm`.
fn discard_with(
    t: &TestRepo,
    repo: &Repo,
    diff: &OurDiff,
    algorithm: Option<DiffAlgorithm>,
    header: HunkHeader,
) {
    let req = SelectionRequest {
        path: "f".into(),
        previous_path: None,
        base_old: diff.base_old,
        base_new: diff.base_new,
        algorithm,
        selection: Selection::Hunks(vec![header]),
    };
    let mut guard = repo.lock_exclusive();
    stage::discard_selection(
        repo,
        guard.write_permission(),
        &t.op_ctx(CancelToken::new(), Arc::new(NoPrompter)),
        &req,
    )
    .expect("the hunk can be discarded");
}

/// A named input pair with the algorithm its diff is displayed with.
type OwnedInput = (String, Vec<u8>, Vec<u8>, Option<DiffAlgorithm>);

/// The discard direction of the oracle: discarding the displayed hunk *i* leaves exactly the worktree bytes that
/// `git apply -R` of Git's patch for that hunk leaves (no filters, so the direct worktree edit must restore the index
/// lines byte for byte), for the fixtures and the tricky inputs alike.
#[test]
fn oracle_discard_each_hunk_equals_git_apply_reverse() {
    let env = TestEnv::new();
    let mut inputs: Vec<OwnedInput> = FIXTURES
        .iter()
        .map(|name| {
            let (old, new) = fixture(name);
            (name.to_string(), old, new, None)
        })
        .collect();
    inputs.extend(
        tricky_inputs()
            .into_iter()
            .map(|(name, old, new, algorithm)| {
                (name.to_owned(), old.to_vec(), new.to_vec(), algorithm)
            }),
    );
    let mut compared = 0;
    for (name, old, new, algorithm) in &inputs {
        let t = env.repo(&format!("discard-{name}"));
        t.write("f", old);
        t.commit_all("base");
        t.write("f", new);
        let repo = t.open();
        let diff = our_diff_with(&repo, *algorithm);
        assert!(!diff.hunks.is_empty(), "{name}: the input has changes");
        for (i, hunk) in diff.hunks.iter().enumerate() {
            let patch = env.path(&format!("discard-{name}-{i}.patch"));
            std::fs::write(&patch, render_patch(hunk, old, new)).expect("patch can be written");
            t.git(&[
                "apply",
                "-R",
                "--whitespace=nowarn",
                patch.to_str().expect("temp paths are UTF-8"),
            ]);
            let expected = t.read("f");
            t.write("f", new);

            discard_with(&t, &repo, &diff, *algorithm, hunk.header);
            assert_eq!(
                t.read("f").as_bstr(),
                expected.as_bstr(),
                "{name}, hunk {i} {:?}: discarding the displayed hunk equals git apply -R",
                hunk.header
            );
            assert_eq!(
                index_id(&t),
                diff.base_old.expect("the file is tracked"),
                "{name}: discarding never touches the index"
            );
            t.write("f", new);
            compared += 1;
        }
    }
    assert!(compared >= inputs.len(), "every input contributed hunks");
}
