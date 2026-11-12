//! Staging, unstaging, discarding and conflict resolution (WP-B3; spec 20 §2.6 `stage.rs` and §4).
//!
//! Whole-file operations run the `git` executable (`git add`, `git restore`, `git rm`, `git checkout --ours`), so
//! filters, LFS, submodules, intent-to-add entries and sparse checkouts behave natively. Every path reaches Git as a
//! `:(literal)` pathspec through stdin.
//!
//! Hunk and line operations are an in-process index edit (§4): the new index blob is the old image plus the selected
//! line edits, written under `index.lock` after a stale check against the ids the displayed diff was computed from.
//! Discarding selected lines writes the worktree file back through the filter pipeline, refusing when the worktree
//! conversion is not lossless.

use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use bstr::{BStr, ByteSlice};
use but_core::index::{Edit, Stat};
use gix::{
    ObjectId,
    index::entry::{Mode, Stage},
};

use crate::{
    CommandLog, DiscardMode, EntryKind, GitError, OpClass, OpContext, OpOutcome, OpWarning,
    PartialBlock, RefreshSet, RelaPath, Repo, RepoExclusive, Resolution, Result, Selection,
    SelectionRequest,
    cli::{self, CommandKind, GitCommand},
};

/// Either side above this size (git form) blocks partial operations (`DiffOptions::max_file_bytes` default).
const MAX_PARTIAL_BYTES: u64 = 50 * 1024 * 1024;
/// More lines than this on either side blocks partial operations (`DiffOptions::max_lines` default).
const MAX_PARTIAL_LINES: usize = 1_000_000;

// ───────────────────────────── whole-file operations (git CLI) ─────────────────────────────

/// Stage whole files and directories: `git add -A` of the literal `paths` (deletions are staged too). For a rename
/// the GUI passes both the old and the new path.
///
/// Paths outside the worktree are refused with [`GitError::InvalidName`]; ignored untracked paths are refused by
/// Git (`CommandFailed` with its stderr). Refresh: `STATUS`.
pub fn stage_paths(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    require_workdir(repo)?;
    validate_paths(paths)?;
    let mut run = Run::default();
    if !paths.is_empty() {
        run.add_all(repo, ctx, paths)?;
    }
    Ok(run.outcome(RefreshSet::STATUS))
}

/// Stage everything: `git add -A`. Refresh: `STATUS`.
pub fn stage_all(repo: &Repo, _perm: &mut RepoExclusive, ctx: &OpContext) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    require_workdir(repo)?;
    let mut run = Run::default();
    run.git(
        repo,
        ctx,
        GitCommand::new(repo, CommandKind::Mutation).args(["add", "-A"]),
        OpClass::Generic,
    )?;
    Ok(run.outcome(RefreshSet::STATUS))
}

/// Unstage whole files: `git restore --staged` (born HEAD) or `git rm --cached -r -q` (unborn HEAD). A staged rename
/// passes both paths.
///
/// Conflicted paths are refused with `PartialNotPossible { reason: Conflict }`; use [`resolve_conflict`] or
/// [`discard_paths`] for them. Refresh: `STATUS`.
pub fn unstage_paths(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    require_workdir(repo)?;
    validate_paths(paths)?;
    let mut run = Run::default();
    if paths.is_empty() {
        return Ok(run.outcome(RefreshSet::STATUS));
    }
    let gix = repo.local();
    let index = read_index(&gix)?;
    if let Some(path) = paths.iter().find(|p| is_conflicted(&index, p.as_ref())) {
        return Err(GitError::PartialNotPossible {
            path: path.clone(),
            reason: PartialBlock::Conflict,
        });
    }
    let cmd = if head_is_unborn(&gix)? {
        GitCommand::new(repo, CommandKind::Mutation).args(["rm", "--cached", "-r", "-q"])
    } else {
        GitCommand::new(repo, CommandKind::Mutation).args(["restore", "--staged"])
    };
    run.git(repo, ctx, cmd.pathspecs(paths), OpClass::Restore)?;
    Ok(run.outcome(RefreshSet::STATUS))
}

/// Unstage everything: `git reset -q -- :(literal).` (born HEAD) or `git rm --cached -r -q -- :(literal).` (unborn
/// HEAD). Refresh: `STATUS`.
///
/// The born-HEAD form passes a pathspec on purpose: a pathless `git reset` also deletes `MERGE_HEAD`,
/// `CHERRY_PICK_HEAD` and friends, so "unstage all" in the middle of a merge would silently turn the merge into an
/// ordinary commit. Like [`unstage_paths`], conflicted paths are refused (`PartialNotPossible { reason: Conflict }`):
/// the reset would drop their conflict stages.
pub fn unstage_all(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    require_workdir(repo)?;
    let gix = repo.local();
    let index = read_index(&gix)?;
    let mut run = Run::default();
    // Both commands fail on a pathspec that matches nothing, and an empty index has nothing to unstage.
    let unborn = head_is_unborn(&gix)?;
    if unborn && index.entries().is_empty() {
        return Ok(run.outcome(RefreshSet::STATUS));
    }
    if let Some(entry) = index
        .entries()
        .iter()
        .find(|entry| entry.flags.stage() != Stage::Unconflicted)
    {
        return Err(GitError::PartialNotPossible {
            path: entry.path(&index).to_owned(),
            reason: PartialBlock::Conflict,
        });
    }
    let cmd = if unborn {
        GitCommand::new(repo, CommandKind::Mutation).args(["rm", "--cached", "-r", "-q"])
    } else {
        GitCommand::new(repo, CommandKind::Mutation).args(["reset", "-q"])
    };
    let class = if unborn {
        OpClass::Generic
    } else {
        OpClass::Reset
    };
    run.git(
        repo,
        ctx,
        cmd.literal_paths_after_dashdash(&[".".into()]),
        class,
    )?;
    Ok(run.outcome(RefreshSet::STATUS))
}

/// Throw away changes of whole files.
///
/// The paths are split by the index:
/// - conflicted paths (stage ≠ 0) are reset to HEAD in both modes (`git restore --source=HEAD --staged --worktree`;
///   `git restore --worktree` refuses unmerged paths);
/// - untracked paths are deleted like [`delete_untracked`];
/// - other paths: [`DiscardMode::Unstaged`] restores the worktree from the index (`git restore --worktree`);
///   [`DiscardMode::StagedAndUnstaged`] restores index and worktree from HEAD (`git restore --source=HEAD --staged
///   --worktree`, which deletes staged new files), or with an unborn HEAD removes the entries from the index
///   (`git rm --cached -r -q`) and deletes their files.
///
/// Refresh: `STATUS`, plus `REPO_STATE` if a conflicted path was discarded.
pub fn discard_paths(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    mode: DiscardMode,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    let workdir = require_workdir(repo)?.to_owned();
    validate_paths(paths)?;
    let gix = repo.local();
    let index = read_index(&gix)?;
    let tracked_lookup = TrackedLookup::new(&gix, &index)?;

    let mut conflicted = Vec::new();
    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    for path in paths {
        let path_ref: &BStr = path.as_ref();
        if is_conflicted(&index, path_ref) {
            conflicted.push(path.clone());
        } else if tracked_lookup.is_tracked(path_ref) {
            // A path that is tracked only under another letter case (`core.ignoreCase`) lands here too: Git then
            // refuses the pathspec instead of the file being deleted as untracked.
            tracked.push(path.clone());
        } else {
            untracked.push(path.clone());
        }
    }
    // Verify every untracked path before anything is changed.
    let untracked_targets = untracked
        .iter()
        .map(|path| verified_worktree_path(&workdir, path.as_ref()))
        .collect::<Result<Vec<_>>>()?;

    let mut run = Run::default();
    if !conflicted.is_empty() {
        run.restore_from_head(repo, ctx, &conflicted)?;
    }
    if !tracked.is_empty() {
        match mode {
            DiscardMode::Unstaged => {
                let cmd = GitCommand::new(repo, CommandKind::Mutation)
                    .args(["restore", "--worktree"])
                    .pathspecs(&tracked);
                run.git(repo, ctx, cmd, OpClass::Restore)?;
            }
            DiscardMode::StagedAndUnstaged => {
                if head_is_unborn(&gix)? {
                    // Exactly the files the index knows below the given paths; untracked neighbours stay.
                    let files = index_files_below(&index, &tracked);
                    let targets = files
                        .iter()
                        .map(|path| verified_worktree_path(&workdir, path.as_ref()))
                        .collect::<Result<Vec<_>>>()?;
                    let cmd = GitCommand::new(repo, CommandKind::Mutation)
                        .args(["rm", "--cached", "-r", "-q"])
                        .pathspecs(&tracked);
                    run.git(repo, ctx, cmd, OpClass::Generic)?;
                    for target in &targets {
                        remove_worktree_item(target)?;
                    }
                } else {
                    run.restore_from_head(repo, ctx, &tracked)?;
                }
            }
        }
    }
    for target in &untracked_targets {
        remove_worktree_item(target)?;
    }
    let refresh = if conflicted.is_empty() {
        RefreshSet::STATUS
    } else {
        RefreshSet::STATUS | RefreshSet::REPO_STATE
    };
    Ok(run.outcome(refresh))
}

