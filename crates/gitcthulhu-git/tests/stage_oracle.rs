//! Differential test of the in-process staging engine against Git (spec 20 §4.1, §4.8).
//!
//! For every fixture pair in `tests/fixtures/oracle/` (LF, CRLF, missing final newlines, Latin-1, repeated lines,
//! adjacent hunks) and every hunk *i* of our own diff, hunk *i* is rendered as a git patch from the raw bytes and
//! applied with `git apply --cached --whitespace=nowarn`; the resulting index blob must equal the one
//! `stage::stage_selection(Hunks([i]))` writes. Staging every hunk (and every line) must equal `git add`.
//!
//! The hunks come from `diff::file_diff` (WP-B2). Until that package is merged, the test derives them with the same
//! algorithm the shared line engine uses (§4.3: gix line diff with slider heuristics, Git's hunk grouping with three
//! context lines); with the real diff available the fallback is no longer taken.

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, DiffContent, DiffOptions, DiffTarget, GitErrorKind, HunkHeader, LineKind, LineRef,
    NoPrompter, Repo, Selection, SelectionRequest, diff, stage,
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

fn our_diff(t: &TestRepo, repo: &Repo, old: &[u8], new: &[u8]) -> OurDiff {
    let target = DiffTarget::Unstaged {
        path: "f".into(),
        previous_path: None,
    };
    let opts = DiffOptions {
        context_lines: CONTEXT_LINES,
        ..DiffOptions::default()
    };
    match diff::file_diff(repo, &target, &opts, &CancelToken::new()) {
        Ok(file_diff) => {
            let DiffContent::Text(text) = &file_diff.content else {
                panic!("fixtures are text, got {:?}", file_diff.content);
            };
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
        Err(err) if err.kind() == GitErrorKind::NotImplemented => OurDiff {
            hunks: engine::hunks(old, new, CONTEXT_LINES),
            base_old: Some(oid(&t.git(&["rev-parse", ":f"]))),
            base_new: Some(oid(&t.git(&["hash-object", "--", "f"]))),
        },
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
    let req = SelectionRequest {
        path: "f".into(),
        previous_path: None,
        base_old: diff.base_old,
        base_new: diff.base_new,
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
        let diff = our_diff(&t, &repo, &old, &new);
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
        let (t, old, new) = setup(&env, name);
        let repo = t.open();
        let diff = our_diff(&t, &repo, &old, &new);
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

/// Test-side hunk computation used while `diff::file_diff` is not available: the algorithm of §4.3.
mod engine {
    use gix::diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};

    use super::{Hunk, Line};
    use gitcthulhu_git::{HunkHeader, LineKind};

    pub fn hunks(old: &[u8], new: &[u8], context: u32) -> Vec<Hunk> {
        let old_lines = super::raw_lines(old);
        let new_lines = super::raw_lines(new);
        let mut input: InternedInput<&[u8]> = InternedInput::default();
        input.update_before(old_lines.iter().copied());
        input.update_after(new_lines.iter().copied());
        let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
        let changes: Vec<_> = diff.hunks().collect();
        let (old_len, new_len) = (old_lines.len() as u32, new_lines.len() as u32);

        // Git's grouping: changes whose gap of equal lines is at most 2 * context share a hunk.
        let mut groups: Vec<Vec<gix::diff::blob::Hunk>> = Vec::new();
        for change in changes {
            match groups.last_mut() {
                Some(group)
                    if change.before.start
                        - group.last().expect("groups are never empty").before.end
                        <= 2 * context =>
                {
                    group.push(change)
                }
                _ => groups.push(vec![change]),
            }
        }

        groups
            .into_iter()
            .map(|group| {
                let first = group.first().expect("groups are never empty");
                let last = group.last().expect("groups are never empty");
                let lead = first.before.start.min(context);
                let trail = (old_len - last.before.end).min(context);
                let (old_start, new_start) = (first.before.start - lead, first.after.start - lead);
                let (old_end, new_end) = (last.before.end + trail, last.after.end + trail);
                debug_assert!(new_end <= new_len, "both sides share the trailing context");

                let mut lines = Vec::new();
                let (mut i, mut j) = (old_start, new_start);
                for change in &group {
                    while i < change.before.start {
                        lines.push(Line {
                            kind: LineKind::Context,
                            old: Some(i + 1),
                            new: Some(j + 1),
                        });
                        i += 1;
                        j += 1;
                    }
                    lines.extend(change.before.clone().map(|o| Line {
                        kind: LineKind::Removed,
                        old: Some(o + 1),
                        new: None,
                    }));
                    lines.extend(change.after.clone().map(|n| Line {
                        kind: LineKind::Added,
                        old: None,
                        new: Some(n + 1),
                    }));
                    i = change.before.end;
                    j = change.after.end;
                }
                while i < old_end {
                    lines.push(Line {
                        kind: LineKind::Context,
                        old: Some(i + 1),
                        new: Some(j + 1),
                    });
                    i += 1;
                    j += 1;
                }
                let (old_count, new_count) = (old_end - old_start, new_end - new_start);
                // An empty side starts at the line before, like Git.
                let start = |s: u32, count: u32| if count == 0 { s } else { s + 1 };
                Hunk {
                    header: HunkHeader {
                        old_start: start(old_start, old_count),
                        old_lines: old_count,
                        new_start: start(new_start, new_count),
                        new_lines: new_count,
                    },
                    lines,
                }
            })
            .collect()
    }
}
