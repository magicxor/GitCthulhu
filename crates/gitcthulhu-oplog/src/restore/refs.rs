//! Ref steps of a restore (spec 17 §3.5 C5 and §3.3 C7): one guarded ref transaction for `HEAD`,
//! `refs/heads/*` and `refs/tags/*`, and the `branch.<name>.*` config of branches the restore creates or deletes.

use std::collections::BTreeSet;

use bstr::{BStr, BString, ByteSlice as _};
use gix::refs::{
    FullName,
    transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
};

use crate::{
    OplogError,
    state::{CapturedState, RefValue, STASH_REF, head_name},
    storage,
};

/// Step C5: move `HEAD`, `refs/heads/*` and `refs/tags/*` from their values in `from` to those in `to` in one
/// transaction. Every edit is guarded by its value in `from` (`MustExistAndMatch` / `MustNotExist`), so a ref that
/// moved meanwhile (a terminal `git commit` during the restore) makes the whole transaction fail before anything
/// is renamed. Refs equal in both states are not touched; `refs/stash` is left to [`super::stash`].
///
/// Errors: `ConcurrentModification` (a guard failed), `RepositoryLocked` (a ref lock is held), `Other`.
pub(super) fn apply(
    repo: &gix::Repository,
    from: &CapturedState,
    to: &CapturedState,
    message: &str,
) -> Result<(), OplogError> {
    let edits = edits(from, to, message)?;
    if edits.is_empty() {
        return Ok(());
    }
    let names: Vec<FullName> = edits.iter().map(|e| e.name.clone()).collect();
    let fallback = storage::signature();
    let mut time = gix::date::parse::TimeBuf::default();
    let committer = match repo.committer() {
        Some(Ok(committer)) => committer,
        _ => fallback.to_ref(&mut time),
    };
    match repo.edit_references_as(edits, Some(committer)) {
        Ok(_) => Ok(()),
        Err(err) => Err(classify_failure(
            repo,
            from,
            &names,
            anyhow::Error::from(err).context("updating the refs"),
        )),
    }
}

/// The edits of [`apply()`].
fn edits(
    from: &CapturedState,
    to: &CapturedState,
    message: &str,
) -> Result<Vec<RefEdit>, OplogError> {
    let log = LogChange {
        mode: RefLog::AndReference,
        force_create_reflog: false,
        message: message.into(),
    };
    let names: BTreeSet<&FullName> = from
        .refs
        .keys()
        .chain(to.refs.keys())
        .filter(|name| name.as_bstr() != STASH_REF)
        .collect();
    let mut edits = Vec::new();
    for name in names {
        let change = match (from.refs.get(name), to.refs.get(name)) {
            (current, target) if current == target => continue,
            (Some(current), Some(target)) => Change::Update {
                log: log.clone(),
                expected: PreviousValue::MustExistAndMatch(current.to_target()),
                new: target.to_target(),
            },
            (None, Some(target)) => Change::Update {
                log: log.clone(),
                expected: PreviousValue::MustNotExist,
                new: target.to_target(),
            },
            (Some(current), None) => Change::Delete {
                expected: PreviousValue::MustExistAndMatch(current.to_target()),
                log: RefLog::AndReference,
            },
            (None, None) => continue,
        };
        edits.push(RefEdit {
            change,
            name: name.clone(),
            deref: false,
        });
    }
    if from.head != to.head {
        edits.push(RefEdit {
            change: Change::Update {
                log,
                expected: PreviousValue::MustExistAndMatch(from.head.to_target()),
                new: to.head.to_target(),
            },
            name: head_name(),
            deref: false,
        });
    }
    Ok(edits)
}

/// Tell a guard failure from a held lock after a failed transaction over `names`.
fn classify_failure(
    repo: &gix::Repository,
    from: &CapturedState,
    names: &[FullName],
    err: anyhow::Error,
) -> OplogError {
    for name in names {
        let expected = if name.as_bstr() == "HEAD" {
            Some(from.head.to_target())
        } else {
            from.refs.get(name).map(RefValue::to_target)
        };
        let actual = repo
            .try_find_reference(name.as_bstr())
            .ok()
            .flatten()
            .map(|r| r.inner.target);
        if actual != expected {
            return OplogError::ConcurrentModification;
        }
    }
    for name in names {
        let base = if name.as_bstr() == "HEAD" {
            repo.git_dir()
        } else {
            repo.common_dir()
        };
        if let Ok(rela) = gix::path::from_bstr(name.as_bstr()) {
            let mut lock = base.join(rela).into_os_string();
            lock.push(".lock");
            let lock = std::path::PathBuf::from(lock);
            if lock.is_file() {
                return OplogError::RepositoryLocked { path: lock };
            }
        }
    }
    let packed = repo.common_dir().join("packed-refs.lock");
    if packed.is_file() {
        return OplogError::RepositoryLocked { path: packed };
    }
    OplogError::Other(err)
}

/// The short names of the local branches of `state`.
fn branches(state: &CapturedState) -> BTreeSet<&BStr> {
    state
        .refs
        .keys()
        .filter_map(|name| name.as_bstr().strip_prefix(b"refs/heads/"))
        .map(|short| short.as_bstr())
        .collect()
}

/// Step C7: branches that `to` has and `from` doesn't get every `branch.<name>.*` value of `to` back; branches
/// that `from` has and `to` doesn't lose their `branch "<name>"` sections. Other branches are untouched (`push -u`
/// legitimately changes their config).
pub(super) fn restore_branch_config(
    repo: &gix::Repository,
    from: &CapturedState,
    to: &CapturedState,
) -> Result<(), OplogError> {
    let (old, new) = (branches(from), branches(to));
    let created: Vec<BString> = new.difference(&old).map(|b| (*b).to_owned()).collect();
    let deleted: Vec<BString> = old.difference(&new).map(|b| (*b).to_owned()).collect();
    let has_config = |b: &BString| to.branch_config.contains_key(b);
    if deleted.is_empty() && !created.iter().any(has_config) {
        // Sections of created branches are removed below only to make a retried restore idempotent; without
        // values to set and nothing to delete there is nothing to do.
        return Ok(());
    }
    but_core::git_config::edit_repo_config(repo, gix::config::Source::Local, |config| {
        for branch in deleted.iter().chain(&created) {
            while config
                .remove_section("branch", Some(branch.as_bstr()))
                .is_some()
            {}
        }
        for branch in &created {
            let Some(values) = to.branch_config.get(branch) else {
                continue;
            };
            let mut section = config.section_mut_or_create_new("branch", Some(branch.as_bstr()))?;
            for (key, value) in values {
                let key = key.to_str().map_err(|_| {
                    anyhow::anyhow!("branch config key '{key}' of '{branch}' is not valid UTF-8")
                })?;
                section.push(key, Some(value.as_bstr()))?;
            }
        }
        Ok(())
    })
    .map_err(|err| OplogError::Other(err.context("restoring the branch config")))?;
    Ok(())
}
