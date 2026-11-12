//! File and commit diffs (WP-B2): the line diff of one file, the files of a commit, budgeted staged patch text for
//! AI prompts, exact patch export and raw blob reads.
//!
//! Reads run in-process on `gix`; only [`export_patch`] runs the `git` executable. The line diff is computed by one
//! engine (`lines` below): the line numbers a [`DiffLine`] carries are exactly the edit-script indexes a staging
//! operation replays.

use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::OsString,
    io::Read,
    num::NonZeroUsize,
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use bstr::{BStr, BString, ByteSlice};
use gix::ObjectId;
use gix::prelude::TreeDiffChangeExt as _;
use lru::LruCache;

use crate::{
    Against, BlobSide, CancelToken, ChangeKind, CommitDetails, CommitFile, CommitFilesOptions,
    CommitInfo, DiffAlgorithm, DiffContent, DiffLine, DiffOptions, DiffTarget, EntryKind, Eol,
    EolStyle, ExportPatch, FileDiff, FileStat, GitError, GitTime, Hunk, HunkHeader, ImageData,
    LineKind, PartialAvailability, PartialBlock, PatchText, PatchTextOptions, Person, RelaPath,
    Repo, Result, Side, Signature, SubmoduleSummary, TextDiff, WhitespaceMode, types::ArenaRange,
};

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions {
            context_lines: 3,
            full_file: false,
            whitespace: WhitespaceMode::None,
            inline_highlights: true,
            max_file_bytes: 50 * 1024 * 1024,
            max_lines: 1_000_000,
            max_image_bytes: 20 * 1024 * 1024,
            algorithm: None,
        }
    }
}

impl Default for PatchTextOptions {
    fn default() -> Self {
        PatchTextOptions {
            budget_chars: 16_000,
            per_file_min_chars: 500,
            skip_globs: [
                "*.lock",
                "package-lock.json",
                "pnpm-lock.yaml",
                "*.min.js",
                "*.map",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}

// ───────────────────────────── small helpers ─────────────────────────────

fn eh<E: Into<anyhow::Error>>(err: E) -> GitError {
    GitError::Other(err.into())
}

fn check_cancel(cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(GitError::Cancelled)
    } else {
        Ok(())
    }
}

fn path_of(rela: &BStr) -> Result<PathBuf> {
    gix::path::from_bstr(rela)
        .map(Cow::into_owned)
        .map_err(|_| GitError::Unsupported {
            what: "a path that is not valid on this platform",
        })
}

fn not_found(what: &'static str, name: impl ToString) -> GitError {
    GitError::NotFound {
        what,
        name: name.to_string(),
    }
}

fn to_gix_kind(kind: EntryKind) -> gix::object::tree::EntryKind {
    use gix::object::tree::EntryKind as K;
    match kind {
        EntryKind::Blob => K::Blob,
        EntryKind::BlobExecutable => K::BlobExecutable,
        EntryKind::Link => K::Link,
        EntryKind::Submodule => K::Commit,
        EntryKind::Tree => K::Tree,
    }
}

/// Regular files of either flavour count as the same class for "type change" decisions.
fn kind_class(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Blob | EntryKind::BlobExecutable => 0,
        EntryKind::Link => 1,
        EntryKind::Submodule => 2,
        EntryKind::Tree => 3,
    }
}

fn index_kind(mode: gix::index::entry::Mode) -> EntryKind {
    use gix::index::entry::Mode as M;
    if mode == M::SYMLINK {
        EntryKind::Link
    } else if mode == M::COMMIT {
        EntryKind::Submodule
    } else if mode == M::DIR {
        EntryKind::Tree
    } else if mode == M::FILE_EXECUTABLE {
        EntryKind::BlobExecutable
    } else {
        EntryKind::Blob
    }
}

/// Whether the file name ends in one of the image extensions previews are offered for.
fn has_image_extension(path: &RelaPath) -> bool {
    const EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff",
    ];
    let name = path.rsplit(|b| *b == b'/').next().unwrap_or_default();
    match name.rsplit(|b| *b == b'.').next() {
        Some(ext) if ext.len() < name.len() => EXTENSIONS
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known.as_bytes())),
        _ => false,
    }
}

/// The MIME type of `bytes` if the path has an image extension and the content sniffs as an image.
fn image_mime(path: &RelaPath, bytes: &[u8]) -> Option<&'static str> {
    if !has_image_extension(path) {
        return None;
    }
    let kind = infer::get(bytes)?;
    (kind.matcher_type() == infer::MatcherType::Image).then(|| kind.mime_type())
}

fn is_blob(kind: EntryKind) -> bool {
    matches!(kind, EntryKind::Blob | EntryKind::BlobExecutable)
}

fn compute_blob_id(repo: &gix::Repository, data: &[u8]) -> Result<ObjectId> {
    gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, data).map_err(eh)
}

// ───────────────────────────── target resolution ─────────────────────────────

/// One resolved side of a diff.
#[derive(Clone)]
struct Resolved {
    path: RelaPath,
    kind: EntryKind,
    /// The blob (or submodule commit); null for the worktree file.
    id: ObjectId,
    worktree: bool,
}

struct Plan {
    path: RelaPath,
    previous_path: Option<RelaPath>,
    kind: ChangeKind,
    old: Option<Resolved>,
    new: Option<Resolved>,
    /// An unstaged rename (index to worktree).
    unstaged_rename: bool,
}

fn tree_entry(tree: &gix::Tree<'_>, path: &BStr) -> Result<Option<(ObjectId, EntryKind)>> {
    let entry = tree
        .lookup_entry(path.split_str("/").map(|c| c.as_bstr().to_owned()))
        .map_err(eh)?;
    Ok(entry.and_then(|entry| {
        let kind = EntryKind::from(entry.mode().kind());
        (kind != EntryKind::Tree).then(|| (entry.object_id(), kind))
    }))
}

fn head_tree(repo: &gix::Repository) -> Result<Option<gix::Tree<'_>>> {
    let head = repo.head().map_err(eh)?;
    if head.is_unborn() {
        return Ok(None);
    }
    let commit = repo.head_commit().map_err(eh)?;
    Ok(Some(commit.tree().map_err(eh)?))
}

fn peel_tree(repo: &gix::Repository, id: ObjectId) -> Result<gix::Tree<'_>> {
    repo.find_object(id)
        .map_err(|_| not_found("commit", id))?
        .peel_to_tree()
        .map_err(eh)
}

fn against_tree<'repo>(
    repo: &'repo gix::Repository,
    commit: &gix::Commit<'_>,
    against: Against,
) -> Result<Option<gix::Tree<'repo>>> {
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    Ok(match against {
        Against::Empty => None,
        Against::FirstParent => match parents.first() {
            Some(parent) => Some(peel_tree(repo, *parent)?),
            None => None,
        },
        Against::Parent(n) => match parents.get(usize::from(n)) {
            Some(parent) => Some(peel_tree(repo, *parent)?),
            None => return Err(not_found("parent commit", n)),
        },
        Against::Commit(other) => Some(peel_tree(repo, other)?),
    })
}

fn tree_resolved(tree: Option<&gix::Tree<'_>>, path: &RelaPath) -> Result<Option<Resolved>> {
    let Some(tree) = tree else {
        return Ok(None);
    };
    Ok(
        tree_entry(tree, path.as_bstr())?.map(|(id, kind)| Resolved {
            path: path.clone(),
            kind,
            id,
            worktree: false,
        }),
    )
}

fn index_resolved(
    index: &gix::index::State,
    path: &RelaPath,
    stage: gix::index::entry::Stage,
) -> Option<(Resolved, bool)> {
    let entry = index.entry_by_path_and_stage(path.as_bstr(), stage)?;
    Some((
        Resolved {
            path: path.clone(),
            kind: index_kind(entry.mode),
            id: entry.id,
            worktree: false,
        },
        entry
            .flags
            .contains(gix::index::entry::Flags::INTENT_TO_ADD),
    ))
}

