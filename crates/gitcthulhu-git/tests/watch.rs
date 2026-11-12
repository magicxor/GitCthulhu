//! Tests of `watch::RepoWatcher` (spec §2.6, §7.3). They watch a real file system, so every wait has a timeout and
//! each test is retried once (file system notifications are timing sensitive on CI).

mod support;

use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

use gitbutler_filemonitor::WatchMode;
use gitcthulhu_git::{FileStamp, RefreshSet, RepoWatcher, WatchEvent};
use support::{TestEnv, TestRepo};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `test` and, if it panics, once more.
fn retried(test: impl Fn()) {
    let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&test));
    if let Err(panic) = first {
        eprintln!("the test failed, trying once more: {panic:?}");
        test();
    }
}

/// A watcher whose events arrive on a channel.
struct Watched {
    watcher: RepoWatcher,
    events: Receiver<WatchEvent>,
}

fn watch(fixture: &TestRepo, mode: WatchMode) -> Watched {
    let repo = fixture.open();
    let (tx, events) = mpsc::channel();
    let watcher = RepoWatcher::start_with_mode(&repo, mode, move |event| {
        tx.send(event).ok();
    })
    .expect("the watcher starts");
    Watched { watcher, events }
}

impl Watched {
    /// Collects events until their union contains `wanted`; panics on timeout.
    fn expect(&self, wanted: RefreshSet, what: &str) -> RefreshSet {
        let deadline = Instant::now() + TIMEOUT;
        let mut union = RefreshSet::empty();
        while !union.contains(wanted) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(WatchEvent::Changed { refresh, .. }) => union |= refresh,
                Ok(WatchEvent::Error { message }) => {
                    panic!("{what}: the watcher failed: {message}")
                }
                Err(_) => panic!("{what}: expected {wanted:?} within {TIMEOUT:?}, got {union:?}"),
            }
        }
        union
    }

    /// Everything that arrives within `quiet`.
    fn drain_for(&self, quiet: Duration) -> Vec<WatchEvent> {
        let deadline = Instant::now() + quiet;
        let mut seen = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(event) => seen.push(event),
                Err(_) => return seen,
            }
        }
    }

    /// Waits until the repository is quiet, forgetting what the preparation caused.
    fn settle(&self) {
        while !self.drain_for(Duration::from_millis(1500)).is_empty() {}
    }
}

fn committed_repo(env: &TestEnv) -> TestRepo {
    let fixture = env.repo("repo");
    fixture.write("a.txt", "one\n");
    fixture.write("sub/b.txt", "two\n");
    fixture.commit_all("first");
    fixture
}

#[test]
fn worktree_edit_maps_to_status() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let watched = watch(&fixture, WatchMode::Auto);
        fixture.write("a.txt", "changed\n");
        let refresh = watched.expect(RefreshSet::STATUS, "an edited file");
        assert!(
            !refresh.contains(RefreshSet::LOG),
            "a worktree edit does not touch the log: {refresh:?}"
        );
    });
}

#[test]
fn commit_via_cli_maps_to_log_branches() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let watched = watch(&fixture, WatchMode::Auto);
        fixture.write("a.txt", "changed\n");
        fixture.commit_all("second");
        watched.expect(
            RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::STATUS,
            "a commit made with the git executable",
        );
    });
}

#[test]
fn tag_creation_maps_to_tags() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        for mode in [WatchMode::Auto, WatchMode::Modern] {
            let watched = watch(&fixture, mode);
            let name = format!("v-{mode:?}");
            fixture.git(&["tag", &name]);
            watched.expect(RefreshSet::TAGS, &format!("a new tag ({mode:?})"));
        }
    });
}

#[test]
fn stash_maps_to_stashes() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let watched = watch(&fixture, WatchMode::Auto);
        fixture.write("a.txt", "to stash\n");
        fixture.git(&["stash", "push", "-q", "-m", "wip"]);
        watched.expect(RefreshSet::STASHES | RefreshSet::STATUS, "a stash");
    });
}

#[test]
fn config_change_maps_to_config() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let watched = watch(&fixture, WatchMode::Auto);
        fixture.git(&["config", "demo.key", "value"]);
        watched.expect(RefreshSet::CONFIG, "a changed repository configuration");
    });
}

