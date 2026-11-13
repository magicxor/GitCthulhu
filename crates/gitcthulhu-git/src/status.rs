//! Working tree status (WP-B1).
//!
//! Computed in-process with gix; the index is never written (see [`status()`] and [`refresh_index()`]).

use std::{collections::HashSet, path::Path};

use bstr::{BString, ByteSlice};
use gix::{
    ObjectId,
    dir::entry,
    status::{
        self, index_worktree,
        index_worktree::RewriteSource,
        plumbing::index_as_worktree::{self, ConflictIndexEntry, EntryStatus},
        tree_index::TrackRenames,
    },
};

use crate::{
    CancelToken, ChangeKind, ConflictInfo, ConflictKind, EntryKind, FileStamp, GitError,
    KindCounts, OpClass, OpContext, PostState, RenameMode, Repo, RepoExclusive, Result, Side,
    Status, StatusCounts, StatusEntry, StatusFingerprint, StatusOptions, SubmoduleChange,
    SubmoduleStatusMode, UntrackedMode,
    cli::{self, CommandKind, GitCommand},
    error::classify,
    internal::errors::gix_error,
    watch::file_stamp,
};

impl Default for StatusOptions {
    fn default() -> Self {
        StatusOptions {
            untracked: UntrackedMode::Files,
            renames: RenameMode::IndexAndWorktree { limit: 1000 },
            submodules: SubmoduleStatusMode::HeadOnly,
            pathspec: Vec::new(),
        }
    }
}

/// Staged and unstaged changes, computed in-process with gix. Never writes the index.
///
/// `index_needs_refresh` reports whether gix found entries with outdated stat data; the caller may then run
/// [`refresh_index()`] (writing the index from here would silently undo a `git add` made while the status ran).
///
/// An index with sparse directory entries (`index.sparse`, Scalar's default) cannot be compared with a tree by gix:
/// there the status comes from `git status --porcelain=v2` (a read; `index_needs_refresh` is then `false`).
///
/// Errors: `BareRepository`, `Cancelled`, `Other` (gix failures), `Io`, and the classified failure of
/// `git status`.
pub fn status(repo: &Repo, opts: &StatusOptions, cancel: &CancelToken) -> Result<Status> {
    let workdir = repo.workdir().ok_or(GitError::BareRepository)?;
    cancel.check()?;
    let local = repo.local();
    // Taken *before* the index and HEAD are read: a change made while the status runs then leaves a fingerprint
    // that no longer matches, so the caller sees the result as outdated instead of trusting it.
    let fingerprint = StatusFingerprint {
        index: file_stamp(&repo.git_dir().join("index")),
        head: local.head_id().ok().map(|id| id.detach()),
    };

    let sparse_index = local.index_or_empty().map_err(gix_error)?.is_sparse();
    let (mut staged, mut unstaged, index_needs_refresh) = if sparse_index {
        let (staged, unstaged) = cli_status(repo, workdir, opts, cancel)?;
        (staged, unstaged, false)
    } else {
        gix_status(&local, workdir, opts, cancel)?
    };

    if opts.submodules == SubmoduleStatusMode::HeadOnly {
        drop_submodules_ignoring_all(&local, &mut unstaged);
    }

    // Conflicted paths are reported once, as unstaged conflicts. The dirwalk of gix looks paths up in the index with
    // `entry_by_path`, which only finds stage 0 or 2: a path that only has a "theirs" entry (added by them) is walked
    // as untracked as well on case-sensitive file systems. Git never calls an unmerged path untracked.
    if unstaged.iter().any(|e| e.kind == ChangeKind::Conflicted) {
        let conflicted: HashSet<BString> = unstaged
            .iter()
            .filter(|e| e.kind == ChangeKind::Conflicted)
            .map(|e| e.path.clone())
            .collect();
        staged.retain(|e| !conflicted.contains(&e.path));
        unstaged.retain(|e| e.kind != ChangeKind::Untracked || !conflicted.contains(&e.path));
    }
    staged.sort_by(|a, b| a.path.cmp(&b.path));
    unstaged.sort_by(|a, b| a.path.cmp(&b.path));
    let counts = StatusCounts {
        staged: count(&staged),
        unstaged: count(&unstaged),
    };
    Ok(Status {
        staged,
        unstaged,
        counts,
        fingerprint,
        index_needs_refresh,
    })
}

