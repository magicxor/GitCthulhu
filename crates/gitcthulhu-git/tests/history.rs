//! Tests of `history::FileHistoryWalker` (spec §2.5): Git's history simplification, rename following, paging.

mod support;

use gitcthulhu_git::{CancelToken, ChangeKind, FileHistoryEntry, FileHistoryWalker, GitErrorKind};
use gix::ObjectId;
use support::{TestEnv, TestRepo};

const BODY: &str = "line one of the file\nline two of the file\nline three of the file\nline four of the file\nline five of the file\n";

/// The whole history of `path` as entries.
fn history(fixture: &TestRepo, path: &str, follow: bool) -> Vec<FileHistoryEntry> {
    let repo = fixture.open();
    let mut walker =
        FileHistoryWalker::new(&repo, &path.into(), None, follow).expect("a walker can be created");
    let page = walker
        .next_page(100_000, &CancelToken::new())
        .expect("the history can be walked");
    assert!(page.exhausted, "everything was requested");
    page.entries
}

fn ids(entries: &[FileHistoryEntry]) -> Vec<ObjectId> {
    entries.iter().map(|entry| entry.commit.id).collect()
}

/// `git log` as the oracle: the commits that touch `path`.
fn git_log(fixture: &TestRepo, path: &str, follow: bool) -> Vec<ObjectId> {
    let mut args = vec!["log", "--format=%H"];
    if follow {
        args.push("--follow");
    }
    args.extend(["--", path]);
    fixture
        .git(&args)
        .lines()
        .map(|line| ObjectId::from_hex(line.trim().as_bytes()).expect("log prints hex ids"))
        .collect()
}

fn kinds(entries: &[FileHistoryEntry]) -> Vec<ChangeKind> {
    entries.iter().map(|entry| entry.kind).collect()
}

#[test]
fn history_lists_only_touching_commits() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    let add_a = fixture.commit_all("add a");
    fixture.write("b.txt", "b\n");
    fixture.commit_all("add b");
    fixture.write("a.txt", "two\n");
    let change_a = fixture.commit_all("change a");
    fixture.write("c.txt", "c\n");
    fixture.commit_all("add c");

    let entries = history(&fixture, "a.txt", false);
    assert_eq!(
        ids(&entries),
        [change_a, add_a],
        "only the commits that touch a.txt, newest first"
    );
    assert_eq!(
        kinds(&entries),
        [ChangeKind::Modified, ChangeKind::Added],
        "kinds of the changes"
    );
    assert_eq!(
        &*entries[0].commit.summary, "change a",
        "the commit summary is filled in"
    );
    assert_eq!(
        ids(&entries),
        git_log(&fixture, "a.txt", false),
        "same as git log"
    );
}

#[test]
fn merge_treesame_side_not_listed() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "base\n");
    let base = fixture.commit_all("base");
    fixture.git(&["switch", "-q", "-c", "side"]);
    fixture.write("a.txt", "side\n");
    let side = fixture.commit_all("side change");
    fixture.git(&["switch", "-q", "main"]);
    fixture.write("a.txt", "main\n");
    let main_change = fixture.commit_all("main change");
    // Resolve the conflict by taking main's version: the merge is TREESAME to its first parent.
    let _ = fixture.git_fail(&["merge", "side"]);
    fixture.write("a.txt", "main\n");
    fixture.git(&["add", "a.txt"]);
    fixture.git(&["commit", "-q", "-m", "merge side, keep main"]);

    let entries = history(&fixture, "a.txt", false);
    assert_eq!(
        ids(&entries),
        [main_change, base],
        "the merge is TREESAME to its first parent, so the side branch is not followed"
    );
    assert!(
        !ids(&entries).contains(&side),
        "the side commit is hidden by the simplification"
    );
    assert_eq!(
        ids(&entries),
        git_log(&fixture, "a.txt", false),
        "same as git log"
    );
}