#[test]
fn merge_conflict_maps_to_repo_state() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        fixture.git(&["switch", "-q", "-c", "other"]);
        fixture.write("a.txt", "other side\n");
        fixture.commit_all("other");
        fixture.git(&["switch", "-q", "main"]);
        fixture.write("a.txt", "main side\n");
        fixture.commit_all("main");

        let watched = watch(&fixture, WatchMode::Auto);
        fixture.git_fail(&["merge", "other"]);
        watched.expect(
            RefreshSet::REPO_STATE | RefreshSet::STATUS,
            "a merge that stops with conflicts",
        );
    });
}

#[test]
fn ignored_file_produces_nothing() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        fixture.write(".gitignore", "*.log\nbuild/\n");
        fixture.commit_all("ignore logs");
        let watched = watch(&fixture, WatchMode::Auto);
        watched.settle();

        fixture.write("debug.log", "noise\n");
        fixture.write("build/output.o", "noise\n");
        let seen = watched.drain_for(Duration::from_millis(2500));
        assert!(seen.is_empty(), "ignored files cause no event: {seen:?}");

        fixture.write("kept.txt", "visible\n");
        watched.expect(RefreshSet::STATUS, "a file that is not ignored");
    });
}

#[test]
fn linked_worktree_sees_common_dir_ref_changes() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let linked = env.path("linked");
        fixture.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-branch",
            &linked.to_string_lossy(),
        ]);

        for mode in [WatchMode::Auto, WatchMode::Modern] {
            let repo =
                gitcthulhu_git::Repo::open(&env.rt, &linked).expect("the linked worktree opens");
            let (tx, events) = mpsc::channel();
            let _watcher = RepoWatcher::start_with_mode(&repo, mode, move |event| {
                tx.send(event).ok();
            })
            .expect("the watcher starts");
            let watched = Watched {
                watcher: _watcher,
                events,
            };

            let tag = format!("seen-from-linked-{mode:?}");
            fixture.git(&["tag", &tag]);
            watched.expect(
                RefreshSet::TAGS,
                &format!("a tag created in the main worktree ({mode:?})"),
            );

            std::fs::write(linked.join("own.txt"), format!("{mode:?}"))
                .expect("file can be written");
            watched.expect(
                RefreshSet::STATUS,
                &format!("a file edited in the linked worktree itself ({mode:?})"),
            );
        }
    });
}

#[test]
fn suppression_holds_and_delivers_once_after_grace() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let watched = watch(&fixture, WatchMode::Auto);
        watched.settle();

        let guard = watched.watcher.suppress();
        fixture.write("a.txt", "during the operation\n");
        let early = watched.drain_for(Duration::from_millis(2000));
        assert!(
            early.is_empty(),
            "nothing is delivered while suppressed: {early:?}"
        );

        let released = Instant::now();
        drop(guard);
        let early = watched.drain_for(Duration::from_millis(500));
        assert!(
            early.is_empty(),
            "nothing during the grace period: {early:?} after {:?}",
            released.elapsed()
        );
        let delivered = watched.drain_for(Duration::from_millis(2000));
        assert_eq!(
            delivered.len(),
            1,
            "exactly one event after the grace: {delivered:?}"
        );
        match &delivered[0] {
            WatchEvent::Changed {
                refresh,
                during_own_operation,
                ..
            } => {
                assert!(
                    refresh.contains(RefreshSet::STATUS),
                    "the held change: {refresh:?}"
                );
                assert!(
                    *during_own_operation,
                    "flagged as caused during our own operation"
                );
            }
            other => panic!("unexpected event {other:?}"),
        }
        assert!(
            watched.drain_for(Duration::from_millis(1200)).is_empty(),
            "and nothing more"
        );
    });
}

