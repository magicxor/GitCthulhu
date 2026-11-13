//! E: untracked and large files (spec 17 §1.3 guarantees).

use gitcthulhu_oplog::{
    ConfirmReason, OperationKind, OplogOptions, Refusal, RestoreTarget, RestoreWarning, Side,
    SkipReason, UndoOutcome, UntrackedCapture,
};

use crate::{
    assert_no_journal, confirmation, confirmed, done, fp, fsck, redo, restore_to_with,
    support::{Fixture, b, desc, has_blob_of, record_ok},
    undo, undo_with,
};

/// 1 KiB per untracked file, 4 KiB per snapshot.
fn small_limits() -> OplogOptions {
    OplogOptions {
        untracked_file_limit: 1024,
        untracked_total_limit: 4 * 1024,
        ..OplogOptions::default()
    }
}

fn big_content() -> Vec<u8> {
    (0..2048u32).map(|i| b'a' + (i % 26) as u8).collect()
}

/// Run `op` (which must not touch `big.bin`) with `untracked`, then undo, redo and undo: `big.bin` is never
/// changed, never stored, and listed as skipped.
fn big_file_survives(kind: OperationKind, untracked: UntrackedCapture, op: impl FnOnce(&Fixture)) {
    let fx = Fixture::new();
    fx.write("a.txt", "a, dirty\n");
    let big = big_content();
    fx.write("big.bin", &big);
    let oplog = fx.oplog_with(small_limits());
    let requested = untracked != UntrackedCapture::None;
    let mut d = desc(kind, "operation next to a big file");
    d.untracked = untracked;
    let before = fp(&fx);
    let id = record_ok(&fx, &oplog, d, op);
    let entry = oplog
        .entry(&fx.repo(), id)
        .expect("reading works")
        .expect("exists");
    let expected_skipped = if requested {
        vec![("big.bin".to_owned(), SkipReason::TooLarge)]
    } else {
        Vec::new()
    };
    assert_eq!(
        entry
            .skipped_untracked
            .iter()
            .map(|s| (s.path.to_string(), s.reason))
            .collect::<Vec<_>>(),
        expected_skipped,
        "a requested big file is listed as not captured"
    );
    let after = fp(&fx);

    let report = done(undo(&fx, &oplog));
    let warned = report.warnings.iter().any(|w| {
        matches!(w, RestoreWarning::SkippedUntrackedLeftAlone(files)
            if files.iter().any(|f| f.path == "big.bin"))
    });
    assert_eq!(warned, requested, "{:?}", report.warnings);
    assert_eq!(fp(&fx), before);
    done(redo(&fx, &oplog));
    assert_eq!(fp(&fx), after);
    done(undo(&fx, &oplog));
    assert_eq!(fp(&fx), before);
    assert_eq!(
        std::fs::read(fx.path.join("big.bin")).expect("readable"),
        big
    );
    assert!(
        !has_blob_of(&fx, &big),
        "the big file was never hashed into the object database"
    );
    fsck(&fx);
}

#[test]
fn skipped_big_file_survives_undo_of_discard_all() {
    big_file_survives(
        OperationKind::Discard,
        UntrackedCapture::Paths(vec![b(".")]),
        |fx| {
            fx.git(&["checkout", "-q", "--", "a.txt"]);
        },
    );
}

#[test]
fn skipped_big_file_survives_undo_of_reset_hard() {
    big_file_survives(OperationKind::ResetHard, UntrackedCapture::None, |fx| {
        fx.git(&["reset", "-q", "--hard"]);
    });
}

#[test]
fn skipped_big_file_survives_undo_of_stash_push_untracked() {
    big_file_survives(OperationKind::StashPush, UntrackedCapture::All, |fx| {
        fx.git(&["stash", "push", "-q", "-u", "--", "a.txt"]);
    });
}

