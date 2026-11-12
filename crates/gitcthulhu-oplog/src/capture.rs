//! Capturing the repository state (spec 17 §1.2, §1.3).

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::Context as _;
use bstr::{BStr, BString, ByteSlice as _};
use but_core::{TreeStatus, snapshot::create_tree};
use gix::{object::tree::EntryKind, prelude::ObjectIdExt as _, refs::FullName};

use crate::{
    OplogError, UnsupportedReason,
    error::{LOCKED_FILE_OS_ERRORS, classify},
    fail_point::{self, FailPoint},
    state::{CapturedState, FileState, HeadState, RefValue, STASH_REF, index_flag_bits},
    types::{Scope, SkipReason, SkippedFile, UntrackedCapture},
};

/// At most this many skipped files are kept per snapshot; `skipped_untracked_truncated` tells about the rest.
const MAX_SKIPPED: usize = 100;

/// Untracked-content limits of a capture (spec 17 §1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CaptureLimits {
    /// Untracked files larger than this are skipped (`TooLarge`).
    pub untracked_file_limit: u64,
    /// The admitted untracked files of one snapshot total at most this many bytes, smallest first (`OverBudget`).
    pub untracked_total_limit: u64,
    /// `false`: no untracked contents at all (`CaptureDisabled`).
    pub capture_untracked: bool,
}

/// Refuse repositories the oplog cannot snapshot and restore: bare, reftable, sparse checkout.
pub(crate) fn check_supported(repo: &gix::Repository) -> Result<(), OplogError> {
    if repo.workdir().is_none() {
        return Err(OplogError::Unsupported(UnsupportedReason::BareRepository));
    }
    let config = repo.config_snapshot();
    if config
        .string("extensions.refStorage")
        .is_some_and(|v| v.eq_ignore_ascii_case(b"reftable"))
    {
        return Err(OplogError::Unsupported(UnsupportedReason::Reftable));
    }
    if config.boolean("core.sparseCheckout") == Some(true) {
        return Err(OplogError::Unsupported(UnsupportedReason::SparseCheckout));
    }
    Ok(())
}

/// Capture the state of `repo` (§1.2). `RefsOnly` costs no status walk.
///
/// Errors: `Unsupported`, `Capture` (after one retry of the file part), `FileLocked`, `RepositoryLocked`.
pub(crate) fn capture(
    repo: &gix::Repository,
    scope: Scope,
    untracked: &UntrackedCapture,
    limits: &CaptureLimits,
) -> Result<CapturedState, OplogError> {
    check_supported(repo)?;
    fail_point::hit(repo, FailPoint::Capture)?;
    let mut state =
        capture_refs(repo, scope).map_err(|err| classify(repo, err, OplogError::Capture))?;
    if scope == Scope::Full {
        let files = match capture_files(repo, untracked, limits) {
            Ok(files) => files,
            Err(first) => {
                // A file may have changed type or become unreadable between status and hashing.
                tracing::debug!(?first, "capturing worktree and index failed, retrying once");
                capture_files(repo, untracked, limits)
                    .map_err(|err| classify(repo, err, OplogError::Capture))?
            }
        };
        state.files = Some(files.files);
        state.skipped_untracked = files.skipped;
        state.skipped_untracked_truncated = files.truncated;
    }
    Ok(state)
}

/// Steps 1-5 of §1.2: HEAD, refs, stash log, branch config, in-progress state.
fn capture_refs(repo: &gix::Repository, scope: Scope) -> anyhow::Result<CapturedState> {
    let head = read_head(repo)?;
    let mut refs = BTreeMap::new();
    for prefix in ["refs/heads/", "refs/tags/"] {
        for reference in repo.references()?.prefixed(prefix)? {
            let reference = reference.map_err(|err| anyhow::anyhow!(err))?;
            let (name, value) = ref_value(&reference.inner);
            refs.insert(name, value);
        }
    }
    if let Some(stash) = repo.try_find_reference(STASH_REF)? {
        let (name, value) = ref_value(&stash.inner);
        refs.insert(name, value);
    }
    let stash_log_path = repo.common_dir().join("logs").join("refs").join("stash");
    let stash_log = match std::fs::read(&stash_log_path) {
        Ok(bytes) => Some(BString::from(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err).with_context(|| format!("reading '{}'", stash_log_path.display()));
        }
    };
    let branch_config = read_branch_config(repo, &refs)?;
    Ok(CapturedState {
        scope,
        head,
        refs,
        stash_log,
        branch_config,
        in_progress: repo.state().map(Into::into),
        files: None,
        skipped_untracked: Vec::new(),
        skipped_untracked_truncated: false,
    })
}