#[test]
fn events_during_suppression_not_lost() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let watched = watch(&fixture, WatchMode::Auto);
        watched.settle();

        let guard = watched.watcher.suppress();
        fixture.write("a.txt", "edited\n");
        fixture.git(&["tag", "held-tag"]);
        fixture.git(&["config", "held.key", "1"]);
        std::thread::sleep(Duration::from_millis(1500));
        drop(guard);

        let union = watched.expect(
            RefreshSet::STATUS | RefreshSet::TAGS | RefreshSet::CONFIG,
            "everything that happened while suppressed",
        );
        assert!(
            union.contains(RefreshSet::LOG),
            "tags touch the log: {union:?}"
        );
    });
}

#[test]
fn flush_delivers_held_events_at_once() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let watched = watch(&fixture, WatchMode::Auto);
        watched.settle();

        let _guard = watched.watcher.suppress();
        fixture.write("a.txt", "edited\n");
        std::thread::sleep(Duration::from_millis(1500));
        watched.watcher.flush();
        match watched.events.recv_timeout(Duration::from_secs(2)) {
            Ok(WatchEvent::Changed { refresh, .. }) => {
                assert!(
                    refresh.contains(RefreshSet::STATUS),
                    "the held change: {refresh:?}"
                );
            }
            other => panic!("flush should deliver the held change, got {other:?}"),
        }
    });
}

#[test]
fn ignored_index_stamp_produces_nothing() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        fixture.write("staged.txt", "new\n");
        let watched = watch(&fixture, WatchMode::Auto);
        watched.settle();

        fixture.git(&["add", "staged.txt"]);
        let metadata =
            std::fs::metadata(fixture.path.join(".git/index")).expect("the index exists");
        let mtime = metadata
            .modified()
            .expect("modification times are supported")
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("after the epoch");
        watched.watcher.ignore_index_stamp(FileStamp {
            mtime_ns: mtime.as_nanos() as i128,
            size: metadata.len(),
        });
        let seen = watched.drain_for(Duration::from_millis(3000));
        assert!(
            seen.is_empty(),
            "our own index write is not a change: {seen:?}"
        );

        fixture.write("other.txt", "x\n");
        fixture.git(&["add", "other.txt"]);
        watched.expect(RefreshSet::STATUS, "a later index change by someone else");
    });
}

#[test]
fn submodule_fetch_objects_not_watched() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let module = fixture.path.join(".git/modules/s");
        std::fs::create_dir_all(module.join("objects/pack")).expect("directories can be created");
        std::fs::create_dir_all(module.join("refs/heads")).expect("directories can be created");
        std::fs::write(module.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD can be written");

        for mode in [WatchMode::Auto, WatchMode::Modern] {
            let watched = watch(&fixture, mode);
            watched.settle();
            for i in 0..1000 {
                std::fs::write(module.join("objects").join(format!("o{mode:?}{i}")), "x")
                    .expect("the object can be written");
            }
            let seen = watched.drain_for(Duration::from_millis(2500));
            assert!(
                seen.is_empty(),
                "writing 1000 objects of a submodule yields no event ({mode:?}): {} events",
                seen.len()
            );

            std::fs::write(module.join("HEAD"), format!("ref: refs/heads/{mode:?}\n"))
                .expect("HEAD can be written");
            watched.expect(
                RefreshSet::SUBMODULES,
                &format!("a change of the submodule's HEAD ({mode:?})"),
            );
        }
    });
}

#[test]
fn drop_stops_thread() {
    retried(|| {
        let env = TestEnv::new();
        let fixture = committed_repo(&env);
        let repo = fixture.open();
        let witness = Arc::new(());
        let seen = Arc::new(parking_lot::Mutex::new(0usize));
        let watcher = {
            let witness = witness.clone();
            let seen = seen.clone();
            RepoWatcher::start_with_mode(&repo, WatchMode::Auto, move |_| {
                let _keep = &witness;
                *seen.lock() += 1;
            })
            .expect("the watcher starts")
        };
        assert_eq!(
            Arc::strong_count(&witness),
            2,
            "the watcher thread owns the callback"
        );
        drop(watcher);
        assert_eq!(
            Arc::strong_count(&witness),
            1,
            "dropping the watcher ends its thread, which releases the callback"
        );

        fixture.write("a.txt", "after the drop\n");
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(*seen.lock(), 0, "no events after the watcher is gone");
    });
}