/// Delete untracked files and directories with `std::fs`, after verifying that every path is untracked (no index
/// entry at or below it) and inside the worktree (no `..`, no `.git`, no symlink escape). Nothing is deleted if any
/// path fails the checks; a tracked path is refused with `InvalidName { what: "untracked path" }`.
/// Refresh: `STATUS`.
pub fn delete_untracked(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    let workdir = require_workdir(repo)?.to_owned();
    validate_paths(paths)?;
    let gix = repo.local();
    let index = read_index(&gix)?;
    let tracked_lookup = TrackedLookup::new(&gix, &index)?;
    let mut targets = Vec::with_capacity(paths.len());
    for path in paths {
        if tracked_lookup.is_tracked(path.as_ref()) || is_conflicted(&index, path.as_ref()) {
            return Err(GitError::InvalidName {
                what: "untracked path",
                name: path.to_str_lossy().into_owned(),
            });
        }
        targets.push(verified_worktree_path(&workdir, path.as_ref())?);
    }
    for target in &targets {
        remove_worktree_item(target)?;
    }
    Ok(Run::default().outcome(RefreshSet::STATUS))
}

/// Resolve conflicted paths.
///
/// - `Ours` / `Theirs`: `git checkout --ours|--theirs` then `git add -A`; a path without that side (deleted by us /
///   deleted by them) is removed with `git rm -q` instead. A path that is no longer conflicted is only staged, so its
///   worktree content (e.g. a manual resolution) is kept.
/// - `MarkResolved`: `git add -A` (Git allows this even if conflict markers remain; the GUI warns first).
/// - `Delete`: `git rm -q`.
///
/// Refresh: `STATUS | REPO_STATE`.
pub fn resolve_conflict(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    how: Resolution,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    require_workdir(repo)?;
    validate_paths(paths)?;
    let refresh = RefreshSet::STATUS | RefreshSet::REPO_STATE;
    let mut run = Run::default();
    if paths.is_empty() {
        return Ok(run.outcome(refresh));
    }
    match how {
        Resolution::Ours | Resolution::Theirs => {
            let (flag, stage) = if how == Resolution::Ours {
                ("--ours", Stage::Ours)
            } else {
                ("--theirs", Stage::Theirs)
            };
            let gix = repo.local();
            let index = read_index(&gix)?;
            let mut checkout = Vec::new();
            let mut remove = Vec::new();
            let mut add_only = Vec::new();
            for path in paths {
                let path_ref: &BStr = path.as_ref();
                if !is_conflicted(&index, path_ref) {
                    // Already resolved (or never conflicted): `git checkout --ours` would check such a path out
                    // from the index and silently drop its worktree changes, so it is only staged.
                    add_only.push(path.clone());
                } else if index.entry_by_path_and_stage(path_ref, stage).is_some() {
                    checkout.push(path.clone());
                } else {
                    remove.push(path.clone());
                }
            }
            if !checkout.is_empty() {
                let cmd = GitCommand::new(repo, CommandKind::Mutation)
                    .args(["checkout", flag])
                    .pathspecs(&checkout);
                run.git(repo, ctx, cmd, OpClass::Checkout)?;
                add_only.extend(checkout);
            }
            if !add_only.is_empty() {
                run.add_all(repo, ctx, &add_only)?;
            }
            if !remove.is_empty() {
                run.rm(repo, ctx, &remove)?;
            }
        }
        Resolution::MarkResolved => run.add_all(repo, ctx, paths)?,
        Resolution::Delete => run.rm(repo, ctx, paths)?,
    }
    Ok(run.outcome(refresh))
}

// ───────────────────────────── partial operations (in-process, §4) ─────────────────────────────

/// Stage selected hunks or lines of the unstaged diff (index → worktree) with an in-process index edit (§4.5).
///
/// Errors: `StaleDiff` if the index entry or the worktree file no longer matches `req.base_old` / `req.base_new`;
/// `PartialNotPossible` for content that can only be staged as a whole (§4.2: binary, LFS, textconv, symlink,
/// submodule, type or mode change, unstaged rename, conflict, too large); `LockExists` if another process holds
/// `index.lock`. Selecting every line of a file whose new content differs from the index only in the executable bit
/// stages the whole file through `git add`. Refresh: `STATUS`.
pub fn stage_selection(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    req: &SelectionRequest,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    let workdir = require_workdir(repo)?.to_owned();
    validate_paths(std::slice::from_ref(&req.path))?;
    let path: &BStr = req.path.as_ref();
    if req.previous_path.is_some() {
        return partial_not_possible(req, PartialBlock::Rename);
    }
    let gix = repo.local();
    let index = read_index(&gix)?;
    if is_conflicted(&index, path) {
        return partial_not_possible(req, PartialBlock::Conflict);
    }
    let sides = unstaged_sides(&gix, &index, &workdir, path, false)?;
    check_sides(&gix, req, &sides, true)?;

    let old_img = split_lines(&sides.old_data);
    let new_img = split_lines(&sides.new_data);
    check_line_counts(req, &old_img, &new_img)?;
    let ops = edit_script(&old_img, &new_img, gix.diff_algorithm()?);
    let selected = LineSelection::new(&req.selection, old_img.len(), new_img.len());
    let applied = apply(Action::Stage, &old_img, &new_img, &ops, &selected);
    if applied.nothing_selected() {
        return Ok(Run::default().outcome(RefreshSet::STATUS));
    }

    let entry = index.entry_by_path_and_stage(path, Stage::Unconflicted);
    let edit = match (&sides.new, entry) {
        // A deleted file with every line selected: stage the deletion.
        (None, _) if applied.all_deletes_selected() => IndexChange::Remove,
        (Some(new), Some(entry))
            if applied.bytes == sides.new_data && kind_of_mode(entry.mode) != new.kind =>
        {
            // The whole content plus an executable-bit change: that is a whole-file stage.
            return stage_paths(repo, perm, ctx, std::slice::from_ref(&req.path));
        }
        (new, entry) => {
            let mode = match (entry, new) {
                (Some(entry), _) => entry.mode,
                (None, Some(new)) => mode_of_kind(new.kind),
                (None, None) => Mode::FILE,
            };
            IndexChange::Upsert {
                data: applied.bytes,
                mode,
            }
        }
    };

    let base_old = req.base_old;
    write_index_change(&gix, path, edit, |index| {
        if is_conflicted(index, path) || unstaged_old_id(index, path) != base_old {
            return Err(GitError::StaleDiff {
                path: req.path.clone(),
            });
        }
        Ok(())
    })?;
    Ok(Run::default().outcome(RefreshSet::STATUS))
}