fn ref_value(reference: &gix::refs::Reference) -> (FullName, RefValue) {
    let value = match &reference.target {
        gix::refs::Target::Object(id) => RefValue::Direct(*id),
        gix::refs::Target::Symbolic(name) => RefValue::Symbolic(name.clone()),
    };
    (reference.name.clone(), value)
}

fn read_head(repo: &gix::Repository) -> anyhow::Result<HeadState> {
    let head = repo.head()?;
    Ok(match head.kind {
        gix::head::Kind::Symbolic(reference) => HeadState::Symbolic(reference.name),
        gix::head::Kind::Unborn(name) => HeadState::Unborn(name),
        gix::head::Kind::Detached { target, .. } => HeadState::Detached(target),
    })
}

/// `branch.<name>.*` of every captured local branch, read freshly from `<common-dir>/config` without includes.
fn read_branch_config(
    repo: &gix::Repository,
    refs: &BTreeMap<FullName, RefValue>,
) -> anyhow::Result<BTreeMap<BString, Vec<(BString, BString)>>> {
    let path = repo.common_dir().join("config");
    let file = gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
        .with_context(|| format!("reading '{}'", path.display()))?;
    let mut out: BTreeMap<BString, Vec<(BString, BString)>> = BTreeMap::new();
    let Some(sections) = file.sections_by_name("branch") else {
        return Ok(out);
    };
    for section in sections {
        let Some(branch) = section.header().subsection_name() else {
            continue;
        };
        let mut ref_name = BString::from("refs/heads/");
        ref_name.extend_from_slice(branch);
        let is_captured = FullName::try_from(ref_name)
            .ok()
            .is_some_and(|name| refs.contains_key(&name));
        if !is_captured {
            continue;
        }
        let values = out.entry(branch.to_owned()).or_default();
        for (key, value) in section.body() {
            values.push((BString::from(key), value));
        }
    }
    out.retain(|_, values| !values.is_empty());
    Ok(out)
}

struct CapturedFiles {
    files: FileState,
    skipped: Vec<SkippedFile>,
    truncated: bool,
}

/// A requested worktree-relative path without trailing slashes and leading `./`; `.` (the worktree root) becomes
/// the empty path.
fn normalize_rela(path: &BStr) -> &BStr {
    let mut path = path.as_bytes();
    loop {
        if let Some(rest) = path.strip_suffix(b"/") {
            path = rest;
        } else if let Some(rest) = path.strip_prefix(b"./") {
            path = rest;
        } else if path == b"." {
            path = b"";
        } else {
            return path.as_bstr();
        }
    }
}

/// Which untracked paths a capture wants.
enum Wanted<'a> {
    Nothing,
    /// Requested paths without trailing slashes; a path matches if it or one of its parent directories is in the set.
    Paths(BTreeSet<&'a BStr>),
    All,
}

impl Wanted<'_> {
    fn from(untracked: &UntrackedCapture) -> Wanted<'_> {
        match untracked {
            UntrackedCapture::None => Wanted::Nothing,
            UntrackedCapture::Paths(paths) => {
                Wanted::Paths(paths.iter().map(|p| normalize_rela(p.as_bstr())).collect())
            }
            UntrackedCapture::All => Wanted::All,
        }
    }

    /// O(depth · log n): look up the path and each of its parent directories.
    fn contains(&self, path: &BStr) -> bool {
        match self {
            Wanted::Nothing => false,
            Wanted::Paths(paths) => {
                paths.contains(path)
                    || paths.contains(BStr::new(b""))
                    || path
                        .rfind_iter("/")
                        .any(|slash| paths.contains(path[..slash].as_bstr()))
            }
            Wanted::All => true,
        }
    }
}

