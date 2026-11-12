//! Restore engine tests of the oplog (spec 17 §5.1 groups B-I).

#[path = "../support/mod.rs"]
mod support;

mod crlf;
mod divergence;
mod in_progress;
mod perf;
mod recovery;
mod undo_matrix;
mod untracked;
mod windows_locks;

use gitcthulhu_oplog::{
    Confirmation, Oplog, RestoreOptions, RestoreReport, RestoreTarget, UndoOutcome,
};

use crate::support::Fixture;

/// `fingerprint()` plus the local `branch.*` config, which undo of branch deletions and renames restores.
pub fn fp(fx: &Fixture) -> String {
    let mut out = support::fingerprint(fx);
    let config = fx.git_output(&["config", "--local", "--get-regexp", r"^branch\."]);
    out.push_str("--- branch config\n");
    out.push_str(&String::from_utf8_lossy(&config.stdout));
    out
}

/// `git fsck --connectivity-only` succeeds (run after every restore).
pub fn fsck(fx: &Fixture) {
    let out = fx.git_output(&["fsck", "--no-progress", "--connectivity-only"]);
    assert!(
        out.status.success(),
        "fsck failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The journal and the index backup are gone.
pub fn assert_no_journal(fx: &Fixture) {
    let dir = fx.repo().git_dir().join("gitcthulhu").join("oplog");
    assert!(
        !dir.join("restore.json").exists(),
        "no restore journal is left behind"
    );
    assert!(
        !dir.join("index.backup").exists(),
        "no index backup is left behind"
    );
}

/// Undo with `options`, taking the guard like the GUI does.
pub fn undo_with(fx: &Fixture, oplog: &Oplog, options: RestoreOptions) -> UndoOutcome {
    let mut guard = fx.guard();
    oplog
        .undo(&fx.repo(), options, guard.write_permission())
        .expect("undo does not fail")
}

/// Redo with `options`.
pub fn redo_with(fx: &Fixture, oplog: &Oplog, options: RestoreOptions) -> UndoOutcome {
    let mut guard = fx.guard();
    oplog
        .redo(&fx.repo(), options, guard.write_permission())
        .expect("redo does not fail")
}

/// Undo without confirmation.
pub fn undo(fx: &Fixture, oplog: &Oplog) -> UndoOutcome {
    undo_with(fx, oplog, RestoreOptions::default())
}

/// Redo without confirmation.
pub fn redo(fx: &Fixture, oplog: &Oplog) -> UndoOutcome {
    redo_with(fx, oplog, RestoreOptions::default())
}

/// Restore-to with `options`.
pub fn restore_to_with(
    fx: &Fixture,
    oplog: &Oplog,
    target: RestoreTarget,
    options: RestoreOptions,
) -> UndoOutcome {
    let mut guard = fx.guard();
    oplog
        .restore_to(&fx.repo(), target, options, guard.write_permission())
        .expect("restore-to does not fail")
}

/// The report of a performed restore; panics on any other outcome.
#[track_caller]
pub fn done(outcome: UndoOutcome) -> RestoreReport {
    match outcome {
        UndoOutcome::Done(report) => report,
        other => panic!("expected the restore to be done, got {other:?}"),
    }
}

/// The confirmation request; panics on any other outcome.
#[track_caller]
pub fn confirmation(outcome: UndoOutcome) -> Confirmation {
    match outcome {
        UndoOutcome::NeedsConfirmation(confirmation) => confirmation,
        other => panic!("expected a confirmation request, got {other:?}"),
    }
}

/// Options that acknowledge `confirmation`.
pub fn confirmed(confirmation: &Confirmation) -> RestoreOptions {
    RestoreOptions {
        confirmation: Some(confirmation.token.clone()),
    }
}