/// Unstage selected hunks or lines of the staged diff (HEAD → index) with an in-process index edit (§4.5).
///
/// For a staged rename (`req.previous_path`), the old side is HEAD's blob at the previous path; the result is written
/// at `req.path` and the previous path stays removed, so unstaging every line leaves a pure rename. Errors as for
/// [`stage_selection`]. Refresh: `STATUS`.
pub fn unstage_selection(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    req: &SelectionRequest,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    require_workdir(repo)?;
    validate_paths(std::slice::from_ref(&req.path))?;
    if let Some(previous) = &req.previous_path {
        validate_paths(std::slice::from_ref(previous))?;
    }
    let path: &BStr = req.path.as_ref();
    let gix = repo.local();
    let index = read_index(&gix)?;
    if is_conflicted(&index, path) {
        return partial_not_possible(req, PartialBlock::Conflict);
    }
    let old_path: &BStr = req.previous_path.as_ref().map_or(path, |p| p.as_ref());
    let sides = staged_sides(&gix, &index, old_path, path)?;
    check_sides(&gix, req, &sides, false)?;

    let old_img = split_lines(&sides.old_data);
    let new_img = split_lines(&sides.new_data);
    check_line_counts(req, &old_img, &new_img)?;
    let ops = edit_script(&old_img, &new_img, gix.diff_algorithm()?);
    let selected = LineSelection::new(&req.selection, old_img.len(), new_img.len());
    let applied = apply(Action::Unstage, &old_img, &new_img, &ops, &selected);
    if applied.nothing_selected() {
        return Ok(Run::default().outcome(RefreshSet::STATUS));
    }

    let edit = if sides.old.is_none() && applied.bytes.is_empty() && applied.all_inserts_selected()
    {
        // A file that HEAD does not have, with every line unstaged: it leaves the index.
        IndexChange::Remove
    } else {
        let mode = match (&sides.new, &sides.old) {
            (Some(new), _) => mode_of_kind(new.kind),
            (None, Some(old)) => mode_of_kind(old.kind),
            (None, None) => Mode::FILE,
        };
        IndexChange::Upsert {
            data: applied.bytes,
            mode,
        }
    };

    let base_new = req.base_new;
    write_index_change(&gix, path, edit, |index| {
        if is_conflicted(index, path) || staged_new_id(index, path) != base_new {
            return Err(GitError::StaleDiff {
                path: req.path.clone(),
            });
        }
        Ok(())
    })?;
    Ok(Run::default().outcome(RefreshSet::STATUS))
}

/// Throw away selected hunks or lines of the unstaged diff (index → worktree) by rewriting the worktree file (§4.6).
///
/// The new content is converted with the worktree filters and written atomically (temporary file + rename, the
/// permissions of the existing file are kept). Refused with `PartialNotPossible { reason: UnsafeRoundTrip }` when the
/// file does not survive a clean/smudge round trip unchanged (mixed line endings with `core.autocrlf`, irreversible
/// filters); a file held open by another program yields [`GitError::Io`]. The index is not changed.
/// Refresh: `STATUS`.
pub fn discard_selection(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    req: &SelectionRequest,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    let workdir = require_workdir(repo)?.to_owned();
    validate_paths(std::slice::from_ref(&req.path))?;
    let path: &BStr = req.path.as_ref();
    if req.previous_path.is_some() {
        return partial_not_possible(req, PartialBlock::Rename);
    }
    let gix = repo.local();
    let index = read_index(&gix)?;
    if is_conflicted(&index, path) {
        return partial_not_possible(req, PartialBlock::Conflict);
    }
    let sides = unstaged_sides(&gix, &index, &workdir, path, true)?;
    check_sides(&gix, req, &sides, true)?;

    let (mut pipeline, _) = gix.filter_pipeline(None)?;
    // §4.6 step 1: the worktree file must survive clean + smudge unchanged, or the rewrite would alter lines the
    // user did not select.
    if let Some(raw) = &sides.worktree_raw
        && to_worktree(&mut pipeline, &sides.new_data, path)? != *raw
    {
        return partial_not_possible(req, PartialBlock::UnsafeRoundTrip);
    }

    let old_img = split_lines(&sides.old_data);
    let new_img = split_lines(&sides.new_data);
    check_line_counts(req, &old_img, &new_img)?;
    let ops = edit_script(&old_img, &new_img, gix.diff_algorithm()?);
    let selected = LineSelection::new(&req.selection, old_img.len(), new_img.len());
    let applied = apply(Action::Discard, &old_img, &new_img, &ops, &selected);
    if applied.nothing_selected() {
        return Ok(Run::default().outcome(RefreshSet::STATUS));
    }
    let content = to_worktree(&mut pipeline, &applied.bytes, path)?;
    if sides.worktree_raw.as_deref() == Some(&content[..]) {
        return Ok(Run::default().outcome(RefreshSet::STATUS));
    }
    let target = verified_worktree_path(&workdir, path)?;
    let index_mode = index
        .entry_by_path_and_stage(path, Stage::Unconflicted)
        .map(|entry| entry.mode);
    atomic_write(&target, &content, index_mode)?;
    Ok(Run::default().outcome(RefreshSet::STATUS))
}

// ───────────────────────────── sides of the diff ─────────────────────────────

/// One side of the diff a selection was made on, resolved again now.
#[derive(Debug, Clone, Copy)]
struct SideInfo {
    /// The blob id (for worktree files: the hash of the git form). Null for symlinks and submodules in the
    /// worktree, which are never hashed (they block partial operations before the stale check).
    id: ObjectId,
    kind: EntryKind,
}

/// Both sides of a diff plus their git-form contents.
struct Sides {
    old: Option<SideInfo>,
    new: Option<SideInfo>,
    old_data: Vec<u8>,
    new_data: Vec<u8>,
    /// The new side is the worktree file.
    new_from_worktree: bool,
    /// The raw worktree bytes (discard only, when the file exists).
    worktree_raw: Option<Vec<u8>>,
}

/// Unstaged diff sides: index entry (stage 0; nothing for untracked and intent-to-add) → worktree file in git form.
fn unstaged_sides(
    gix: &gix::Repository,
    index: &gix::index::File,
    workdir: &Path,
    path: &BStr,
    keep_raw: bool,
) -> Result<Sides> {
    let entry = index.entry_by_path_and_stage(path, Stage::Unconflicted);
    let old = entry
        .filter(|entry| !is_intent_to_add(entry))
        .map(|entry| SideInfo {
            id: entry.id,
            kind: kind_of_mode(entry.mode),
        });
    let file = verified_worktree_path(workdir, path)?;
    let null = gix.object_hash().null();
    let md = match std::fs::symlink_metadata(&file) {
        Ok(md) => Some(md),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        // A file where a directory is expected (`a` is a file, the path is `a/b`): nothing there.
        Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => None,
        Err(err) => return Err(GitError::io(&file, err)),
    };
    let new = match &md {
        None => None,
        Some(md) if md.is_symlink() => Some(SideInfo {
            id: null,
            kind: EntryKind::Link,
        }),
        Some(md) if md.is_dir() => {
            entry
                .filter(|entry| entry.mode == Mode::COMMIT)
                .map(|_| SideInfo {
                    id: null,
                    kind: EntryKind::Submodule,
                })
        }
        Some(md) if md.is_file() => {
            let executable_bit = gix.filesystem_options()?.executable_bit;
            let kind = if !executable_bit {
                // Without core.fileMode the index decides, like Git.
                match entry.map(|entry| kind_of_mode(entry.mode)) {
                    Some(EntryKind::BlobExecutable) => EntryKind::BlobExecutable,
                    _ => EntryKind::Blob,
                }
            } else if is_executable(md) {
                EntryKind::BlobExecutable
            } else {
                EntryKind::Blob
            };
            Some(SideInfo { id: null, kind })
        }
        // FIFOs, sockets: untrackable.
        Some(_) => None,
    };

    let mut sides = Sides {
        old,
        new,
        old_data: Vec::new(),
        new_data: Vec::new(),
        new_from_worktree: true,
        worktree_raw: None,
    };
    if kind_block(sides.old.as_ref(), sides.new.as_ref()).is_some() {
        // Reported by `check_sides`; reading would be pointless.
        return Ok(sides);
    }
    if let Some(md) = md.as_ref().filter(|_| sides.new.is_some()) {
        if md.len() > MAX_PARTIAL_BYTES {
            sides.new = sides.new.map(|side| SideInfo { id: null, ..side });
            return Ok(sides);
        }
        let raw = std::fs::read(&file).map_err(|err| GitError::io(&file, err))?;
        let (mut pipeline, _) = gix.filter_pipeline(None)?;
        let rela = gix::path::from_bstr(path)?;
        let mut data = Vec::with_capacity(raw.len());
        pipeline
            .convert_to_git(&raw[..], &rela, index)?
            .read_to_end(&mut data)
            .map_err(|err| GitError::io(&file, err))?;
        let id = gix::objs::compute_hash(gix.object_hash(), gix::objs::Kind::Blob, &data)
            .map_err(|err| GitError::Other(err.into()))?;
        if let Some(new) = &mut sides.new {
            new.id = id;
        }
        sides.new_data = data;
        if keep_raw {
            sides.worktree_raw = Some(raw);
        }
    }
    if let Some(old) = &sides.old {
        sides.old_data = read_blob_limited(gix, old.id)?.unwrap_or_default();
    }
    Ok(sides)
}

