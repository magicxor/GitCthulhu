//! Tests of `blame::blame` (spec §2.5).

mod support;

use std::{sync::Mutex, time::Duration};

use gitcthulhu_git::{
    BlameOptions, BlameResult, BlameRev, CancelToken, GitErrorKind, RelaPath, Repo, blame::blame,
};
use gix::ObjectId;
use support::{TestEnv, TestRepo};

/// Serialises the blames of this binary so the `computations()` counter can be asserted on.
static SERIAL: Mutex<()> = Mutex::new(());

fn options() -> BlameOptions {
    BlameOptions {
        follow_renames: true,
        ..Default::default()
    }
}

fn run(repo: &Repo, path: &str, rev: BlameRev, opts: &BlameOptions) -> BlameResult {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    blame(repo, &RelaPath::from(path), rev, opts, &CancelToken::new()).expect("the blame succeeds")
}

fn run_err(repo: &Repo, path: &str, rev: BlameRev) -> GitErrorKind {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    blame(
        repo,
        &RelaPath::from(path),
        rev,
        &options(),
        &CancelToken::new(),
    )
    .expect_err("the blame fails")
    .kind()
}

/// `(start_line, len, commit id)` of every chunk.
fn attribution(result: &BlameResult) -> Vec<(u32, u32, Option<ObjectId>)> {
    result
        .chunks
        .iter()
        .map(|chunk| {
            (
                chunk.start_line,
                chunk.len,
                chunk.commit.map(|index| result.commits[index as usize].id),
            )
        })
        .collect()
}

fn head(fixture: &TestRepo) -> ObjectId {
    ObjectId::from_hex(fixture.git(&["rev-parse", "HEAD"]).as_bytes()).expect("a hex id")
}

#[test]
fn blame_attributes_lines_to_commits() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\ntwo\nthree\n");
    let first = fixture.commit_all("first");
    fixture.write("a.txt", "one\nTWO\nthree\n");
    let second = fixture.commit_all("second");
    fixture.write("a.txt", "one\nTWO\nthree\nfour\n");
    let third = fixture.commit_all("third");

    let repo = fixture.open();
    let result = run(&repo, "a.txt", BlameRev::Commit(third), &options());
    assert_eq!(
        attribution(&result),
        [
            (0, 1, Some(first)),
            (1, 1, Some(second)),
            (2, 1, Some(first)),
            (3, 1, Some(third)),
        ],
        "every line is attributed to the commit that introduced it"
    );
    assert_eq!(
        result.text, "oneTWOthreefour",
        "the decoded text has no line endings"
    );
    assert_eq!(
        result.lines,
        [0..3, 3..6, 6..11, 11..15],
        "byte range per line"
    );
    let commit = &result.commits[result.chunks[1].commit.expect("committed") as usize];
    assert_eq!(&commit.summary, "second", "the summary of the commit");
    assert_eq!(
        commit.author.person.name.to_string(),
        "Test User",
        "the author of the commit"
    );
    assert_eq!(
        commit.source_path.to_string(),
        "a.txt",
        "no rename involved"
    );
    assert_eq!(
        result.rev,
        BlameRev::Commit(third),
        "the result names what was blamed"
    );

    let at_first = run(&repo, "a.txt", BlameRev::Commit(first), &options());
    assert_eq!(
        attribution(&at_first),
        [(0, 3, Some(first))],
        "an older revision blames only what existed then"
    );
}

