//! I: performance on a large repository (spec 17 §3.8). Ignored by default; run with
//! `cargo test -p gitcthulhu-oplog --release --test restore -- --ignored perf`.
//!
//! Object growth per capture is asserted; timings are printed, not asserted (targets on the Windows reference
//! machine: capture < 1.5 s, `RefsOnly` capture < 50 ms, undo of a commit < 4 s).

use std::time::{Duration, Instant};

use gitcthulhu_oplog::{OperationKind, OperationResult, UntrackedCapture};

use crate::{
    done, fsck,
    support::{Fixture, desc, record_commit, refs_only},
    undo,
};

const DIRS: usize = 1000;
const FILES_PER_DIR: usize = 100;

/// The number of loose objects in the repository.
fn loose_objects(fx: &Fixture) -> usize {
    let objects = fx.repo().common_dir().join("objects");
    std::fs::read_dir(&objects)
        .expect("the objects directory can be read")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().len() == 2 && e.path().is_dir())
        .map(|e| {
            std::fs::read_dir(e.path())
                .map(|files| files.count())
                .unwrap_or(0)
        })
        .sum()
}

fn time<T>(what: &str, f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = f();
    let elapsed = start.elapsed();
    eprintln!("{what}: {elapsed:?}");
    (out, elapsed)
}

#[test]
#[ignore = "performance test: 100k files; run with --release -- --ignored"]
fn capture_and_undo_on_100k_files() {
    let fx = Fixture::new();
    time("creating 100k files", || {
        for d in 0..DIRS {
            let dir = fx.path.join(format!("d{d:04}"));
            std::fs::create_dir_all(&dir).expect("creatable");
            for f in 0..FILES_PER_DIR {
                std::fs::write(dir.join(format!("f{f:03}.txt")), format!("file {d} {f}\n"))
                    .expect("writable");
            }
        }
    });
    time("committing them with git", || {
        fx.git(&["add", "-A"]);
        fx.git(&["commit", "-q", "-m", "100k files"]);
        fx.git(&["gc", "-q"]);
    });
    for i in 0..10 {
        fx.write(
            &format!("d{:04}/f000.txt", i * 97),
            format!("modified {i}\n"),
        );
    }
    for i in 0..5 {
        fx.write(
            &format!("d{:04}/new{i}.txt", i * 13),
            format!("untracked {i}\n"),
        );
    }
    // Warm the status caches like a running GUI would.
    fx.git(&["status", "--porcelain"]);
    let oplog = fx.oplog();

    let before = loose_objects(&fx);
    let mut guard = fx.guard();
    let mut full = desc(OperationKind::Commit, "capture");
    full.untracked = UntrackedCapture::All;
    let (pending, _) = time("capture (Scope::Full, 10 modified + 5 untracked)", || {
        oplog
            .prepare(&fx.repo(), full, guard.write_permission())
            .expect("capture works")
            .expect("enabled")
    });
    let created = loose_objects(&fx) - before;
    let depth = 1;
    let budget = 15 + 15 * (depth + 1) + 3;
    eprintln!("new loose objects: {created} (budget {budget})");
    assert!(
        created <= budget,
        "a capture writes only the changed blobs and the trees on their paths: {created} > {budget}"
    );
    drop(pending);

    let (pending, _) = time("capture (Scope::RefsOnly)", || {
        oplog
            .prepare(
                &fx.repo(),
                refs_only(OperationKind::CreateBranch, "refs only"),
                guard.write_permission(),
            )
            .expect("capture works")
            .expect("enabled")
    });
    oplog
        .finish(
            &fx.repo(),
            pending,
            OperationResult::Succeeded,
            guard.write_permission(),
        )
        .expect("finish works");
    drop(guard);

    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-q", "-m", "staged everything"]);
    let (_, recorded) = time("recording a commit (two Full captures)", || {
        record_commit(&fx, &oplog, "d0001/f001.txt", "committed\n", "commit")
    });
    let (report, undone) = time("undo of a commit", || done(undo(&fx, &oplog)));
    eprintln!("undo report: {report:?}");
    eprintln!("summary: record {recorded:?}, undo {undone:?}");
    fsck(&fx);
}