#[test]
fn unrequested_untracked_file_is_never_touched() {
    let fx = Fixture::new();
    fx.write("junk.txt", "junk 1\n");
    let oplog = fx.oplog();
    record_ok(&fx, &oplog, desc(OperationKind::ResetHard, "Reset"), |fx| {
        fx.commit_file("a.txt", "a2\n", "change");
        fx.git(&["reset", "-q", "--hard", "HEAD~1"]);
        fx.write("a.txt", "a3\n");
    });
    for (step, content) in ["junk 2\n", "junk 3\n", "junk 4\n"].into_iter().enumerate() {
        fx.write("junk.txt", content);
        let outcome = if step % 2 == 0 {
            undo(&fx, &oplog)
        } else {
            redo(&fx, &oplog)
        };
        done(outcome);
        assert_eq!(
            std::fs::read_to_string(fx.path.join("junk.txt")).expect("readable"),
            content,
            "step {step}: the untracked file is unchanged"
        );
    }
    // The checkout hashes untracked files to see them, but no snapshot holds them, so nothing that refs/gitcthulhu
    // reaches (and `push --mirror` would transfer) contains them.
    let reachable = fx.git(&["rev-list", "--objects", "refs/gitcthulhu/oplog/main"]);
    for content in ["junk 1\n", "junk 2\n", "junk 3\n", "junk 4\n"] {
        let id = gix::objs::compute_hash(
            gix::hash::Kind::Sha1,
            gix::object::Kind::Blob,
            content.as_bytes(),
        )
        .expect("hashing works");
        assert!(
            !reachable.contains(&id.to_string()),
            "never in a snapshot: {content:?}"
        );
    }
    fsck(&fx);
}

/// A recorded commit deleted `p.bin`; afterwards an untracked big file appears at that path.
fn big_untracked_where_target_writes() -> (Fixture, gitcthulhu_oplog::Oplog, Vec<u8>) {
    let fx = Fixture::new();
    fx.commit_file("p.bin", "tracked content\n", "add p");
    let oplog = fx.oplog_with(small_limits());
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::Commit, "Commit \"remove p\""),
        |fx| {
            fx.git(&["rm", "-q", "p.bin"]);
            fx.git(&["commit", "-q", "-m", "remove p"]);
        },
    );
    let big = big_content();
    fx.write("p.bin", &big);
    (fx, oplog, big)
}

#[test]
fn untracked_file_in_the_way_asks_then_refuses() {
    let (fx, oplog, big) = big_untracked_where_target_writes();
    let before = fp(&fx);
    let ask = confirmation(undo(&fx, &oplog));
    assert_eq!(
        ask.reasons,
        vec![ConfirmReason::WorktreeChangesConflict(vec![b("p.bin")])]
    );
    match undo_with(&fx, &oplog, confirmed(&ask)) {
        UndoOutcome::Refused(Refusal::UntrackedInTheWay(paths)) => {
            assert_eq!(paths, vec![b("p.bin")]);
        }
        other => panic!("expected UntrackedInTheWay, got {other:?}"),
    }
    assert_eq!(fp(&fx), before, "nothing changed");
    assert_eq!(std::fs::read(fx.path.join("p.bin")).expect("readable"), big);
    assert!(
        !has_blob_of(&fx, &big),
        "no blob of the untracked file was written"
    );
    assert_no_journal(&fx);
}

#[test]
fn untracked_file_in_the_way_refuses_a_restore_to() {
    let (fx, oplog, big) = big_untracked_where_target_writes();
    let entry = oplog
        .state(&fx.repo())
        .expect("reading works")
        .undo
        .expect("undoable")
        .reverses;
    let target = RestoreTarget {
        entry,
        side: Side::Before,
    };
    let ask = confirmation(restore_to_with(&fx, &oplog, target, Default::default()));
    assert!(
        matches!(
            ask.reasons.as_slice(),
            [ConfirmReason::ArbitraryRestore { .. }]
        ),
        "{:?}",
        ask.reasons
    );
    match restore_to_with(&fx, &oplog, target, confirmed(&ask)) {
        UndoOutcome::Refused(Refusal::UntrackedInTheWay(paths)) => {
            assert_eq!(paths, vec![b("p.bin")]);
        }
        other => panic!("expected UntrackedInTheWay, got {other:?}"),
    }
    assert_eq!(std::fs::read(fx.path.join("p.bin")).expect("readable"), big);
    assert!(!has_blob_of(&fx, &big));
    assert_no_journal(&fx);
}