#[test]
fn follow_rename_continues_with_old_path() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("old.txt", BODY);
    let add = fixture.commit_all("add old");
    fixture.write("old.txt", format!("{BODY}six\n"));
    let edit_old = fixture.commit_all("edit old");
    fixture.git(&["mv", "old.txt", "new.txt"]);
    let rename = fixture.commit_all("rename");
    fixture.write("new.txt", format!("{BODY}six\nseven\n"));
    let edit_new = fixture.commit_all("edit new");

    let entries = history(&fixture, "new.txt", true);
    assert_eq!(
        ids(&entries),
        [edit_new, rename, edit_old, add],
        "the history continues under the old name"
    );
    assert_eq!(
        kinds(&entries),
        [
            ChangeKind::Modified,
            ChangeKind::Renamed,
            ChangeKind::Modified,
            ChangeKind::Added
        ],
        "the rename is reported as such"
    );
    assert_eq!(
        entries[1].previous_path.as_ref().map(|p| p.to_string()),
        Some("old.txt".to_owned()),
        "the rename names its source"
    );
    assert_eq!(
        entries[2].path.to_string(),
        "old.txt",
        "older entries carry the path of their time"
    );
    assert_eq!(
        ids(&entries),
        git_log(&fixture, "new.txt", true),
        "same as git log --follow"
    );
}

#[test]
fn no_follow_stops_at_add() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("old.txt", BODY);
    fixture.commit_all("add old");
    fixture.git(&["mv", "old.txt", "new.txt"]);
    let rename = fixture.commit_all("rename");
    fixture.write("new.txt", format!("{BODY}six\n"));
    let edit = fixture.commit_all("edit new");

    let entries = history(&fixture, "new.txt", false);
    assert_eq!(
        ids(&entries),
        [edit, rename],
        "without follow the history ends where the path appears"
    );
    assert_eq!(
        kinds(&entries),
        [ChangeKind::Modified, ChangeKind::Added],
        "the rename is an addition of the new path"
    );
    assert!(
        entries[1].previous_path.is_none(),
        "no previous path without follow"
    );
}

#[test]
fn deletion_listed() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    let add = fixture.commit_all("add");
    fixture.write("b.txt", "other\n");
    fixture.commit_all("other");
    fixture.remove("a.txt");
    let delete = fixture.commit_all("delete");

    let entries = history(&fixture, "a.txt", false);
    assert_eq!(ids(&entries), [delete, add], "deletion and addition");
    assert_eq!(
        kinds(&entries),
        [ChangeKind::Deleted, ChangeKind::Added],
        "the deletion is listed as such"
    );
    assert_eq!(
        ids(&entries),
        git_log(&fixture, "a.txt", false),
        "same as git log"
    );
}

#[test]
fn paging_equals_full() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    for i in 0..9 {
        fixture.write("a.txt", format!("version {i}\n"));
        fixture.write("noise.txt", format!("noise {i}\n"));
        fixture.commit_all(&format!("change {i}"));
        fixture.write("noise.txt", format!("more noise {i}\n"));
        fixture.commit_all(&format!("noise {i}"));
    }
    let full = history(&fixture, "a.txt", false);
    assert_eq!(full.len(), 9, "nine commits touch a.txt");

    let repo = fixture.open();
    let mut walker = FileHistoryWalker::new(&repo, &"a.txt".into(), None, false)
        .expect("a walker can be created");
    let cancel = CancelToken::new();
    let mut paged = Vec::new();
    let mut pages = 0;
    loop {
        let page = walker.next_page(2, &cancel).expect("a page can be read");
        assert!(page.entries.len() <= 2, "a page has at most n entries");
        paged.extend(page.entries);
        pages += 1;
        if page.exhausted {
            break;
        }
        assert!(pages < 100, "paging terminates");
    }
    assert_eq!(
        ids(&paged),
        ids(&full),
        "pages concatenate to the full history"
    );
    assert!(pages >= 5, "nine entries in pages of two need five pages");
}