/// The worktree file at `path`, if any, with its size. `old_kind` decides the executable bit when the file system
/// does not carry one.
fn worktree_resolved(
    repo: &gix::Repository,
    workdir: &Path,
    path: &RelaPath,
    old_kind: Option<EntryKind>,
) -> Result<Option<(Resolved, u64)>> {
    let full = workdir.join(path_of(path.as_bstr())?);
    let md = match std::fs::symlink_metadata(&full) {
        Ok(md) => md,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(err) => return Err(GitError::io(&full, err)),
    };
    let (kind, size) = if md.is_symlink() {
        (EntryKind::Link, md.len())
    } else if md.is_dir() {
        if !full.join(".git").exists() {
            return Ok(None);
        }
        (EntryKind::Submodule, 0)
    } else if md.is_file() && old_kind == Some(EntryKind::Link) && !symlinks_enabled(repo) {
        // `core.symlinks=false` (the Git for Windows default): Git checks a link out as a plain file holding the
        // target and keeps the index entry a link, so this is still a link and not a type change.
        (EntryKind::Link, md.len())
    } else if md.is_file() {
        let honours_exec_bit = repo
            .config_snapshot()
            .boolean("core.filemode")
            .unwrap_or(cfg!(unix));
        let executable = if honours_exec_bit {
            is_executable(&md)
        } else {
            old_kind == Some(EntryKind::BlobExecutable)
        };
        (
            if executable {
                EntryKind::BlobExecutable
            } else {
                EntryKind::Blob
            },
            md.len(),
        )
    } else {
        return Ok(None);
    };
    let id = if kind == EntryKind::Submodule {
        gix::open(&full)
            .ok()
            .and_then(|sub| sub.head_id().ok().map(|id| id.detach()))
            .unwrap_or_else(|| repo.object_hash().null())
    } else {
        repo.object_hash().null()
    };
    Ok(Some((
        Resolved {
            path: path.clone(),
            kind,
            id,
            worktree: true,
        },
        size,
    )))
}

/// `core.symlinks` (gix' default: `true`).
fn symlinks_enabled(repo: &gix::Repository) -> bool {
    repo.filesystem_options().map_or(true, |caps| caps.symlink)
}

#[cfg(unix)]
fn is_executable(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn is_executable(_md: &std::fs::Metadata) -> bool {
    false
}

fn change_kind(old: &Resolved, new: &Resolved, renamed: bool) -> ChangeKind {
    if renamed {
        ChangeKind::Renamed
    } else if kind_class(old.kind) != kind_class(new.kind) {
        ChangeKind::TypeChanged
    } else {
        ChangeKind::Modified
    }
}

fn plan_target(repo: &Repo, g: &gix::Repository, target: &DiffTarget) -> Result<Plan> {
    use gix::index::entry::Stage;
    match target {
        DiffTarget::Staged {
            path,
            previous_path,
        } => {
            let index = g.index_or_empty().map_err(eh)?;
            let tree = head_tree(g)?;
            let old_path = previous_path.as_ref().unwrap_or(path);
            let old = tree_resolved(tree.as_ref(), old_path)?;
            let new = index_resolved(&index, path, Stage::Unconflicted).map(|(r, _)| r);
            let kind = match (&old, &new) {
                (None, None) => return Err(not_found("file", path)),
                (None, Some(_)) => ChangeKind::Added,
                (Some(_), None) => ChangeKind::Deleted,
                (Some(old), Some(new)) => change_kind(old, new, previous_path.is_some()),
            };
            Ok(Plan {
                path: path.clone(),
                previous_path: previous_path.clone(),
                kind,
                old,
                new,
                unstaged_rename: false,
            })
        }
        DiffTarget::Unstaged {
            path,
            previous_path,
        } => {
            let workdir = repo.workdir().ok_or(GitError::BareRepository)?;
            let index = g.index_or_empty().map_err(eh)?;
            let old_path = previous_path.as_ref().unwrap_or(path);
            let conflicted = [Stage::Base, Stage::Ours, Stage::Theirs]
                .into_iter()
                .any(|stage| {
                    index
                        .entry_by_path_and_stage(path.as_bstr(), stage)
                        .is_some()
                });
            let (old, kind_hint) = if conflicted {
                (
                    index_resolved(&index, path, Stage::Ours).map(|(r, _)| r),
                    Some(ChangeKind::Conflicted),
                )
            } else {
                match index_resolved(&index, old_path, Stage::Unconflicted) {
                    Some((_, true)) => (None, Some(ChangeKind::IntentToAdd)),
                    Some((resolved, false)) => (Some(resolved), None),
                    None => (None, None),
                }
            };
            let new = worktree_resolved(g, workdir, path, old.as_ref().map(|o| o.kind))?
                .map(|(resolved, _)| resolved);
            let renamed = previous_path.is_some() && old.is_some();
            let kind = match (kind_hint, &old, &new) {
                (Some(hint), _, _) => hint,
                (None, None, None) => return Err(not_found("file", path)),
                (None, None, Some(_)) => ChangeKind::Untracked,
                (None, Some(_), None) => ChangeKind::Deleted,
                (None, Some(old), Some(new)) => change_kind(old, new, renamed),
            };
            Ok(Plan {
                path: path.clone(),
                previous_path: previous_path.clone(),
                kind,
                old,
                new,
                unstaged_rename: renamed,
            })
        }
        DiffTarget::Commit {
            commit,
            against,
            path,
            previous_path,
        } => {
            let object = g
                .find_object(*commit)
                .map_err(|_| not_found("commit", commit))?;
            // `tree_files` hands out targets whose `commit` may be a tree (or a tag) id.
            let (new_tree, old_tree) = if object.kind == gix::object::Kind::Commit {
                let commit_obj = object.into_commit();
                let old_tree = against_tree(g, &commit_obj, *against)?;
                (commit_obj.tree().map_err(eh)?, old_tree)
            } else {
                let old_tree = match against {
                    Against::Empty => None,
                    Against::Commit(other) => Some(peel_tree(g, *other)?),
                    Against::FirstParent | Against::Parent(_) => {
                        return Err(not_found("commit", commit));
                    }
                };
                (object.peel_to_tree().map_err(eh)?, old_tree)
            };
            let old_path = previous_path.as_ref().unwrap_or(path);
            let old = tree_resolved(old_tree.as_ref(), old_path)?;
            let new = tree_resolved(Some(&new_tree), path)?;
            let kind = match (&old, &new) {
                (None, None) => return Err(not_found("file", path)),
                (None, Some(_)) => ChangeKind::Added,
                (Some(_), None) => ChangeKind::Deleted,
                (Some(old), Some(new)) => change_kind(old, new, previous_path.is_some()),
            };
            Ok(Plan {
                path: path.clone(),
                previous_path: previous_path.clone(),
                kind,
                old,
                new,
                unstaged_rename: false,
            })
        }
    }
}

// ───────────────────────────── file_diff ─────────────────────────────

/// The diff of one file.
///
/// Check order (spec §2.5): submodule, type change, size, image, binary, mode only, empty, text. Errors: `NotFound`
/// (the path is on neither side), `BareRepository` (unstaged diff without a work tree), `Cancelled`, `Io`.
pub fn file_diff(
    repo: &Repo,
    target: &DiffTarget,
    opts: &DiffOptions,
    cancel: &CancelToken,
) -> Result<FileDiff> {
    check_cancel(cancel)?;
    let g = repo.local();
    let plan = plan_target(repo, &g, target)?;
    let (content, old, new) = diff_sides(repo, &g, &plan, opts, cancel)?;
    Ok(FileDiff {
        target: target.clone(),
        path: plan.path,
        previous_path: plan.previous_path,
        kind: plan.kind,
        old,
        new,
        content,
    })
}

type Sides = (DiffContent, Option<BlobSide>, Option<BlobSide>);

fn blob_size(g: &gix::Repository, id: ObjectId) -> Result<u64> {
    Ok(g.try_find_header(id)
        .map_err(eh)?
        .ok_or_else(|| not_found("blob", id))?
        .size())
}

/// The content of a worktree file in the form Git would store, via the repository's filter pipeline.
fn read_worktree_git_form(g: &gix::Repository, workdir: &Path, path: &RelaPath) -> Result<Vec<u8>> {
    let full = workdir.join(path_of(path.as_bstr())?);
    let (mut pipeline, index) = g.filter_pipeline(None).map_err(eh)?;
    let md = gix::index::fs::Metadata::from_path_no_follow(&full)
        .map_err(|err| GitError::io(&full, err))?;
    let mut buf = Vec::new();
    but_core::worktree::worktree_file_to_git_in_buf(
        &mut buf,
        &md,
        path.as_bstr(),
        &full,
        &mut pipeline,
        &index,
    )
    .map_err(eh)?;
    Ok(buf)
}

fn read_raw(g: &gix::Repository, workdir: Option<&Path>, side: &Resolved) -> Result<Vec<u8>> {
    if side.worktree {
        let workdir = workdir.ok_or(GitError::BareRepository)?;
        read_worktree_git_form(g, workdir, &side.path)
    } else {
        Ok(g.find_blob(side.id)
            .map_err(|_| not_found("blob", side.id))?
            .detach()
            .data)
    }
}

/// The side of `side` whose (git-form) content is `raw`: worktree sides get the hash of that content as id.
fn side_from_bytes(
    g: &gix::Repository,
    side: Option<&Resolved>,
    raw: Option<&[u8]>,
) -> Result<Option<BlobSide>> {
    let (Some(side), Some(raw)) = (side, raw) else {
        return Ok(None);
    };
    Ok(Some(BlobSide {
        id: if side.worktree {
            compute_blob_id(g, raw)?
        } else {
            side.id
        },
        kind: side.kind,
        size: raw.len() as u64,
        from_worktree: side.worktree,
    }))
}

/// The bytes the line diff runs on: the textconv output if a driver produced one, else the git form (for worktree
/// sides the index-aware conversion in `raw`).
fn diffable_bytes<'a>(
    data: gix::diff::blob::platform::resource::Data<'a>,
    side: Option<&Resolved>,
    raw: Option<&'a [u8]>,
) -> &'a [u8] {
    use gix::diff::blob::platform::resource::Data;
    match data {
        Data::Buffer {
            buf,
            is_derived: true,
        } => buf,
        _ if side.is_some_and(|s| s.worktree) => raw.unwrap_or_default(),
        _ => data.as_slice().unwrap_or_default(),
    }
}

