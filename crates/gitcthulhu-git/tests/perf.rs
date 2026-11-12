//! Performance tests of the read paths of spec §9 that have no test next to their module (the budgets of `status`,
//! graph layout, `file_diff` and `stage_selection` are measured in `status.rs`, `graph.rs`, `diff.rs`, `stage.rs`).
//!
//! All tests are `#[ignore]`: run them one at a time, in release, on a quiet machine:
//!
//! ```text
//! GITCTHULHU_PERF_ASSERT=1 cargo test -p gitcthulhu-git --release --test perf -- --ignored --exact perf_refs_list_10k --nocapture
//! ```
//!
//! Without `GITCTHULHU_PERF_ASSERT=1` they only print what they measured; with it, a budget that is missed fails the
//! test. The reference machine of §9 is Windows 11 with an NVMe SSD, 8 cores and a warm cache.
//!
//! The history sizes of the spec (1M commits) are too slow to generate in a routine run; the defaults below are
//! smaller and the budgets that depend on the size are scaled linearly. Override them to measure the full scale:
//! `GITCTHULHU_PERF_COMMITS` (log, default 100000), `GITCTHULHU_PERF_BLAME_COMMITS` (blame history, default 10000),
//! `GITCTHULHU_PERF_HISTORY_COMMITS` (file history, default 100000).

#[path = "support/graphs.rs"]
mod graphs;
mod support;

use std::{
    io::Write as _,
    time::{Duration, Instant},
};

use gitcthulhu_git::{
    BlameOptions, BlameRev, CancelToken, CommitFilesOptions, FileHistoryWalker, LogOptions,
    LogWalker, RefListOptions, Repo, blame, diff, refs,
};
use support::TestEnv;

fn asserting() -> bool {
    std::env::var_os("GITCTHULHU_PERF_ASSERT").is_some_and(|value| value == "1")
}

fn from_env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// The budgets a test has missed; a test prints every measurement first and fails at its end, so one slow path does
/// not hide the numbers of the others.
static MISSED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Print the measurement against its budget; a miss fails the test (in [`finish`]) if the budgets are asserted.
fn check(what: &str, measured: Duration, budget: Duration) {
    let missed = measured > budget;
    println!(
        "perf: {what}: {measured:?} (budget {budget:?}){}",
        if missed { "  ** OVER BUDGET **" } else { "" }
    );
    if missed {
        MISSED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(format!(
                "{what} took {measured:?}, the budget is {budget:?}"
            ));
    }
}

