//! G: files and repositories locked by other programs (spec 17 §3.7). The sharing-violation tests are Windows-only;
//! the leftover `index.lock` test runs everywhere.

use gitcthulhu_oplog::{OperationKind, OplogError, UntrackedCapture};

use crate::{
    assert_no_journal, done, fp, fsck,
    support::{Fixture, b, desc, record_ok},
};

/// Record a discard of a modification of `a.txt`.
fn record_discard(fx: &Fixture, oplog: &gitcthulhu_oplog::Oplog) {
    fx.write("a.txt", "a, dirty\n");
    let mut d = desc(OperationKind::Discard, "Discard 'a.txt'");
    d.untracked = UntrackedCapture::Paths(vec![b("a.txt")]);
    record_ok(fx, oplog, d, |fx| {
        fx.git(&["checkout", "-q", "--", "a.txt"]);
    });
}

fn try_undo(
    fx: &Fixture,
    oplog: &gitcthulhu_oplog::Oplog,
) -> Result<gitcthulhu_oplog::UndoOutcome, OplogError> {
    let mut guard = fx.guard();
    oplog.undo(&fx.repo(), Default::default(), guard.write_permission())
}

#[test]
fn leftover_index_lock_is_repository_locked() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_discard(&fx, &oplog);
    let before = fp(&fx);
    let lock = fx.repo().git_dir().join("index.lock");
    std::fs::write(&lock, b"").expect("the lock can be created");
    match try_undo(&fx, &oplog) {
        Err(OplogError::RepositoryLocked { path }) => {
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some("index.lock")
            );
        }
        other => panic!("expected RepositoryLocked, got {other:?}"),
    }
    std::fs::remove_file(&lock).expect("the lock can be removed");
    assert_eq!(fp(&fx), before, "nothing changed");
    assert_no_journal(&fx);
    done(try_undo(&fx, &oplog).expect("undo works once the lock is gone"));
    fsck(&fx);
}

#[cfg(windows)]
mod sharing_violations {
    use std::os::windows::fs::OpenOptionsExt as _;

    use gitcthulhu_oplog::SkipReason;

    use super::*;
    use crate::support::record_commit;

    /// Open `path` without any sharing (no other program may read, write or delete it).
    fn lock_exclusively(path: &std::path::Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .expect("the file can be opened exclusively")
    }

    /// Make sure no index entry is racily clean, so status trusts the stat of unchanged files and never opens them.
    fn settle_index(fx: &Fixture) {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fx.git(&["update-index", "-q", "--really-refresh"]);
        // Force a rewrite of the index with a newer timestamp than every entry.
        fx.git(&["update-index", "--index-version", "3"]);
        fx.git(&["update-index", "--index-version", "2"]);
    }

    #[test]
    fn locked_file_the_undo_rewrites_is_file_locked() {
        let fx = Fixture::new();
        let oplog = fx.oplog();
        record_discard(&fx, &oplog);
        let before = fp(&fx);
        let handle = lock_exclusively(&fx.path.join("a.txt"));
        match try_undo(&fx, &oplog) {
            Err(OplogError::FileLocked { code, .. }) => {
                assert!(matches!(code, 5 | 32 | 33), "code {code}");
            }
            other => panic!("expected FileLocked, got {other:?}"),
        }
        drop(handle);
        assert_eq!(fp(&fx), before, "nothing changed");
        assert_no_journal(&fx);
        done(try_undo(&fx, &oplog).expect("undo works once the file is closed"));
        assert_eq!(
            std::fs::read_to_string(fx.path.join("a.txt")).expect("readable"),
            "a, dirty\n"
        );
        fsck(&fx);
    }

    /// Like above, with two files to rewrite and a settled index (status trusts the stat of the locked file, so the
    /// lock is met by the checkout's write rather than by the capture): nothing changes, not even the other file.
    #[test]
    fn locked_file_fails_the_checkout_and_changes_nothing() {
        let fx = Fixture::new();
        fx.commit_file("unrelated.txt", "unrelated\n", "add unrelated");
        let oplog = fx.oplog();
        fx.write("a.txt", "a, dirty\n");
        fx.write("unrelated.txt", "unrelated, dirty\n");
        let mut d = desc(OperationKind::Discard, "Discard all");
        d.untracked = UntrackedCapture::Paths(vec![b("a.txt"), b("unrelated.txt")]);
        record_ok(&fx, &oplog, d, |fx| {
            fx.git(&["checkout", "-q", "--", "."]);
        });
        settle_index(&fx);
        let before = fp(&fx);
        let handle = lock_exclusively(&fx.path.join("unrelated.txt"));
        match try_undo(&fx, &oplog) {
            Err(OplogError::FileLocked { code, .. }) => {
                assert!(matches!(code, 5 | 32 | 33), "code {code}");
            }
            other => panic!("expected FileLocked, got {other:?}"),
        }
        drop(handle);
        assert_eq!(
            fp(&fx),
            before,
            "the checkout rolled back: a.txt was not written either"
        );
        assert_no_journal(&fx);
        done(try_undo(&fx, &oplog).expect("undo works once the file is closed"));
        fsck(&fx);
    }

    #[test]
    fn locked_file_the_undo_does_not_touch_is_fine() {
        let fx = Fixture::new();
        fx.commit_file("unrelated.txt", "unrelated\n", "add unrelated");
        let oplog = fx.oplog();
        record_discard(&fx, &oplog);
        settle_index(&fx);
        let handle = lock_exclusively(&fx.path.join("unrelated.txt"));
        done(try_undo(&fx, &oplog).expect("undo works"));
        drop(handle);
        assert_eq!(
            std::fs::read_to_string(fx.path.join("a.txt")).expect("readable"),
            "a, dirty\n"
        );
        fsck(&fx);
    }

    #[test]
    fn locked_untracked_file_is_skipped_not_barrier() {
        let fx = Fixture::new();
        let oplog = fx.oplog();
        record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
        fx.write("locked.txt", "locked\n");
        fx.write("junk.txt", "junk\n");
        let handle = lock_exclusively(&fx.path.join("locked.txt"));
        let mut d = desc(OperationKind::DeleteUntracked, "Delete 'junk.txt'");
        d.untracked = UntrackedCapture::Paths(vec![b("junk.txt"), b("locked.txt")]);
        let id = record_ok(&fx, &oplog, d, |fx| {
            std::fs::remove_file(fx.path.join("junk.txt")).expect("removable");
        });
        drop(handle);
        let entry = oplog
            .entry(&fx.repo(), id)
            .expect("reading works")
            .expect("exists");
        assert_eq!(
            entry
                .skipped_untracked
                .iter()
                .map(|s| (s.path.to_string(), s.reason))
                .collect::<Vec<_>>(),
            vec![("locked.txt".to_owned(), SkipReason::Unreadable)]
        );
        let state = oplog.state(&fx.repo()).expect("reading works");
        assert_eq!(state.undo_depth, 2, "the stacks were not cleared");
    }
}