fn cheap_side(side: &Resolved, size: u64) -> BlobSide {
    BlobSide {
        id: side.id,
        kind: side.kind,
        size,
        from_worktree: side.worktree,
    }
}

fn submodule_summary(
    workdir: Option<&Path>,
    path: &RelaPath,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
) -> Option<SubmoduleSummary> {
    const LIMIT: usize = 50;
    let new = new?;
    let sub = gix::open(workdir?.join(path_of(path.as_bstr()).ok()?)).ok()?;
    let mut walk = sub.rev_walk([new]);
    if let Some(old) = old {
        walk = walk.with_hidden([old]);
    }
    let mut commits = Vec::new();
    let mut truncated = false;
    for info in walk.all().ok()? {
        let id = info.ok()?.id;
        if commits.len() == LIMIT {
            truncated = true;
            break;
        }
        let summary = sub
            .find_commit(id)
            .ok()
            .and_then(|commit| {
                commit
                    .message()
                    .ok()
                    .map(|message| message.summary().to_str_lossy().into_owned())
            })
            .unwrap_or_default();
        commits.push((id, summary));
    }
    let rewound = match old {
        Some(old) => sub
            .merge_base(old, new)
            .map(|base| base.map(|base| base.detach()) != Some(old))
            .unwrap_or(false),
        None => false,
    };
    Some(SubmoduleSummary {
        commits,
        truncated,
        rewound,
    })
}

fn diff_algorithm(
    g: &gix::Repository,
    opts: &DiffOptions,
    from_platform: gix::diff::blob::Algorithm,
) -> gix::diff::blob::Algorithm {
    use gix::diff::blob::Algorithm as A;
    match opts.algorithm {
        Some(DiffAlgorithm::Myers) => A::Myers,
        Some(DiffAlgorithm::MyersMinimal) => A::MyersMinimal,
        Some(DiffAlgorithm::Histogram) => A::Histogram,
        None if g.config_snapshot().string("diff.algorithm").is_some() => from_platform,
        // Git's own default; gix' blob default is Histogram.
        None => A::Myers,
    }
}

/// Whether `.gitattributes` routes `path` through the LFS filter.
fn is_lfs(g: &gix::Repository, path: &RelaPath) -> bool {
    let attempt = || -> Option<bool> {
        let index = g.index_or_empty().ok()?;
        let mut attributes = g
            .attributes_only(
                &index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            )
            .ok()?;
        let mut selected = attributes.selected_attribute_matches(Some("filter"));
        let at_path = attributes
            .at_entry(path.as_bstr(), Some(gix::index::entry::Mode::FILE))
            .ok()?;
        if !at_path.matching_attributes(&mut selected) {
            return Some(false);
        }
        Some(selected.iter().any(|attribute| {
            attribute
                .assignment
                .state
                .as_bstr()
                .is_some_and(|name| name == "lfs")
        }))
    };
    attempt().unwrap_or(false)
}

fn diff_sides(
    repo: &Repo,
    g: &gix::Repository,
    plan: &Plan,
    opts: &DiffOptions,
    cancel: &CancelToken,
) -> Result<Sides> {
    let workdir = repo.workdir();
    let (old, new) = (plan.old.as_ref(), plan.new.as_ref());

    // (1) submodules.
    if old.is_some_and(|s| s.kind == EntryKind::Submodule)
        || new.is_some_and(|s| s.kind == EntryKind::Submodule)
    {
        let commit_of = |side: Option<&Resolved>| {
            side.filter(|s| s.kind == EntryKind::Submodule && !s.id.is_null())
                .map(|s| s.id)
        };
        let (old_commit, new_commit) = (commit_of(old), commit_of(new));
        let both_submodules = old
            .zip(new)
            .is_none_or(|(o, n)| o.kind == EntryKind::Submodule && n.kind == EntryKind::Submodule);
        if both_submodules {
            return Ok((
                DiffContent::Submodule {
                    old_commit,
                    new_commit,
                    dirty: None,
                    summary: submodule_summary(workdir, &plan.path, old_commit, new_commit),
                },
                old.map(|s| cheap_side(s, 0)),
                new.map(|s| cheap_side(s, 0)),
            ));
        }
    }

    // (2) kind changes (file <-> link <-> submodule).
    if let Some((o, n)) = old.zip(new)
        && kind_class(o.kind) != kind_class(n.kind)
    {
        // Worktree file/link sides are hashed (staleness checks of a whole-file stage) unless they are too large.
        let type_change_side = |side: &Resolved| -> Result<BlobSide> {
            if !side.worktree || side.kind == EntryKind::Submodule {
                return Ok(cheap_side(side, 0));
            }
            let workdir = workdir.ok_or(GitError::BareRepository)?;
            let full = workdir.join(path_of(side.path.as_bstr())?);
            let size = std::fs::symlink_metadata(&full)
                .map(|md| md.len())
                .map_err(|err| GitError::io(full, err))?;
            if size > opts.max_file_bytes {
                return Ok(cheap_side(side, size));
            }
            let raw = read_raw(g, workdir.into(), side)?;
            Ok(side_from_bytes(g, Some(side), Some(&raw))?
                .unwrap_or_else(|| cheap_side(side, size)))
        };
        return Ok((
            DiffContent::TypeChange {
                old: o.kind,
                new: n.kind,
            },
            Some(type_change_side(o)?),
            Some(type_change_side(n)?),
        ));
    }

    // (3) size thresholds, before anything is read.
    let size_of = |side: &Resolved| -> Result<u64> {
        if side.worktree {
            let workdir = workdir.ok_or(GitError::BareRepository)?;
            let full = workdir.join(path_of(side.path.as_bstr())?);
            std::fs::symlink_metadata(&full)
                .map(|md| md.len())
                .map_err(|err| GitError::io(full, err))
        } else {
            blob_size(g, side.id)
        }
    };
    let old_size = old.map(size_of).transpose()?;
    let new_size = new.map(size_of).transpose()?;
    if old_size
        .into_iter()
        .chain(new_size)
        .any(|s| s > opts.max_file_bytes)
    {
        let null = g.object_hash().null();
        let side = |side: Option<&Resolved>, size: Option<u64>| {
            side.zip(size).map(|(s, size)| BlobSide {
                id: if s.worktree { null } else { s.id },
                kind: s.kind,
                size,
                from_worktree: s.worktree,
            })
        };
        return Ok((
            DiffContent::TooLarge {
                threshold: opts.max_file_bytes,
            },
            side(old, old_size),
            side(new, new_size),
        ));
    }
    check_cancel(cancel)?;

    let old_path = plan.previous_path.as_ref().unwrap_or(&plan.path);

    // (4) images: by extension, confirmed by sniffing the bytes, each side within `max_image_bytes`. Checked before
    // the binary decision (spec order), so an image with a textconv driver is still previewed as an image.
    let mut old_raw: Option<Vec<u8>> = None;
    let mut new_raw: Option<Vec<u8>> = None;
    let image_candidate = old.is_some_and(|s| is_blob(s.kind) && has_image_extension(old_path))
        || new.is_some_and(|s| is_blob(s.kind) && has_image_extension(&plan.path));
    let within_image_limit = |size: Option<u64>| size.is_none_or(|s| s <= opts.max_image_bytes);
    if image_candidate && within_image_limit(old_size) && within_image_limit(new_size) {
        old_raw = old.map(|s| read_raw(g, workdir, s)).transpose()?;
        new_raw = new.map(|s| read_raw(g, workdir, s)).transpose()?;
        let old_mime = old_raw.as_deref().map(|b| image_mime(old_path, b));
        let new_mime = new_raw.as_deref().map(|b| image_mime(&plan.path, b));
        if [old_mime, new_mime]
            .into_iter()
            .flatten()
            .all(|m| m.is_some())
        {
            let old_side = side_from_bytes(g, old, old_raw.as_deref())?;
            let new_side = side_from_bytes(g, new, new_raw.as_deref())?;
            let image = |bytes: Option<Vec<u8>>, mime: Option<Option<&'static str>>| {
                bytes.zip(mime.flatten()).map(|(bytes, mime)| ImageData {
                    mime,
                    bytes: Arc::from(bytes),
                })
            };
            return Ok((
                DiffContent::Image {
                    old: image(old_raw, old_mime),
                    new: image(new_raw, new_mime),
                },
                old_side,
                new_side,
            ));
        }
    }
    check_cancel(cancel)?;

    // (5) - (8): gix decides binary / textconv (attributes, diff drivers, the NUL heuristic).
    let missing_kind = gix::object::tree::EntryKind::Blob;
    let null = g.object_hash().null();
    let symlinks = symlinks_enabled(g);
    // A link checked out as a plain file (`core.symlinks=false`) is read as a file.
    let gix_kind = |side: &Resolved| {
        if side.worktree && side.kind == EntryKind::Link && !symlinks {
            gix::object::tree::EntryKind::Blob
        } else {
            to_gix_kind(side.kind)
        }
    };
    let mut cache = g
        .diff_resource_cache(
            gix::diff::blob::pipeline::Mode::ToGitUnlessBinaryToTextIsPresent,
            gix::diff::blob::pipeline::WorktreeRoots {
                old_root: None,
                new_root: new
                    .filter(|n| n.worktree)
                    .and_then(|_| workdir.map(ToOwned::to_owned)),
            },
        )
        .map_err(eh)?;
    cache
        .set_resource(
            old.map_or(null, |s| s.id),
            old.map_or(missing_kind, gix_kind),
            old_path.as_bstr(),
            gix::diff::blob::ResourceKind::OldOrSource,
            &g.objects,
        )
        .map_err(eh)?;
    cache
        .set_resource(
            new.map_or(null, |s| s.id),
            new.map_or(missing_kind, gix_kind),
            plan.path.as_bstr(),
            gix::diff::blob::ResourceKind::NewOrDestination,
            &g.objects,
        )
        .map_err(eh)?;
    let prepared = cache.prepare_diff().map_err(eh)?;

    let algorithm = match prepared.operation {
        gix::diff::blob::platform::prepare_diff::Operation::InternalDiff { algorithm } => {
            diff_algorithm(g, opts, algorithm)
        }
        _ => gix::diff::blob::Algorithm::Myers,
    };
    use gix::diff::blob::platform::resource::Data;
    let (old_data, new_data) = (prepared.old.data, prepared.new.data);
    let is_binary =
        matches!(old_data, Data::Binary { .. }) || matches!(new_data, Data::Binary { .. });
    let derived = prepared.old_or_new_is_derived;

    // Worktree sides: the git form exactly as `git add` would store it, hashed (stage/discard staleness checks).
    // gix' diff pipeline converts worktree files without the index, so it misses Git's "CRLF already in the index"
    // rule of `core.autocrlf`; the side is read again through the index-aware filter pipeline.
    if old.is_some_and(|s| s.worktree) && old_raw.is_none() {
        old_raw = old.map(|s| read_raw(g, workdir, s)).transpose()?;
    }
    if new.is_some_and(|s| s.worktree) && new_raw.is_none() {
        new_raw = new.map(|s| read_raw(g, workdir, s)).transpose()?;
    }
    let side_of = |side: Option<&Resolved>, raw: Option<&[u8]>, size: Option<u64>| match raw {
        Some(_) => side_from_bytes(g, side, raw),
        None => Ok(side.map(|s| cheap_side(s, size.unwrap_or_default()))),
    };
    let old_side = side_of(old, old_raw.as_deref(), old_size)?;
    let new_side = side_of(new, new_raw.as_deref(), new_size)?;

    // (5) binary content.
    if is_binary {
        return Ok((DiffContent::Binary, old_side, new_side));
    }

    // (6) the same content with another mode.
    if let Some((o, n)) = old_side.zip(new_side)
        && o.id == n.id
        && o.kind != n.kind
    {
        return Ok((
            DiffContent::ModeOnly {
                old: o.kind,
                new: n.kind,
            },
            old_side,
            new_side,
        ));
    }

    // The bytes to diff: textconv output if a driver produced it, else the git form (for worktree sides the
    // index-aware conversion read above).
    let old_bytes = diffable_bytes(old_data, old, old_raw.as_deref());
    let new_bytes = diffable_bytes(new_data, new, new_raw.as_deref());

    // (7) an empty file added or deleted.
    if old_bytes.is_empty() && new_bytes.is_empty() && (old.is_none() || new.is_none()) {
        return Ok((DiffContent::Empty, old_side, new_side));
    }

    // Why partial operations are not available, if they are not (spec §4.2).
    let partial = if plan.kind == ChangeKind::Conflicted {
        Some(PartialBlock::Conflict)
    } else if old.is_some_and(|s| s.kind == EntryKind::Link)
        || new.is_some_and(|s| s.kind == EntryKind::Link)
    {
        Some(PartialBlock::Symlink)
    } else if derived {
        Some(PartialBlock::TextConv)
    } else if is_lfs(g, &plan.path) {
        Some(PartialBlock::Lfs)
    } else if plan.unstaged_rename {
        Some(PartialBlock::Rename)
    } else if opts.whitespace != WhitespaceMode::None {
        Some(PartialBlock::WhitespaceIgnored)
    } else {
        None
    };
    let partial = partial.map_or(
        PartialAvailability::Allowed,
        PartialAvailability::WholeFileOnly,
    );

    check_cancel(cancel)?;
    match lines::text_diff(old_bytes, new_bytes, algorithm, opts, derived, partial) {
        Some(text) => Ok((DiffContent::Text(Arc::new(text)), old_side, new_side)),
        None => Ok((
            DiffContent::TooLarge {
                threshold: u64::from(opts.max_lines),
            },
            old_side,
            new_side,
        )),
    }
}

