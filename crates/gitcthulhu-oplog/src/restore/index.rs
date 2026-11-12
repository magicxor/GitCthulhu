//! Index steps of a restore (spec 17 §3.3 C2 and C8): the conflict-free pre-index, its backup, and the rebuild of
//! the index from a snapshot with a stat carry-over.
//!
//! Every write holds `index.lock` across read and write, so no other writer can slip in between.

use std::{collections::BTreeMap, path::Path, time::Duration};

use anyhow::Context as _;
use bstr::{BString, ByteSlice as _};
use but_core::index::{Edit, Stat};
use gix::{
    index::entry::{Flags, Mode, Stage},
    prelude::ObjectIdExt as _,
};

use crate::{
    OplogError,
    state::{
        FLAG_ASSUME_UNCHANGED, FLAG_INTENT_TO_ADD, FLAG_SKIP_WORKTREE, FileState,
        index_flags_from_bits,
    },
};

/// The index a restore writes: a stage-0 tree plus the but-core conflict tree (`<path>/<stage>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NewIndex {
    pub tree: gix::ObjectId,
    pub conflicts: Option<gix::ObjectId>,
}

impl NewIndex {
    /// The index recorded in `files`.
    pub(super) fn of(files: &FileState) -> NewIndex {
        NewIndex {
            tree: files.index,
            conflicts: files.index_conflicts,
        }
    }
}

/// How long to wait for `index.lock` held by another process before giving up.
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Take `index.lock` of `repo`'s worktree. A lock that stays held maps to `RepositoryLocked`.
fn lock(repo: &gix::Repository) -> Result<gix::lock::File, OplogError> {
    gix::lock::File::acquire_to_update_resource(
        repo.index_path(),
        gix::lock::acquire::Fail::AfterDurationWithBackoff(LOCK_TIMEOUT),
        None,
        0,
    )
    .map_err(|err| {
        let path = repo.index_path().with_extension("lock");
        if path.is_file() {
            OplogError::RepositoryLocked { path }
        } else {
            OplogError::Other(anyhow::Error::from(err).context("locking the index"))
        }
    })
}

/// Read `.git/index` freshly (the repository's cached index may predate the checkout); empty if it doesn't exist.
fn read(repo: &gix::Repository) -> anyhow::Result<gix::index::File> {
    let path = repo.index_path();
    if path.is_file() {
        Ok(repo.open_index()?)
    } else {
        Ok(gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            path,
        ))
    }
}

/// Write `state` into the held `lock` and commit it.
fn write(state: gix::index::State, path: &Path, lock: gix::lock::File) -> anyhow::Result<()> {
    let file = gix::index::File::from_state(state, path);
    // Buffered: the index is written entry by entry, and unbuffered small writes cost seconds for 100k entries.
    let mut out = std::io::BufWriter::new(lock);
    file.write_to(&mut out, gix::index::write::Options::default())?;
    let lock = out
        .into_inner()
        .map_err(|err| anyhow::Error::from(err.into_error()))
        .with_context(|| format!("writing '{}'", path.display()))?;
    lock.commit()
        .map_err(|err| anyhow::Error::from(err.error))
        .with_context(|| format!("writing '{}'", path.display()))?;
    Ok(())
}

/// `true` if the current index has conflict stages.
pub(super) fn has_conflicts(repo: &gix::Repository) -> Result<bool, OplogError> {
    let index = read(repo)?;
    Ok(index
        .entries()
        .iter()
        .any(|e| e.stage() != Stage::Unconflicted))
}

/// Step C2: if the index has conflict stages, back it up to `backup` (unless `keep_existing_backup` and a backup
/// exists already) and replace every conflicted path by a stage-0 entry from `worktree` (removed if absent there),
/// so that safe checkout's "unresolved conflicts" precondition holds. Returns `true` if the index was changed.
pub(super) fn make_conflict_free(
    repo: &gix::Repository,
    backup: &Path,
    keep_existing_backup: bool,
    worktree: gix::ObjectId,
) -> Result<bool, OplogError> {
    let lock = lock(repo)?;
    let file = read(repo)?;
    let conflicted: Vec<BString> = {
        let mut paths: Vec<BString> = file
            .entries()
            .iter()
            .filter(|e| e.stage() != Stage::Unconflicted)
            .map(|e| e.path(&file).to_owned())
            .collect();
        paths.dedup();
        paths
    };
    if conflicted.is_empty() {
        return Ok(false);
    }
    if !(keep_existing_backup && backup.is_file()) {
        let bytes = std::fs::read(repo.index_path())
            .with_context(|| format!("reading '{}'", repo.index_path().display()))?;
        let dir = backup
            .parent()
            .context("the index backup has a parent directory")?;
        super::journal::write_atomically(dir, backup, &bytes)?;
    }
    let tree = repo.find_tree(worktree).map_err(anyhow::Error::from)?;
    let (mut state, path) = file.into_parts();
    let mut edit = Edit::new(&mut state);
    for rela in &conflicted {
        match tree
            .lookup_entry(rela.split_str("/"))
            .map_err(anyhow::Error::from)?
        {
            Some(entry) if !entry.mode().is_tree() => {
                edit.upsert(
                    rela.as_bstr(),
                    entry.object_id(),
                    entry.mode().into(),
                    Stat::Unknown,
                );
            }
            _ => edit.remove(rela.as_bstr()),
        }
    }
    edit.finish();
    write(state, &path, lock)?;
    Ok(true)
}