/// The staged and unstaged entries plus `index_needs_refresh`, computed by gix.
fn gix_status(
    local: &gix::Repository,
    workdir: &Path,
    opts: &StatusOptions,
    cancel: &CancelToken,
) -> Result<(Vec<StatusEntry>, Vec<StatusEntry>, bool)> {
    let null = local.object_hash().null();
    let rewrites = gix::diff::Rewrites {
        copies: None,
        percentage: Some(0.5),
        limit: match opts.renames {
            RenameMode::IndexAndWorktree { limit } => limit,
            RenameMode::IndexOnly | RenameMode::Off => 1000,
        },
        track_empty: false,
    };
    let (tree_index_renames, worktree_rewrites) = match opts.renames {
        RenameMode::Off => (TrackRenames::Disabled, None),
        RenameMode::IndexOnly => (TrackRenames::Given(rewrites), None),
        RenameMode::IndexAndWorktree { .. } => (TrackRenames::Given(rewrites), Some(rewrites)),
    };
    let submodules = match opts.submodules {
        SubmoduleStatusMode::Ignore => status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::All,
            check_dirty: false,
        },
        // Only the checked-out commit is compared, which `ignore = dirty` gives cheaply for every submodule; the ones
        // configured with `ignore = all` are dropped afterwards (`drop_submodules_ignoring_all`).
        SubmoduleStatusMode::HeadOnly => status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::Dirty,
            check_dirty: false,
        },
        // `submodule.<name>.ignore` decides how far each submodule is looked at, as in Git.
        SubmoduleStatusMode::HeadAndDirty => status::Submodule::AsConfigured { check_dirty: true },
    };
    let (untracked, emission) = match opts.untracked {
        UntrackedMode::Files => (
            status::UntrackedFiles::Files,
            Some(gix::dir::walk::EmissionMode::Matching),
        ),
        UntrackedMode::Collapsed => (
            status::UntrackedFiles::Collapsed,
            Some(gix::dir::walk::EmissionMode::CollapseDirectory),
        ),
        UntrackedMode::None => (status::UntrackedFiles::None, None),
    };
    let patterns: Vec<BString> = opts
        .pathspec
        .iter()
        .map(|path| {
            let mut spec = BString::from(":(literal)");
            spec.extend_from_slice(path);
            spec
        })
        .collect();
    let platform = |worktree_rewrites: Option<gix::diff::Rewrites>| -> Result<_> {
        Ok(local
            .status(gix::progress::Discard)
            .map_err(gix_error)?
            .should_interrupt_owned(cancel.as_atomic())
            .tree_index_track_renames(tree_index_renames)
            .index_worktree_rewrites(worktree_rewrites)
            .index_worktree_submodules(submodules)
            .untracked_files(untracked)
            .index_worktree_options_mut(|options| {
                if let (Some(dirwalk), Some(emission)) =
                    (options.dirwalk_options.as_mut(), emission)
                {
                    dirwalk
                        .set_emit_ignored(None)
                        .set_emit_pruned(false)
                        .set_emit_tracked(false)
                        .set_emit_untracked(emission)
                        .set_emit_collapsed(None);
                }
            }))
    };

    // gix hashes *every* untracked file as soon as worktree rewrites are tracked, even when nothing was deleted
    // (on Windows ~180 ms for 1k small files, and the full content of any large untracked file). A worktree rename
    // needs a deleted index entry and an untracked destination, so the first pass runs without worktree rewrites
    // and only a status that has both is repeated (index-worktree half only) with them.
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut iter = platform(None)?
        .into_iter(patterns.clone())
        .map_err(gix_error)?;
    for item in iter.by_ref() {
        let item = match item {
            Ok(item) => item,
            Err(err) => {
                cancel.check()?;
                return Err(gix_error(err));
            }
        };
        match item {
            status::Item::TreeIndex(change) => {
                if let Some(entry) = tree_index_entry(change)? {
                    staged.push(entry);
                }
            }
            status::Item::IndexWorktree(item) => {
                if let Some(entry) = index_worktree_entry(item, workdir, null, opts.submodules)? {
                    unstaged.push(entry);
                }
            }
        }
    }
    cancel.check()?;
    let mut index_needs_refresh = iter
        .outcome_mut()
        .is_some_and(|outcome| outcome.has_changes());
    drop(iter);

    if worktree_rewrites.is_some()
        && unstaged.iter().any(|e| e.kind == ChangeKind::Deleted)
        && unstaged.iter().any(|e| e.kind == ChangeKind::Untracked)
    {
        let mut iter = platform(worktree_rewrites)?
            .into_index_worktree_iter(patterns)
            .map_err(gix_error)?;
        unstaged.clear();
        for item in iter.by_ref() {
            let item = match item {
                Ok(item) => item,
                Err(err) => {
                    cancel.check()?;
                    return Err(gix_error(err));
                }
            };
            if let Some(entry) = index_worktree_entry(item, workdir, null, opts.submodules)? {
                unstaged.push(entry);
            }
        }
        cancel.check()?;
        index_needs_refresh |= iter
            .outcome_mut()
            .is_some_and(|outcome| outcome.has_changes());
    }
    Ok((staged, unstaged, index_needs_refresh))
}