/// Staged diff sides: HEAD's tree entry at `old_path` (nothing if unborn or absent) → index entry at `path`
/// (stage 0; nothing for intent-to-add).
fn staged_sides(
    gix: &gix::Repository,
    index: &gix::index::File,
    old_path: &BStr,
    path: &BStr,
) -> Result<Sides> {
    let old = head_entry(gix, old_path)?;
    let new = index
        .entry_by_path_and_stage(path, Stage::Unconflicted)
        .filter(|entry| !is_intent_to_add(entry))
        .map(|entry| SideInfo {
            id: entry.id,
            kind: kind_of_mode(entry.mode),
        });
    let mut sides = Sides {
        old,
        new,
        old_data: Vec::new(),
        new_data: Vec::new(),
        new_from_worktree: false,
        worktree_raw: None,
    };
    if kind_block(sides.old.as_ref(), sides.new.as_ref()).is_some() {
        return Ok(sides);
    }
    if let Some(old) = &sides.old {
        sides.old_data = read_blob_limited(gix, old.id)?.unwrap_or_default();
    }
    if let Some(new) = &sides.new {
        sides.new_data = read_blob_limited(gix, new.id)?.unwrap_or_default();
    }
    Ok(sides)
}

/// The blob `id`, or `None` if it is larger than [`MAX_PARTIAL_BYTES`] (checked from the object header first).
fn read_blob_limited(gix: &gix::Repository, id: ObjectId) -> Result<Option<Vec<u8>>> {
    if gix.find_header(id)?.size() > MAX_PARTIAL_BYTES {
        return Ok(None);
    }
    Ok(Some(gix.find_blob(id)?.take_data()))
}

/// HEAD's tree entry at `path`, `None` if HEAD is unborn or has no such entry.
fn head_entry(gix: &gix::Repository, path: &BStr) -> Result<Option<SideInfo>> {
    let Some(head_id) = gix.head()?.id() else {
        return Ok(None);
    };
    let tree = head_id.object()?.peel_to_tree()?;
    let entry = tree.lookup_entry(path.split(|b| *b == b'/'))?;
    Ok(entry.map(|entry| SideInfo {
        id: entry.object_id(),
        kind: entry.mode().kind().into(),
    }))
}

/// The partial-availability rules of §4.2 that need only the entry kinds.
fn kind_block(old: Option<&SideInfo>, new: Option<&SideInfo>) -> Option<PartialBlock> {
    fn category(kind: EntryKind) -> u8 {
        match kind {
            EntryKind::Blob | EntryKind::BlobExecutable => 0,
            EntryKind::Link => 1,
            EntryKind::Submodule => 2,
            EntryKind::Tree => 3,
        }
    }
    let kinds = || old.into_iter().chain(new).map(|side| side.kind);
    if kinds().any(|kind| kind == EntryKind::Submodule) {
        return Some(PartialBlock::Submodule);
    }
    if let (Some(old), Some(new)) = (old, new)
        && category(old.kind) != category(new.kind)
    {
        return Some(PartialBlock::TypeChange);
    }
    if kinds().any(|kind| kind == EntryKind::Link) {
        return Some(PartialBlock::Symlink);
    }
    if kinds().any(|kind| kind == EntryKind::Tree) {
        return Some(PartialBlock::TypeChange);
    }
    None
}

/// §4.5 steps 1-2: kinds, stale check, then the content rules (size, LFS, binary, textconv).
fn check_sides(
    gix: &gix::Repository,
    req: &SelectionRequest,
    sides: &Sides,
    unstaged: bool,
) -> Result<()> {
    if let Some(reason) = kind_block(sides.old.as_ref(), sides.new.as_ref()) {
        return partial_not_possible(req, reason);
    }
    let too_large = |side: &Option<SideInfo>| side.is_some_and(|side| side.id.is_null());
    if too_large(&sides.new) && sides.new_from_worktree {
        // Not hashed because of its size.
        return partial_not_possible(req, PartialBlock::TooLarge);
    }
    if req.base_old != sides.old.map(|side| side.id) || req.base_new != sides.new.map(|s| s.id) {
        return Err(GitError::StaleDiff {
            path: req.path.clone(),
        });
    }
    if let (Some(old), Some(new)) = (&sides.old, &sides.new)
        && old.id == new.id
        && old.kind != new.kind
    {
        return partial_not_possible(req, PartialBlock::ModeOnly);
    }
    let blob_too_large = |side: &Option<SideInfo>, data: &[u8]| -> Result<bool> {
        Ok(match side {
            Some(side) if data.is_empty() => gix.find_header(side.id)?.size() > MAX_PARTIAL_BYTES,
            _ => false,
        })
    };
    if blob_too_large(&sides.old, &sides.old_data)?
        || (!sides.new_from_worktree && blob_too_large(&sides.new, &sides.new_data)?)
    {
        return partial_not_possible(req, PartialBlock::TooLarge);
    }
    if let Some(reason) = content_block(gix, req, sides, unstaged)? {
        return partial_not_possible(req, reason);
    }
    Ok(())
}

/// LFS (`filter=lfs`), binary content and textconv drivers, decided like the diff does (gix diff resource cache in
/// `ToGitUnlessBinaryToTextIsPresent` mode).
fn content_block(
    gix: &gix::Repository,
    req: &SelectionRequest,
    sides: &Sides,
    unstaged: bool,
) -> Result<Option<PartialBlock>> {
    use gix::diff::blob::{
        ResourceKind,
        pipeline::{Mode as ConversionMode, WorktreeRoots},
        platform::prepare_diff::Operation,
    };
    let path: &BStr = req.path.as_ref();
    let old_path: &BStr = req.previous_path.as_ref().map_or(path, |p| p.as_ref());

    {
        let index = gix.index_or_empty()?;
        let mut attributes = gix.attributes_only(
            &index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )?;
        let candidates = if old_path == path {
            &[path][..]
        } else {
            &[path, old_path][..]
        };
        for &candidate in candidates {
            let mut outcome = attributes.selected_attribute_matches(["filter"]);
            attributes
                .at_entry(candidate, None)?
                .matching_attributes(&mut outcome);
            let lfs = outcome.iter_selected().any(|m| {
                matches!(m.assignment.state, gix::attrs::StateRef::Value(value) if value.as_bstr() == "lfs")
            });
            if lfs {
                return Ok(Some(PartialBlock::Lfs));
            }
        }
    }

    let null = gix.object_hash().null();
    let Some(any_kind) = sides.new.or(sides.old).map(|side| side.kind) else {
        return Ok(None);
    };
    let new_root = (unstaged && sides.new.is_some())
        .then(|| gix.workdir().map(ToOwned::to_owned))
        .flatten();
    let mut cache = gix.diff_resource_cache(
        ConversionMode::ToGitUnlessBinaryToTextIsPresent,
        WorktreeRoots {
            old_root: None,
            new_root,
        },
    )?;
    let (new_id, new_kind) = match &sides.new {
        Some(side) if sides.new_from_worktree => (null, side.kind),
        Some(side) => (side.id, side.kind),
        None => (null, any_kind),
    };
    let (old_id, old_kind) = sides
        .old
        .map_or((null, any_kind), |side| (side.id, side.kind));
    cache
        .set_resource(
            new_id,
            tree_kind(new_kind),
            path,
            ResourceKind::NewOrDestination,
            gix,
        )
        .map_err(|err| GitError::Other(err.into()))?;
    cache
        .set_resource(
            old_id,
            tree_kind(old_kind),
            old_path,
            ResourceKind::OldOrSource,
            gix,
        )
        .map_err(|err| GitError::Other(err.into()))?;
    let prep = cache
        .prepare_diff()
        .map_err(|err| GitError::Other(err.into()))?;
    Ok(match prep.operation {
        Operation::SourceOrDestinationIsBinary => Some(PartialBlock::Binary),
        _ if prep.old_or_new_is_derived => Some(PartialBlock::TextConv),
        _ => None,
    })
}