// ───────────────────────────── the line diff engine ─────────────────────────────

mod lines {
    use super::*;
    use gix::diff::blob::{Algorithm, InternedInput, Interner};

    pub(super) fn split_lines(bytes: &[u8]) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        for newline in bytes.find_iter(b"\n") {
            out.push(start..newline + 1);
            start = newline + 1;
        }
        if start < bytes.len() {
            out.push(start..bytes.len());
        }
        out
    }

    fn line_eol(line: &[u8]) -> Eol {
        if line.ends_with(b"\r\n") {
            Eol::CrLf
        } else if line.ends_with(b"\n") {
            Eol::Lf
        } else {
            Eol::None
        }
    }

    fn line_content(line: &[u8]) -> &[u8] {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        line.strip_suffix(b"\r").unwrap_or(line)
    }

    fn eol_style(bytes: &[u8], lines: &[Range<usize>]) -> EolStyle {
        let (mut crlf, mut lf) = (0usize, 0usize);
        for line in lines {
            match line_eol(&bytes[line.clone()]) {
                Eol::CrLf => crlf += 1,
                Eol::Lf => lf += 1,
                Eol::None => {}
            }
        }
        let (major, minor, style) = if crlf >= lf {
            (crlf, lf, EolStyle::CrLf)
        } else {
            (lf, crlf, EolStyle::Lf)
        };
        if major == 0 {
            EolStyle::None
        } else if minor >= 1 && minor * 20 > major + minor {
            EolStyle::Mixed
        } else {
            style
        }
    }

    fn normalise(line: &[u8], mode: WhitespaceMode) -> Cow<'_, [u8]> {
        let is_ws = |b: &u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c);
        match mode {
            WhitespaceMode::None => Cow::Borrowed(line),
            WhitespaceMode::IgnoreAtEol => {
                let end = line.iter().rposition(|b| !is_ws(b)).map_or(0, |i| i + 1);
                Cow::Borrowed(&line[..end])
            }
            WhitespaceMode::IgnoreChange => {
                let end = line.iter().rposition(|b| !is_ws(b)).map_or(0, |i| i + 1);
                let mut out = Vec::with_capacity(end);
                let mut in_ws = false;
                for b in &line[..end] {
                    if is_ws(b) {
                        if !in_ws {
                            out.push(b' ');
                        }
                        in_ws = true;
                    } else {
                        out.push(*b);
                        in_ws = false;
                    }
                }
                Cow::Owned(out)
            }
            WhitespaceMode::IgnoreAll => {
                Cow::Owned(line.iter().copied().filter(|b| !is_ws(b)).collect())
            }
        }
    }

    /// The change blocks of the edit script as `(old range, new range)` of 0-based line indexes. Within a block all
    /// removals precede all additions.
    pub(super) fn change_blocks(
        old: &[u8],
        old_lines: &[Range<usize>],
        new: &[u8],
        new_lines: &[Range<usize>],
        algorithm: Algorithm,
        ws: WhitespaceMode,
    ) -> Vec<(Range<u32>, Range<u32>)> {
        let mut input: InternedInput<Cow<'_, [u8]>> = InternedInput {
            before: Vec::new(),
            after: Vec::new(),
            interner: Interner::new(old_lines.len() + new_lines.len()),
        };
        input.update_before(old_lines.iter().map(|l| normalise(&old[l.clone()], ws)));
        input.update_after(new_lines.iter().map(|l| normalise(&new[l.clone()], ws)));
        let diff = gix::diff::blob::diff_with_slider_heuristics(algorithm, &input);
        diff.hunks().map(|h| (h.before, h.after)).collect()
    }

    /// Detect the encoding once per file: chardetng over the first 64 KiB of the new side, else the old side.
    fn detect_encoding(old: &[u8], new: &[u8]) -> &'static encoding_rs::Encoding {
        let sample = if new.is_empty() { old } else { new };
        let sample = &sample[..sample.len().min(64 * 1024)];
        match std::str::from_utf8(sample) {
            Ok(_) => return encoding_rs::UTF_8,
            // A multi-byte sequence cut by the sample boundary is still UTF-8.
            Err(err) if err.error_len().is_none() => return encoding_rs::UTF_8,
            Err(_) => {}
        }
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(sample, true);
        let (encoding, confident) = detector.guess_assess(None, true);
        if confident {
            encoding
        } else {
            encoding_rs::UTF_8
        }
    }

    fn decode<'a>(encoding: &'static encoding_rs::Encoding, bytes: &'a [u8]) -> Cow<'a, str> {
        if encoding == encoding_rs::UTF_8 {
            String::from_utf8_lossy(bytes)
        } else {
            encoding.decode_without_bom_handling(bytes).0
        }
    }

    struct Builder<'a> {
        old: &'a [u8],
        old_lines: &'a [Range<usize>],
        new: &'a [u8],
        new_lines: &'a [Range<usize>],
        encoding: &'static encoding_rs::Encoding,
        text: String,
        spans: Vec<Range<u32>>,
        lines: Vec<DiffLine>,
        added: u32,
        removed: u32,
    }

    impl Builder<'_> {
        fn push_line(
            &mut self,
            kind: LineKind,
            old_index: Option<u32>,
            new_index: Option<u32>,
        ) -> usize {
            let raw = match (kind, old_index, new_index) {
                (LineKind::Removed, Some(i), _) => &self.old[self.old_lines[i as usize].clone()],
                (_, _, Some(i)) => &self.new[self.new_lines[i as usize].clone()],
                (_, Some(i), None) => &self.old[self.old_lines[i as usize].clone()],
                (_, None, None) => &[],
            };
            let eol = line_eol(raw);
            let start = self.text.len() as u32;
            self.text
                .push_str(&decode(self.encoding, line_content(raw)));
            let end = self.text.len() as u32;
            let spans_at = self.spans.len() as u32;
            self.lines.push(DiffLine {
                kind,
                old_lineno: old_index.map(|i| i + 1),
                new_lineno: new_index.map(|i| i + 1),
                eol,
                text: ArenaRange { start, end },
                spans: ArenaRange {
                    start: spans_at,
                    end: spans_at,
                },
            });
            self.lines.len() - 1
        }

        fn line_text(&self, line: usize) -> &str {
            let range = self.lines[line].text;
            &self.text[range.start as usize..range.end as usize]
        }

        /// Word-level highlights for the i-th removed line against the i-th added line of one change block.
        fn highlight(&mut self, removed: Range<usize>, added: Range<usize>) {
            const MAX_LINES: usize = 200;
            const MAX_CHARS: usize = 1_000;
            let (k, m) = (removed.len(), added.len());
            if k == 0 || m == 0 || k > MAX_LINES || m > MAX_LINES {
                return;
            }
            for i in 0..k.min(m) {
                let (old_line, new_line) = (removed.start + i, added.start + i);
                let (old_text, new_text) = (self.line_text(old_line), self.line_text(new_line));
                if old_text.chars().take(MAX_CHARS + 1).count() > MAX_CHARS
                    || new_text.chars().take(MAX_CHARS + 1).count() > MAX_CHARS
                {
                    continue;
                }
                let (old_spans, new_spans) = word_diff(old_text, new_text);
                for (line, spans) in [(old_line, old_spans), (new_line, new_spans)] {
                    let start = self.spans.len() as u32;
                    self.spans.extend(spans);
                    self.lines[line].spans = ArenaRange {
                        start,
                        end: self.spans.len() as u32,
                    };
                }
            }
        }
    }

    /// Split into words, whitespace runs and single punctuation characters, with their byte offsets.
    fn tokens(text: &str) -> Vec<Range<usize>> {
        #[derive(PartialEq, Clone, Copy)]
        enum Class {
            Word,
            Space,
            Other,
        }
        let class = |c: char| {
            if c.is_alphanumeric() || c == '_' {
                Class::Word
            } else if c.is_whitespace() {
                Class::Space
            } else {
                Class::Other
            }
        };
        let mut out = Vec::new();
        let mut current: Option<(usize, Class)> = None;
        for (at, c) in text.char_indices() {
            let this = class(c);
            match current {
                Some((_, prev)) if prev == this && this != Class::Other => {}
                Some((start, _)) => {
                    out.push(start..at);
                    current = Some((at, this));
                }
                None => current = Some((at, this)),
            }
        }
        if let Some((start, _)) = current {
            out.push(start..text.len());
        }
        out
    }

    fn merge_push(spans: &mut Vec<Range<u32>>, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let range = range.start as u32..range.end as u32;
        match spans.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => spans.push(range),
        }
    }

    /// The byte ranges of `old` and `new` that differ, token by token.
    fn word_diff(old: &str, new: &str) -> (Vec<Range<u32>>, Vec<Range<u32>>) {
        let old_tokens = tokens(old);
        let new_tokens = tokens(new);
        let old_words: Vec<&str> = old_tokens.iter().map(|r| &old[r.clone()]).collect();
        let new_words: Vec<&str> = new_tokens.iter().map(|r| &new[r.clone()]).collect();
        let deadline = Instant::now() + Duration::from_millis(5);
        let ops = similar::capture_diff_slices_deadline(
            similar::Algorithm::Myers,
            &old_words,
            &new_words,
            Some(deadline),
        );
        let (mut old_spans, mut new_spans) = (Vec::new(), Vec::new());
        let byte_range = |tokens: &[Range<usize>], start: usize, len: usize| {
            if len == 0 {
                0..0
            } else {
                tokens[start].start..tokens[start + len - 1].end
            }
        };
        for op in ops {
            match op {
                similar::DiffOp::Equal { .. } => {}
                similar::DiffOp::Delete {
                    old_index, old_len, ..
                } => merge_push(&mut old_spans, byte_range(&old_tokens, old_index, old_len)),
                similar::DiffOp::Insert {
                    new_index, new_len, ..
                } => merge_push(&mut new_spans, byte_range(&new_tokens, new_index, new_len)),
                similar::DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    merge_push(&mut old_spans, byte_range(&old_tokens, old_index, old_len));
                    merge_push(&mut new_spans, byte_range(&new_tokens, new_index, new_len));
                }
            }
        }
        (old_spans, new_spans)
    }

    /// `None` when a side has more than `opts.max_lines` lines.
    pub(super) fn text_diff(
        old: &[u8],
        new: &[u8],
        algorithm: Algorithm,
        opts: &DiffOptions,
        textconv: bool,
        partial: PartialAvailability,
    ) -> Option<TextDiff> {
        let old_lines = split_lines(old);
        let new_lines = split_lines(new);
        if old_lines.len() > opts.max_lines as usize || new_lines.len() > opts.max_lines as usize {
            return None;
        }
        let blocks = change_blocks(old, &old_lines, new, &new_lines, algorithm, opts.whitespace);
        let encoding = detect_encoding(old, new);
        let (n_old, n_new) = (old_lines.len() as u32, new_lines.len() as u32);

        // Git's hunk grouping: every change gets `context` lines around it; changes with at most 2 * context equal
        // lines between them share a hunk. Full-file: one hunk over everything.
        let context = if opts.full_file {
            u32::MAX
        } else {
            opts.context_lines
        };
        let mut groups: Vec<Range<usize>> = Vec::new();
        if opts.full_file {
            if n_old > 0 || n_new > 0 {
                groups.push(0..blocks.len());
            }
        } else {
            let mut start = 0;
            for i in 1..=blocks.len() {
                let split = i == blocks.len()
                    || u64::from(blocks[i].0.start - blocks[i - 1].0.end) > 2 * u64::from(context);
                if split {
                    if i > start {
                        groups.push(start..i);
                    }
                    start = i;
                }
            }
        }

        let mut builder = Builder {
            old,
            old_lines: &old_lines,
            new,
            new_lines: &new_lines,
            encoding,
            text: String::new(),
            spans: Vec::new(),
            lines: Vec::new(),
            added: 0,
            removed: 0,
        };
        let mut hunks = Vec::with_capacity(groups.len());
        for group in groups {
            let first = group.start;
            let (lead_old, lead_new) = match blocks.get(first) {
                Some((b, a)) => (b.start, a.start),
                None => (n_old, n_new),
            };
            let lead = context.min(lead_old).min(lead_new);
            let (old_start, new_start) = (lead_old - lead, lead_new - lead);
            let first_line = builder.lines.len() as u32;
            let (mut old_pos, mut new_pos) = (old_start, new_start);
            for block in &blocks[group.clone()] {
                while old_pos < block.0.start {
                    builder.push_line(LineKind::Context, Some(old_pos), Some(new_pos));
                    old_pos += 1;
                    new_pos += 1;
                }
                let removed_first = builder.lines.len();
                for i in block.0.clone() {
                    builder.push_line(LineKind::Removed, Some(i), None);
                    builder.removed += 1;
                }
                let added_first = builder.lines.len();
                for j in block.1.clone() {
                    builder.push_line(LineKind::Added, None, Some(j));
                    builder.added += 1;
                }
                if opts.inline_highlights {
                    builder.highlight(removed_first..added_first, added_first..builder.lines.len());
                }
                old_pos = block.0.end;
                new_pos = block.1.end;
            }
            let trail = context.min(n_old - old_pos).min(n_new - new_pos);
            for _ in 0..trail {
                builder.push_line(LineKind::Context, Some(old_pos), Some(new_pos));
                old_pos += 1;
                new_pos += 1;
            }
            let (old_count, new_count) = (old_pos - old_start, new_pos - new_start);
            hunks.push(Hunk {
                header: HunkHeader {
                    old_start: if old_count == 0 {
                        old_start
                    } else {
                        old_start + 1
                    },
                    old_lines: old_count,
                    new_start: if new_count == 0 {
                        new_start
                    } else {
                        new_start + 1
                    },
                    new_lines: new_count,
                },
                lines: first_line..builder.lines.len() as u32,
            });
        }

        let missing_final_newline = |bytes: &[u8]| !bytes.is_empty() && !bytes.ends_with(b"\n");
        Some(TextDiff {
            hunks,
            lines: builder.lines,
            text: builder.text,
            spans: builder.spans,
            added: builder.added,
            removed: builder.removed,
            encoding: encoding.name(),
            old_eol: eol_style(old, &old_lines),
            new_eol: eol_style(new, &new_lines),
            old_missing_final_newline: missing_final_newline(old),
            new_missing_final_newline: missing_final_newline(new),
            textconv,
            partial,
        })
    }
}