/// The staged and unstaged entries from `git status --porcelain=v2 -z` (for a sparse index, see [`status()`]), with
/// the options mapped to Git's: untracked files `all` / `normal` / `no`, `--find-renames` / `--no-renames` (Git
/// tracks staged renames only), `--ignore-submodules=dirty` for `HeadOnly`, `=all` for `Ignore`.
fn cli_status(
    repo: &Repo,
    workdir: &Path,
    opts: &StatusOptions,
    cancel: &CancelToken,
) -> Result<(Vec<StatusEntry>, Vec<StatusEntry>)> {
    let mut cmd = GitCommand::new(repo, CommandKind::Read).args([
        "status",
        "--porcelain=v2",
        "-z",
        match opts.untracked {
            UntrackedMode::Files => "--untracked-files=all",
            UntrackedMode::Collapsed => "--untracked-files=normal",
            UntrackedMode::None => "--untracked-files=no",
        },
        match opts.renames {
            RenameMode::Off => "--no-renames",
            RenameMode::IndexOnly | RenameMode::IndexAndWorktree { .. } => "--find-renames",
        },
    ]);
    cmd = match opts.submodules {
        SubmoduleStatusMode::Ignore => cmd.arg("--ignore-submodules=all"),
        SubmoduleStatusMode::HeadOnly => cmd.arg("--ignore-submodules=dirty"),
        SubmoduleStatusMode::HeadAndDirty => cmd,
    };
    if !opts.pathspec.is_empty() {
        cmd = cmd.literal_paths_after_dashdash(&opts.pathspec);
    }
    let out = cli::run(repo.runtime(), cmd, &OpContext::silent(cancel.clone()))?;
    if !out.success() {
        return Err(classify(OpClass::Generic, &out, &PostState::default()));
    }
    let null = repo.local().object_hash().null();
    parse_porcelain_v2(&out.stdout, workdir, null, opts.submodules)
}