fn check_line_counts(req: &SelectionRequest, old: &LineImage, new: &LineImage) -> Result<()> {
    if old.len() > MAX_PARTIAL_LINES || new.len() > MAX_PARTIAL_LINES {
        return partial_not_possible(req, PartialBlock::TooLarge);
    }
    Ok(())
}

fn partial_not_possible<T>(req: &SelectionRequest, reason: PartialBlock) -> Result<T> {
    Err(GitError::PartialNotPossible {
        path: req.path.clone(),
        reason,
    })
}

// ───────────────────────────── index write (§4.5 steps 3-7) ─────────────────────────────

/// What a partial stage/unstage does to the index entry of the path.
enum IndexChange {
    /// Write `data` as a blob and record it with `mode` (stat zeroed, intent-to-add cleared).
    Upsert { data: Vec<u8>, mode: Mode },
    /// Remove the path from the index.
    Remove,
}

/// Lock `index.lock` (fail immediately), re-read the index under the lock, run `recheck` on it, apply `change`
/// and commit the lock. An unchanged index is not written.
fn write_index_change(
    gix: &gix::Repository,
    path: &BStr,
    change: IndexChange,
    recheck: impl FnOnce(&gix::index::State) -> Result<()>,
) -> Result<()> {
    let index_path = gix.index_path();
    let lock = gix::lock::File::acquire_to_update_resource(
        &index_path,
        gix::lock::acquire::Fail::Immediately,
        None,
        0,
    )
    .map_err(|err| {
        let lock_path = lock_path_of(&index_path);
        if lock_path.exists() {
            GitError::LockExists { path: lock_path }
        } else {
            GitError::Other(anyhow::Error::from(err).context("could not lock the index"))
        }
    })?;
    // The lock is released (and the lock file removed) when `lock` is dropped on any early return.
    let mut index = read_index(gix)?;
    recheck(&index)?;

    let current = index.entry_by_path_and_stage(path, Stage::Unconflicted);
    let changed = match change {
        IndexChange::Upsert { data, mode } => {
            let id = gix.write_blob(&data)?.detach();
            let unchanged = current.is_some_and(|entry| {
                entry.id == id && entry.mode == mode && !is_intent_to_add(entry)
            });
            if unchanged {
                false
            } else {
                let mut edit = Edit::new(&mut index);
                edit.upsert(path, id, mode, Stat::Unknown);
                edit.finish()
            }
        }
        IndexChange::Remove => {
            let mut edit = Edit::new(&mut index);
            edit.remove(path);
            edit.finish()
        }
    };
    if !changed {
        return Ok(());
    }
    smudge_racily_clean_entries(&mut index);
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, lock);
    index.write_to(&mut out, gix::index::write::Options::default())?;
    let lock = out
        .into_inner()
        .map_err(|err| GitError::io(lock_path_of(&index_path), err.into_error()))?;
    lock.commit()
        .map_err(|err| GitError::io(&index_path, err.error))?;
    Ok(())
}

/// Git's racy-git protection (`ce_smudge_racily_clean_entry`), applied before the index is rewritten.
///
/// An entry whose file was modified in the same second as (or after) the index was last written may have stat data
/// that matches a newer content. Git trusts such an entry only while the index timestamp is not newer than the file,
/// so rewriting the index now would make it look clean for good. Setting its cached size to 0, as Git does, makes Git
/// and gix compare the content the next time instead; for an unchanged file that costs one re-hash. (Git smudges only
/// entries whose content really differs; doing it for every racy entry is the cheap, conservative version.)
fn smudge_racily_clean_entries(index: &mut gix::index::File) {
    let index_secs = index.timestamp().unix_seconds();
    for entry in index.entries_mut() {
        let is_file = entry.mode == Mode::FILE
            || entry.mode == Mode::FILE_EXECUTABLE
            || entry.mode == Mode::SYMLINK;
        if is_file && entry.stat.size != 0 && i64::from(entry.stat.mtime.secs) >= index_secs {
            entry.stat.size = 0;
        }
    }
}

fn lock_path_of(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    PathBuf::from(lock)
}

/// The index, read fresh from disk (no shared snapshot), or an empty one if there is no index file yet.
fn read_index(gix: &gix::Repository) -> Result<gix::index::File> {
    let path = gix.index_path();
    if path.exists() {
        Ok(gix.open_index()?)
    } else {
        Ok(gix::index::File::from_state(
            gix::index::State::new(gix.object_hash()),
            path,
        ))
    }
}

/// The id of the unstaged diff's old side in `index`: the stage-0 entry unless it is intent-to-add.
fn unstaged_old_id(index: &gix::index::State, path: &BStr) -> Option<ObjectId> {
    index
        .entry_by_path_and_stage(path, Stage::Unconflicted)
        .filter(|entry| !is_intent_to_add(entry))
        .map(|entry| entry.id)
}

/// The id of the staged diff's new side in `index` (same rule as the old side of the unstaged diff).
fn staged_new_id(index: &gix::index::State, path: &BStr) -> Option<ObjectId> {
    unstaged_old_id(index, path)
}

fn is_intent_to_add(entry: &gix::index::Entry) -> bool {
    entry
        .flags
        .contains(gix::index::entry::Flags::INTENT_TO_ADD)
}

/// Whether `path` has conflict stages in the index.
fn is_conflicted(index: &gix::index::State, path: &BStr) -> bool {
    index.entry_range(path).is_some_and(|range| {
        index.entries()[range]
            .iter()
            .any(|entry| entry.flags.stage() != Stage::Unconflicted)
    })
}

/// Whether the index has an entry at `path` or below it (a tracked file or a directory with tracked files).
fn is_tracked(index: &gix::index::State, path: &BStr) -> bool {
    let path = path.trim_end_with(|c| c == '/').as_bstr();
    if index.entry_range(path).is_some() {
        return true;
    }
    let mut prefix = path.to_owned();
    prefix.push(b'/');
    index
        .prefixed_entries(prefix.as_ref())
        .is_some_and(|entries| !entries.is_empty())
}

/// "Is this path tracked?" for the operations that delete untracked paths.
///
/// With `core.ignoreCase` (the default on Windows and macOS) a path that differs from an index entry only in ASCII
/// letter case names the same file on disk, so it counts as tracked too; otherwise deleting "the untracked
/// `Readme.md`" would delete the tracked `README.md`.
struct TrackedLookup<'a> {
    index: &'a gix::index::State,
    /// The ASCII-lowercased paths of all entries, sorted; built on the first miss, only with `core.ignoreCase`.
    folded: Option<std::cell::OnceCell<Vec<Vec<u8>>>>,
}

impl<'a> TrackedLookup<'a> {
    fn new(gix: &gix::Repository, index: &'a gix::index::State) -> Result<Self> {
        let ignore_case = gix.filesystem_options()?.ignore_case;
        Ok(TrackedLookup {
            index,
            folded: ignore_case.then(std::cell::OnceCell::new),
        })
    }

    fn is_tracked(&self, path: &BStr) -> bool {
        if is_tracked(self.index, path) {
            return true;
        }
        let Some(folded) = &self.folded else {
            return false;
        };
        let folded = folded.get_or_init(|| {
            let mut paths: Vec<Vec<u8>> = self
                .index
                .entries()
                .iter()
                .map(|entry| entry.path(self.index).to_ascii_lowercase())
                .collect();
            paths.sort_unstable();
            paths
        });
        let path = path.trim_end_with(|c| c == '/').to_ascii_lowercase();
        let exact = folded.partition_point(|p| p.as_slice() < path.as_slice());
        if folded.get(exact).is_some_and(|p| *p == path) {
            return true;
        }
        let mut prefix = path;
        prefix.push(b'/');
        let below = folded.partition_point(|p| p.as_slice() < prefix.as_slice());
        folded.get(below).is_some_and(|p| p.starts_with(&prefix))
    }
}