// ───────────────────────────── commit files ─────────────────────────────

// Commits are immutable, so these caches can never go stale (spec §9: LRU 64 each, keyed by commit, against and
// options). `Repo` has no cache slot of its own, so they are process-wide and keyed by the common dir as well.
const CACHE_ENTRIES: usize = 64;

type DetailsKey = (PathBuf, ObjectId, bool, bool);

/// `Against` is not `Hash`; this mirrors it for cache keys.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum AgainstKey {
    FirstParent,
    Parent(u8),
    Commit(ObjectId),
    Empty,
}

impl From<Against> for AgainstKey {
    fn from(against: Against) -> Self {
        match against {
            Against::FirstParent => AgainstKey::FirstParent,
            Against::Parent(n) => AgainstKey::Parent(n),
            Against::Commit(id) => AgainstKey::Commit(id),
            Against::Empty => AgainstKey::Empty,
        }
    }
}

type FilesKey = (PathBuf, ObjectId, AgainstKey, bool, bool);

fn details_cache() -> &'static Mutex<LruCache<DetailsKey, CommitDetails>> {
    static CACHE: OnceLock<Mutex<LruCache<DetailsKey, CommitDetails>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(CACHE_ENTRIES).expect("not zero"),
        ))
    })
}

fn files_cache() -> &'static Mutex<LruCache<FilesKey, Arc<Vec<CommitFile>>>> {
    static CACHE: OnceLock<Mutex<LruCache<FilesKey, Arc<Vec<CommitFile>>>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(CACHE_ENTRIES).expect("not zero"),
        ))
    })
}