/// Step 6 of §1.2: index and worktree as trees.
fn capture_files(
    repo: &gix::Repository,
    untracked: &UntrackedCapture,
    limits: &CaptureLimits,
) -> anyhow::Result<CapturedFiles> {
    let workdir = repo.workdir().context("need a worktree")?;
    let head_tree = repo.head_tree_id_or_empty()?.detach();
    let changes = but_core::diff::worktree_changes_no_renames(repo)?;
    let wanted = Wanted::from(untracked);

    let mut selection = BTreeSet::<BString>::new();
    let mut skipped = Vec::new();
    let mut candidates = Vec::new();
    for change in &changes.changes {
        match &change.status {
            TreeStatus::Addition {
                state,
                is_untracked: true,
            } => {
                if state.kind == EntryKind::Commit || !wanted.contains(change.path.as_bstr()) {
                    // Nested repositories are never captured; unrequested untracked files stay out of the snapshot.
                    continue;
                }
                let full_path = workdir.join(gix::path::from_bstr(change.path.as_bstr())?);
                let size = file_size(&full_path);
                let reason = if !limits.capture_untracked {
                    Some(SkipReason::CaptureDisabled)
                } else if size > limits.untracked_file_limit {
                    Some(SkipReason::TooLarge)
                } else if probe_unreadable(&full_path) {
                    Some(SkipReason::Unreadable)
                } else {
                    None
                };
                match reason {
                    Some(reason) => skipped.push(SkippedFile {
                        path: change.path.clone(),
                        size,
                        reason,
                    }),
                    None => candidates.push((size, change.path.clone())),
                }
            }
            _ => {
                selection.insert(change.path.clone());
            }
        }
    }
    // Small files win: admit the smallest first until the budget is used up.
    candidates.sort();
    let mut running = 0u64;
    for (size, path) in candidates {
        match running.checked_add(size) {
            Some(total) if total <= limits.untracked_total_limit => {
                running = total;
                selection.insert(path);
            }
            _ => skipped.push(SkippedFile {
                path,
                size,
                reason: SkipReason::OverBudget,
            }),
        }
    }
    selection.extend(
        changes
            .index_changes
            .iter()
            .map(|c| c.location().to_owned()),
    );
    selection.extend(changes.index_conflicts.iter().map(|(p, _)| p.clone()));

    let out = create_tree(
        head_tree.attach(repo),
        create_tree::State {
            changes,
            selection,
            head: false,
        },
    )?;

    let index = repo.index_or_empty()?;
    let mut index_flags = BTreeMap::new();
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let bits = index_flag_bits(entry.flags);
        if bits != 0 {
            index_flags.insert(entry.path(&index).to_owned(), bits);
        }
    }

    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    let truncated = skipped.len() > MAX_SKIPPED;
    skipped.truncate(MAX_SKIPPED);

    Ok(CapturedFiles {
        files: FileState {
            head_tree,
            worktree: out.worktree.unwrap_or(head_tree),
            index: out.index.unwrap_or(head_tree),
            index_conflicts: out.index_conflicts,
            index_flags,
        },
        skipped,
        truncated,
    })
}

/// The size of the file at `path` without following symlinks; 0 if it is gone.
fn file_size(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).map_or(0, |m| m.len())
}

/// `true` if `path` cannot be opened for reading because of permissions or because another program holds it
/// without `FILE_SHARE_READ` (Windows). `create_tree` would abort on the first such file.
fn probe_unreadable(path: &Path) -> bool {
    match std::fs::File::open(path) {
        Ok(_) => false,
        Err(err) => {
            err.kind() == std::io::ErrorKind::PermissionDenied
                || err
                    .raw_os_error()
                    .is_some_and(|code| LOCKED_FILE_OS_ERRORS.contains(&code))
        }
    }
}