/// The paths of all index entries at or below each of `paths`, deduplicated.
fn index_files_below(index: &gix::index::State, paths: &[RelaPath]) -> Vec<RelaPath> {
    let mut out: Vec<RelaPath> = Vec::new();
    for path in paths {
        let path = path.trim_end_with(|c| c == '/').as_bstr();
        if index.entry_range(path).is_some() {
            out.push(path.to_owned());
        }
        let mut prefix = path.to_owned();
        prefix.push(b'/');
        if let Some(entries) = index.prefixed_entries(prefix.as_ref()) {
            out.extend(entries.iter().map(|entry| entry.path(index).to_owned()));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn head_is_unborn(gix: &gix::Repository) -> Result<bool> {
    Ok(gix.head()?.is_unborn())
}

fn kind_of_mode(mode: Mode) -> EntryKind {
    if mode == Mode::FILE_EXECUTABLE {
        EntryKind::BlobExecutable
    } else if mode == Mode::SYMLINK {
        EntryKind::Link
    } else if mode == Mode::COMMIT {
        EntryKind::Submodule
    } else if mode == Mode::DIR {
        EntryKind::Tree
    } else {
        EntryKind::Blob
    }
}

fn mode_of_kind(kind: EntryKind) -> Mode {
    match kind {
        EntryKind::Blob => Mode::FILE,
        EntryKind::BlobExecutable => Mode::FILE_EXECUTABLE,
        EntryKind::Link => Mode::SYMLINK,
        EntryKind::Submodule => Mode::COMMIT,
        EntryKind::Tree => Mode::DIR,
    }
}

fn tree_kind(kind: EntryKind) -> gix::object::tree::EntryKind {
    use gix::object::tree::EntryKind as K;
    match kind {
        EntryKind::Blob => K::Blob,
        EntryKind::BlobExecutable => K::BlobExecutable,
        EntryKind::Link => K::Link,
        EntryKind::Submodule => K::Commit,
        EntryKind::Tree => K::Tree,
    }
}

#[cfg(unix)]
fn is_executable(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    md.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_md: &std::fs::Metadata) -> bool {
    false
}

// ───────────────────────────── worktree writes ─────────────────────────────

/// `data` (git form) converted for the worktree at `path` (filters, `eol`, `core.autocrlf`), never delayed.
fn to_worktree(
    pipeline: &mut gix::filter::Pipeline<'_>,
    data: &[u8],
    path: &BStr,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 16);
    pipeline
        .convert_to_worktree(
            data,
            path,
            gix::filter::plumbing::pipeline::convert::to_worktree::Options {
                can_delay: gix::filter::plumbing::driver::apply::Delay::Forbid,
                ..Default::default()
            },
        )?
        .read_to_end(&mut out)
        .map_err(|err| {
            GitError::Other(anyhow::Error::from(err).context("worktree filter failed"))
        })?;
    Ok(out)
}

/// Write `content` to `target` atomically: a temporary file next to it, the permissions of the existing file (or the
/// executable bit of `index_mode` for a file that is gone), then a rename over the target. On failure the temporary
/// file is removed and the error names `target`.
fn atomic_write(target: &Path, content: &[u8], index_mode: Option<Mode>) -> Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|err| GitError::io(parent, err))?;
    let mut tmp_name = target.file_name().unwrap_or_default().to_owned();
    tmp_name.push(format!(
        ".tmp-gitcthulhu-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = parent.join(tmp_name);
    let existing = std::fs::metadata(target).ok();

    let write = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        copy_permissions(&tmp, existing.as_ref(), index_mode)?;
        std::fs::rename(&tmp, target)
    };
    write().map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        GitError::io(target, err)
    })
}

#[cfg(unix)]
fn copy_permissions(
    tmp: &Path,
    existing: Option<&std::fs::Metadata>,
    index_mode: Option<Mode>,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = match (existing, index_mode) {
        (Some(md), _) => md.permissions().mode(),
        (None, Some(mode)) if mode == Mode::FILE_EXECUTABLE => {
            std::fs::metadata(tmp)?.permissions().mode() | 0o111
        }
        (None, _) => return Ok(()),
    };
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn copy_permissions(
    _tmp: &Path,
    _existing: Option<&std::fs::Metadata>,
    _index_mode: Option<Mode>,
) -> std::io::Result<()> {
    // Windows has no executable bit; a read-only flag is not copied, as the rename could not replace such a file
    // afterwards anyway.
    Ok(())
}

/// Remove a file, a symlink (not its target) or a whole directory. A path that is already gone is fine.
fn remove_worktree_item(path: &Path) -> Result<()> {
    let md = match std::fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(GitError::io(path, err)),
    };
    let result = if md.is_dir() {
        std::fs::remove_dir_all(path)
    } else if cfg!(windows) && md.file_type().is_symlink() {
        // A directory symlink or junction is removed with `RemoveDirectoryW`, a file symlink with `DeleteFileW`;
        // neither follows the link.
        std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(GitError::io(path, err)),
    }
}

/// `workdir/path`, verified not to pass through a symbolic link (or, on Windows, a junction) on the way, so nothing
/// outside the worktree can be written or deleted through it. The last component itself may be a link: deleting it
/// removes the link, not its target. Missing components are fine (the rest of the path does not exist).
///
/// This is done here rather than with gix's `SymlinkCheck`, which performs no verification on Windows.
fn verified_worktree_path(workdir: &Path, path: &BStr) -> Result<PathBuf> {
    let trimmed = path.trim_end_with(|c| c == '/').as_bstr();
    let invalid = || GitError::InvalidName {
        what: "path inside the worktree",
        name: path.to_str_lossy().into_owned(),
    };
    let mut current = workdir.to_owned();
    let mut components = trimmed.split_str("/").peekable();
    let mut checking = true;
    while let Some(component) = components.next() {
        current.push(gix::path::from_byte_slice(component).map_err(|_| invalid())?);
        if !checking || components.peek().is_none() {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(md) if md.file_type().is_symlink() => return Err(invalid()),
            Ok(_) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                checking = false;
            }
            Err(err) => return Err(GitError::io(&current, err)),
        }
    }
    Ok(current)
}

// ───────────────────────────── validation and plumbing ─────────────────────────────

fn require_workdir(repo: &Repo) -> Result<&Path> {
    repo.workdir().ok_or(GitError::BareRepository)
}

/// Refuse paths that are not inside the worktree: absolute paths, drive prefixes, `.` / `..` / empty components,
/// `.git` components (including the aliases Git's `protectNTFS` / `protectHFS` checks catch) and NUL bytes. A trailing
/// `/` (directory) is allowed.
fn validate_paths(paths: &[RelaPath]) -> Result<()> {
    for path in paths {
        if !is_valid_rela_path(path.as_ref()) {
            return Err(GitError::InvalidName {
                what: "path",
                name: path.to_str_lossy().into_owned(),
            });
        }
    }
    Ok(())
}

fn is_valid_rela_path(path: &BStr) -> bool {
    let path = path.strip_suffix(b"/").unwrap_or(path);
    if path.is_empty() || path.contains(&0) || path.starts_with(b"/") {
        return false;
    }
    let separator = |b: &u8| *b == b'/' || (cfg!(windows) && *b == b'\\');
    let mut components = path.split(separator).peekable();
    if cfg!(windows)
        && let Some(first) = components.peek()
        && first.len() >= 2
        && first[1] == b':'
    {
        return false;
    }
    // Git's own checks (`core.protectNTFS` is on by default everywhere): `.git` spelled as an 8.3 alias (`GIT~1`) or
    // with an NTFS stream suffix, and on Windows reserved device names and trailing dots/spaces. A path like that could
    // name the repository directory itself, which `delete_untracked` must never reach.
    let options = gix::validate::path::component::Options {
        protect_windows: cfg!(windows),
        protect_hfs: cfg!(target_os = "macos"),
        protect_ntfs: true,
    };
    components.all(|component| {
        !component.is_empty()
            && component != b"."
            && component != b".."
            && !component.eq_ignore_ascii_case(b".git")
            && gix::validate::path::component(component.as_bstr(), None, options).is_ok()
    })
}