/// Fail if a budget was missed and `GITCTHULHU_PERF_ASSERT=1`.
fn finish() {
    let missed = std::mem::take(
        &mut *MISSED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    if asserting() {
        assert!(missed.is_empty(), "budgets missed:\n{}", missed.join("\n"));
    }
}

/// Print a measurement that has no budget.
fn report(what: &str, measured: Duration) {
    println!("perf: {what}: {measured:?} (no budget)");
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = f();
    (value, started.elapsed())
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

/// `budget` for a history of `commits` when the spec states it for `spec_commits`.
fn scaled(budget: Duration, commits: usize, spec_commits: usize) -> Duration {
    budget.mul_f64((commits as f64 / spec_commits as f64).max(1.0 / 8.0))
}

// ───────────────────────────── Repo::open ─────────────────────────────

#[test]
#[ignore = "performance: run in release with GITCTHULHU_PERF_ASSERT=1 to assert the budget"]
fn perf_repo_open() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.fast_import_with(|stdin| {
        let mut out = std::io::BufWriter::new(stdin);
        writeln!(
            out,
            "commit refs/heads/main\ncommitter C <c@example.com> 1600000000 +0000\ndata 5\nfiles"
        )?;
        for i in 0..5000 {
            let content = format!("{i}\n");
            writeln!(
                out,
                "M 100644 inline d{}/f{i}.txt\ndata {}\n{content}",
                i % 100,
                content.len()
            )?;
        }
        out.flush()
    });
    let samples: Vec<Duration> = (0..9)
        .map(|_| timed(|| Repo::open(&env.rt, &fixture.path).expect("the repository opens")).1)
        .collect();
    report("Repo::open, first open", samples[0]);
    check(
        "Repo::open (median of 9)",
        median(samples),
        Duration::from_millis(20),
    );
    finish();
}

// ───────────────────────────── log ─────────────────────────────

#[test]
#[ignore = "performance: run in release with GITCTHULHU_PERF_ASSERT=1 to assert the budget"]
fn perf_log_first_page_and_next_page() {
    let commits = from_env("GITCTHULHU_PERF_COMMITS", 100_000);
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let (_, generated) = timed(|| {
        fixture.fast_import(&graphs::branchy(commits, 20, 7).fast_import_stream());
    });
    report(&format!("importing {commits} commits"), generated);
    let repo = fixture.open();
    let cancel = CancelToken::new();
    let opts = LogOptions::default();

    // Without a commit-graph: the whole in-degree exploration happens in `new` (spec: 1M <= 6 s, 100k <= 600 ms).
    let ((walker, page), without_graph) = timed(|| {
        let mut walker = LogWalker::new(&repo, &opts, &cancel).expect("the walk starts");
        let page = walker
            .next_page(2000, &cancel)
            .expect("the first page loads");
        (walker, page)
    });
    assert_eq!(page.commits.len(), 2000, "a full first page");
    check(
        &format!("LogWalker first page (2000) of {commits} commits, no commit-graph"),
        without_graph,
        scaled(Duration::from_millis(600), commits, 100_000),
    );
    drop(walker);

    fixture.git(&["commit-graph", "write", "--reachable"]);
    let repo = fixture.open();
    let ((mut walker, page), first) = timed(|| {
        let mut walker = LogWalker::new(&repo, &opts, &cancel).expect("the walk starts");
        let page = walker
            .next_page(2000, &cancel)
            .expect("the first page loads");
        (walker, page)
    });
    assert_eq!(page.commits.len(), 2000, "a full first page");
    check(
        &format!("LogWalker first page (2000) of {commits} commits, with commit-graph"),
        first,
        Duration::from_millis(150),
    );

    let pages: Vec<Duration> = (0..10)
        .map(|_| {
            let (page, took) = timed(|| walker.next_page(500, &cancel).expect("a page loads"));
            assert_eq!(page.commits.len(), 500, "a full page");
            took
        })
        .collect();
    report(
        "LogWalker::next_page(500), median of 10",
        median(pages.clone()),
    );
    check(
        "LogWalker::next_page(500), slowest of 10",
        pages.into_iter().max().expect("ten samples"),
        Duration::from_millis(30),
    );
    finish();
}

/// A linear history is the worst case of the walk without a commit-graph: there is only one line to read, so the
/// threads of the prefetch cannot share the work (`branchy` above has up to 20 lines open at a time).
#[test]
#[ignore = "performance: run in release with GITCTHULHU_PERF_ASSERT=1 to assert the budget"]
fn perf_log_first_page_linear_history() {
    let commits = from_env("GITCTHULHU_PERF_COMMITS", 100_000);
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let (_, generated) =
        timed(|| fixture.fast_import(&graphs::linear(commits).fast_import_stream()));
    report(&format!("importing {commits} commits"), generated);
    let repo = fixture.open();
    let cancel = CancelToken::new();
    let samples: Vec<Duration> = (0..3)
        .map(|_| {
            let (page, took) = timed(|| {
                LogWalker::new(&repo, &LogOptions::default(), &cancel)
                    .expect("the walk starts")
                    .next_page(2000, &cancel)
                    .expect("the first page loads")
            });
            assert_eq!(page.commits.len(), 2000, "a full first page");
            took
        })
        .collect();
    check(
        &format!(
            "LogWalker first page (2000) of {commits} linear commits, no commit-graph (median of 3)"
        ),
        median(samples),
        scaled(Duration::from_millis(600), commits, 100_000),
    );
    finish();
}

// ───────────────────────────── refs ─────────────────────────────

#[test]
#[ignore = "performance: run in release with GITCTHULHU_PERF_ASSERT=1 to assert the budget"]
fn perf_refs_list_10k() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.fast_import_with(|stdin| {
        writeln!(
            stdin,
            "commit refs/heads/main\ncommitter C <c@example.com> 1600000000 +0000\ndata 4\none"
        )
    });
    let main = fixture.git(&["rev-parse", "refs/heads/main"]);
    // The loose files are written directly, as Git writes them: 10k `reset`s through fast-import take minutes on
    // Windows (a lock file and a rename per ref).
    let names: Vec<String> = (0..5000)
        .map(|i| format!("refs/heads/feature/b{i}"))
        .chain((0..5000).map(|i| format!("refs/tags/v{i}")))
        .collect();
    let ((), generated) = timed(|| {
        for name in &names {
            let path = fixture.path.join(".git").join(name);
            std::fs::create_dir_all(path.parent().expect("a ref has a directory"))
                .expect("writable");
            std::fs::write(path, format!("{main}\n")).expect("writable");
        }
    });
    report("writing the 10k loose ref files", generated);

    let cancel = CancelToken::new();
    // The log's tips are the same 10k refs, all at one commit: the walk costs nothing, listing the tips is measured.
    let log_tips = || {
        let repo = fixture.open();
        let samples: Vec<Duration> = (0..5)
            .map(|_| {
                timed(|| {
                    LogWalker::new(&repo, &LogOptions::default(), &cancel)
                        .expect("the walk starts")
                        .next_page(10, &cancel)
                        .expect("the page loads")
                })
                .1
            })
            .collect();
        median(samples)
    };
    let list = || {
        let repo = fixture.open();
        // A warm-up lets the file system cache settle; then the median of five.
        refs::list(&repo, RefListOptions::ALL).expect("the refs list");
        let samples: Vec<Duration> = (0..5)
            .map(|_| timed(|| refs::list(&repo, RefListOptions::ALL).expect("the refs list")).1)
            .collect();
        let list = refs::list(&repo, RefListOptions::ALL).expect("the refs list");
        assert_eq!(
            list.local.len() + list.tags.len(),
            10_001,
            "main, 5000 branches and 5000 tags"
        );
        median(samples)
    };

    // 10k loose refs are 10k file opens: on Windows 50-200 us each (on-access scanning included), so no listing of
    // them can meet the budget of the spec, which holds for the packed layout of a repository that ever ran `git gc`
    // or `git pack-refs`. The loose layout must instead beat the floor of reading the same files one after the other
    // with plain `std::fs::read` (the listing reads them on several threads).
    let loose_list = list();
    let (files, floor) = timed(|| {
        let mut files = 0;
        for name in &names {
            std::fs::read(fixture.path.join(".git").join(name)).expect("readable");
            files += 1;
        }
        files
    });
    report(
        &format!("reading the {files} loose ref files serially (the floor)"),
        floor,
    );
    check("refs::list of 10k loose refs", loose_list, floor);
    check(
        "LogWalker::new + first page with 10k loose refs as tips (median of 5)",
        log_tips(),
        floor,
    );

    fixture.git(&["pack-refs", "--all"]);
    check(
        "refs::list of 10k packed refs",
        list(),
        Duration::from_millis(30),
    );
    // Spec §9 has no row for the tips of the walk; they are a listing of the same refs, so its budget applies.
    check(
        "LogWalker::new + first page with 10k packed refs as tips (median of 5)",
        log_tips(),
        Duration::from_millis(30),
    );
    finish();
}

