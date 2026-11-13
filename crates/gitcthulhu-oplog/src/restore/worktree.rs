//! Worktree steps of a restore (spec 17 §3.1, §3.3 B2/C3, §3.7): the 3-way checkout through but-core's safe
//! checkout, and the forced merge base.

use bstr::{BString, ByteSlice as _};
use but_core::worktree::checkout::{Options, UncommittedChangesInTheWay};

use crate::{OplogError, error::classify, state::CapturedState};

/// The result of [`checkout()`].
pub(super) enum Checkout {
    /// The worktree now holds `target` with the edits made since `base` carried over.
    Done,
    /// Nothing was changed: the files at these paths changed since `base` in a way that conflicts with the
    /// target, or are untracked files the checkout didn't see (not hashed) where the target writes files.
    InTheWay(Vec<BString>),
}

/// Step C3: turn the worktree into `target` by a 3-way merge of the current disk state against `base → target`
/// (`safe_checkout_from_head` with `merge_base_override = base`). HEAD and refs are not touched. The checkout is
/// transactional: on any failure, the worktree is left as it was.
///
/// Errors: `FileLocked` (a file held by another program), `RepositoryLocked` (`index.lock` exists), `Other`.
pub(super) fn checkout(
    repo: &gix::Repository,
    target: gix::ObjectId,
    base: gix::ObjectId,
    untracked_file_limit: u64,
) -> Result<Checkout, OplogError> {
    let options = Options {
        skip_head_update: true,
        merge_base_override: Some(base),
        allow_uncommitted_changes_to_conflict_with_new_head: false,
        untracked_size_limit: Some(untracked_file_limit),
    };
    match but_core::worktree::safe_checkout_from_head(target, repo, options) {
        Ok(_outcome) => Ok(Checkout::Done),
        Err(err) => match err.downcast_ref::<UncommittedChangesInTheWay>() {
            Some(refusal) => Ok(Checkout::InTheWay(refusal.paths.clone())),
            None => Err(classify(repo, err, OplogError::Other)),
        },
    }
}

/// [`checkout()`], refusing first if the checkout would write over an untracked file that it cannot see.
///
/// The checkout's own protection judges the files of its worktree snapshot, which never holds ignored files: Git
/// treats them as expendable and the checkout replaces them. For a restore that loses data, as the target writes
/// a tracked file where an ignored one is now (undo of `git rm --cached` plus a `.gitignore` entry, after the
/// file was edited). So every untracked file on disk at a path that `base → target` writes must hold the content
/// of `base` or of `target` there; any other is in the way.
pub(super) fn checkout_guarded(
    repo: &gix::Repository,
    target: gix::ObjectId,
    base: gix::ObjectId,
    untracked_file_limit: u64,
) -> Result<Checkout, OplogError> {
    let in_the_way = untracked_in_the_way(repo, target, base, untracked_file_limit)
        .map_err(|err| classify(repo, err, OplogError::Other))?;
    if !in_the_way.is_empty() {
        return Ok(Checkout::InTheWay(in_the_way));
    }
    checkout(repo, target, base, untracked_file_limit)
}

/// The paths where `base → target` writes a file (addition or modification) and the worktree has an untracked
/// object whose content is neither `base`'s nor `target`'s there. A file larger than `untracked_file_limit` is not
/// hashed and counts as in the way. Directories are left to the checkout.
fn untracked_in_the_way(
    repo: &gix::Repository,
    target: gix::ObjectId,
    base: gix::ObjectId,
    untracked_file_limit: u64,
) -> anyhow::Result<Vec<BString>> {
    if target == base {
        return Ok(Vec::new());
    }
    let Some(workdir) = repo.workdir() else {
        return Ok(Vec::new());
    };
    let (base_tree, target_tree) = (repo.find_tree(base)?, repo.find_tree(target)?);
    let changes = repo.diff_tree_to_tree(
        &base_tree,
        &target_tree,
        gix::diff::Options::default().with_rewrites(None),
    )?;
    let index = repo.index_or_empty()?;
    let mut tracked = Tracked::new(repo, &index)?;
    let mut hasher: Option<(
        gix::filter::Pipeline<'_>,
        gix::worktree::IndexPersistedOrInMemory,
    )> = None;
    let mut buf = Vec::new();
    let mut out = Vec::new();
    for change in changes {
        use gix::object::tree::diff::ChangeDetached;
        let (location, wanted, previous) = match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                id,
                ..
            } => (location, (entry_mode, id), None),
            ChangeDetached::Modification {
                location,
                entry_mode,
                id,
                previous_id,
                ..
            } => (location, (entry_mode, id), Some(previous_id)),
            ChangeDetached::Deletion { .. } | ChangeDetached::Rewrite { .. } => continue,
        };
        if wanted.0.is_tree() || wanted.0.is_commit() || tracked.contains(location.as_ref()) {
            continue;
        }
        let path = workdir.join(gix::path::from_bstr(location.as_bstr())?);
        let md = match gix::index::fs::Metadata::from_path_no_follow(&path) {
            Ok(md) => md,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        if md.is_dir() {
            continue;
        }
        if !md.is_symlink() && md.len() > untracked_file_limit {
            out.push(location);
            continue;
        }
        let (pipeline, pipeline_index) = match &mut hasher {
            Some(hasher) => hasher,
            None => hasher.insert(repo.filter_pipeline(None)?),
        };
        but_core::worktree::worktree_file_to_git_in_buf(
            &mut buf,
            &md,
            location.as_bstr(),
            &path,
            pipeline,
            pipeline_index,
        )?;
        let on_disk = gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, &buf)?;
        if on_disk != wanted.1 && Some(on_disk) != previous {
            out.push(location);
        }
    }
    out.sort();
    Ok(out)
}