/// [`commit_files_inner`] through the files cache.
fn cached_commit_files(
    repo: &Repo,
    g: &gix::Repository,
    commit: ObjectId,
    against: Against,
    opts: CommitFilesOptions,
) -> Result<Arc<Vec<CommitFile>>> {
    let key: FilesKey = (
        repo.common_dir().to_owned(),
        commit,
        against.into(),
        opts.line_stats,
        opts.stash,
    );
    if let Some(hit) = files_cache()
        .lock()
        .ok()
        .and_then(|mut cache| cache.get(&key).cloned())
    {
        return Ok(hit);
    }
    let files = Arc::new(commit_files_inner(g, commit, against, opts)?);
    if let Ok(mut cache) = files_cache().lock() {
        cache.put(key, files.clone());
    }
    Ok(files)
}

fn person_key(name: &BStr, email: &BStr) -> (BString, BString) {
    (name.to_owned(), email.to_owned())
}

fn signature_of(
    sig: gix::actor::SignatureRef<'_>,
    interned: &mut HashMap<(BString, BString), Arc<Person>>,
) -> Signature {
    let person = interned
        .entry(person_key(sig.name, sig.email))
        .or_insert_with(|| {
            Arc::new(Person {
                name: sig.name.to_owned(),
                email: sig.email.to_owned(),
            })
        })
        .clone();
    let time = sig.time().unwrap_or_default();
    Signature {
        person,
        time: GitTime {
            seconds: time.seconds,
            offset_minutes: time.offset / 60,
        },
    }
}

fn decode_message(raw: &[u8], encoding: Option<&BStr>) -> String {
    match encoding.and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes())) {
        Some(enc) if enc != encoding_rs::UTF_8 => {
            enc.decode_without_bom_handling(raw).0.into_owned()
        }
        _ => String::from_utf8_lossy(raw).into_owned(),
    }
}

fn first_parent_tree_for_stats(
    g: &gix::Repository,
    old: Option<ObjectId>,
    new: ObjectId,
) -> Result<(gix::Tree<'_>, gix::Tree<'_>)> {
    let new_tree = peel_tree(g, new)?;
    let old_tree = match old {
        Some(old) => peel_tree(g, old)?,
        None => g.empty_tree(),
    };
    Ok((old_tree, new_tree))
}