// ───────────────────────────── commit_details ─────────────────────────────

#[test]
#[ignore = "performance: run in release with GITCTHULHU_PERF_ASSERT=1 to assert the budget"]
fn perf_commit_details_1k_files() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.fast_import_with(|stdin| {
        let mut out = std::io::BufWriter::new(stdin);
        for (generation, version) in [("add", "one\ntwo\nthree\n"), ("modify", "one\n2\nthree\nfour\n")] {
            writeln!(
                out,
                "commit refs/heads/main\ncommitter C <c@example.com> 1600000000 +0000\ndata {}\n{generation}",
                generation.len()
            )?;
            for i in 0..1000 {
                let content = format!("{version}{i}\n");
                writeln!(out, "M 100644 inline d{}/f{i}.txt\ndata {}\n{content}", i % 40, content.len())?;
            }
        }
        out.flush()
    });
    let head = gix::ObjectId::from_hex(fixture.git(&["rev-parse", "HEAD"]).as_bytes())
        .expect("a commit id");
    let repo = fixture.open();

    let (details, plain) = timed(|| {
        diff::commit_details(
            &repo,
            head,
            CommitFilesOptions {
                line_stats: false,
                stash: false,
            },
        )
        .expect("the details load")
    });
    assert_eq!(details.files.len(), 1000, "every file changed");
    check(
        "commit_details of 1k files",
        plain,
        Duration::from_millis(50),
    );

    let (details, with_stats) = timed(|| {
        diff::commit_details(
            &repo,
            head,
            CommitFilesOptions {
                line_stats: true,
                stash: false,
            },
        )
        .expect("the details load")
    });
    assert!(
        details.files.iter().all(|file| file.lines_added.is_some()),
        "line statistics were asked for"
    );
    check(
        "commit_details of 1k files with line statistics",
        with_stats,
        Duration::from_millis(300),
    );
    finish();
}

// ───────────────────────────── blame ─────────────────────────────