/// Whether a path is tracked in the index, case-insensitively where the file system ignores case (a file on disk
/// found under another case than the index has it is tracked, not untracked).
struct Tracked<'a> {
    index: &'a gix::index::State,
    ignore_case: bool,
    /// The lowercased index paths, built on first use.
    folded: Option<std::collections::HashSet<BString>>,
}

impl<'a> Tracked<'a> {
    fn new(repo: &gix::Repository, index: &'a gix::index::State) -> anyhow::Result<Self> {
        Ok(Tracked {
            index,
            ignore_case: repo.filesystem_options()?.ignore_case,
            folded: None,
        })
    }

    fn contains(&mut self, rela: &bstr::BStr) -> bool {
        if self.index.entry_range(rela).is_some() {
            return true;
        }
        if !self.ignore_case {
            return false;
        }
        let index = self.index;
        self.folded
            .get_or_insert_with(|| {
                index
                    .entries()
                    .iter()
                    .map(|e| e.path(index).to_lowercase().into())
                    .collect()
            })
            .contains(rela.to_lowercase().as_slice())
    }
}

/// Add the untracked files at `paths` that `state` lacks to its worktree tree (in the same filtered form as a
/// capture), so a later restore of `state` gives them back instead of deleting them. These are ignored files at
/// paths the restore changes: a capture never takes ignored files, but here the restore makes them tracked (or
/// untracks them), so without them in the snapshot a redo deletes the file the undo made tracked.
///
/// Skipped: tracked paths, paths `state` holds or lists as skipped, directories, missing paths, files larger than
/// `limits.untracked_file_limit`, and everything when untracked capture is disabled.
pub(super) fn capture_left_out(
    repo: &gix::Repository,
    state: &mut CapturedState,
    paths: &crate::types::UntrackedCapture,
    limits: &crate::capture::CaptureLimits,
) -> anyhow::Result<()> {
    let crate::types::UntrackedCapture::Paths(paths) = paths else {
        return Ok(());
    };
    let (Some(files), Some(workdir)) = (state.files.as_mut(), repo.workdir()) else {
        return Ok(());
    };
    if !limits.capture_untracked || paths.is_empty() {
        return Ok(());
    }
    let index = repo.index_or_empty()?;
    let mut tracked = Tracked::new(repo, &index)?;
    let worktree = repo.find_tree(files.worktree)?;
    let mut editor: Option<gix::object::tree::Editor<'_>> = None;
    let mut hasher: Option<(
        gix::filter::Pipeline<'_>,
        gix::worktree::IndexPersistedOrInMemory,
    )> = None;
    let mut buf = Vec::new();
    for rela in paths {
        let rela = rela.as_bstr();
        if tracked.contains(rela)
            || state.skipped_untracked.iter().any(|s| s.path == rela)
            || worktree.lookup_entry(rela.split_str("/"))?.is_some()
        {
            continue;
        }
        let path = workdir.join(gix::path::from_bstr(rela)?);
        let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&path) else {
            continue;
        };
        let mode = if md.is_symlink() {
            gix::object::tree::EntryKind::Link
        } else if md.is_file() && md.len() <= limits.untracked_file_limit {
            if md.is_executable() {
                gix::object::tree::EntryKind::BlobExecutable
            } else {
                gix::object::tree::EntryKind::Blob
            }
        } else {
            continue;
        };
        let (pipeline, pipeline_index) = match &mut hasher {
            Some(hasher) => hasher,
            None => hasher.insert(repo.filter_pipeline(None)?),
        };
        if let Err(err) = but_core::worktree::worktree_file_to_git_in_buf(
            &mut buf,
            &md,
            rela,
            &path,
            pipeline,
            pipeline_index,
        ) {
            // Unreadable (held by another program): left out like by a capture. The checkout then refuses to
            // write over it, or fails on it with `FileLocked`.
            tracing::debug!(%rela, %err, "an untracked file at a restored path cannot be read");
            continue;
        }
        let id = repo.write_blob(&buf)?.detach();
        let editor = match &mut editor {
            Some(editor) => editor,
            None => editor.insert(repo.edit_tree(files.worktree)?),
        };
        editor.upsert(rela, mode, id)?;
    }
    if let Some(mut editor) = editor {
        files.worktree = editor.write()?.detach();
    }
    Ok(())
}

/// The forced merge base of `pre → target` (§3.3 B2): `pre.worktree` without the paths that are untracked in `pre`
/// and absent from `target.worktree`.
///
/// With it, base equals the current disk state at every captured path, so the 3-way result is the target's content
/// for those paths, while brand-new untracked files (absent from base and target) are kept. Both states must be
/// `Scope::Full`.
pub(super) fn force_base(
    repo: &gix::Repository,
    pre: &CapturedState,
    target: &CapturedState,
) -> anyhow::Result<gix::ObjectId> {
    let (Some(pre_files), Some(target_files)) = (&pre.files, &target.files) else {
        anyhow::bail!("a forced worktree restore needs the files of both states");
    };
    let untracked = pre.untracked_paths(repo)?;
    if untracked.is_empty() {
        return Ok(pre_files.worktree);
    }
    let target_tree = repo.find_tree(target_files.worktree)?;
    let mut editor = repo.edit_tree(pre_files.worktree)?;
    let mut changed = false;
    for path in &untracked {
        let in_target = target_tree
            .lookup_entry(bstr::ByteSlice::split_str(path.as_slice(), "/"))?
            .is_some();
        if !in_target {
            editor.remove(path)?;
            changed = true;
        }
    }
    Ok(if changed {
        editor.write()?.detach()
    } else {
        pre_files.worktree
    })
}
