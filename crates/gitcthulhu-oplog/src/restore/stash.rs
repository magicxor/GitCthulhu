//! Stash step of a restore (spec 17 §3.1, §3.3 C6): `refs/stash` and the stash list, byte-exact.
//!
//! The stash list *is* the reflog of `refs/stash`, so the ref is moved with reflog writing disabled and the log
//! file is then replaced atomically with the captured bytes.

use std::time::Duration;

use anyhow::Context as _;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

use crate::{
    OplogError,
    state::{CapturedState, RefValue, stash_name},
};

/// How long to wait for a stash lock held by another process.
const LOCK_TIMEOUT: Duration = Duration::from_secs(1);

/// Step C6: move `refs/stash` from its value in `from` to the one in `to` (guarded by `from`) and write `to`'s
/// stash log (or delete the log when `to` has none). Nothing happens when both already agree.
///
/// Errors: `ConcurrentModification` (the stash moved meanwhile), `RepositoryLocked`, `Other`.
pub(super) fn restore(
    repo: &gix::Repository,
    from: &CapturedState,
    to: &CapturedState,
) -> Result<(), OplogError> {
    let name = stash_name();
    let (current, target) = (from.refs.get(&name), to.refs.get(&name));
    if current == target && from.stash_log == to.stash_log {
        return Ok(());
    }
    if current != target {
        let change = match (current, target) {
            (Some(current), Some(target)) => Change::Update {
                log: no_log(),
                expected: PreviousValue::MustExistAndMatch(current.to_target()),
                new: target.to_target(),
            },
            (None, Some(target)) => Change::Update {
                log: no_log(),
                expected: PreviousValue::MustNotExist,
                new: target.to_target(),
            },
            (Some(current), None) => Change::Delete {
                expected: PreviousValue::MustExistAndMatch(current.to_target()),
                log: RefLog::AndReference,
            },
            (None, None) => unreachable!("the values differ"),
        };
        let edit = RefEdit {
            change,
            name: name.clone(),
            deref: false,
        };
        let mut store = repo.refs.clone();
        store.write_reflog = gix::refs::store::WriteReflog::Disable;
        let lock_mode = gix::lock::acquire::Fail::AfterDurationWithBackoff(LOCK_TIMEOUT);
        let result = store
            .transaction()
            .prepare([edit], lock_mode, lock_mode)
            .and_then(|transaction| transaction.commit(None));
        if let Err(err) = result {
            let actual = repo
                .try_find_reference(name.as_bstr())
                .ok()
                .flatten()
                .map(|r| r.inner.target);
            if actual != current.map(RefValue::to_target) {
                return Err(OplogError::ConcurrentModification);
            }
            let lock = repo.common_dir().join("refs").join("stash.lock");
            if lock.is_file() {
                return Err(OplogError::RepositoryLocked { path: lock });
            }
            return Err(OplogError::Other(
                anyhow::Error::from(err).context("updating refs/stash"),
            ));
        }
    }
    write_log(repo, to.stash_log.as_ref().map(|log| log.as_slice()))
}

/// A log change for an edit made with reflog writing disabled.
fn no_log() -> LogChange {
    LogChange {
        mode: RefLog::AndReference,
        force_create_reflog: false,
        message: "gitcthulhu: restore stash".into(),
    }
}

/// Replace `<common-dir>/logs/refs/stash` with `log` atomically (lock file + rename), or delete it for `None`.
fn write_log(repo: &gix::Repository, log: Option<&[u8]>) -> Result<(), OplogError> {
    let path = repo.common_dir().join("logs").join("refs").join("stash");
    let Some(log) = log else {
        return super::journal::remove_file(&path);
    };
    let context = || format!("writing '{}'", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(context)?;
    }
    let mut lock = gix::lock::File::acquire_to_update_resource(
        &path,
        gix::lock::acquire::Fail::AfterDurationWithBackoff(LOCK_TIMEOUT),
        None,
        0,
    )
    .map_err(|err| {
        let mut lock = path.clone().into_os_string();
        lock.push(".lock");
        let lock = std::path::PathBuf::from(lock);
        if lock.is_file() {
            OplogError::RepositoryLocked { path: lock }
        } else {
            OplogError::Other(anyhow::Error::from(err).context(context()))
        }
    })?;
    std::io::Write::write_all(&mut lock, log).with_context(context)?;
    lock.commit()
        .map_err(|err| anyhow::Error::from(err.error))
        .with_context(context)?;
    Ok(())
}
