//! The restore engine: undo, redo, restore-to and recovery of interrupted restores (spec 17 §3).
//!
//! Stub with the final signatures; the engine is implemented by work package OP-WP3, which owns this module.

use but_core::sync::RepoExclusive;

use crate::{
    Oplog, OplogError,
    types::{InterruptedRestore, Recovery, RestoreOptions, RestoreTarget, UndoOutcome},
};

fn not_implemented() -> OplogError {
    OplogError::Other(anyhow::anyhow!("restore not implemented"))
}

/// Undo the top of the undo stack (§3.3, §4.3).
pub(crate) fn undo(
    _oplog: &Oplog,
    _repo: &gix::Repository,
    _options: RestoreOptions,
    _perm: &mut RepoExclusive,
) -> Result<UndoOutcome, OplogError> {
    Err(not_implemented())
}

/// Redo the top of the redo stack (§3.3, §4.3).
pub(crate) fn redo(
    _oplog: &Oplog,
    _repo: &gix::Repository,
    _options: RestoreOptions,
    _perm: &mut RepoExclusive,
) -> Result<UndoOutcome, OplogError> {
    Err(not_implemented())
}

/// Restore the before- or after-state of any entry with a snapshot (§3.3).
pub(crate) fn restore_to(
    _oplog: &Oplog,
    _repo: &gix::Repository,
    _target: RestoreTarget,
    _options: RestoreOptions,
    _perm: &mut RepoExclusive,
) -> Result<UndoOutcome, OplogError> {
    Err(not_implemented())
}

/// Finish or roll back an interrupted restore (§3.6).
pub(crate) fn recover(
    _oplog: &Oplog,
    _repo: &gix::Repository,
    _how: Recovery,
    _perm: &mut RepoExclusive,
) -> Result<(), OplogError> {
    Err(not_implemented())
}

/// The interrupted restore described by the journal, if one exists (§3.6).
pub(crate) fn interrupted_restore(
    _oplog: &Oplog,
    _repo: &gix::Repository,
) -> Result<Option<InterruptedRestore>, OplogError> {
    Ok(None)
}