/// Parse `git status --porcelain=v2 -z` into staged and unstaged entries shaped like gix's (see
/// [`tree_index_entry`] and [`index_worktree_entry`]): `1` ordinary and `2` renamed records give a staged entry for
/// `X` and an unstaged one for `Y`, `u` a conflict, `?` an untracked path. Worktree sides have a null id; a moved
/// submodule's new commit is read from its `HEAD`.
fn parse_porcelain_v2(
    stdout: &[u8],
    workdir: &Path,
    null: ObjectId,
    submodules: SubmoduleStatusMode,
) -> Result<(Vec<StatusEntry>, Vec<StatusEntry>)> {
    let bad = |record: &[u8]| {
        GitError::Other(anyhow::anyhow!(
            "unexpected `git status --porcelain=v2` record: {:?}",
            record.as_bstr()
        ))
    };
    let mode = |record: &[u8], field: &[u8]| porcelain_kind(field).ok_or_else(|| bad(record));
    let id = |record: &[u8], field: &[u8]| ObjectId::from_hex(field).map_err(|_| bad(record));

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut records = stdout.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        match record[0] {
            b'1' | b'2' => {
                let renamed = record[0] == b'2';
                let f = porcelain_fields(record, if renamed { 10 } else { 9 })
                    .ok_or_else(|| bad(record))?;
                let (x, y) = (f[1][0], f[1][1]);
                let sub = f[2];
                let (m_head, m_index, m_worktree) = (
                    mode(record, f[3])?,
                    mode(record, f[4])?,
                    mode(record, f[5])?,
                );
                let (h_head, h_index) = (id(record, f[6])?, id(record, f[7])?);
                let path = BString::from(f[if renamed { 9 } else { 8 }]);
                let previous_path = if renamed {
                    Some(BString::from(records.next().ok_or_else(|| bad(record))?))
                } else {
                    None
                };
                let head_side = m_head.map(|kind| side(h_head, kind));
                let index_side = m_index.map(|kind| side(h_index, kind));
                let worktree_side = m_worktree.map(|kind| side(null, kind));
                if x != b'.' {
                    let kind = match x {
                        b'A' | b'C' => ChangeKind::Added,
                        b'D' => ChangeKind::Deleted,
                        b'R' => ChangeKind::Renamed,
                        b'T' => ChangeKind::TypeChanged,
                        b'M' => match head_side.zip(index_side) {
                            Some((old, new)) if kind_changed(old.kind, new.kind) => {
                                ChangeKind::TypeChanged
                            }
                            _ => ChangeKind::Modified,
                        },
                        _ => return Err(bad(record)),
                    };
                    let (old, new) = match kind {
                        ChangeKind::Added => (None, index_side),
                        ChangeKind::Deleted => (head_side, None),
                        _ => (head_side, index_side),
                    };
                    let mut entry = plain(path.clone(), kind, old, new);
                    if kind == ChangeKind::Renamed {
                        entry.previous_path = previous_path.clone();
                    }
                    if let (Some(old), Some(new)) = (old, new)
                        && old.kind == EntryKind::Submodule
                        && new.kind == EntryKind::Submodule
                    {
                        entry.submodule = Some(SubmoduleChange {
                            old_commit: Some(old.id),
                            new_commit: Some(new.id),
                            dirty: None,
                        });
                    }
                    staged.push(entry);
                }
                let entry = match y {
                    b'.' => continue,
                    b'M' if sub.first() == Some(&b'S') => {
                        let Some(entry) =
                            porcelain_submodule(sub, workdir, path, h_index, submodules)
                        else {
                            continue;
                        };
                        entry
                    }
                    b'M' => plain(path, ChangeKind::Modified, index_side, worktree_side),
                    b'T' => plain(path, ChangeKind::TypeChanged, index_side, worktree_side),
                    b'D' => plain(path, ChangeKind::Deleted, index_side, None),
                    // `git add -N`: the index side is not a real entry yet.
                    b'A' => plain(
                        path,
                        ChangeKind::IntentToAdd,
                        None,
                        Some(side(null, m_worktree.unwrap_or(EntryKind::Blob))),
                    ),
                    // A rename onto an intent-to-add path.
                    b'R' => {
                        let mut entry = plain(path, ChangeKind::Renamed, None, worktree_side);
                        entry.previous_path = previous_path;
                        entry
                    }
                    _ => return Err(bad(record)),
                };
                unstaged.push(entry);
            }
            b'u' => {
                let f = porcelain_fields(record, 11).ok_or_else(|| bad(record))?;
                let stage = |m: &[u8], h: &[u8]| -> Result<Option<Side>> {
                    Ok(match mode(record, m)? {
                        Some(kind) => Some(side(id(record, h)?, kind)),
                        None => None,
                    })
                };
                let kind = match f[1] {
                    b"DD" => ConflictKind::BothDeleted,
                    b"AU" => ConflictKind::AddedByUs,
                    b"UD" => ConflictKind::DeletedByThem,
                    b"UA" => ConflictKind::AddedByThem,
                    b"DU" => ConflictKind::DeletedByUs,
                    b"AA" => ConflictKind::BothAdded,
                    b"UU" => ConflictKind::BothModified,
                    _ => return Err(bad(record)),
                };
                let mut entry = plain(BString::from(f[10]), ChangeKind::Conflicted, None, None);
                entry.conflict = Some(ConflictInfo {
                    kind,
                    base: stage(f[3], f[7])?,
                    ours: stage(f[4], f[8])?,
                    theirs: stage(f[5], f[9])?,
                });
                unstaged.push(entry);
            }
            b'?' => {
                let mut path = BString::from(record.get(2..).ok_or_else(|| bad(record))?);
                let full = workdir.join(os_path(&path));
                let kind = if path.ends_with(b"/") {
                    if full.join(".git").exists() {
                        path.pop();
                        EntryKind::Submodule
                    } else {
                        EntryKind::Tree
                    }
                } else {
                    match full.symlink_metadata() {
                        Ok(meta) if meta.file_type().is_symlink() => EntryKind::Link,
                        Ok(meta) if gix::fs::is_executable(&meta) => EntryKind::BlobExecutable,
                        _ => EntryKind::Blob,
                    }
                };
                unstaged.push(plain(
                    path,
                    ChangeKind::Untracked,
                    None,
                    Some(side(null, kind)),
                ));
            }
            b'!' | b'#' => {}
            _ => return Err(bad(record)),
        }
    }
    Ok((staged, unstaged))
}