#[test]
fn blame_follows_rename() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("old.txt", "alpha\nbeta\ngamma\ndelta\nepsilon\n");
    let add = fixture.commit_all("add old");
    fixture.git(&["mv", "old.txt", "new.txt"]);
    let rename = fixture.commit_all("rename");
    fixture.write("new.txt", "alpha\nbeta\nGAMMA\ndelta\nepsilon\n");
    let edit = fixture.commit_all("edit");

    let repo = fixture.open();
    let followed = run(&repo, "new.txt", BlameRev::Commit(edit), &options());
    assert_eq!(
        attribution(&followed),
        [(0, 2, Some(add)), (2, 1, Some(edit)), (3, 2, Some(add))],
        "unchanged lines go back to the commit that wrote them under the old name"
    );
    let old = &followed.commits[followed.chunks[0].commit.expect("committed") as usize];
    assert_eq!(
        old.source_path.to_string(),
        "old.txt",
        "the path the lines had in that commit"
    );

    let not_followed = run(
        &repo,
        "new.txt",
        BlameRev::Commit(edit),
        &BlameOptions {
            follow_renames: false,
            ..Default::default()
        },
    );
    assert_eq!(
        attribution(&not_followed),
        [
            (0, 2, Some(rename)),
            (2, 1, Some(edit)),
            (3, 2, Some(rename))
        ],
        "without rename tracking the renaming commit owns the lines"
    );
}

#[test]
fn worktree_blame_marks_uncommitted_lines() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\ntwo\nthree\n");
    let first = fixture.commit_all("first");
    fixture.write("a.txt", "one\ntwo\nlocal edit\nthree\n");

    let repo = fixture.open();
    let result = run(&repo, "a.txt", BlameRev::Worktree, &options());
    assert_eq!(
        attribution(&result),
        [(0, 2, Some(first)), (2, 1, None), (3, 1, Some(first))],
        "the edited line is not committed yet, the rest still belongs to the first commit"
    );
    assert_eq!(
        result.text, "onetwolocal editthree",
        "the text is the worktree content"
    );
}

#[test]
fn worktree_blame_converts_crlf_like_git() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    // An attribute rather than `core.autocrlf`: the test isolation pins `core.autocrlf=false` for gix.
    fixture.write(".gitattributes", "*.txt text eol=crlf\n");
    fixture.write("a.txt", "one\r\ntwo\r\nthree\r\n");
    let first = fixture.commit_all("first");
    assert_eq!(
        fixture.index_blob("a.txt"),
        b"one\ntwo\nthree\n",
        "precondition: Git stores LF"
    );
    fixture.write("a.txt", "one\r\nTWO\r\nthree\r\n");

    let repo = fixture.open();
    let result = run(&repo, "a.txt", BlameRev::Worktree, &options());
    assert_eq!(
        attribution(&result),
        [(0, 1, Some(first)), (1, 1, None), (2, 1, Some(first))],
        "CRLF line endings in the worktree are not changes: only the edited line is uncommitted"
    );
    assert_eq!(result.text, "oneTWOthree", "no line endings in the text");
}

#[test]
fn untracked_file_all_uncommitted() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("tracked.txt", "x\n");
    fixture.commit_all("first");
    fixture.write("new.txt", "a\nb\nc\n");

    let repo = fixture.open();
    let result = run(&repo, "new.txt", BlameRev::Worktree, &options());
    assert_eq!(
        attribution(&result),
        [(0, 3, None)],
        "an untracked file is one uncommitted chunk"
    );
    assert!(result.commits.is_empty(), "no commit is referenced");
}

#[test]
fn untracked_file_in_unborn_repository() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("new.txt", "a\nb\n");

    let repo = fixture.open();
    let result = run(&repo, "new.txt", BlameRev::Worktree, &options());
    assert_eq!(
        attribution(&result),
        [(0, 2, None)],
        "without any commit all lines are uncommitted"
    );
}

#[test]
fn missing_file_not_found() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    let first = fixture.commit_all("first");
    fixture.remove("a.txt");

    let repo = fixture.open();
    assert_eq!(
        run_err(&repo, "missing.txt", BlameRev::Commit(first)),
        GitErrorKind::NotFound,
        "a path that is not in the commit"
    );
    assert_eq!(
        run_err(&repo, "a.txt", BlameRev::Worktree),
        GitErrorKind::NotFound,
        "a path that is gone from the worktree"
    );
    assert_eq!(
        run_err(&repo, "nothing/anywhere.txt", BlameRev::Worktree),
        GitErrorKind::NotFound,
        "a path that is nowhere"
    );
    let result = run(&repo, "a.txt", BlameRev::Commit(first), &options());
    assert_eq!(
        attribution(&result),
        [(0, 1, Some(first))],
        "the committed version can still be blamed"
    );
}