/// Collects the command log and warnings of the git commands an operation runs.
#[derive(Default)]
struct Run {
    log: CommandLog,
    warnings: Vec<OpWarning>,
}

impl Run {
    fn git(&mut self, repo: &Repo, ctx: &OpContext, cmd: GitCommand, class: OpClass) -> Result<()> {
        ctx.cancel.check()?;
        let out = cli::run_checked(repo, cmd, ctx, class)?;
        self.log.commands.push(out.record);
        self.log.hooks.extend(out.hooks);
        self.log.removed_stale_locks.extend(out.removed_stale_locks);
        self.warnings.extend(out.warnings);
        Ok(())
    }

    /// `git add -A` of `paths`.
    fn add_all(&mut self, repo: &Repo, ctx: &OpContext, paths: &[RelaPath]) -> Result<()> {
        let cmd = GitCommand::new(repo, CommandKind::Mutation)
            .args(["add", "-A"])
            .pathspecs(paths);
        self.git(repo, ctx, cmd, OpClass::Generic)
    }

    /// `git rm -q` of `paths`.
    fn rm(&mut self, repo: &Repo, ctx: &OpContext, paths: &[RelaPath]) -> Result<()> {
        let cmd = GitCommand::new(repo, CommandKind::Mutation)
            .args(["rm", "-q"])
            .pathspecs(paths);
        self.git(repo, ctx, cmd, OpClass::Generic)
    }

    /// `git restore --source=HEAD --staged --worktree` of `paths`.
    fn restore_from_head(
        &mut self,
        repo: &Repo,
        ctx: &OpContext,
        paths: &[RelaPath],
    ) -> Result<()> {
        let cmd = GitCommand::new(repo, CommandKind::Mutation)
            .args(["restore", "--source=HEAD", "--staged", "--worktree"])
            .pathspecs(paths);
        self.git(repo, ctx, cmd, OpClass::Restore)
    }

    fn outcome(self, refresh: RefreshSet) -> OpOutcome<()> {
        OpOutcome {
            value: (),
            refresh,
            log: self.log,
            warnings: self.warnings,
        }
    }
}

// ───────────────────────────── line engine ─────────────────────────────
//
// Private copy of the shared line engine of §4.3 (`internal::linediff`, WP-B0d), restricted to what staging needs
// (`WhitespaceMode::None`). It uses the same tokens (lines including their terminator), the same gix diff with
// slider heuristics and the same op order, so the line numbers of the displayed diff replay exactly. Once
// `internal::linediff` lands, these functions are replaced by calls into it.

/// A byte image split into lines; each range includes its terminator (`\n`, a preceding `\r` stays in the line).
struct LineImage<'a> {
    bytes: &'a [u8],
    lines: Vec<std::ops::Range<usize>>,
}

impl LineImage<'_> {
    fn len(&self) -> usize {
        self.lines.len()
    }

    fn line(&self, idx: u32) -> &[u8] {
        &self.bytes[self.lines[idx as usize].clone()]
    }
}

fn split_lines(bytes: &[u8]) -> LineImage<'_> {
    let mut lines = Vec::with_capacity(bytes.len() / 32 + 1);
    let mut start = 0;
    for end in memchr_newlines(bytes) {
        lines.push(start..end + 1);
        start = end + 1;
    }
    if start < bytes.len() {
        lines.push(start..bytes.len());
    }
    LineImage { bytes, lines }
}

fn memchr_newlines(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    bytes.find_iter(b"\n")
}

/// `\r\n` if more lines end in CRLF than in a lone LF, else `\n`.
fn dominant_eol(img: &LineImage<'_>) -> &'static [u8] {
    let (mut crlf, mut lf) = (0usize, 0usize);
    for range in &img.lines {
        let line = &img.bytes[range.clone()];
        if line.ends_with(b"\r\n") {
            crlf += 1;
        } else if line.ends_with(b"\n") {
            lf += 1;
        }
    }
    if crlf > lf { b"\r\n" } else { b"\n" }
}

/// One step of the edit script; indexes are 0-based line numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Equal { old: u32, new: u32 },
    Delete { old: u32 },
    Insert { new: u32 },
}

/// The line edit script from `old` to `new`, in order; within a change block all deletes precede all inserts.
fn edit_script(
    old: &LineImage<'_>,
    new: &LineImage<'_>,
    algorithm: gix::diff::blob::Algorithm,
) -> Vec<Op> {
    use gix::diff::blob::{InternedInput, diff_with_slider_heuristics};
    let mut input: InternedInput<&[u8]> = InternedInput::default();
    input.reserve(old.len() as u32, new.len() as u32);
    input.update_before(old.lines.iter().map(|r| &old.bytes[r.clone()]));
    input.update_after(new.lines.iter().map(|r| &new.bytes[r.clone()]));
    let diff = diff_with_slider_heuristics(algorithm, &input);

    let mut ops = Vec::with_capacity(old.len().max(new.len()) + 16);
    let (mut i, mut j) = (0u32, 0u32);
    for hunk in diff.hunks() {
        while i < hunk.before.start {
            ops.push(Op::Equal { old: i, new: j });
            i += 1;
            j += 1;
        }
        ops.extend(hunk.before.clone().map(|old| Op::Delete { old }));
        ops.extend(hunk.after.clone().map(|new| Op::Insert { new }));
        i = hunk.before.end;
        j = hunk.after.end;
    }
    while (i as usize) < old.len() {
        ops.push(Op::Equal { old: i, new: j });
        i += 1;
        j += 1;
    }
    ops
}

/// The selected removed (old) and added (new) lines, as bitmaps over the two images.
struct LineSelection {
    del: Vec<bool>,
    ins: Vec<bool>,
}

impl LineSelection {
    fn new(selection: &Selection, old_lines: usize, new_lines: usize) -> Self {
        let mut sel = LineSelection {
            del: vec![false; old_lines],
            ins: vec![false; new_lines],
        };
        fn mark(bits: &mut [bool], start_1based: u32, count: u32) {
            let start = (start_1based as usize).saturating_sub(1).min(bits.len());
            let end = start.saturating_add(count as usize).min(bits.len());
            bits[start..end].fill(true);
        }
        match selection {
            Selection::Hunks(headers) => {
                for header in headers {
                    mark(&mut sel.del, header.old_start, header.old_lines);
                    mark(&mut sel.ins, header.new_start, header.new_lines);
                }
            }
            Selection::Lines(lines) => {
                for line in lines {
                    match *line {
                        crate::LineRef::Removed { old_lineno } if old_lineno > 0 => {
                            mark(&mut sel.del, old_lineno, 1)
                        }
                        crate::LineRef::Added { new_lineno } if new_lineno > 0 => {
                            mark(&mut sel.ins, new_lineno, 1)
                        }
                        _ => {}
                    }
                }
            }
        }
        sel
    }
}

/// Which way the selected edits are applied (§4.4).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    /// `old` = index, `new` = worktree: selected changes move into the index.
    Stage,
    /// `old` = HEAD, `new` = index: selected changes are reverted in the index.
    Unstage,
    /// `old` = index, `new` = worktree: selected changes are reverted in the worktree.
    Discard,
}

/// The result of applying a selection.
struct Applied {
    bytes: Vec<u8>,
    selected_deletes: u32,
    selected_inserts: u32,
    total_deletes: u32,
    total_inserts: u32,
}

impl Applied {
    fn nothing_selected(&self) -> bool {
        self.selected_deletes == 0 && self.selected_inserts == 0
    }

    fn all_deletes_selected(&self) -> bool {
        self.selected_deletes == self.total_deletes && self.total_inserts == 0
    }

    fn all_inserts_selected(&self) -> bool {
        self.selected_inserts == self.total_inserts && self.total_deletes == 0
    }
}

