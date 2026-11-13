//! F: line endings and filters survive undo/redo (spec 17 §3.1: the checkout runs Git's conversions).

use gitcthulhu_oplog::{OperationKind, UntrackedCapture};

use crate::{
    done, fp, fsck, redo,
    support::{Fixture, b, desc, record_ok},
    undo,
};

/// `core.autocrlf=true` and attributes for CRLF text, LF text and binary files, all committed and checked out by
/// Git (so the files on disk are what Git writes).
fn eol_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.git(&["config", "core.autocrlf", "true"]);
    fx.write(
        ".gitattributes",
        "*.txt text eol=crlf\n*.sh text eol=lf\n*.bin binary\n",
    );
    fx.write("doc.txt", "one\ntwo\nthree\n");
    fx.write("run.sh", "#!/bin/sh\necho one\n");
    fx.write("data.bin", b"\x00\r\x01\r\n\x02\n\r\r");
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "eol files"]);
    // Let Git write every file the way checkout writes it.
    for file in ["a.txt", "doc.txt", "run.sh", "data.bin"] {
        std::fs::remove_file(fx.path.join(file)).expect("the file can be removed");
    }
    fx.git(&["checkout", "-q", "--", "."]);
    assert_eq!(
        fx.git(&["status", "--porcelain"]),
        "",
        "Git's own checkout is clean"
    );
    fx
}

fn read(fx: &Fixture, path: &str) -> Vec<u8> {
    std::fs::read(fx.path.join(path)).expect("the file can be read")
}

/// Modify `path` to `modified` (worktree bytes), record a discard, then undo/redo/undo and compare bytes.
fn discard_round_trip(path: &str, modified: &[u8]) {
    let fx = eol_fixture();
    let checked_out = read(&fx, path);
    fx.write(path, modified);
    let oplog = fx.oplog();
    let before = fp(&fx);
    let mut d = desc(OperationKind::Discard, "Discard");
    d.untracked = UntrackedCapture::Paths(vec![b(path)]);
    record_ok(&fx, &oplog, d, |fx| {
        fx.git(&["checkout", "-q", "--", path]);
    });
    assert_eq!(
        read(&fx, path),
        checked_out,
        "the discard restored Git's bytes"
    );
    let after = fp(&fx);

    done(undo(&fx, &oplog));
    assert_eq!(
        read(&fx, path),
        modified,
        "undo brings the modified bytes back exactly"
    );
    assert_eq!(fp(&fx), before);
    fsck(&fx);

    done(redo(&fx, &oplog));
    assert_eq!(
        read(&fx, path),
        checked_out,
        "redo writes the same bytes as git checkout"
    );
    assert_eq!(
        fx.git(&["status", "--porcelain"]),
        "",
        "status is clean after redo"
    );
    assert_eq!(fp(&fx), after);

    done(undo(&fx, &oplog));
    assert_eq!(read(&fx, path), modified);
    fsck(&fx);
}

#[test]
fn crlf_text_file() {
    discard_round_trip("doc.txt", b"one\r\nTWO\r\nthree\r\n");
}

#[test]
fn autocrlf_text_file_without_attribute() {
    discard_round_trip("a.txt", b"a\r\nchanged\r\n");
}

#[test]
fn lf_text_file() {
    discard_round_trip("run.sh", b"#!/bin/sh\necho two\n");
}

#[test]
fn binary_file_with_cr_bytes() {
    discard_round_trip("data.bin", b"\r\r\n\x00\r\x03\n\r");
}

#[test]
fn files_checked_out_by_git_are_untouched_by_an_unrelated_undo() {
    let fx = eol_fixture();
    let originals: Vec<_> = ["doc.txt", "run.sh", "data.bin"]
        .iter()
        .map(|p| read(&fx, p))
        .collect();
    let oplog = fx.oplog();
    record_ok(&fx, &oplog, desc(OperationKind::Commit, "Commit"), |fx| {
        fx.write("new.txt", "new\n");
        fx.git(&["add", "new.txt"]);
        fx.git(&["commit", "-q", "-m", "new"]);
    });
    done(undo(&fx, &oplog));
    done(redo(&fx, &oplog));
    for (path, original) in ["doc.txt", "run.sh", "data.bin"].iter().zip(originals) {
        assert_eq!(read(&fx, path), original, "{path} is byte-identical");
    }
    assert_eq!(fx.git(&["status", "--porcelain"]), "");
    fsck(&fx);
}

/// LFS: only with `GITCTHULHU_TEST_LFS=1` and `git lfs` available.
#[test]
fn lfs_discard_and_undo() {
    if std::env::var_os("GITCTHULHU_TEST_LFS").is_none_or(|v| v != "1") {
        eprintln!("skipped: set GITCTHULHU_TEST_LFS=1 to run the LFS test");
        return;
    }
    let fx = Fixture::new();
    if !fx.git_output(&["lfs", "version"]).status.success() {
        eprintln!("skipped: git lfs is not available");
        return;
    }
    fx.git(&["lfs", "install", "--local"]);
    fx.git(&["lfs", "track", "*.dat"]);
    fx.write("big.dat", "real content v1\n");
    fx.git(&["add", ".gitattributes", "big.dat"]);
    fx.git(&["commit", "-q", "-m", "lfs file"]);
    fx.write("big.dat", "real content v2\n");
    let oplog = fx.oplog();
    let mut d = desc(OperationKind::Discard, "Discard 'big.dat'");
    d.untracked = UntrackedCapture::Paths(vec![b("big.dat")]);
    record_ok(&fx, &oplog, d, |fx| {
        fx.git(&["checkout", "-q", "--", "big.dat"]);
    });
    done(undo(&fx, &oplog));
    assert_eq!(
        read(&fx, "big.dat"),
        b"real content v2\n",
        "the real content, not the pointer"
    );
    let status = fx.git(&["lfs", "status", "--porcelain"]);
    assert!(
        !status.contains("big.dat") || status.contains(" M big.dat"),
        "git lfs sees the restored file as an ordinary modification: {status}"
    );

    // Snapshot, prune, undo again: objects referenced only from refs/gitcthulhu may be pruned (open point §7.5).
    done(redo(&fx, &oplog));
    fx.git(&["lfs", "prune"]);
    done(undo(&fx, &oplog));
    let content = read(&fx, "big.dat");
    eprintln!(
        "after git lfs prune, undo restored {}",
        if content == b"real content v2\n" {
            "the real content (the object survived the prune)"
        } else {
            "a pointer or other content (the object was pruned)"
        }
    );
    fsck(&fx);
}