/// Changed files between two trees, sorted by path. `target` builds the per-file diff target.
fn changed_files(
    g: &gix::Repository,
    old: Option<ObjectId>,
    new: ObjectId,
    line_stats: bool,
    target: &dyn Fn(&RelaPath, Option<&RelaPath>) -> DiffTarget,
) -> Result<Vec<CommitFile>> {
    use gix::object::tree::diff::ChangeDetached as C;
    let (old_tree, new_tree) = first_parent_tree_for_stats(g, old, new)?;
    let options = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites {
        limit: 1000,
        ..Default::default()
    }));
    let changes = g
        .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), options)
        .map_err(eh)?;
    let mut cache = if line_stats {
        Some(g.diff_resource_cache_for_tree_diff().map_err(eh)?)
    } else {
        None
    };
    let mut files = Vec::with_capacity(changes.len());
    for change in changes {
        let (path, previous_path, kind, old_side, new_side) = match &change {
            C::Addition {
                location,
                entry_mode,
                id,
                ..
            } => (
                location.clone(),
                None,
                ChangeKind::Added,
                None,
                Some((*id, entry_mode)),
            ),
            C::Deletion {
                location,
                entry_mode,
                id,
                ..
            } => (
                location.clone(),
                None,
                ChangeKind::Deleted,
                Some((*id, entry_mode)),
                None,
            ),
            C::Modification {
                location,
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
            } => (
                location.clone(),
                None,
                if kind_class(EntryKind::from(previous_entry_mode.kind()))
                    != kind_class(EntryKind::from(entry_mode.kind()))
                {
                    ChangeKind::TypeChanged
                } else {
                    ChangeKind::Modified
                },
                Some((*previous_id, previous_entry_mode)),
                Some((*id, entry_mode)),
            ),
            C::Rewrite {
                source_location,
                source_entry_mode,
                source_id,
                location,
                entry_mode,
                id,
                ..
            } => (
                location.clone(),
                Some(source_location.clone()),
                ChangeKind::Renamed,
                Some((*source_id, source_entry_mode)),
                Some((*id, entry_mode)),
            ),
        };
        let is_tree = old_side.is_some_and(|(_, m)| m.is_tree())
            || new_side.is_some_and(|(_, m)| m.is_tree());
        if is_tree {
            continue;
        }
        let side = |s: Option<(ObjectId, &gix::object::tree::EntryMode)>| {
            s.map(|(id, mode)| Side {
                id,
                kind: EntryKind::from(mode.kind()),
            })
        };
        let (mut lines_added, mut lines_removed) = (None, None);
        if let Some(cache) = cache.as_mut() {
            cache.clear_resource_cache_keep_allocation();
            let counts = change
                .attach(g, g)
                .diff(cache)
                .ok()
                .and_then(|mut platform| platform.line_counts().ok())
                .flatten();
            if let Some(counts) = counts {
                lines_added = Some(counts.insertions);
                lines_removed = Some(counts.removals);
            }
        }
        files.push(CommitFile {
            target: target(&path, previous_path.as_ref()),
            path,
            previous_path,
            kind,
            old: side(old_side),
            new: side(new_side),
            lines_added,
            lines_removed,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn commit_files_inner(
    g: &gix::Repository,
    commit: ObjectId,
    against: Against,
    opts: CommitFilesOptions,
) -> Result<Vec<CommitFile>> {
    let commit_obj = g
        .find_commit(commit)
        .map_err(|_| not_found("commit", commit))?;
    let old_tree = against_tree(g, &commit_obj, against)?.map(|t| t.id);
    let mut files = changed_files(g, old_tree, commit, opts.line_stats, &|path, previous| {
        DiffTarget::Commit {
            commit,
            against,
            path: path.clone(),
            previous_path: previous.cloned(),
        }
    })?;
    if opts.stash {
        let parents: Vec<ObjectId> = commit_obj.parent_ids().map(|id| id.detach()).collect();
        if let [_, _, untracked] = parents[..] {
            let untracked_commit = g
                .find_commit(untracked)
                .map_err(|_| not_found("commit", untracked))?;
            if untracked_commit.parent_ids().next().is_none() {
                let extra = changed_files(g, None, untracked, opts.line_stats, &|path, _| {
                    DiffTarget::Commit {
                        commit: untracked,
                        against: Against::Empty,
                        path: path.clone(),
                        previous_path: None,
                    }
                })?;
                files.extend(extra);
            }
        }
    }
    Ok(files)
}

/// A commit with its message and changed files.
///
/// The files are those of [`commit_files`] against the first parent (the empty tree for a root commit). Results are
/// cached (LRU, 64 entries) per `(repository, commit, options)`. Errors: `NotFound` (unknown commit).
pub fn commit_details(
    repo: &Repo,
    commit: ObjectId,
    opts: CommitFilesOptions,
) -> Result<CommitDetails> {
    let key: DetailsKey = (
        repo.common_dir().to_owned(),
        commit,
        opts.line_stats,
        opts.stash,
    );
    if let Some(hit) = details_cache()
        .lock()
        .ok()
        .and_then(|mut cache| cache.get(&key).cloned())
    {
        return Ok(hit);
    }
    let g = repo.local();
    let commit_obj = g
        .find_commit(commit)
        .map_err(|_| not_found("commit", commit))?;
    let decoded = commit_obj.decode().map_err(eh)?;
    let encoding = decoded.encoding.map(|e| e.to_str_lossy().into_owned());
    let message = decode_message(decoded.message.as_bytes(), decoded.encoding);
    let signed = decoded.extra_headers().pgp_signature().is_some();
    let mut interned = HashMap::new();
    let summary = message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
        .into_boxed_str();
    let has_body = message
        .split_once('\n')
        .is_some_and(|(_, rest)| !rest.trim().is_empty());
    let info = CommitInfo {
        id: commit,
        parents: commit_obj.parent_ids().map(|id| id.detach()).collect(),
        author: signature_of(decoded.author().map_err(eh)?, &mut interned),
        committer: signature_of(decoded.committer().map_err(eh)?, &mut interned),
        summary,
        has_body,
    };
    let files = cached_commit_files(repo, &g, commit, Against::FirstParent, opts)?;
    let details = CommitDetails {
        info,
        message,
        encoding,
        signed,
        files,
    };
    if let Ok(mut cache) = details_cache().lock() {
        cache.put(key, details.clone());
    }
    Ok(details)
}

/// The files a commit changed relative to `against`, sorted by path.
///
/// Renames are detected (limit 1000). With `opts.stash`, the files of the untracked-files commit of a stash (third
/// parent without parents) are appended as `Added`. Results are cached (LRU, 64 entries) per
/// `(repository, commit, against, options)`. Errors: `NotFound` (unknown commit or parent).
pub fn commit_files(
    repo: &Repo,
    commit: ObjectId,
    against: Against,
    opts: CommitFilesOptions,
) -> Result<Arc<Vec<CommitFile>>> {
    let g = repo.local();
    cached_commit_files(repo, &g, commit, against, opts)
}

/// Compare any two commits/trees ("compare branches", GitKraken). `old = None` => empty tree.
///
/// Each file's target is a `DiffTarget::Commit` of `new` against `old`.
pub fn tree_files(
    repo: &Repo,
    old: Option<ObjectId>,
    new: ObjectId,
    line_stats: bool,
) -> Result<Vec<CommitFile>> {
    let g = repo.local();
    let against = old.map_or(Against::Empty, Against::Commit);
    changed_files(&g, old, new, line_stats, &|path, previous| {
        DiffTarget::Commit {
            commit: new,
            against,
            path: path.clone(),
            previous_path: previous.cloned(),
        }
    })
}

// ───────────────────────────── staged patch text ─────────────────────────────

/// A minimal glob (`*` and `?`) matcher.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|b| *b == b'*')
}

fn skipped(globs: &[String], path: &[u8]) -> bool {
    let name = path.rsplit(|b| *b == b'/').next().unwrap_or(path);
    globs
        .iter()
        .any(|glob| glob_match(glob.as_bytes(), path) || glob_match(glob.as_bytes(), name))
}

fn kind_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
        ChangeKind::Renamed => "renamed",
        ChangeKind::TypeChanged => "type changed",
        ChangeKind::Untracked => "untracked",
        ChangeKind::Conflicted => "conflicted",
        ChangeKind::IntentToAdd => "intent to add",
    }
}

fn staged_changes(g: &gix::Repository) -> Result<Vec<(RelaPath, Option<RelaPath>, ChangeKind)>> {
    let index = g.index_or_empty().map_err(eh)?;
    let head_tree = match head_tree(g)? {
        Some(tree) => tree.id,
        None => g.empty_tree().id,
    };
    let mut changes = Vec::new();
    g.tree_index_status(
        &head_tree,
        &index,
        None,
        gix::status::tree_index::TrackRenames::AsConfigured,
        |change, _, _| {
            use gix::diff::index::ChangeRef as C;
            match change {
                C::Addition { location, .. } => {
                    changes.push((location.into_owned(), None, ChangeKind::Added));
                }
                C::Deletion { location, .. } => {
                    changes.push((location.into_owned(), None, ChangeKind::Deleted));
                }
                C::Modification {
                    location,
                    previous_entry_mode,
                    entry_mode,
                    ..
                } => {
                    let kind = if kind_class(index_kind(previous_entry_mode))
                        != kind_class(index_kind(entry_mode))
                    {
                        ChangeKind::TypeChanged
                    } else {
                        ChangeKind::Modified
                    };
                    changes.push((location.into_owned(), None, kind));
                }
                C::Rewrite {
                    source_location,
                    location,
                    ..
                } => {
                    changes.push((
                        location.into_owned(),
                        Some(source_location.into_owned()),
                        ChangeKind::Renamed,
                    ));
                }
            }
            Ok(std::ops::ControlFlow::Continue(()))
        },
    )
    .map_err(eh)?;
    changes.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(changes)
}

fn render_body(diff: &FileDiff) -> (String, u32, u32, bool) {
    match &diff.content {
        DiffContent::Text(text) => {
            let mut out = String::new();
            for hunk in &text.hunks {
                let h = hunk.header;
                out.push_str(&format!(
                    "@@ -{},{} +{},{} @@\n",
                    h.old_start, h.old_lines, h.new_start, h.new_lines
                ));
                for line in text.hunk_lines(hunk) {
                    out.push(match line.kind {
                        LineKind::Context => ' ',
                        LineKind::Added => '+',
                        LineKind::Removed => '-',
                    });
                    out.push_str(text.line_text(line));
                    out.push('\n');
                }
            }
            (out, text.added, text.removed, false)
        }
        DiffContent::Binary | DiffContent::Image { .. } => {
            ("Binary or too large to show.\n".to_owned(), 0, 0, true)
        }
        DiffContent::TooLarge { .. } => ("Binary or too large to show.\n".to_owned(), 0, 0, false),
        DiffContent::Empty => ("Empty file.\n".to_owned(), 0, 0, false),
        DiffContent::ModeOnly { old, new } => (
            format!(
                "Mode changed: {} -> {}.\n",
                mode_label(*old),
                mode_label(*new)
            ),
            0,
            0,
            false,
        ),
        DiffContent::TypeChange { old, new } => (
            format!(
                "Type changed: {} -> {}.\n",
                mode_label(*old),
                mode_label(*new)
            ),
            0,
            0,
            false,
        ),
        DiffContent::Submodule {
            old_commit,
            new_commit,
            ..
        } => {
            let id = |c: &Option<ObjectId>| c.map_or_else(|| "none".to_owned(), |c| c.to_string());
            (
                format!(
                    "Submodule commit {} -> {}.\n",
                    id(old_commit),
                    id(new_commit)
                ),
                0,
                0,
                false,
            )
        }
    }
}