/// The `count` space-separated fields of a record (the last one is the path, which may contain spaces); the second
/// is the two-letter `XY` state.
fn porcelain_fields(record: &[u8], count: usize) -> Option<Vec<&[u8]>> {
    let fields: Vec<&[u8]> = record.splitn(count, |b| *b == b' ').collect();
    (fields.len() == count && fields[1].len() == 2).then_some(fields)
}

/// The kind of a porcelain mode (octal); `Some(None)` for `000000` (no such side), `None` if it is no mode.
fn porcelain_kind(field: &[u8]) -> Option<Option<EntryKind>> {
    let mode = u32::from_str_radix(std::str::from_utf8(field).ok()?, 8).ok()?;
    Some(Some(match mode & 0o170000 {
        0 if mode == 0 => return Some(None),
        0o100000 if mode & 0o111 != 0 => EntryKind::BlobExecutable,
        0o100000 => EntryKind::Blob,
        0o120000 => EntryKind::Link,
        0o160000 => EntryKind::Submodule,
        0o040000 => EntryKind::Tree,
        _ => return None,
    }))
}

/// The unstaged entry of a submodule from its porcelain state `S<c><m><u>` (commit changed, tracked changes,
/// untracked files), filtered like gix's: a change that is only "dirty" shows in `HeadAndDirty` mode only.
fn porcelain_submodule(
    sub: &[u8],
    workdir: &Path,
    path: BString,
    index_commit: ObjectId,
    submodules: SubmoduleStatusMode,
) -> Option<StatusEntry> {
    let flag = |ix: usize, set: u8| sub.get(ix) == Some(&set);
    let moved = flag(1, b'C');
    let dirty = flag(2, b'M') || flag(3, b'U');
    let with_dirty = submodules == SubmoduleStatusMode::HeadAndDirty;
    if !moved && !(with_dirty && dirty) {
        return None;
    }
    let new_commit = if moved {
        gix::open(workdir.join(os_path(&path)))
            .ok()
            .and_then(|sm| sm.head_id().ok().map(gix::Id::detach))
            .unwrap_or_else(|| index_commit.kind().null())
    } else {
        index_commit
    };
    let mut out = plain(
        path,
        ChangeKind::Modified,
        Some(side(index_commit, EntryKind::Submodule)),
        Some(side(new_commit, EntryKind::Submodule)),
    );
    out.submodule = Some(SubmoduleChange {
        old_commit: Some(index_commit),
        new_commit: Some(new_commit),
        dirty: with_dirty.then_some(dirty),
    });
    Some(out)
}

/// Drop the work tree changes of submodules configured with `ignore = all` (`.gitmodules` or
/// `submodule.<name>.ignore`): Git never reports them as modified, and `git add` skips them, so such an entry could
/// never be staged. Staged changes stay, as in `git status`. An unreadable `.gitmodules` filters nothing.
fn drop_submodules_ignoring_all(repo: &gix::Repository, unstaged: &mut Vec<StatusEntry>) {
    if !unstaged.iter().any(|entry| entry.submodule.is_some()) {
        return;
    }
    let Ok(Some(submodules)) = repo.submodules() else {
        return;
    };
    let ignored: HashSet<BString> = submodules
        .filter(|sm| matches!(sm.ignore(), Ok(Some(gix::submodule::config::Ignore::All))))
        .filter_map(|sm| sm.path().ok())
        .collect();
    if !ignored.is_empty() {
        unstaged.retain(|entry| entry.submodule.is_none() || !ignored.contains(&entry.path));
    }
}