#[test]
fn matches_git_log_follow_on_fixture() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("src/lib.txt", BODY);
    fixture.commit_all("add lib");
    fixture.write("src/lib.txt", format!("{BODY}a\n"));
    fixture.commit_all("edit lib");
    fixture.git(&["switch", "-q", "-c", "feature"]);
    fixture.write("src/lib.txt", format!("{BODY}a\nfeature\n"));
    fixture.commit_all("feature edit");
    fixture.write("other.txt", "unrelated\n");
    fixture.commit_all("feature unrelated");
    fixture.git(&["switch", "-q", "main"]);
    fixture.git(&["mv", "src/lib.txt", "src/core.txt"]);
    fixture.commit_all("move lib to core");
    fixture.write("src/core.txt", format!("{BODY}a\nmain\n"));
    fixture.commit_all("main edit");
    fixture.git(&["mv", "src/core.txt", "engine.txt"]);
    fixture.commit_all("move core to engine");
    fixture.write("engine.txt", format!("{BODY}a\nmain\nengine\n"));
    fixture.commit_all("engine edit");

    for follow in [true, false] {
        let entries = history(&fixture, "engine.txt", follow);
        assert_eq!(
            ids(&entries),
            git_log(&fixture, "engine.txt", follow),
            "history of engine.txt matches git log (follow = {follow})"
        );
    }
}

#[test]
fn history_merge_diamond_no_duplicates() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "base\n");
    fixture.write("b.txt", "base\n");
    fixture.commit_all("base");
    fixture.git(&["switch", "-q", "-c", "left"]);
    fixture.write("a.txt", "left\nbase\n");
    fixture.commit_all("left");
    fixture.git(&["switch", "-q", "main"]);
    fixture.git(&["switch", "-q", "-c", "right"]);
    fixture.write("a.txt", "base\nright\n");
    fixture.commit_all("right");
    fixture.git(&["merge", "-q", "--no-edit", "left"]);
    fixture.git(&["switch", "-q", "main"]);
    fixture.git(&["merge", "-q", "--no-edit", "right"]);
    fixture.git(&["merge", "-q", "--no-edit", "left"]);

    let entries = history(&fixture, "a.txt", false);
    let found = ids(&entries);
    let mut unique = found.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(found.len(), unique.len(), "no commit is listed twice");
    let mut expected = git_log(&fixture, "a.txt", false);
    let mut sorted_found = found;
    sorted_found.sort();
    expected.sort();
    assert_eq!(sorted_found, expected, "the same commits as git log");
}

/// A fast-import stream of `diamonds` nested merge diamonds that all rewrite `a.txt`, so that neither side of a
/// merge is TREESAME to the merge and both parents are walked.
fn nested_diamonds(diamonds: usize) -> (Vec<u8>, usize) {
    use std::fmt::Write as _;
    let mut stream = String::new();
    let mut mark = 0usize;
    let mut time = 1_700_000_000u64;
    let mut commit = |stream: &mut String,
                      parents: &[usize],
                      file: &str,
                      content: &str,
                      message: &str|
     -> usize {
        mark += 1;
        time += 60;
        writeln!(stream, "commit refs/heads/main\nmark :{mark}").expect("string write");
        writeln!(
            stream,
            "author T <t@example.com> {time} +0000\ncommitter T <t@example.com> {time} +0000"
        )
        .expect("string write");
        writeln!(stream, "data {}\n{message}", message.len()).expect("string write");
        if let Some(first) = parents.first() {
            writeln!(stream, "from :{first}").expect("string write");
        }
        for other in parents.iter().skip(1) {
            writeln!(stream, "merge :{other}").expect("string write");
        }
        writeln!(
            stream,
            "M 100644 inline {file}\ndata {}\n{content}",
            content.len()
        )
        .expect("string write");
        mark
    };
    let mut tip = commit(&mut stream, &[], "a.txt", "root\n", "root");
    let mut count = 1;
    for i in 0..diamonds {
        let side = commit(
            &mut stream,
            &[tip],
            "side.txt",
            &format!("side {i}\n"),
            "side",
        );
        let main = commit(&mut stream, &[tip], "a.txt", &format!("main {i}\n"), "main");
        tip = commit(
            &mut stream,
            &[main, side],
            "a.txt",
            &format!("merged {i}\n"),
            "merge",
        );
        count += 3;
    }
    (stream.into_bytes(), count)
}