fn mode_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Blob => "file",
        EntryKind::BlobExecutable => "executable file",
        EntryKind::Link => "symbolic link",
        EntryKind::Submodule => "submodule",
        EntryKind::Tree => "directory",
    }
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The longest prefix of `text` of at most `max_chars` characters that ends at a line boundary.
fn cut_to_chars(text: &str, max_chars: usize) -> &str {
    let mut end = text.len();
    for (count, (at, _)) in text.char_indices().enumerate() {
        if count == max_chars {
            end = at;
            break;
        }
    }
    let head = &text[..end];
    if end == text.len() {
        return head;
    }
    match head.rfind('\n') {
        Some(newline) => &head[..=newline],
        None => "",
    }
}

/// Budgeted staged diff for AI (16 A7): index vs HEAD.
///
/// The text starts with a header listing every file with `+added -removed`, followed by one `File: <path> (<kind>)`
/// block per file. Each block gets `max(per_file_min_chars, remaining / remaining_files)` characters; files matching
/// `skip_globs` get the header only; files that no longer fit are cut entirely and counted in `truncated_files`,
/// with a `[diff truncated: N more files]` footer. A file whose diff cannot be computed becomes header-only.
/// Errors: `Cancelled`, `Gix`.
pub fn staged_patch_text(
    repo: &Repo,
    opts: &PatchTextOptions,
    cancel: &CancelToken,
) -> Result<PatchText> {
    let g = repo.local();
    let changes = staged_changes(&g)?;
    let diff_opts = DiffOptions {
        context_lines: 3,
        inline_highlights: false,
        ..DiffOptions::default()
    };

    struct Entry {
        path: RelaPath,
        kind: ChangeKind,
        skip: bool,
        body: Option<String>,
    }
    let mut stats = Vec::with_capacity(changes.len());
    let mut entries = Vec::with_capacity(changes.len());
    for (path, previous_path, kind) in changes {
        check_cancel(cancel)?;
        let skip = skipped(&opts.skip_globs, path.as_slice());
        let (mut added, mut removed, mut binary) = (0, 0, false);
        let mut body = None;
        let target = DiffTarget::Staged {
            path: path.clone(),
            previous_path,
        };
        if let Ok(diff) = file_diff(repo, &target, &diff_opts, cancel) {
            let (text, a, r, b) = render_body(&diff);
            (added, removed, binary) = (a, r, b);
            body = Some(text);
        } else if cancel.is_cancelled() {
            return Err(GitError::Cancelled);
        }
        stats.push(FileStat {
            path: path.clone(),
            kind,
            added,
            removed,
            binary,
        });
        entries.push(Entry {
            path,
            kind,
            skip,
            body,
        });
    }

    let mut text = String::from("Changed files:\n");
    for stat in &stats {
        text.push_str(&format!(
            "- {} ({}) +{} -{}\n",
            stat.path.to_str_lossy(),
            kind_label(stat.kind),
            stat.added,
            stat.removed
        ));
    }
    text.push('\n');
    let mut remaining = opts.budget_chars.saturating_sub(char_len(&text));
    // Only files that get a body share the budget (skipped and failed files are header-only).
    let mut files_left = entries
        .iter()
        .filter(|e| !e.skip && e.body.is_some())
        .count();
    let mut truncated_files = 0;
    for entry in &entries {
        if entry.skip {
            continue;
        }
        let Some(body) = &entry.body else { continue };
        let share = (remaining / files_left.max(1)).max(opts.per_file_min_chars);
        files_left -= 1;
        if remaining == 0 {
            truncated_files += 1;
            continue;
        }
        let header = format!(
            "File: {} ({})\n",
            entry.path.to_str_lossy(),
            kind_label(entry.kind)
        );
        let allowed = share.min(remaining);
        let body_budget = allowed.saturating_sub(char_len(&header));
        let shown = if char_len(body) <= body_budget {
            body.as_str()
        } else {
            cut_to_chars(body, body_budget)
        };
        if shown.is_empty() && !body.is_empty() {
            truncated_files += 1;
            continue;
        }
        let cut = shown.len() < body.len();
        text.push_str(&header);
        text.push_str(shown);
        if cut {
            text.push_str("[... file truncated]\n");
        }
        remaining = remaining.saturating_sub(char_len(&header) + char_len(shown));
    }
    if truncated_files > 0 {
        text.push_str(&format!("[diff truncated: {truncated_files} more files]\n"));
    }
    Ok(PatchText {
        files: stats,
        text,
        truncated_files,
    })
}

// ───────────────────────────── export_patch / read_blob ─────────────────────────────

/// Runs `git` with `args` in the repository and returns stdout. Reads do not need the full executor: no askpass, no
/// hooks, no progress. Polls `cancel` and kills the child when it fires.
fn run_git_read(repo: &Repo, args: &[OsString], cancel: &CancelToken) -> Result<Vec<u8>> {
    let cwd = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let mut cmd = repo.runtime().blocking_git(Some(cwd));
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let printable = || {
        args.iter()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut child = cmd.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound && cwd.is_dir() {
            GitError::GitNotFound {
                searched: repo.runtime().git_path().display().to_string(),
            }
        } else {
            GitError::io(cwd, err)
        }
    })?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(err) = stderr.as_mut() {
            let _ = err.read_to_end(&mut buf);
        }
        buf
    });
    let status = loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_thread.join();
            let _ = err_thread.join();
            return Err(GitError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::io(cwd, err));
            }
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    if !status.success() {
        return Err(GitError::CommandFailed {
            args: printable(),
            exit_code: status.code(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            stdout_tail: String::new(),
        });
    }
    Ok(stdout)
}

/// The argument budget of one `git diff` invocation (Windows allows 32 767 characters in total).
const PATHSPEC_CHUNK_BYTES: usize = 24_000;

/// Exact git patch text for "Copy as patch" / "Save patch".
///
/// `Unstaged`/`Staged` run `git diff --binary --no-color --no-ext-diff --no-textconv [--cached]` over literal
/// pathspecs (in chunks below 24 000 argument bytes; outputs are concatenated in path order; no paths means
/// everything; untracked files are not part of `git diff`); `Commit` runs
/// `git format-patch -1 --stdout --binary <sha>`. Errors: `Cancelled`, `CommandFailed`, `GitNotFound`.
pub fn export_patch(repo: &Repo, what: &ExportPatch, cancel: &CancelToken) -> Result<BString> {
    check_cancel(cancel)?;
    let (paths, cached) = match what {
        ExportPatch::Commit(commit) => {
            let args = ["format-patch", "-1", "--stdout", "--binary"]
                .map(OsString::from)
                .into_iter()
                .chain([OsString::from(commit.to_string())])
                .collect::<Vec<_>>();
            return run_git_read(repo, &args, cancel).map(BString::from);
        }
        ExportPatch::Unstaged(paths) => (paths, false),
        ExportPatch::Staged(paths) => (paths, true),
    };
    let base = |cached: bool| {
        // `--no-textconv`: a textconv'd diff is not a patch `git apply` can use.
        let mut args: Vec<OsString> = [
            "diff",
            "--binary",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
        ]
        .map(OsString::from)
        .into();
        if cached {
            args.push("--cached".into());
        }
        args
    };
    let mut out = Vec::new();
    if paths.is_empty() {
        out.extend(run_git_read(repo, &base(cached), cancel)?);
        return Ok(out.into());
    }
    let mut chunk: Vec<OsString> = Vec::new();
    let mut chunk_bytes = 0;
    let flush = |chunk: &mut Vec<OsString>, out: &mut Vec<u8>| -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        let mut args = base(cached);
        args.push("--".into());
        args.append(chunk);
        out.extend(run_git_read(repo, &args, cancel)?);
        Ok(())
    };
    // One `git diff` prints its files in path order; sorting the pathspecs keeps that order across chunks.
    let mut sorted: Vec<&RelaPath> = paths.iter().collect();
    sorted.sort();
    sorted.dedup();
    for path in sorted {
        let arg = crate::internal::pathspec::literal_arg(path);
        let len = arg.len() + 1;
        if !chunk.is_empty() && chunk_bytes + len > PATHSPEC_CHUNK_BYTES {
            flush(&mut chunk, &mut out)?;
            chunk_bytes = 0;
        }
        chunk_bytes += len;
        chunk.push(arg);
    }
    flush(&mut chunk, &mut out)?;
    Ok(out.into())
}

/// Raw blob bytes (image previews, "open file at revision"), refusing above `max_bytes`.
///
/// Errors: `NotFound` (no such blob), `Unsupported` (the blob is larger than `max_bytes`; the check reads only the
/// object header).
pub fn read_blob(repo: &Repo, id: ObjectId, max_bytes: u64) -> Result<Arc<[u8]>> {
    let g = repo.local();
    let size = blob_size(&g, id)?;
    if size > max_bytes {
        return Err(GitError::Unsupported {
            what: "blob larger than the allowed size",
        });
    }
    let blob = g.find_blob(id).map_err(|_| not_found("blob", id))?;
    Ok(Arc::from(blob.detach().data))
}