/// `git update-index -q --ignore-submodules --unmerged --refresh` (CommandKind::Mutation, no hooks, no progress).
/// Git takes `index.lock` *before* reading the index, so staging done concurrently by a terminal or IDE is never
/// lost. Returns the new `index` FileStamp (the GUI passes it to `RepoWatcher::ignore_index_stamp`, §7.3). Lock held
/// by another git => Ok(None), silently.
///
/// `update-index` applies its options in command-line order, so `-q`, `--ignore-submodules` and `--unmerged` must
/// precede `--refresh` to take effect. Without `--unmerged` an index with conflicts makes Git exit 1 ("needs merge")
/// although it refreshed and wrote the index.
pub fn refresh_index(
    repo: &Repo,
    perm: &mut RepoExclusive,
    cancel: &CancelToken,
) -> Result<Option<FileStamp>> {
    let _ = perm;
    if repo.workdir().is_none() {
        return Err(GitError::BareRepository);
    }
    let cmd = GitCommand::new(repo, CommandKind::Mutation).args(REFRESH_INDEX_ARGS);
    let ctx = OpContext::silent(cancel.clone());
    let out = cli::run(repo.runtime(), cmd, &ctx)?;
    if out.exit_code != Some(0) {
        if out.stderr.contains("index.lock") {
            return Ok(None);
        }
        return Err(classify(OpClass::Generic, &out, &PostState::default()));
    }
    Ok(file_stamp(&repo.git_dir().join("index")))
}

/// The arguments of [`refresh_index()`]; order matters (see there).
const REFRESH_INDEX_ARGS: [&str; 5] = [
    "update-index",
    "-q",
    "--ignore-submodules",
    "--unmerged",
    "--refresh",
];

fn count(entries: &[StatusEntry]) -> KindCounts {
    let mut counts = KindCounts::default();
    for entry in entries {
        match entry.kind {
            ChangeKind::Added | ChangeKind::IntentToAdd => counts.added += 1,
            ChangeKind::Modified | ChangeKind::TypeChanged => counts.modified += 1,
            ChangeKind::Deleted => counts.deleted += 1,
            ChangeKind::Renamed => counts.renamed += 1,
            ChangeKind::Untracked => counts.untracked += 1,
            ChangeKind::Conflicted => counts.conflicted += 1,
        }
    }
    counts
}

fn entry_kind(mode: gix::index::entry::Mode) -> Result<EntryKind> {
    mode.to_tree_entry_mode()
        .map(|mode| EntryKind::from(mode.kind()))
        .ok_or_else(|| {
            GitError::Other(anyhow::anyhow!("index entry has an invalid mode: {mode:?}"))
        })
}

/// Skips untrackable entries (named pipes, sockets, devices) like Git. `path` is stat'ed again for the executable
/// bit.
fn disk_kind_to_entry_kind(
    disk_kind: Option<entry::Kind>,
    index_kind: Option<entry::Kind>,
    path: &Path,
) -> Option<EntryKind> {
    Some(match disk_kind.or(index_kind)? {
        entry::Kind::Repository => EntryKind::Submodule,
        entry::Kind::Directory => EntryKind::Tree,
        entry::Kind::Untrackable => return None,
        // The file can vanish after the walk listed it; Git doesn't notice either.
        entry::Kind::File => match path.symlink_metadata() {
            Ok(meta) if gix::fs::is_executable(&meta) => EntryKind::BlobExecutable,
            _ => EntryKind::Blob,
        },
        entry::Kind::Symlink => EntryKind::Link,
    })
}

fn is_file(kind: EntryKind) -> bool {
    matches!(kind, EntryKind::Blob | EntryKind::BlobExecutable)
}

/// The sides are entries of different types (file / symlink / submodule); the executable bit is no type.
fn kind_changed(old: EntryKind, new: EntryKind) -> bool {
    old != new && !(is_file(old) && is_file(new))
}

fn side(id: ObjectId, kind: EntryKind) -> Side {
    Side { id, kind }
}

fn plain(path: BString, kind: ChangeKind, old: Option<Side>, new: Option<Side>) -> StatusEntry {
    StatusEntry {
        path,
        previous_path: None,
        kind,
        old,
        new,
        conflict: None,
        submodule: None,
    }
}

fn os_path(path: &BString) -> std::path::PathBuf {
    match gix::path::from_bstr(path.as_bstr()) {
        Ok(path) => path.into_owned(),
        // Only on Windows: a name that is not UTF-8 cannot exist on disk there; the lossy path just won't be found.
        Err(_) => std::path::PathBuf::from(path.to_str_lossy().into_owned()),
    }
}