#[test]
fn history_100_nested_merges_linear_time() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let (stream, commits) = nested_diamonds(100);
    fixture.fast_import(&stream);
    fixture.git(&["reset", "-q", "--hard", "main"]);

    let repo = fixture.open();
    let started = std::time::Instant::now();
    let mut walker = FileHistoryWalker::new(&repo, &"a.txt".into(), None, false)
        .expect("a walker can be created");
    let page = walker
        .next_page(usize::MAX, &CancelToken::new())
        .expect("the history can be walked");
    assert!(page.exhausted, "the walk finished");
    assert_eq!(
        walker.visited_commits(),
        commits,
        "every commit is visited exactly once, not once per merge path"
    );
    let found = ids(&page.entries);
    let mut unique = found.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(found.len(), unique.len(), "no duplicates");
    assert_eq!(
        found.len(),
        git_log(&fixture, "a.txt", false).len(),
        "as many entries as git log"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "linear time, not exponential: {:?}",
        started.elapsed()
    );
}

#[test]
fn shallow_clone_history_ends_at_the_boundary() {
    let env = TestEnv::new();
    let origin = env.repo("origin");
    for i in 0..3 {
        origin.write("a.txt", format!("version {i}\n"));
        origin.commit_all(&format!("change {i}"));
    }
    let url = format!(
        "file:///{}",
        origin
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    let clone_dir = env.path("shallow");
    let clone = TestRepo::init_with(
        &env,
        &clone_dir,
        &["clone", "-q", "--depth", "2", &url, "."],
    );
    assert!(
        clone_dir.join(".git/shallow").is_file(),
        "the clone is shallow"
    );

    for follow in [false, true] {
        let entries = history(&clone, "a.txt", follow);
        assert_eq!(
            ids(&entries),
            git_log(&clone, "a.txt", follow),
            "the walk stops at the shallow boundary like git log (follow = {follow})"
        );
        assert_eq!(
            kinds(&entries),
            [ChangeKind::Modified, ChangeKind::Added],
            "the boundary commit has no parents, so it adds the file (follow = {follow})"
        );
    }
}

#[test]
fn cancelled_walk_is_reported() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("add");
    let repo = fixture.open();
    let mut walker = FileHistoryWalker::new(&repo, &"a.txt".into(), None, false)
        .expect("a walker can be created");
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = walker
        .next_page(10, &cancel)
        .expect_err("a cancelled token stops the walk");
    assert_eq!(err.kind(), GitErrorKind::Cancelled, "cancelled: {err:?}");
}

#[test]
fn unborn_head_is_an_empty_history() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let repo = fixture.open();
    let mut walker = FileHistoryWalker::new(&repo, &"a.txt".into(), None, true)
        .expect("a walker can be created");
    let page = walker
        .next_page(10, &CancelToken::new())
        .expect("an empty walk can be paged");
    assert!(page.entries.is_empty(), "no commits, no entries");
    assert!(page.exhausted, "and nothing more to come");
}

#[test]
fn unknown_start_commit_is_not_found() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.commit_all("add");
    let repo = fixture.open();
    let missing =
        ObjectId::from_hex(b"1111111111111111111111111111111111111111").expect("valid hex");
    let Err(err) = FileHistoryWalker::new(&repo, &"a.txt".into(), Some(missing), false) else {
        panic!("a missing start commit must be refused");
    };
    assert_eq!(err.kind(), GitErrorKind::NotFound, "not found: {err:?}");
}