/// Replay `ops`, keeping or reverting each change per `action` and `selected` (§4.4), with the glue rule: a line
/// that lacks a terminator gets the dominant line ending of its image appended before another line follows it.
fn apply(
    action: Action,
    old: &LineImage<'_>,
    new: &LineImage<'_>,
    ops: &[Op],
    selected: &LineSelection,
) -> Applied {
    struct Out {
        bytes: Vec<u8>,
        glue: &'static [u8],
    }
    impl Out {
        fn emit(&mut self, line: &[u8], eol: &'static [u8]) {
            if !self.bytes.is_empty() && !self.bytes.ends_with(b"\n") {
                self.bytes.extend_from_slice(self.glue);
            }
            self.bytes.extend_from_slice(line);
            self.glue = eol;
        }
    }
    let (old_eol, new_eol) = (dominant_eol(old), dominant_eol(new));
    let mut out = Out {
        bytes: Vec::with_capacity(old.bytes.len().max(new.bytes.len()) + 2),
        glue: b"\n",
    };
    let mut applied = Applied {
        bytes: Vec::new(),
        selected_deletes: 0,
        selected_inserts: 0,
        total_deletes: 0,
        total_inserts: 0,
    };
    for op in ops {
        match *op {
            Op::Equal { old: i, new: j } => match action {
                Action::Stage => out.emit(old.line(i), old_eol),
                Action::Unstage | Action::Discard => out.emit(new.line(j), new_eol),
            },
            Op::Delete { old: i } => {
                applied.total_deletes += 1;
                let is_selected = selected.del[i as usize];
                applied.selected_deletes += u32::from(is_selected);
                let keep_old = match action {
                    Action::Stage => !is_selected,
                    Action::Unstage | Action::Discard => is_selected,
                };
                if keep_old {
                    out.emit(old.line(i), old_eol);
                }
            }
            Op::Insert { new: j } => {
                applied.total_inserts += 1;
                let is_selected = selected.ins[j as usize];
                applied.selected_inserts += u32::from(is_selected);
                let keep_new = match action {
                    Action::Stage => is_selected,
                    Action::Unstage | Action::Discard => !is_selected,
                };
                if keep_new {
                    out.emit(new.line(j), new_eol);
                }
            }
        }
    }
    applied.bytes = out.bytes;
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HunkHeader, LineRef};

    fn run(action: Action, old: &[u8], new: &[u8], selection: Selection) -> Vec<u8> {
        let (old, new) = (split_lines(old), split_lines(new));
        let ops = edit_script(&old, &new, gix::diff::blob::Algorithm::Myers);
        let sel = LineSelection::new(&selection, old.len(), new.len());
        apply(action, &old, &new, &ops, &sel).bytes
    }

    #[test]
    fn split_lines_keeps_terminators_and_last_partial_line() {
        let img = split_lines(b"a\r\nb\nc");
        let lines: Vec<&[u8]> = (0..img.len() as u32).map(|i| img.line(i)).collect();
        assert_eq!(lines, [&b"a\r\n"[..], b"b\n", b"c"], "CR stays in the line");
        assert_eq!(split_lines(b"").len(), 0, "empty input has no lines");
        assert_eq!(split_lines(b"\n").len(), 1, "a lone newline is one line");
    }

    #[test]
    fn dominant_eol_prefers_crlf_only_when_it_is_the_majority() {
        assert_eq!(dominant_eol(&split_lines(b"a\r\nb\r\nc\n")), b"\r\n");
        assert_eq!(
            dominant_eol(&split_lines(b"a\r\nb\n")),
            b"\n",
            "a tie is LF"
        );
        assert_eq!(
            dominant_eol(&split_lines(b"a")),
            b"\n",
            "no terminator at all"
        );
    }

    #[test]
    fn edit_script_orders_deletes_before_inserts() {
        let (old, new) = (split_lines(b"a\nb\nc\n"), split_lines(b"a\nB\nc\n"));
        let ops = edit_script(&old, &new, gix::diff::blob::Algorithm::Myers);
        assert_eq!(
            ops,
            [
                Op::Equal { old: 0, new: 0 },
                Op::Delete { old: 1 },
                Op::Insert { new: 1 },
                Op::Equal { old: 2, new: 2 },
            ]
        );
    }

    #[test]
    fn stage_unstage_discard_of_single_lines() {
        let sel = || Selection::Lines(vec![LineRef::Added { new_lineno: 2 }]);
        assert_eq!(
            run(Action::Stage, b"a\nc\n", b"a\nb\nc\n", sel()),
            b"a\nb\nc\n"
        );
        assert_eq!(
            run(Action::Unstage, b"a\nc\n", b"a\nb\nc\n", sel()),
            b"a\nc\n"
        );
        assert_eq!(
            run(Action::Discard, b"a\nc\n", b"a\nb\nc\n", sel()),
            b"a\nc\n"
        );
    }

    #[test]
    fn glue_rule_adds_the_dominant_eol_after_a_line_without_terminator() {
        let sel = Selection::Lines(vec![LineRef::Added { new_lineno: 3 }]);
        assert_eq!(
            run(Action::Stage, b"a\r\nb", b"a\r\nb\r\nc\r\n", sel),
            b"a\r\nb\r\nc\r\n",
            "the old last line gets CRLF before the staged line"
        );
    }

    #[test]
    fn hunk_selection_covers_only_changes_inside_the_header_ranges() {
        let header = HunkHeader {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
        };
        assert_eq!(
            run(
                Action::Stage,
                b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n",
                b"A\nb\nc\nd\ne\nf\ng\nh\ni\nJ\n",
                Selection::Hunks(vec![header])
            ),
            b"A\nb\nc\nd\ne\nf\ng\nh\ni\nj\n"
        );
    }

    #[test]
    fn paths_outside_the_worktree_are_invalid() {
        for bad in [
            "",
            "/abs",
            "../x",
            "a/../b",
            "a//b",
            ".",
            ".git",
            "sub/.GIT/x",
            "a\0b",
            "GIT~1",
            "sub/git~1/config",
            ".git::$INDEX_ALLOCATION/config",
        ] {
            assert!(!is_valid_rela_path(bad.into()), "{bad:?} must be refused");
        }
        if cfg!(windows) {
            for bad in [".git.", ".git ", "c:x", "a\\..\\b", "con", "dir/aux.txt"] {
                assert!(
                    !is_valid_rela_path(bad.into()),
                    "{bad:?} must be refused on Windows"
                );
            }
        }
        for good in [
            "a",
            "dir/",
            "a/b.txt",
            "[12].txt",
            ".gitignore",
            "a..b",
            "git~1x",
        ] {
            assert!(
                is_valid_rela_path(good.into()),
                "{good:?} is a worktree path"
            );
        }
    }

    #[test]
    fn racy_entries_are_smudged_before_the_index_is_rewritten() {
        let mut state = gix::index::State::new(gix::hash::Kind::Sha1);
        let index_secs = u32::try_from(state.timestamp().unix_seconds()).expect("a current time");
        let stat = |secs: u32| gix::index::entry::Stat {
            mtime: gix::index::entry::stat::Time { secs, nsecs: 0 },
            size: 5,
            ..Default::default()
        };
        let id = gix::hash::Kind::Sha1.null();
        let flags = gix::index::entry::Flags::empty();
        let entries = [
            (index_secs - 1, Mode::FILE, "old"),
            (index_secs, Mode::FILE, "racy"),
            (index_secs + 1, Mode::COMMIT, "sub"),
        ];
        for (secs, mode, path) in entries {
            state.dangerously_push_entry(stat(secs), id, flags, mode, path.into());
        }
        state.sort_entries();
        let mut index = gix::index::File::from_state(state, "unused-index-path");

        smudge_racily_clean_entries(&mut index);
        let size = |path: &str| {
            index
                .entry_by_path(path.into())
                .expect("entry exists")
                .stat
                .size
        };
        assert_eq!(size("old"), 5, "older than the index: trusted, kept");
        assert_eq!(size("racy"), 0, "same second as the index: smudged");
        assert_eq!(size("sub"), 5, "gitlinks have no file stat to protect");
    }
}