fn tree_index_entry(change: gix::diff::index::Change) -> Result<Option<StatusEntry>> {
    use gix::diff::index::Change;
    Ok(Some(match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } => plain(
            location.into_owned(),
            ChangeKind::Added,
            None,
            Some(side(id.into_owned(), entry_kind(entry_mode)?)),
        ),
        Change::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => plain(
            location.into_owned(),
            ChangeKind::Deleted,
            Some(side(id.into_owned(), entry_kind(entry_mode)?)),
            None,
        ),
        Change::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => {
            let old = side(previous_id.into_owned(), entry_kind(previous_entry_mode)?);
            let new = side(id.into_owned(), entry_kind(entry_mode)?);
            let kind = if kind_changed(old.kind, new.kind) {
                ChangeKind::TypeChanged
            } else {
                ChangeKind::Modified
            };
            let mut entry = plain(location.into_owned(), kind, Some(old), Some(new));
            if old.kind == EntryKind::Submodule && new.kind == EntryKind::Submodule {
                entry.submodule = Some(SubmoduleChange {
                    old_commit: Some(old.id),
                    new_commit: Some(new.id),
                    dirty: None,
                });
            }
            entry
        }
        Change::Rewrite {
            source_location,
            location,
            source_entry_mode,
            source_id,
            entry_mode,
            id,
            ..
        } => {
            let mut entry = plain(
                location.into_owned(),
                ChangeKind::Renamed,
                Some(side(source_id.into_owned(), entry_kind(source_entry_mode)?)),
                Some(side(id.into_owned(), entry_kind(entry_mode)?)),
            );
            entry.previous_path = Some(source_location.into_owned());
            entry
        }
    }))
}

fn conflict_side(entry: &ConflictIndexEntry) -> Option<Side> {
    entry_kind(entry.mode).ok().map(|kind| side(entry.id, kind))
}

fn conflict_info(entries: &[Option<ConflictIndexEntry>; 3]) -> ConflictInfo {
    let present = [
        entries[0].is_some(),
        entries[1].is_some(),
        entries[2].is_some(),
    ];
    let kind = match present {
        [true, true, true] => ConflictKind::BothModified,
        [false, true, true] => ConflictKind::BothAdded,
        [true, false, false] => ConflictKind::BothDeleted,
        [false, true, false] => ConflictKind::AddedByUs,
        [false, false, true] => ConflictKind::AddedByThem,
        [true, false, true] => ConflictKind::DeletedByUs,
        [true, true, false] => ConflictKind::DeletedByThem,
        // gix does not report a conflict without any stage.
        [false, false, false] => ConflictKind::BothDeleted,
    };
    ConflictInfo {
        kind,
        base: entries[0].as_ref().and_then(conflict_side),
        ours: entries[1].as_ref().and_then(conflict_side),
        theirs: entries[2].as_ref().and_then(conflict_side),
    }
}