/// Which of the untracked `paths` (directories expand to the untracked, non-ignored files below them) a capture
/// would skip: `CaptureDisabled`, `TooLarge` or `Unreadable`. Stat and open probe only: no hashing, no object
/// writes, no lock. Tracked, ignored and missing paths and nested repositories are not reported.
pub(crate) fn preview_skipped(
    repo: &gix::Repository,
    paths: &[BString],
    limits: &CaptureLimits,
) -> anyhow::Result<Vec<SkippedFile>> {
    let workdir = repo.workdir().context("need a worktree")?;
    let index = repo.index_or_empty()?;
    let mut excludes = repo.excludes(
        &index,
        None,
        gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
    )?;
    let mut files = BTreeSet::new();
    for path in paths {
        let rela = normalize_rela(path.as_bstr());
        if rela.is_empty() {
            collect_children(workdir, workdir, rela, &index, &mut excludes, &mut files)?;
            continue;
        }
        // Git never descends into an ignored directory, so nothing below one is untracked.
        let mut below_ignored_dir = false;
        for slash in rela.find_iter("/") {
            if excludes
                .at_entry(rela[..slash].as_bstr(), Some(gix::index::entry::Mode::DIR))?
                .is_excluded()
            {
                below_ignored_dir = true;
                break;
            }
        }
        if !below_ignored_dir {
            collect_untracked_files(workdir, rela, &index, &mut excludes, &mut files)?;
        }
    }
    let mut out = Vec::new();
    for rela in files {
        let full_path = workdir.join(gix::path::from_bstr(rela.as_bstr())?);
        let size = file_size(&full_path);
        let reason = if !limits.capture_untracked {
            Some(SkipReason::CaptureDisabled)
        } else if size > limits.untracked_file_limit {
            Some(SkipReason::TooLarge)
        } else if probe_unreadable(&full_path) {
            Some(SkipReason::Unreadable)
        } else {
            None
        };
        if let Some(reason) = reason {
            out.push(SkippedFile {
                path: rela,
                size,
                reason,
            });
        }
    }
    Ok(out)
}

/// Add `rela` (or, for a directory, every file below it) to `out` unless it is tracked, ignored, inside `.git` or a
/// nested repository.
fn collect_untracked_files(
    workdir: &Path,
    rela: &BStr,
    index: &gix::index::State,
    excludes: &mut gix::AttributeStack<'_>,
    out: &mut BTreeSet<BString>,
) -> anyhow::Result<()> {
    if rela.is_empty() || rela.split_str("/").any(|c| c.eq_ignore_ascii_case(b".git")) {
        return Ok(());
    }
    let full_path = workdir.join(gix::path::from_bstr(rela)?);
    let Ok(metadata) = std::fs::symlink_metadata(&full_path) else {
        return Ok(());
    };
    let is_dir = metadata.is_dir();
    let mode = if is_dir {
        gix::index::entry::Mode::DIR
    } else {
        gix::index::entry::Mode::FILE
    };
    if excludes.at_entry(rela, Some(mode))?.is_excluded() {
        return Ok(());
    }
    if !is_dir {
        if index.entry_by_path(rela).is_none() {
            out.insert(rela.to_owned());
        }
        return Ok(());
    }
    if full_path.join(".git").exists() {
        return Ok(());
    }
    collect_children(workdir, &full_path, rela, index, excludes, out)
}

/// Run [`collect_untracked_files()`] for every entry of the directory `rela` (empty: the worktree root) at
/// `full_path`.
fn collect_children(
    workdir: &Path,
    full_path: &Path,
    rela: &BStr,
    index: &gix::index::State,
    excludes: &mut gix::AttributeStack<'_>,
    out: &mut BTreeSet<BString>,
) -> anyhow::Result<()> {
    let mut children = Vec::new();
    for entry in std::fs::read_dir(full_path)
        .with_context(|| format!("reading directory '{}'", full_path.display()))?
    {
        let entry = entry?;
        let name = gix::path::os_str_into_bstr(&entry.file_name())
            .map(ToOwned::to_owned)
            .context("non-UTF-8 file name")?;
        let mut child = rela.to_owned();
        if !child.is_empty() {
            child.push(b'/');
        }
        child.extend_from_slice(&name);
        children.push(child);
    }
    for child in children {
        collect_untracked_files(workdir, child.as_bstr(), index, excludes, out)?;
    }
    Ok(())
}