#[test]
fn ignored_file_is_never_touched() {
    let fx = Fixture::new();
    fx.commit_file(".gitignore", "*.log\n", "ignore logs");
    fx.write("build.log", "log 1\n");
    let oplog = fx.oplog();
    fx.write("a.txt", "a, dirty\n");
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::Discard, "Discard everything"),
        |fx| {
            fx.git(&["checkout", "-q", "--", "."]);
        },
    );
    let mut d = desc(OperationKind::DeleteUntracked, "Delete untracked files");
    d.untracked = UntrackedCapture::Paths(vec![b(".")]);
    fx.write("junk.txt", "junk\n");
    record_ok(&fx, &oplog, d, |fx| {
        fx.git(&["clean", "-q", "-f"]);
    });
    for (step, content) in ["log 2\n", "log 3\n", "log 4\n"].into_iter().enumerate() {
        fx.write("build.log", content);
        let outcome = if step % 2 == 0 {
            undo(&fx, &oplog)
        } else {
            redo(&fx, &oplog)
        };
        done(outcome);
        assert_eq!(
            std::fs::read_to_string(fx.path.join("build.log")).expect("readable"),
            content,
            "step {step}: the ignored file is unchanged"
        );
    }
    assert!(
        fx.path.join("junk.txt").exists(),
        "the deleted untracked file is back"
    );
    fsck(&fx);
}

/// A recorded commit stops tracking `config.local` and ignores it. Undo makes it tracked again.
fn untracked_then_ignored() -> (Fixture, gitcthulhu_oplog::Oplog) {
    let fx = Fixture::new();
    fx.commit_file("config.local", "tracked\n", "add config");
    let oplog = fx.oplog();
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::Commit, "Commit \"stop tracking config\""),
        |fx| {
            fx.write(".gitignore", "config.local\n");
            fx.git(&["rm", "-q", "--cached", "config.local"]);
            fx.git(&["add", ".gitignore"]);
            fx.git(&["commit", "-q", "-m", "stop tracking config"]);
        },
    );
    (fx, oplog)
}

#[test]
fn edited_ignored_file_where_the_target_writes_is_not_overwritten() {
    let (fx, oplog) = untracked_then_ignored();
    fx.write("config.local", "local edit\n");
    let before = fp(&fx);
    let ask = confirmation(undo(&fx, &oplog));
    assert_eq!(
        ask.reasons,
        vec![ConfirmReason::WorktreeChangesConflict(vec![b(
            "config.local"
        )])]
    );
    match undo_with(&fx, &oplog, confirmed(&ask)) {
        UndoOutcome::Refused(Refusal::UntrackedInTheWay(paths)) => {
            assert_eq!(paths, vec![b("config.local")]);
        }
        other => panic!("expected UntrackedInTheWay, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(fx.path.join("config.local")).expect("readable"),
        "local edit\n",
        "the ignored file's edit is not lost"
    );
    assert_eq!(fp(&fx), before, "nothing changed");
    assert_no_journal(&fx);
}

#[test]
fn unchanged_ignored_file_where_the_target_writes_does_not_block_undo() {
    let (fx, oplog) = untracked_then_ignored();
    done(undo(&fx, &oplog));
    assert_eq!(
        fx.git(&["status", "--porcelain"]),
        "",
        "config.local is tracked again with the content it has"
    );
    done(redo(&fx, &oplog));
    assert_eq!(
        std::fs::read_to_string(fx.path.join("config.local")).expect("readable"),
        "tracked\n"
    );
    fsck(&fx);
}

/// On a case-insensitive file system the file on disk is found under the other case; it is tracked, not an
/// untracked file in the way.
#[test]
fn case_only_rename_with_a_content_change_round_trips() {
    let fx = Fixture::new();
    fx.commit_file("readme.md", "old\n", "add readme");
    let oplog = fx.oplog();
    let before = fp(&fx);
    record_ok(
        &fx,
        &oplog,
        desc(OperationKind::Commit, "Commit \"rename readme\""),
        |fx| {
            fx.git(&["mv", "readme.md", "README.md"]);
            fx.write("README.md", "new\n");
            fx.git(&["add", "README.md"]);
            fx.git(&["commit", "-q", "-m", "rename readme"]);
        },
    );
    let after = fp(&fx);
    done(undo(&fx, &oplog));
    assert_eq!(fp(&fx), before);
    done(redo(&fx, &oplog));
    assert_eq!(fp(&fx), after);
    fsck(&fx);
}