/// Blame of a 5000-line file in a history of 10k commits. Two files live in the repository: `big.txt` is rewritten
/// (one line) by every 20th commit, the typical case the budget is about (most commits of a history do not touch a
/// given file); `busy.txt` by every commit, the worst case, which is only reported.
#[test]
#[ignore = "performance: run in release with GITCTHULHU_PERF_ASSERT=1 to assert the budget"]
fn perf_blame_5k_lines() {
    let history = from_env("GITCTHULHU_PERF_BLAME_COMMITS", 10_000);
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let (_, generated) = timed(|| {
        fixture.fast_import_with(move |stdin| {
            let mut out = std::io::BufWriter::new(stdin);
            let initial = |name: &str| -> Vec<String> {
                (0..5000)
                    .map(|line| {
format!("{name}{line:04}v0\n")
                    })
                    .collect()
            };
            let mut big = initial("B");
            let mut busy = initial("U");
            for commit in 0..history {
                let message = format!("edit {commit}");
                writeln!(
                    out,
                    "commit refs/heads/main\ncommitter C <c@example.com> {} +0000\ndata {}\n{message}",
                    1_600_000_000 + commit * 60,
                    message.len()
                )?;
                let line = commit * 7919 % 5000;
                busy[line] = format!("U{line:04}v{}\n", commit + 1);
                let mut files = vec![("busy.txt", busy.concat())];
                if commit % 20 == 0 {
                    big[line] = format!("B{line:04}v{}\n", commit + 1);
                    files.push(("big.txt", big.concat()));
                }
                for (path, content) in files {
                    writeln!(
                        out,
                        "M 100644 inline {path}\ndata {}",
                        content.len()
                    )?;
                    out.write_all(content.as_bytes())?;
                    out.write_all(b"\n")?;
                }
            }
            out.flush()
        });
    });
    report(
        &format!("importing {history} commits of two 5000-line files"),
        generated,
    );
    fixture.git(&["commit-graph", "write", "--reachable"]);
    let head = gix::ObjectId::from_hex(fixture.git(&["rev-parse", "HEAD"]).as_bytes())
        .expect("a commit id");
    let repo = fixture.open();
    let blame_of = |path: &str| {
        let (result, took) = timed(|| {
            blame::blame(
                &repo,
                &path.into(),
                BlameRev::Commit(head),
                &BlameOptions::default(),
                &CancelToken::new(),
            )
            .expect("the blame succeeds")
        });
        let lines: u32 = result.chunks.iter().map(|chunk| chunk.len).sum();
        assert_eq!(lines, 5000, "every line of {path} is attributed");
        took
    };
    check(
        &format!("blame of 5k lines over {history} commits, 1 in 20 touches the file"),
        blame_of("big.txt"),
        scaled(Duration::from_millis(500), history, 10_000),
    );
    report(
        &format!("blame of 5k lines over {history} commits, every commit touches the file"),
        blame_of("busy.txt"),
    );
    finish();
}

// ───────────────────────────── file history ─────────────────────────────

#[test]
#[ignore = "performance: run in release with GITCTHULHU_PERF_ASSERT=1 to assert the budget"]
fn perf_file_history_walker() {
    let commits = from_env("GITCTHULHU_PERF_HISTORY_COMMITS", 100_000);
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    fixture.fast_import_with(move |stdin| {
        let mut out = std::io::BufWriter::new(stdin);
        for commit in 0..commits {
            let message = format!("c{commit}");
            writeln!(
                out,
                "commit refs/heads/main\ncommitter C <c@example.com> {} +0000\ndata {}\n{message}",
                1_600_000_000 + commit * 60,
                message.len()
            )?;
            // Every third commit touches the hot file, every thousandth the cold one; the others change a file of
            // their own, so every commit has a tree of its own.
            let (path, content) = if commit % 3 == 0 {
                ("hot.txt".to_owned(), format!("hot {commit}\n"))
            } else if commit % 1000 == 1 {
                ("cold.txt".to_owned(), format!("cold {commit}\n"))
            } else {
                (format!("other/f{}.txt", commit % 50), format!("{commit}\n"))
            };
            writeln!(
                out,
                "M 100644 inline {path}\ndata {}\n{content}",
                content.len()
            )?;
        }
        out.flush()
    });
    fixture.git(&["commit-graph", "write", "--reachable"]);
    let repo = fixture.open();
    let cancel = CancelToken::new();

    let (page, hot) = timed(|| {
        let mut walker = FileHistoryWalker::new(&repo, &"hot.txt".into(), None, false)
            .expect("the walker starts");
        walker
            .next_page(100, &cancel)
            .expect("the first page loads")
    });
    assert_eq!(
        page.entries.len(),
        100,
        "a frequently touched file fills a page"
    );
    check(
        &format!("FileHistoryWalker::next_page(100), frequently touched file, {commits} commits"),
        hot,
        Duration::from_millis(300),
    );

    // A rarely touched file costs one walk over the history (no budget in the spec: "seconds on 1M commits").
    let (page, cold) = timed(|| {
        let mut walker = FileHistoryWalker::new(&repo, &"cold.txt".into(), None, false)
            .expect("the walker starts");
        walker.next_page(100, &cancel).expect("the page loads")
    });
    assert!(!page.entries.is_empty(), "the cold file has history");
    report(
        &format!(
            "FileHistoryWalker::next_page(100), rarely touched file ({} entries), {commits} commits",
            page.entries.len()
        ),
        cold,
    );
    finish();
}