#[test]
fn shallow_clone_blames_the_boundary_commit() {
    let env = TestEnv::new();
    let origin = env.repo("origin");
    origin.write("a.txt", "one\ntwo\n");
    origin.commit_all("first");
    origin.write("a.txt", "one\nTWO\n");
    origin.commit_all("second");
    origin.write("a.txt", "one\nTWO\nthree\n");
    origin.commit_all("third");
    let url = format!(
        "file:///{}",
        origin
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    let clone = TestRepo::init_with(
        &env,
        &env.path("shallow"),
        &["clone", "-q", "--depth", "2", &url, "."],
    );
    let tip = head(&clone);
    let boundary =
        ObjectId::from_hex(clone.git(&["rev-parse", "HEAD~1"]).as_bytes()).expect("a hex id");

    let repo = clone.open();
    let result = run(&repo, "a.txt", BlameRev::Commit(tip), &options());
    assert_eq!(
        attribution(&result),
        [(0, 2, Some(boundary)), (2, 1, Some(tip))],
        "lines older than the boundary belong to the boundary commit, like `git blame` in a shallow clone"
    );
}

#[test]
fn binary_file_is_unsupported() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("blob.bin", b"\0\x01\x02binary\n");
    let first = fixture.commit_all("binary");
    let repo = fixture.open();
    assert_eq!(
        run_err(&repo, "blob.bin", BlameRev::Commit(first)),
        GitErrorKind::Unsupported,
        "binary content cannot be blamed"
    );
}

#[test]
fn line_range_limits_work() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let text = |n: usize| (0..n).map(|i| format!("line {i}\n")).collect::<String>();
    fixture.write("a.txt", text(6));
    let first = fixture.commit_all("first");
    fixture.write("a.txt", text(6).replace("line 4\n", "changed 4\n"));
    let second = fixture.commit_all("second");

    let repo = fixture.open();
    let limited = run(
        &repo,
        "a.txt",
        BlameRev::Commit(second),
        &BlameOptions {
            follow_renames: true,
            line_range: Some(3..6),
            ..Default::default()
        },
    );
    assert_eq!(
        attribution(&limited),
        [
            (3, 1, Some(first)),
            (4, 1, Some(second)),
            (5, 1, Some(first))
        ],
        "only the requested lines are attributed"
    );
    assert_eq!(limited.lines.len(), 6, "the text still has every line");

    let beyond = run(
        &repo,
        "a.txt",
        BlameRev::Commit(second),
        &BlameOptions {
            follow_renames: true,
            line_range: Some(10..20),
            ..Default::default()
        },
    );
    assert!(
        beyond.chunks.is_empty(),
        "a range past the end attributes nothing"
    );
}

#[test]
fn cached_result_reused() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\ntwo\n");
    let first = fixture.commit_all("first");
    let repo = fixture.open();
    let blame_it = |opts: &BlameOptions| {
        blame(
            &repo,
            &RelaPath::from("a.txt"),
            BlameRev::Commit(first),
            opts,
            &CancelToken::new(),
        )
        .expect("the blame succeeds")
    };

    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = gitcthulhu_git::blame::computations();
    let one = blame_it(&options());
    let two = blame_it(&options());
    assert_eq!(
        gitcthulhu_git::blame::computations() - before,
        1,
        "the second request is served from the cache"
    );
    assert_eq!(attribution(&one), attribution(&two), "same result");

    blame_it(&BlameOptions {
        follow_renames: false,
        ..Default::default()
    });
    assert_eq!(
        gitcthulhu_git::blame::computations() - before,
        2,
        "different options are a different key"
    );

    fixture.write("a.txt", "one\ntwo\nthree\n");
    let second = fixture.commit_all("second");
    blame(
        &repo,
        &RelaPath::from("a.txt"),
        BlameRev::Commit(second),
        &options(),
        &CancelToken::new(),
    )
    .expect("the blame succeeds");
    assert_eq!(
        gitcthulhu_git::blame::computations() - before,
        3,
        "a different commit is computed again"
    );

    fixture.write("a.txt", "uncommitted\n");
    blame(
        &repo,
        &RelaPath::from("a.txt"),
        BlameRev::Worktree,
        &options(),
        &CancelToken::new(),
    )
    .expect("the blame succeeds");
    blame(
        &repo,
        &RelaPath::from("a.txt"),
        BlameRev::Worktree,
        &options(),
        &CancelToken::new(),
    )
    .expect("the blame succeeds");
    assert_eq!(
        gitcthulhu_git::blame::computations() - before,
        5,
        "worktree blames are never cached"
    );
}