/// Put the index backup of step C2 back in place (if it exists) and delete it. Returns `true` if it existed.
pub(super) fn restore_backup(repo: &gix::Repository, backup: &Path) -> Result<bool, OplogError> {
    let bytes = match std::fs::read(backup) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(OplogError::Other(
                anyhow::Error::from(err).context(format!("reading '{}'", backup.display())),
            ));
        }
    };
    let mut lock = lock(repo)?;
    std::io::Write::write_all(&mut lock, &bytes).map_err(anyhow::Error::from)?;
    lock.commit()
        .map_err(|err| anyhow::Error::from(err.error).context("restoring the index backup"))?;
    super::journal::remove_file(backup)?;
    Ok(true)
}

/// Step C8: rebuild the index from `new_index`, carry the stat of unchanged, non-racy entries over from the current
/// (post-checkout) index so the next status doesn't re-hash every file, and re-apply the captured `flags`
/// (`SKIP_WORKTREE`, `ASSUME_UNCHANGED`, and `INTENT_TO_ADD` on entries that still are intent-to-add).
pub(super) fn write_restored(
    repo: &gix::Repository,
    new_index: NewIndex,
    flags: &BTreeMap<BString, u16>,
) -> Result<(), OplogError> {
    let lock = lock(repo)?;
    let (mut state, path) = repo
        .index_from_tree(&new_index.tree)
        .map_err(anyhow::Error::from)?
        .into_parts();
    if let Some(conflicts) = new_index.conflicts {
        but_core::snapshot::restore_conflicts_into_index(&mut state, conflicts.attach(repo))?;
    }
    let previous = read(repo)?;
    let timestamp = previous.timestamp();
    let stat_options = repo.stat_options().map_err(anyhow::Error::from)?;
    for (entry, rela) in state.entries_mut_with_paths() {
        if entry.stage() != Stage::Unconflicted {
            continue;
        }
        if let Some(prev) = previous.entry_by_path_and_stage(rela, Stage::Unconflicted)
            && prev.id == entry.id
            && prev.mode == entry.mode
            && !prev.stat.is_racy(timestamp, stat_options)
        {
            entry.stat = prev.stat;
        }
    }

    let empty_blob = gix::ObjectId::empty_blob(repo.object_hash());
    let mut missing_intent_to_add = Vec::new();
    for (rela, bits) in flags {
        let kept = bits & (FLAG_SKIP_WORKTREE | FLAG_ASSUME_UNCHANGED);
        match state.entry_mut_by_path_and_stage(rela.as_bstr(), Stage::Unconflicted) {
            Some(entry) => {
                let mut add = index_flags_from_bits(kept);
                if bits & FLAG_INTENT_TO_ADD != 0 && entry.id == empty_blob {
                    add |= Flags::INTENT_TO_ADD;
                }
                if add.intersects(Flags::INTENT_TO_ADD | Flags::SKIP_WORKTREE) {
                    add |= Flags::EXTENDED;
                }
                entry.flags |= add;
            }
            None if bits & FLAG_INTENT_TO_ADD != 0 => missing_intent_to_add.push(rela.clone()),
            None => {}
        }
    }
    if !missing_intent_to_add.is_empty() {
        // An intent-to-add entry has no content yet, so the index tree of the snapshot doesn't list it.
        repo.write_blob(b"").map_err(anyhow::Error::from)?;
        for rela in &missing_intent_to_add {
            let bits = flags.get(rela).copied().unwrap_or(FLAG_INTENT_TO_ADD);
            let entry_flags = index_flags_from_bits(bits) | Flags::EXTENDED;
            state.dangerously_push_entry(
                gix::index::entry::Stat::default(),
                empty_blob,
                entry_flags,
                Mode::FILE,
                rela.as_bstr(),
            );
        }
        state.sort_entries();
    }
    write(state, &path, lock)?;
    Ok(())
}
