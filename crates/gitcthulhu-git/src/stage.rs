//! Staging, unstaging, discarding and conflict resolution (WP-B3; spec 20 §2.6 `stage.rs` and §4).
//!
//! Whole-file operations run the `git` executable (`git add`, `git restore`, `git rm`, `git checkout --ours`), so
//! filters, LFS, submodules, intent-to-add entries and sparse checkouts behave natively. Every path reaches Git as a
//! `:(literal)` pathspec through stdin.
//!
//! Hunk and line operations are an in-process index edit (§4): the new index blob is the old image plus the selected
//! line edits, written under `index.lock` after a stale check against the ids the displayed diff was computed from.
//! Both sides are read, checked and diffed with the same shared code as `diff::file_diff` (`internal::content`,
//! `internal::partial`, `internal::linediff`), so a selection replays exactly the displayed edit script. Discarding
//! selected lines edits the worktree file's own bytes (§4.6).

use std::{
    io::Write as _,
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
    CommandLog, DiffOptions, DiscardMode, EntryKind, GitError, OpClass, OpContext, OpOutcome,
    OpWarning, PartialAvailability, PartialBlock, RefreshSet, RelaPath, Repo, RepoExclusive,
    Resolution, Result, Selection, SelectionRequest, Side, WhitespaceMode,
    cli::{self, CommandKind, GitCommand},
    internal::{
        content::{self, SideContent, WorktreeReader},
        linediff::{self, LineImage, Op, dominant_eol, edit_script, split_lines},
        partial,
    },
};

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
    if let Some(path) = paths
        .iter()
        .find(|p| content::is_conflicted(&index, p.as_ref()))
    {
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
/// A path that would delete a nested repository (a directory with a `.git` entry at or below it) is refused with
/// [`GitError::NestedRepository`] before anything changes, unless `allow_nested` (the user agreed to lose it).
///
/// Refresh: `STATUS`, plus `REPO_STATE` if a conflicted path was discarded.
pub fn discard_paths(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    mode: DiscardMode,
    allow_nested: bool,
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
        if content::is_conflicted(&index, path_ref) {
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
        .map(|path| content::writable_worktree_path(&workdir, path.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    if !allow_nested {
        refuse_nested_repositories(&untracked_targets)?;
    }

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
                        .map(|path| content::writable_worktree_path(&workdir, path.as_ref()))
                        .collect::<Result<Vec<_>>>()?;
                    if !allow_nested {
                        refuse_nested_repositories(&targets)?;
                    }
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
/// path fails the checks; a tracked path is refused with `InvalidName { what: "untracked path" }`, a path that holds
/// a nested repository with [`GitError::NestedRepository`] unless `allow_nested` (the user agreed to lose it).
/// Refresh: `STATUS`.
pub fn delete_untracked(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    paths: &[RelaPath],
    allow_nested: bool,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    let workdir = require_workdir(repo)?.to_owned();
    validate_paths(paths)?;
    let gix = repo.local();
    let index = read_index(&gix)?;
    let tracked_lookup = TrackedLookup::new(&gix, &index)?;
    let mut targets = Vec::with_capacity(paths.len());
    for path in paths {
        if tracked_lookup.is_tracked(path.as_ref()) || content::is_conflicted(&index, path.as_ref())
        {
            return Err(GitError::InvalidName {
                what: "untracked path",
                name: path.to_str_lossy().into_owned(),
            });
        }
        targets.push(content::writable_worktree_path(&workdir, path.as_ref())?);
    }
    if !allow_nested {
        refuse_nested_repositories(&targets)?;
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
                if !content::is_conflicted(&index, path_ref) {
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
    require_workdir(repo)?;
    validate_request(req)?;
    let path: &BStr = req.path.as_ref();
    let gix = repo.local();
    let index = read_index(&gix)?;
    let mut reader = WorktreeReader::new(&gix)?;
    let sides = recompute(&gix, &index, Some(&mut reader), req, false)?;

    let (old_img, new_img) = (split_lines(sides.old_data()), split_lines(sides.new_data()));
    let ops = edit_script(&old_img, &new_img, sides.algorithm, WhitespaceMode::None);
    let selected = LineSelection::new(&req.selection, old_img.len(), new_img.len());
    let applied = apply(Action::Stage, &old_img, &new_img, &ops, &selected);
    if applied.nothing_selected() {
        return Ok(Run::default().outcome(RefreshSet::STATUS));
    }

    let entry = index.entry_by_path_and_stage(path, Stage::Unconflicted);
    let entry_kind = entry
        .map(|entry| content::entry_kind_from_mode(entry.mode))
        .transpose()?;
    let new_kind = sides.new.as_ref().map(|new| new.side.kind);
    let edit = match (new_kind, entry) {
        // A deleted file with every line selected: stage the deletion.
        (None, _) if applied.all_deletes_selected() => IndexChange::Remove,
        (Some(new_kind), Some(_))
            if applied.bytes == sides.new_data() && entry_kind != Some(new_kind) =>
        {
            // The whole content plus an executable-bit change: that is a whole-file stage.
            return stage_paths(repo, perm, ctx, std::slice::from_ref(&req.path));
        }
        (new_kind, entry) => {
            let mode = match (entry, new_kind) {
                (Some(entry), _) => entry.mode,
                (None, Some(kind)) => mode_of_kind(kind),
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
        if content::is_conflicted(index, path) || unstaged_old_id(index, path) != base_old {
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
    validate_request(req)?;
    let path: &BStr = req.path.as_ref();
    let gix = repo.local();
    let index = read_index(&gix)?;
    let sides = recompute(&gix, &index, None, req, false)?;

    let (old_img, new_img) = (split_lines(sides.old_data()), split_lines(sides.new_data()));
    let ops = edit_script(&old_img, &new_img, sides.algorithm, WhitespaceMode::None);
    let selected = LineSelection::new(&req.selection, old_img.len(), new_img.len());
    let applied = apply(Action::Unstage, &old_img, &new_img, &ops, &selected);
    if applied.nothing_selected() {
        return Ok(Run::default().outcome(RefreshSet::STATUS));
    }

    let (old_kind, new_kind) = (
        sides.old.as_ref().map(|old| old.side.kind),
        sides.new.as_ref().map(|new| new.side.kind),
    );
    let edit = if old_kind.is_none() && applied.bytes.is_empty() && applied.all_inserts_selected() {
        // A file that HEAD does not have, with every line unstaged: it leaves the index.
        IndexChange::Remove
    } else {
        IndexChange::Upsert {
            data: applied.bytes,
            mode: new_kind.or(old_kind).map_or(Mode::FILE, mode_of_kind),
        }
    };

    let base_new = req.base_new;
    write_index_change(&gix, path, edit, |index| {
        if content::is_conflicted(index, path) || staged_new_id(index, path) != base_new {
            return Err(GitError::StaleDiff {
                path: req.path.clone(),
            });
        }
        Ok(())
    })?;
    Ok(Run::default().outcome(RefreshSet::STATUS))
}

/// Throw away selected hunks or lines of the unstaged diff (index → worktree) by editing the worktree file (§4.6).
///
/// The file's own bytes are edited: every line that is not discarded keeps its exact bytes (its own line ending and
/// encoding), and each restored line is what a fresh checkout of the index content writes for it, ending like the
/// rest of the file where the filters make both endings equivalent (an LF file under `core.autocrlf=true` stays LF;
/// without any conversion the index bytes are restored exactly). The result is verified to clean (`git add`) to
/// exactly the expected content before it is written atomically (temporary file + rename, the permissions of the
/// existing file are kept). The index is never changed.
///
/// Refused with `PartialNotPossible { reason: UnsafeRoundTrip }` only when neither works: the worktree file's lines
/// do not correspond one to one to its git form, or the edited file would not clean to the expected content, *and*
/// the filters do not reproduce the file unchanged (`smudge(clean(file)) != file`). In practice that is a clean /
/// smudge filter driver or a `working-tree-encoding` that is not lossless for this file. Binary, LFS, textconv and
/// the other §4.2 cases are refused with their own reasons. A path that leads through a symbolic link or junction is
/// refused with `InvalidName`; a file held open by another program yields [`GitError::Io`]. Refresh: `STATUS`.
pub fn discard_selection(
    repo: &Repo,
    _perm: &mut RepoExclusive,
    ctx: &OpContext,
    req: &SelectionRequest,
) -> Result<OpOutcome<()>> {
    ctx.cancel.check()?;
    let workdir = require_workdir(repo)?.to_owned();
    validate_request(req)?;
    let path: &BStr = req.path.as_ref();
    let gix = repo.local();
    let index = read_index(&gix)?;
    let mut reader = WorktreeReader::new(&gix)?;
    let sides = recompute(&gix, &index, Some(&mut reader), req, true)?;

    let (old_img, new_img) = (split_lines(sides.old_data()), split_lines(sides.new_data()));
    let ops = edit_script(&old_img, &new_img, sides.algorithm, WhitespaceMode::None);
    let selected = LineSelection::new(&req.selection, old_img.len(), new_img.len());
    let applied = apply(Action::Discard, &old_img, &new_img, &ops, &selected);
    if applied.nothing_selected() {
        return Ok(Run::default().outcome(RefreshSet::STATUS));
    }
    let edit = DiscardEdit {
        path,
        old: &old_img,
        new: &new_img,
        ops: &ops,
        selected: &selected,
        result: &applied.bytes,
    };
    let Some(content) = edit.worktree_bytes(&mut reader, sides.raw.as_deref())? else {
        return partial_not_possible(req, PartialBlock::UnsafeRoundTrip);
    };
    if sides.raw.as_deref() == Some(&content[..]) {
        return Ok(Run::default().outcome(RefreshSet::STATUS));
    }
    // Checked right before the write: never write through a symbolic link or junction.
    let target = content::writable_worktree_path(&workdir, path)?;
    let index_mode = index
        .entry_by_path_and_stage(path, Stage::Unconflicted)
        .map(|entry| entry.mode);
    atomic_write(&target, &content, index_mode)?;
    Ok(Run::default().outcome(RefreshSet::STATUS))
}

// ───────────────────────────── sides of the diff ─────────────────────────────

/// Both sides of the diff a selection was made on, recomputed now (§4.5 step 1) and checked (steps 1-2).
struct Recomputed {
    /// The old side: index entry (unstaged diff) or HEAD's blob (staged diff), with its git-form bytes.
    old: Option<SideContent>,
    /// The new side: worktree file in git form (unstaged diff) or index entry (staged diff).
    new: Option<SideContent>,
    /// The bytes of the worktree file on disk (discard only).
    raw: Option<Vec<u8>>,
    /// The line algorithm of the diff the selection was made on.
    algorithm: gix::diff::blob::Algorithm,
}

impl Recomputed {
    fn old_data(&self) -> &[u8] {
        self.old
            .as_ref()
            .and_then(|side| side.data.as_deref())
            .unwrap_or_default()
    }

    fn new_data(&self) -> &[u8] {
        self.new
            .as_ref()
            .and_then(|side| side.data.as_deref())
            .unwrap_or_default()
    }
}

/// Resolve and read both sides exactly as `diff::file_diff` does, then apply the shared §4.2 rules
/// (`internal::partial`) and the stale check against `req.base_old` / `req.base_new`.
///
/// `worktree` selects the unstaged diff (index stage 0, nothing for untracked and intent-to-add → worktree file in
/// git form); `None` selects the staged diff (HEAD's entry at the previous path of a rename → index stage 0).
/// `keep_raw` also returns the worktree bytes on disk, from the same read as the git form.
fn recompute(
    gix: &gix::Repository,
    index: &gix::index::State,
    worktree: Option<&mut WorktreeReader<'_>>,
    req: &SelectionRequest,
    keep_raw: bool,
) -> Result<Recomputed> {
    let limits = DiffOptions::default();
    let path: &BStr = req.path.as_ref();
    let old_path: &BStr = req.previous_path.as_ref().map_or(path, |p| p.as_ref());
    let conflicted = content::is_conflicted(index, path);
    let read_object = |side: Side| content::read_blob(gix, side, limits.max_file_bytes);
    let entry_side = |entry: content::IndexEntry| Side {
        id: entry.id,
        kind: entry.kind,
    };
    let unstaged = worktree.is_some();
    let (old, new, raw) = match worktree {
        Some(reader) => {
            let entry = content::index_entry(index, old_path, Stage::Unconflicted)?;
            let old = entry
                .filter(|entry| !entry.intent_to_add)
                .map(|entry| read_object(entry_side(entry)))
                .transpose()?;
            let index_kind = entry.map(|entry| entry.kind);
            let (new, raw) = if keep_raw {
                match reader.read_with_raw(path, limits.max_file_bytes, index_kind)? {
                    Some(side) => (Some(side.content), side.raw),
                    None => (None, None),
                }
            } else {
                (reader.read(path, limits.max_file_bytes, index_kind)?, None)
            };
            (old, new, raw)
        }
        None => {
            // A directory in HEAD where the index now has a file is no old side, as in `diff::file_diff`.
            let old = content::head_entry(gix, old_path)?
                .filter(|side| side.kind != EntryKind::Tree)
                .map(read_object)
                .transpose()?;
            let new = content::index_entry(index, path, Stage::Unconflicted)?
                .filter(|entry| !entry.intent_to_add)
                .map(|entry| read_object(entry_side(entry)))
                .transpose()?;
            (old, new, None)
        }
    };

    let kind = |side: &Option<SideContent>| side.as_ref().map(|side| side.side.kind);
    let id = |side: &Option<SideContent>| side.as_ref().map(|side| side.side.id);
    let too_large = [&old, &new].into_iter().flatten().any(|side| {
        side.is_too_large()
            || side
                .data
                .as_deref()
                .is_some_and(|data| linediff::count_lines(data) > limits.max_lines as usize)
    });
    let has_submodule = [kind(&old), kind(&new)].contains(&Some(EntryKind::Submodule));
    // The diff platform decides binary and textconv; it is not asked about sides an earlier rule blocks anyway
    // (it would read a submodule directory or a file above the size limit).
    let prepared = if has_submodule || too_large {
        None
    } else {
        let resource = |side: &Option<SideContent>, path| {
            side.as_ref().map(|side| content::Resource {
                id: side.side.id,
                kind: side.side.kind,
                path,
                worktree: side.side.from_worktree,
            })
        };
        Some(content::prepare_text(
            gix,
            resource(&old, old_path),
            resource(&new, path),
        )?)
    };
    let content_class = if too_large {
        partial::ContentClass::TooLarge
    } else if prepared.as_ref().is_some_and(|p| p.binary) {
        partial::ContentClass::Binary
    } else if id(&old).is_some() && id(&old) == id(&new) {
        partial::ContentClass::Identical
    } else {
        partial::ContentClass::Text
    };
    let textconv = prepared
        .as_ref()
        .is_some_and(|p| p.old_textconv.is_some() || p.new_textconv.is_some());
    let availability = partial::partial_availability(
        gix,
        index,
        path,
        if unstaged {
            partial::Sides::IndexToWorktree
        } else {
            partial::Sides::HeadToIndex
        },
        &partial::Kinds {
            old: kind(&old),
            new: kind(&new),
            content: content_class,
            renamed: req.previous_path.is_some(),
            conflicted,
            whitespace: WhitespaceMode::None,
        },
        textconv,
    )?;
    if let PartialAvailability::WholeFileOnly(reason) = availability {
        return partial_not_possible(req, reason);
    }
    if req.base_old != id(&old) || req.base_new != id(&new) {
        return Err(GitError::StaleDiff {
            path: req.path.clone(),
        });
    }
    let configured = match &prepared {
        Some(prepared) => prepared.algorithm,
        None => gix.diff_algorithm()?,
    };
    Ok(Recomputed {
        old,
        new,
        raw,
        algorithm: linediff::resolve_algorithm(req.algorithm, configured),
    })
}

fn validate_request(req: &SelectionRequest) -> Result<()> {
    validate_paths(std::slice::from_ref(&req.path))?;
    if let Some(previous) = &req.previous_path {
        validate_paths(std::slice::from_ref(previous))?;
    }
    Ok(())
}

fn partial_not_possible<T>(req: &SelectionRequest, reason: PartialBlock) -> Result<T> {
    Err(GitError::PartialNotPossible {
        path: req.path.clone(),
        reason,
    })
}

// ───────────────────────────── discard: editing the worktree bytes (§4.6) ─────────────────────────────

/// A partial discard computed in git form, to be carried over to the worktree file's own bytes.
struct DiscardEdit<'a> {
    path: &'a BStr,
    /// The index image (git form).
    old: &'a LineImage<'a>,
    /// The worktree image in git form.
    new: &'a LineImage<'a>,
    ops: &'a [Op],
    selected: &'a LineSelection,
    /// The discard result in git form (§4.4): what the worktree file must clean to afterwards.
    result: &'a [u8],
}

impl DiscardEdit<'_> {
    /// The new worktree bytes, or `None` if they cannot be produced without risking lines the user did not select.
    ///
    /// 1. Direct edit. Clean conversions (`core.autocrlf`, `eol`, `text`, `ident`) keep the number of lines, so the
    ///    git-form line *j* is worktree line *j*. The same §4.4 discard runs over the raw worktree lines (kept
    ///    byte-for-byte) and the checked-out index lines (restored lines). Restored lines first get the file's
    ///    dominant line ending, then, if that does not clean to [`Self::result`], exactly the checkout's bytes.
    /// 2. Whole file. If the filters reproduce the file unchanged (`smudge(clean(raw)) == raw`, e.g. a lossless
    ///    `working-tree-encoding`), the result is smudged as a whole, and accepted only if that cleans back to exactly
    ///    [`Self::result`].
    fn worktree_bytes(
        &self,
        reader: &mut WorktreeReader<'_>,
        raw: Option<&[u8]>,
    ) -> Result<Option<Vec<u8>>> {
        let raw_img = split_lines(raw.unwrap_or_default());
        if raw_img.len() == self.new.len() {
            let checkout = reader.convert_to_worktree(self.old.bytes, self.path)?;
            let checkout_img = split_lines(&checkout);
            if checkout_img.len() == self.old.len() {
                let file_eol = raw_img
                    .lines
                    .iter()
                    .any(|line| raw_img.bytes[line.clone()].ends_with(b"\n"))
                    .then(|| dominant_eol(&raw_img));
                let reterminated = file_eol
                    .map(|eol| reterminate(&checkout_img, eol))
                    .filter(|bytes| *bytes != checkout);
                for restored in reterminated.iter().chain([&checkout]) {
                    let restored_img = split_lines(restored);
                    let bytes = apply(
                        Action::Discard,
                        &restored_img,
                        &raw_img,
                        self.ops,
                        self.selected,
                    )
                    .bytes;
                    // Spliced lines of an encoding that is not line-aligned may not even convert; that is a
                    // mismatch like any other, and the whole-file path below decides.
                    if reader
                        .convert_to_git(&bytes, self.path)
                        .is_ok_and(|git| git == self.result)
                    {
                        return Ok(Some(bytes));
                    }
                }
            }
        }
        let round_trips = match raw {
            Some(raw) => reader.convert_to_worktree(self.new.bytes, self.path)? == raw,
            // Nothing on disk to preserve: write what a checkout of the result writes.
            None => true,
        };
        if !round_trips {
            return Ok(None);
        }
        let smudged = reader.convert_to_worktree(self.result, self.path)?;
        // The same guarantee as the direct edit: the written file cleans to exactly the expected content.
        Ok((reader.convert_to_git(&smudged, self.path)? == self.result).then_some(smudged))
    }
}

/// The lines of `img` with every terminator (`\n` or `\r\n`) replaced by `eol`; a last line without one stays so.
fn reterminate(img: &LineImage<'_>, eol: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.bytes.len() + img.len());
    for idx in 0..img.len() as u32 {
        out.extend_from_slice(img.content(idx));
        if img.eol(idx) != crate::Eol::None {
            out.extend_from_slice(eol);
        }
    }
    out
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

fn mode_of_kind(kind: EntryKind) -> Mode {
    match kind {
        EntryKind::Blob => Mode::FILE,
        EntryKind::BlobExecutable => Mode::FILE_EXECUTABLE,
        EntryKind::Link => Mode::SYMLINK,
        EntryKind::Submodule => Mode::COMMIT,
        EntryKind::Tree => Mode::DIR,
    }
}

// ───────────────────────────── worktree writes ─────────────────────────────

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

/// The nested repositories that deleting the untracked `paths` with [`delete_untracked`] or [`discard_paths`] would
/// take with them: every directory at or below one of the paths that holds a `.git` entry (a nested repository, or
/// a work tree whose `.git` file points elsewhere), worktree-relative, sorted. These are exactly what those
/// functions refuse without `allow_nested`, so a caller names them in its question before it passes the flag. Links
/// are not followed; a missing path holds none. Read-only (no lock needed).
///
/// Errors: `BareRepository`, `InvalidName` (a path outside the worktree), `Io`.
pub fn nested_repositories(repo: &Repo, paths: &[RelaPath]) -> Result<Vec<RelaPath>> {
    let workdir = require_workdir(repo)?.to_owned();
    validate_paths(paths)?;
    let targets = paths
        .iter()
        .map(|path| content::writable_worktree_path(&workdir, path.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    // A directory that cannot be named is an error, never left out of the caller's question.
    let mut out = find_nested_repositories(&targets, false)?
        .into_iter()
        .map(|dir| {
            dir.strip_prefix(&workdir)
                .ok()
                .and_then(|rela| gix::path::into_bstr(rela).ok())
                .map(|rela| gix::path::to_unix_separators_on_windows(rela).into_owned())
                .ok_or_else(|| GitError::NestedRepository { path: dir.clone() })
        })
        .collect::<Result<Vec<RelaPath>>>()?;
    out.sort();
    out.dedup();
    Ok(out)
}

/// `NestedRepository` for the first of `targets` that is, or holds below it, a directory with a `.git` entry (a
/// nested repository or a submodule work tree): `remove_dir_all` would delete its history, and snapshots never
/// capture nested repositories. Links are not followed, as [`remove_worktree_item`] does not follow them either.
fn refuse_nested_repositories(targets: &[PathBuf]) -> Result<()> {
    match find_nested_repositories(targets, true)?.into_iter().next() {
        Some(path) => Err(GitError::NestedRepository { path }),
        None => Ok(()),
    }
}

/// The directories at or below `targets` that hold a `.git` entry (not descended into further; links not
/// followed), only the first one if `first_only`.
fn find_nested_repositories(targets: &[PathBuf], first_only: bool) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for target in targets {
        let mut pending = match std::fs::symlink_metadata(target) {
            Ok(md) if md.is_dir() => vec![target.clone()],
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(GitError::io(target, err)),
        };
        while let Some(dir) = pending.pop() {
            let mut subdirs = Vec::new();
            let mut is_repository = false;
            let entries = std::fs::read_dir(&dir).map_err(|err| GitError::io(&dir, err))?;
            for entry in entries {
                let entry = entry.map_err(|err| GitError::io(&dir, err))?;
                if entry.file_name().eq_ignore_ascii_case(".git") {
                    is_repository = true;
                    break;
                }
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    subdirs.push(entry.path());
                }
            }
            if is_repository {
                found.push(dir);
                if first_only {
                    return Ok(found);
                }
            } else {
                pending.extend(subdirs);
            }
        }
    }
    Ok(found)
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

// ───────────────────────────── selection replay (§4.4) ─────────────────────────────
//
// The edit script comes from the shared line engine (`internal::linediff`, the one `diff::file_diff` displays), so
// the line numbers of a `LineRef` select exactly the displayed lines.

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
        let ops = edit_script(
            &old,
            &new,
            gix::diff::blob::Algorithm::Myers,
            WhitespaceMode::None,
        );
        let sel = LineSelection::new(&selection, old.len(), new.len());
        apply(action, &old, &new, &ops, &sel).bytes
    }

    #[test]
    fn reterminate_replaces_every_line_ending() {
        let img = split_lines(b"a\r\nb\nc");
        assert_eq!(
            reterminate(&img, b"\n").as_bstr(),
            b"a\nb\nc".as_bstr(),
            "CRLF and LF both become the given ending, the unterminated last line stays"
        );
        assert_eq!(
            reterminate(&split_lines(b"x\n"), b"\r\n").as_bstr(),
            b"x\r\n".as_bstr(),
            "LF becomes CRLF"
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