/// A repository whose `big.txt` has `lines` lines and `commits` commits that each change a few of them.
fn long_history(fixture: &TestRepo, lines: usize, commits: usize) {
    use std::fmt::Write as _;
    let mut stream = String::new();
    let mut time = 1_700_000_000u64;
    for commit in 0..commits {
        time += 60;
        let content: String = (0..lines)
            .map(|line| {
                let version = if line % 97 == commit % 97 { commit } else { 0 };
                format!("line {line} version {version}\n")
            })
            .collect();
        writeln!(
            stream,
            "commit refs/heads/main
mark :{}",
            commit + 1
        )
        .expect("string write");
        writeln!(
            stream,
            "author T <t@example.com> {time} +0000\ncommitter T <t@example.com> {time} +0000"
        )
        .expect("string write");
        let message = format!("commit {commit}");
        writeln!(stream, "data {}\n{message}", message.len()).expect("string write");
        if commit > 0 {
            writeln!(stream, "from :{commit}").expect("string write");
        }
        writeln!(stream, "M 100644 inline big.txt\ndata {}", content.len()).expect("string write");
        stream.push_str(&content);
        stream.push('\n');
    }
    fixture.fast_import(stream.as_bytes());
}

#[test]
fn cancel_returns_promptly() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    long_history(&fixture, 1500, 300);
    let tip = head_of_main(&fixture);
    let repo = fixture.open();
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let cancelled = CancelToken::new();
    cancelled.cancel();
    let started = std::time::Instant::now();
    let err = blame(
        &repo,
        &RelaPath::from("big.txt"),
        BlameRev::Commit(tip),
        &options(),
        &cancelled,
    )
    .expect_err("a token cancelled up front refuses the work");
    assert_eq!(err.kind(), GitErrorKind::Cancelled, "cancelled: {err:?}");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "returns at once: {:?}",
        started.elapsed()
    );

    // The computation takes most of a second in a debug build (gix blame has no interrupt flag); a cancel 20 ms in
    // must not wait for it.
    let token = CancelToken::new();
    let canceller = {
        let token = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            token.cancel();
        })
    };
    let started = std::time::Instant::now();
    let outcome = blame(
        &repo,
        &RelaPath::from("big.txt"),
        BlameRev::Commit(tip),
        &BlameOptions {
            // A distinct key, so the cache cannot answer.
            line_range: Some(0..1999),
            ..options()
        },
        &token,
    );
    let elapsed = started.elapsed();
    canceller.join().expect("the canceller finishes");
    match outcome {
        Err(err) => {
            assert_eq!(err.kind(), GitErrorKind::Cancelled, "cancelled: {err:?}");
            assert!(
                elapsed < Duration::from_millis(400),
                "the call returns right after the cancel, without waiting for the computation: {elapsed:?}"
            );
        }
        // An optimised build can finish the whole blame before the cancel arrives; a debug build takes far longer
        // than 20 ms, so there the cancel must have won.
        Ok(_) => {
            let optimised = !cfg!(debug_assertions);
            assert!(
                optimised,
                "a debug build cannot finish this blame in 20 ms ({elapsed:?}), so the cancel must have won"
            );
        }
    }
}

fn head_of_main(fixture: &TestRepo) -> ObjectId {
    fixture.git(&["reset", "-q", "--hard", "main"]);
    head(fixture)
}