fn index_worktree_entry(
    item: index_worktree::Item,
    workdir: &Path,
    null: ObjectId,
    submodules: SubmoduleStatusMode,
) -> Result<Option<StatusEntry>> {
    Ok(Some(match item {
        index_worktree::Item::Modification {
            rela_path,
            entry,
            status,
            ..
        } => match status {
            EntryStatus::Conflict { entries, .. } => {
                let mut out = plain(rela_path, ChangeKind::Conflicted, None, None);
                out.conflict = Some(conflict_info(&entries));
                out
            }
            EntryStatus::IntentToAdd => plain(
                rela_path,
                ChangeKind::IntentToAdd,
                None,
                Some(side(null, entry_kind(entry.mode)?)),
            ),
            EntryStatus::NeedsUpdate(_) => return Ok(None),
            EntryStatus::Change(change) => {
                let old_kind = entry_kind(entry.mode)?;
                let old = Some(side(entry.id, old_kind));
                match change {
                    index_as_worktree::Change::Removed => {
                        plain(rela_path, ChangeKind::Deleted, old, None)
                    }
                    index_as_worktree::Change::Type { worktree_mode } => plain(
                        rela_path,
                        ChangeKind::TypeChanged,
                        old,
                        Some(side(null, entry_kind(worktree_mode)?)),
                    ),
                    index_as_worktree::Change::Modification {
                        executable_bit_changed,
                        ..
                    } => {
                        let new_kind = match (executable_bit_changed, old_kind) {
                            (true, EntryKind::BlobExecutable) => EntryKind::Blob,
                            (true, _) => EntryKind::BlobExecutable,
                            (false, kind) => kind,
                        };
                        plain(
                            rela_path,
                            ChangeKind::Modified,
                            old,
                            Some(side(null, new_kind)),
                        )
                    }
                    index_as_worktree::Change::SubmoduleModification(change) => {
                        let moved = change.checked_out_head_id.filter(|head| *head != entry.id);
                        // Whether the submodule's own work tree or index has changes. gix's `is_dirty()` also
                        // counts a moved HEAD, and with `check_dirty` it skips the work tree scan once the HEAD
                        // moved (`changes` stays `None`): that is "unknown", not "dirty".
                        let dirty = change.changes.as_ref().map(|changes| !changes.is_empty());
                        let with_dirty = submodules == SubmoduleStatusMode::HeadAndDirty;
                        // A change that is only "dirty" (same commit) shows up in `HeadAndDirty` mode only.
                        if moved.is_none() && !(with_dirty && dirty == Some(true)) {
                            return Ok(None);
                        }
                        let new_commit = moved.unwrap_or(entry.id);
                        let mut out = plain(
                            rela_path,
                            ChangeKind::Modified,
                            old,
                            Some(side(new_commit, EntryKind::Submodule)),
                        );
                        out.submodule = Some(SubmoduleChange {
                            old_commit: Some(entry.id),
                            new_commit: Some(new_commit),
                            dirty: if with_dirty { dirty } else { None },
                        });
                        out
                    }
                }
            }
        },
        index_worktree::Item::DirectoryContents {
            entry:
                gix::dir::Entry {
                    rela_path,
                    disk_kind,
                    index_kind,
                    status: entry::Status::Untracked,
                    ..
                },
            ..
        } => {
            let Some(kind) =
                disk_kind_to_entry_kind(disk_kind, index_kind, &workdir.join(os_path(&rela_path)))
            else {
                return Ok(None);
            };
            let mut path = rela_path;
            if kind == EntryKind::Tree && !path.ends_with(b"/") {
                path.push(b'/');
            }
            plain(path, ChangeKind::Untracked, None, Some(side(null, kind)))
        }
        index_worktree::Item::DirectoryContents { .. } => return Ok(None),
        index_worktree::Item::Rewrite {
            source,
            dirwalk_entry,
            ..
        } => {
            let previous_path: BString = source.rela_path().into();
            let old = match source {
                RewriteSource::RewriteFromIndex { source_entry, .. } => {
                    side(source_entry.id, entry_kind(source_entry.mode)?)
                }
                RewriteSource::CopyFromDirectoryEntry {
                    source_dirwalk_entry,
                    source_dirwalk_entry_id,
                    ..
                } => {
                    let Some(kind) = disk_kind_to_entry_kind(
                        source_dirwalk_entry.disk_kind,
                        source_dirwalk_entry.index_kind,
                        &workdir.join(os_path(&previous_path)),
                    ) else {
                        return Ok(None);
                    };
                    side(source_dirwalk_entry_id, kind)
                }
            };
            let Some(kind) = disk_kind_to_entry_kind(
                dirwalk_entry.disk_kind,
                dirwalk_entry.index_kind,
                &workdir.join(os_path(&dirwalk_entry.rela_path)),
            ) else {
                return Ok(None);
            };
            let mut out = plain(
                dirwalk_entry.rela_path,
                ChangeKind::Renamed,
                Some(old),
                Some(side(null, kind)),
            );
            out.previous_path = Some(previous_path);
            out
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::REFRESH_INDEX_ARGS;

    #[test]
    fn refresh_index_options_precede_refresh() {
        // `git update-index` acts on `--refresh` when it parses it: options after it are ignored for the refresh.
        assert_eq!(
            REFRESH_INDEX_ARGS.last(),
            Some(&"--refresh"),
            "--refresh comes last: {REFRESH_INDEX_ARGS:?}"
        );
        for option in ["-q", "--ignore-submodules", "--unmerged"] {
            assert!(
                REFRESH_INDEX_ARGS.contains(&option),
                "{option} is passed: {REFRESH_INDEX_ARGS:?}"
            );
        }
    }
}
