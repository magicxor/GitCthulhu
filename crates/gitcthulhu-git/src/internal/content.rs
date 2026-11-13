//! Blob and worktree reads in git form (WP-B0d), shared by `diff.rs` and `stage.rs`.
//!
//! - Tree, HEAD and index entry lookups by byte path (no lossy conversions).
//! - [`read_blob`]: an object side, size-checked from the object header before anything is read.
//! - [`WorktreeReader`]: a worktree side converted to git form (clean filters, `core.autocrlf`, `eol`,
//!   `working-tree-encoding`) through the repository's filter pipeline, hashed with `gix::objs::compute_hash` so the
//!   id equals what `git hash-object <path>` prints. It also converts back to worktree form for discards.
//! - [`resolve_worktree_path`] / [`writable_worktree_path`]: the one place that walks a worktree path component by
//!   component and refuses to look or write through a symbolic link or (Windows) junction.
//! - [`prepare_text`]: the binary / textconv / algorithm decision of the diff platform (attributes, diff drivers,
//!   the NUL heuristic) for both the displayed diff and the partial operations.
//!
//! Sizes are checked before reading (`find_header` / `metadata().len()`), so a side above the limit costs no read.
//! In a partial clone, [`fetch_missing_objects`] first lets Git fetch the objects that are not present locally.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use bstr::{BStr, ByteSlice};
use gix::{ObjectId, index::entry::Stage};

use crate::{
    BlobSide, CancelToken, EntryKind, GitError, OpClass, OpContext, PostState, Repo, Result, Side,
    cli::{self, CommandKind, GitCommand},
    error::classify,
};

/// One side of a diff plus the bytes the line engine needs.
#[derive(Debug, Clone)]
pub(crate) struct SideContent {
    /// Id, kind, git-form size (worktree size when not read) and origin. The id is null only for a worktree file
    /// above the size limit (not hashed) and for a worktree submodule (its commit comes from the status).
    pub side: BlobSide,
    /// The git-form bytes. `None` above the size limit, for submodules and for tree entries.
    pub data: Option<Vec<u8>>,
}

impl SideContent {
    /// `true` if the side was not read because it exceeds the size limit it was read with.
    pub fn is_too_large(&self) -> bool {
        self.data.is_none() && !matches!(self.side.kind, EntryKind::Submodule | EntryKind::Tree)
    }
}

/// An index entry as the diff and staging code need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    /// The blob (or commit for submodules) id.
    pub id: ObjectId,
    /// The kind derived from the mode.
    pub kind: EntryKind,
    /// The raw index mode, kept unchanged by partial staging (§4.5 step 6).
    pub mode: gix::index::entry::Mode,
    /// `git add -N`: the entry exists but is not yet staged (its id is the empty blob).
    pub intent_to_add: bool,
}

/// The entry kind of an index `mode` (copied from `but-core`'s private `into_tree_entry_kind`).
pub(crate) fn entry_kind_from_mode(mode: gix::index::entry::Mode) -> Result<EntryKind> {
    mode.to_tree_entry_mode()
        .map(|mode| mode.kind().into())
        .ok_or_else(|| {
            GitError::Other(anyhow::anyhow!("index entry has an invalid mode: {mode:?}"))
        })
}

/// The index entry at `path` and `stage` (`Stage::Ours` is the "old" side of an unstaged conflict diff).
pub(crate) fn index_entry(
    index: &gix::index::State,
    path: &BStr,
    stage: Stage,
) -> Result<Option<IndexEntry>> {
    let Some(entry) = index.entry_by_path_and_stage(path, stage) else {
        return Ok(None);
    };
    Ok(Some(IndexEntry {
        id: entry.id,
        kind: entry_kind_from_mode(entry.mode)?,
        mode: entry.mode,
        intent_to_add: entry
            .flags
            .contains(gix::index::entry::Flags::INTENT_TO_ADD),
    }))
}

/// `true` if `path` has an entry with a stage other than 0 (an unmerged path).
pub(crate) fn is_conflicted(index: &gix::index::State, path: &BStr) -> bool {
    index.entry_range(path).is_some_and(|range| {
        index.entries()[range]
            .iter()
            .any(|e| e.stage() != Stage::Unconflicted)
    })
}

/// The entry at the slash-separated `path` in `tree`, or `None` if there is none.
pub(crate) fn tree_entry(
    repo: &gix::Repository,
    tree: ObjectId,
    path: &BStr,
) -> Result<Option<Side>> {
    let tree = repo.find_tree(tree)?;
    let entry = tree.lookup_entry(path.split(|b| *b == b'/'))?;
    Ok(entry.map(|entry| Side {
        id: entry.object_id(),
        kind: entry.mode().kind().into(),
    }))
}

/// The entry at `path` in `HEAD^{tree}`; `None` if absent or if HEAD is unborn.
pub(crate) fn head_entry(repo: &gix::Repository, path: &BStr) -> Result<Option<Side>> {
    let tree = repo.head_tree_id_or_empty()?.detach();
    if tree == ObjectId::empty_tree(repo.object_hash()) {
        return Ok(None);
    }
    tree_entry(repo, tree, path)
}

/// Make the objects `ids` readable by gix in a partial clone (`extensions.partialClone`, or a remote with
/// `promisor = true`). Git fetches objects that are missing locally (after `clone --filter=blob:none` the blobs of
/// older commits) from the promisor remote on demand; gix does not. Missing objects are looked up by
/// `git cat-file --batch-check` (several, one process) or `git cat-file -s <id>`, which trigger Git's lazy fetch
/// (credential helpers apply; prompts are refused).
///
/// Nothing runs outside partial clones or when every object is present. Errors: `Cancelled`, and the classified
/// failure of the fetch (`Network`, `AuthFailed`, ...).
pub(crate) fn fetch_missing_objects(
    repo: &Repo,
    ids: &[ObjectId],
    cancel: &CancelToken,
) -> Result<()> {
    let g = repo.local();
    let mut missing: Vec<ObjectId> = ids
        .iter()
        .filter(|id| !id.is_null() && !g.has_object(id))
        .copied()
        .collect();
    if missing.is_empty() || !is_partial_clone(&g) {
        return Ok(());
    }
    missing.sort();
    missing.dedup();
    let ctx = OpContext::silent(cancel.clone());
    let lazy_fetching = |cmd: GitCommand| {
        let mut cmd = cmd;
        // A user's `GIT_NO_LAZY_FETCH=1` would turn the fetch off.
        cmd.extra_env.push(("GIT_NO_LAZY_FETCH".into(), "0".into()));
        cmd
    };
    if missing.len() > 1 {
        let mut cmd = lazy_fetching(
            GitCommand::new(repo, CommandKind::Network).args(["cat-file", "--batch-check"]),
        );
        cmd.stdin = Some(
            missing
                .iter()
                .map(|id| format!("{id}\n"))
                .collect::<String>()
                .into(),
        );
        // An object it could not fetch is reported as missing; the single lookups below name the failure.
        cli::run(repo.runtime(), cmd, &ctx)?;
        missing.retain(|id| !g.has_object(id));
    }
    for id in missing {
        let cmd = lazy_fetching(
            GitCommand::new(repo, CommandKind::Network)
                .args(["cat-file", "-s"])
                .arg(id.to_string()),
        );
        let out = cli::run(repo.runtime(), cmd, &ctx)?;
        if !out.success() {
            return Err(classify(OpClass::Fetch, &out, &PostState::default()));
        }
    }
    Ok(())
}

/// Whether Git may fetch missing objects on demand: `extensions.partialClone` or any `remote.<name>.promisor`.
pub(crate) fn is_partial_clone(repo: &gix::Repository) -> bool {
    let config = repo.config_snapshot();
    config.string("extensions.partialClone").is_some()
        || repo
            .remote_names()
            .iter()
            .any(|name| config.boolean(format!("remote.{name}.promisor").as_str()) == Some(true))
}

/// Read the object side `side`. Submodules and trees (a directory entry of a sparse index) are not read. If the
/// object is larger than `max_bytes` (known from the object header), its data is `None`.
pub(crate) fn read_blob(repo: &gix::Repository, side: Side, max_bytes: u64) -> Result<SideContent> {
    let blob_side = |size| BlobSide {
        id: side.id,
        kind: side.kind,
        size,
        from_worktree: false,
    };
    if matches!(side.kind, EntryKind::Submodule | EntryKind::Tree) {
        return Ok(SideContent {
            side: blob_side(0),
            data: None,
        });
    }
    let size = repo.find_header(side.id)?.size();
    if size > max_bytes {
        return Ok(SideContent {
            side: blob_side(size),
            data: None,
        });
    }
    let data = repo.find_blob(side.id)?.take_data();
    Ok(SideContent {
        side: blob_side(data.len() as u64),
        data: Some(data),
    })
}

/// Where a worktree path leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreePath {
    /// The absolute path. No component before the last one is a symbolic link or junction; components that do not
    /// exist are fine (nothing below them exists either). The last component itself may be a link.
    Direct(PathBuf),
    /// A leading component is a symbolic link or (Windows) junction, so the path would name a different file than
    /// Git's path, possibly outside the worktree. Git treats such a path as missing (`has_symlinked_leading_path`).
    BehindLink,
}

/// Resolve the slash-separated `rela` below `workdir`, checking every leading component with `symlink_metadata`.
///
/// This is done here rather than with gix's `SymlinkCheck`, which performs no verification on Windows. Rust's
/// `FileType::is_symlink` is true for every name-surrogate reparse point on Windows, so directory symlinks and
/// junctions (`mklink /J`) are both caught. A trailing `/` (a directory path) is ignored.
///
/// Errors: `InvalidName` for a component that is not a valid path on this platform, `Io` if a component cannot be
/// inspected.
pub(crate) fn resolve_worktree_path(workdir: &Path, rela: &BStr) -> Result<WorktreePath> {
    let trimmed = rela.trim_end_with(|c| c == '/').as_bstr();
    let mut current = workdir.to_owned();
    let mut components = trimmed.split_str("/").peekable();
    let mut checking = true;
    while let Some(component) = components.next() {
        current.push(gix::path::from_byte_slice(component).map_err(|_| invalid_path(rela))?);
        if !checking || components.peek().is_none() {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(md) if md.file_type().is_symlink() => return Ok(WorktreePath::BehindLink),
            Ok(_) => {}
            Err(err) if is_missing(&err) => checking = false,
            Err(err) => return Err(GitError::io(&current, err)),
        }
    }
    Ok(WorktreePath::Direct(current))
}

/// [`resolve_worktree_path`] for writes and deletions: a path behind a symbolic link or junction is refused with
/// `InvalidName { what: "path inside the worktree" }`, even if the link stays inside the worktree (the write would
/// land on another path than the one the user selected).
pub(crate) fn writable_worktree_path(workdir: &Path, rela: &BStr) -> Result<PathBuf> {
    match resolve_worktree_path(workdir, rela)? {
        WorktreePath::Direct(path) => Ok(path),
        WorktreePath::BehindLink => Err(invalid_path(rela)),
    }
}

fn invalid_path(rela: &BStr) -> GitError {
    GitError::InvalidName {
        what: "path inside the worktree",
        name: rela.to_str_lossy().into_owned(),
    }
}

/// Reads worktree files in git form. Create one per operation; it holds the filter pipeline (with its attribute
/// stack) and the index that configured it.
pub(crate) struct WorktreeReader<'repo> {
    repo: &'repo gix::Repository,
    pipeline: gix::filter::Pipeline<'repo>,
    index: gix::worktree::IndexPersistedOrInMemory,
    workdir: PathBuf,
    /// `core.fileMode`: if `false`, the executable bit comes from the index entry.
    executable_bit: bool,
    /// `core.symlinks`: if `false`, Git checks links out as plain files holding the target.
    symlinks: bool,
}

/// A worktree side together with the raw (worktree form) bytes it was converted from.
pub(crate) struct RawSide {
    /// The side in git form, as [`WorktreeReader::read`] returns it.
    pub content: SideContent,
    /// The bytes on disk; `None` when the side was not read (too large, submodule) and for symbolic links.
    pub raw: Option<Vec<u8>>,
}

impl<'repo> WorktreeReader<'repo> {
    /// Set up the filter pipeline for `repo`'s worktree. `BareRepository` without a worktree.
    pub fn new(repo: &'repo gix::Repository) -> Result<Self> {
        let workdir = repo.workdir().ok_or(GitError::BareRepository)?.to_owned();
        let (pipeline, index) = repo.filter_pipeline(None)?;
        let fs = repo.filesystem_options()?;
        Ok(WorktreeReader {
            repo,
            pipeline,
            index,
            workdir,
            executable_bit: fs.executable_bit,
            symlinks: fs.symlink,
        })
    }

    /// The index the pipeline was configured with (read when the reader was created).
    pub fn index(&self) -> &gix::index::State {
        &self.index
    }

    /// Read the worktree file at `rela` in git form. `None` if nothing trackable is there (missing, behind a symbolic
    /// link or junction, a plain directory, a FIFO). A nested repository is a submodule side without data whose id is
    /// the nested repository's `HEAD` (null if it has none). If the file on disk is larger than `max_bytes`, it is
    /// neither read nor hashed (null id, `data: None`, the on-disk size).
    ///
    /// `index_kind` is the kind of the index entry the file is compared with (stage 0, or "ours" for a conflict, at
    /// the old path of a rename). The kind is a symlink for symbolic links, and for a plain file whose index entry is
    /// a link while `core.symlinks` is false; executable if `core.fileMode` is true and the file has an executable
    /// bit, or (with `core.fileMode=false`) if `index_kind` is executable.
    pub fn read(
        &mut self,
        rela: &BStr,
        max_bytes: u64,
        index_kind: Option<EntryKind>,
    ) -> Result<Option<SideContent>> {
        Ok(self
            .read_inner(rela, max_bytes, index_kind, false)?
            .map(|side| side.content))
    }

    /// [`Self::read`], also returning the raw bytes the git form was converted from (one read, so both belong to the
    /// same file content).
    pub fn read_with_raw(
        &mut self,
        rela: &BStr,
        max_bytes: u64,
        index_kind: Option<EntryKind>,
    ) -> Result<Option<RawSide>> {
        self.read_inner(rela, max_bytes, index_kind, true)
    }

    fn read_inner(
        &mut self,
        rela: &BStr,
        max_bytes: u64,
        index_kind: Option<EntryKind>,
        keep_raw: bool,
    ) -> Result<Option<RawSide>> {
        let path = match resolve_worktree_path(&self.workdir, rela)? {
            WorktreePath::Direct(path) => path,
            // Git does not look behind a symbolic link either (`has_symlinked_leading_path`): the tracked file is gone.
            WorktreePath::BehindLink => return Ok(None),
        };
        let md = match gix::index::fs::Metadata::from_path_no_follow(&path) {
            Ok(md) => md,
            Err(err) if is_missing(&err) => return Ok(None),
            Err(err) => return Err(GitError::io(path, err)),
        };
        let side = |id, kind, size| BlobSide {
            id,
            kind,
            size,
            from_worktree: true,
        };
        let unread = |content| RawSide { content, raw: None };
        let null = ObjectId::null(self.repo.object_hash());
        if md.is_dir() {
            if !path.join(".git").exists() {
                return Ok(None);
            }
            let head = gix::open(&path)
                .ok()
                .and_then(|sub| sub.head_id().ok().map(|id| id.detach()))
                .unwrap_or(null);
            return Ok(Some(unread(SideContent {
                side: side(head, EntryKind::Submodule, 0),
                data: None,
            })));
        }
        let kind = if md.is_symlink() {
            EntryKind::Link
        } else if !md.is_file() {
            // FIFOs, sockets, devices: nothing Git can track.
            return Ok(None);
        } else if !self.symlinks && index_kind == Some(EntryKind::Link) {
            // Without symlink support Git checks a link out as a plain file holding the target, and the index entry
            // stays a link: this is still a link, not a type change.
            EntryKind::Link
        } else if self.executable_bit {
            if md.is_executable() {
                EntryKind::BlobExecutable
            } else {
                EntryKind::Blob
            }
        } else if index_kind == Some(EntryKind::BlobExecutable) {
            EntryKind::BlobExecutable
        } else {
            EntryKind::Blob
        };
        if md.len() > max_bytes {
            return Ok(Some(unread(SideContent {
                side: side(null, kind, md.len()),
                data: None,
            })));
        }
        let (data, raw) = if md.is_symlink() {
            let target = match std::fs::read_link(&path) {
                Ok(target) => target,
                Err(err) if is_missing(&err) => return Ok(None),
                Err(err) => return Err(GitError::io(path, err)),
            };
            let target =
                gix::path::os_string_into_bstring(target.into()).map_err(|_| invalid_path(rela))?;
            (Vec::from(target), None)
        } else {
            let raw = match std::fs::read(&path) {
                Ok(raw) => raw,
                Err(err) if is_missing(&err) => return Ok(None),
                Err(err) => return Err(GitError::io(path, err)),
            };
            self.raw_to_git(raw, rela, keep_raw)?
        };
        let id = hash_blob(self.repo, &data)?;
        Ok(Some(RawSide {
            content: SideContent {
                side: side(id, kind, data.len() as u64),
                data: Some(data),
            },
            raw,
        }))
    }

    /// Convert `raw` to git form; the raw bytes are handed back only if `keep_raw` (an unchanged file is then copied
    /// once, otherwise it is moved).
    fn raw_to_git(
        &mut self,
        raw: Vec<u8>,
        rela: &BStr,
        keep_raw: bool,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
        use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
        let path = rela_to_path(rela)?;
        let converted = match self.pipeline.convert_to_git(&raw[..], &path, &self.index)? {
            ToGitOutcome::Unchanged(_) => None,
            ToGitOutcome::Buffer(buf) => Some(buf.to_vec()),
            ToGitOutcome::Process(mut stream) => {
                let mut out = Vec::with_capacity(raw.len());
                stream
                    .read_to_end(&mut out)
                    .map_err(|err| GitError::io(path.to_path_buf(), err))?;
                Some(out)
            }
        };
        Ok(match (converted, keep_raw) {
            (Some(git), true) => (git, Some(raw)),
            (Some(git), false) => (git, None),
            (None, true) => (raw.clone(), Some(raw)),
            (None, false) => (raw, None),
        })
    }

    /// Convert raw worktree bytes of `rela` to git form (`convert_to_git`, with the index-aware `core.autocrlf`
    /// rules). Used to verify a discard result before it is written.
    pub fn convert_to_git(&mut self, raw: &[u8], rela: &BStr) -> Result<Vec<u8>> {
        let path = rela_to_path(rela)?;
        let mut out = Vec::with_capacity(raw.len());
        self.pipeline
            .convert_to_git(raw, &path, &self.index)?
            .read_to_end(&mut out)
            .map_err(|err| GitError::io(path.into_owned(), err))?;
        Ok(out)
    }

    /// Convert git-form bytes of `rela` to worktree form (`convert_to_worktree`, delayed filters forbidden): what a
    /// checkout writes.
    pub fn convert_to_worktree(&mut self, git_form: &[u8], rela: &BStr) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(git_form.len() + git_form.len() / 16);
        self.pipeline
            .convert_to_worktree(
                git_form,
                rela,
                gix::filter::plumbing::pipeline::convert::to_worktree::Options {
                    can_delay: gix::filter::plumbing::driver::apply::Delay::Forbid,
                    ..Default::default()
                },
            )?
            .read_to_end(&mut out)
            .map_err(|err| GitError::io(PathBuf::from(rela.to_string()), err))?;
        Ok(out)
    }
}

/// One side as the diff platform sees it, for [`prepare_text`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resource<'a> {
    /// The object id; ignored for worktree sides (the platform reads the file itself).
    pub id: ObjectId,
    /// The entry kind.
    pub kind: EntryKind,
    /// The path whose attributes and diff driver apply.
    pub path: &'a BStr,
    /// The side is the worktree file.
    pub worktree: bool,
}

/// What the diff platform decided for a pair of sides.
#[derive(Debug)]
pub(crate) struct Prepared {
    /// Either side is binary (attributes, a diff driver's `binary`, or the NUL heuristic).
    pub binary: bool,
    /// The text a diff driver's textconv produced for the old side, if it ran.
    pub old_textconv: Option<Vec<u8>>,
    /// The text a diff driver's textconv produced for the new side, if it ran.
    pub new_textconv: Option<Vec<u8>>,
    /// The line algorithm Git would use: the diff driver's `algorithm`, else `diff.algorithm` (default Myers).
    pub algorithm: gix::diff::blob::Algorithm,
}

/// Decide binary / textconv / algorithm like Git's diff machinery, through gix's diff resource cache in
/// `ToGitUnlessBinaryToTextIsPresent` mode. A missing side counts as empty, with the other side's kind.
///
/// The displayed diff and the partial operations both call this, so `TextDiff::partial` and the refusal of a
/// partial operation always agree. A worktree side is read by the platform itself (only to decide this); the bytes
/// the line engine diffs come from [`WorktreeReader`], whose conversion knows the index.
pub(crate) fn prepare_text(
    repo: &gix::Repository,
    old: Option<Resource<'_>>,
    new: Option<Resource<'_>>,
) -> Result<Prepared> {
    use gix::diff::blob::{
        ResourceKind,
        pipeline::{Mode, WorktreeRoots},
        platform::{prepare_diff::Operation, resource::Data},
    };
    let Some(any) = new.or(old) else {
        return Ok(Prepared {
            binary: false,
            old_textconv: None,
            new_textconv: None,
            algorithm: repo.diff_algorithm()?,
        });
    };
    let symlinks = repo.filesystem_options()?.symlink;
    let root = |side: Option<Resource<'_>>| {
        side.filter(|s| s.worktree)
            .and_then(|_| repo.workdir().map(ToOwned::to_owned))
    };
    let mut cache = repo.diff_resource_cache(
        Mode::ToGitUnlessBinaryToTextIsPresent,
        WorktreeRoots {
            old_root: root(old),
            new_root: root(new),
        },
    )?;
    let null = repo.object_hash().null();
    for (side, role) in [
        (old, ResourceKind::OldOrSource),
        (new, ResourceKind::NewOrDestination),
    ] {
        let resource = side.unwrap_or(Resource {
            id: null,
            worktree: false,
            ..any
        });
        // A link checked out as a plain file (`core.symlinks=false`) is read as a file.
        let kind = if resource.worktree && resource.kind == EntryKind::Link && !symlinks {
            EntryKind::Blob
        } else {
            resource.kind
        };
        let id = if resource.worktree || side.is_none() {
            null
        } else {
            resource.id
        };
        cache
            .set_resource(id, tree_kind(kind), resource.path, role, &repo.objects)
            .map_err(|err| GitError::Other(err.into()))?;
    }
    let prepared = cache
        .prepare_diff()
        .map_err(|err| GitError::Other(err.into()))?;
    let textconv = |data: Data<'_>| match data {
        Data::Buffer {
            buf,
            is_derived: true,
        } => Some(buf.to_vec()),
        _ => None,
    };
    let binary = matches!(prepared.old.data, Data::Binary { .. })
        || matches!(prepared.new.data, Data::Binary { .. })
        || matches!(prepared.operation, Operation::SourceOrDestinationIsBinary);
    let algorithm = match prepared.operation {
        Operation::InternalDiff { algorithm } => algorithm,
        Operation::ExternalCommand { .. } | Operation::SourceOrDestinationIsBinary => {
            repo.diff_algorithm()?
        }
    };
    Ok(Prepared {
        binary,
        old_textconv: textconv(prepared.old.data),
        new_textconv: textconv(prepared.new.data),
        algorithm,
    })
}

/// The tree entry kind gix's diff platform expects for `kind`.
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

/// The blob id of `data` (what `git hash-object` computes for git-form content).
pub(crate) fn hash_blob(repo: &gix::Repository, data: &[u8]) -> Result<ObjectId> {
    gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, data)
        .map_err(|err| GitError::Other(anyhow::Error::new(err).context("could not hash blob")))
}

fn rela_to_path(rela: &BStr) -> Result<std::borrow::Cow<'_, Path>> {
    gix::path::from_bstr(rela).map_err(|err| {
        GitError::Other(
            anyhow::Error::new(err).context(format!("path '{rela}' is not valid on this platform")),
        )
    })
}

/// A path that vanished or whose parent is no longer a directory counts as missing.
fn is_missing(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// Throw-away repositories for the unit tests of `internal`, built with the `git` executable in an isolated
/// environment (no system or global configuration of the developer leaks in).
#[cfg(test)]
pub(crate) mod fixture {
    use std::{path::PathBuf, process::Command};

    /// A repository in a temporary directory.
    pub(crate) struct TestRepo {
        pub path: PathBuf,
        home: PathBuf,
        _tmp: tempfile::TempDir,
    }

    /// `git init -b main` in a fresh temporary directory with a minimal global configuration.
    pub(crate) fn repo() -> TestRepo {
        let tmp = tempfile::Builder::new()
            .prefix("gc")
            .tempdir()
            .expect("temp dir");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).expect("temp dir is writable");
        std::fs::write(
            home.join("gitconfig"),
            "[user]\n\tname = Test\n\temail = test@example.com\n[core]\n\tautocrlf = false\n\
             [commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n",
        )
        .expect("temp dir is writable");
        let path = tmp.path().join("repo");
        std::fs::create_dir_all(&path).expect("temp dir is writable");
        let repo = TestRepo {
            path,
            home,
            _tmp: tmp,
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    impl TestRepo {
        fn command(&self, args: &[&str]) -> Command {
            let mut cmd = Command::new("git");
            cmd.current_dir(&self.path)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
                .env("HOME", &self.home)
                .env("USERPROFILE", &self.home)
                .env("XDG_CONFIG_HOME", self.home.join(".config"))
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args);
            cmd
        }

        /// Run `git <args>` in the repository; panics with stderr on failure. Returns stdout bytes.
        pub fn git_bytes(&self, args: &[&str]) -> Vec<u8> {
            let out = self.command(args).output().expect("git is installed");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        }

        /// Run `git <args>`, expecting it to fail.
        pub fn git_fail(&self, args: &[&str]) {
            let out = self.command(args).output().expect("git is installed");
            assert!(!out.status.success(), "git {args:?} was expected to fail");
        }

        /// Like [`Self::git_bytes`], as trimmed UTF-8.
        pub fn git(&self, args: &[&str]) -> String {
            String::from_utf8_lossy(&self.git_bytes(args))
                .trim_end()
                .to_owned()
        }

        /// Like [`Self::git`], with extra environment variables (e.g. `GIT_COMMITTER_DATE`).
        pub fn git_env(&self, args: &[&str], env: &[(&str, &str)]) -> String {
            let out = self
                .command(args)
                .envs(env.iter().copied())
                .output()
                .expect("git is installed");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
        }

        /// Write `rela` (creating parent directories).
        pub fn write(&self, rela: &str, content: impl AsRef<[u8]>) {
            let path = self.path.join(rela);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("temp dir is writable");
            }
            std::fs::write(path, content).expect("temp dir is writable");
        }

        /// Open with gix in isolation (only the repository's own configuration counts).
        pub fn open(&self) -> gix::Repository {
            gix::open_opts(&self.path, gix::open::Options::isolated()).expect("valid repository")
        }
    }
}

#[cfg(test)]
mod tests {
    use bstr::ByteSlice;

    use super::{fixture, *};

    fn oid(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.trim().as_bytes()).expect("git prints hex ids")
    }

    #[test]
    fn worktree_side_id_equals_git_hash_object() {
        let repo = fixture::repo();
        repo.git(&["config", "core.autocrlf", "true"]);
        repo.write("crlf.txt", b"one\r\ntwo\r\n");
        repo.write("plain.txt", b"plain\n");
        let local = repo.open();
        let mut reader = WorktreeReader::new(&local).expect("worktree");

        let crlf = reader
            .read(b"crlf.txt".as_bstr(), u64::MAX, None)
            .expect("readable")
            .expect("present");
        assert_eq!(
            crlf.data.as_deref(),
            Some(b"one\ntwo\n".as_slice()),
            "autocrlf=true stores LF"
        );
        assert_eq!(
            crlf.side.id,
            oid(&repo.git(&["hash-object", "crlf.txt"])),
            "same id as git hash-object"
        );
        assert_eq!(crlf.side.size, 8, "the size is the git-form size");
        assert!(crlf.side.from_worktree, "it came from the worktree");
        assert_eq!(crlf.side.kind, EntryKind::Blob, "a plain file");

        let plain = reader
            .read(b"plain.txt".as_bstr(), u64::MAX, None)
            .expect("readable")
            .expect("present");
        assert_eq!(
            plain.side.id,
            oid(&repo.git(&["hash-object", "plain.txt"])),
            "same id as git hash-object"
        );

        assert!(
            reader
                .read(b"missing.txt".as_bstr(), u64::MAX, None)
                .expect("no error")
                .is_none(),
            "missing is None"
        );
        assert!(
            reader
                .read(b"plain.txt/below".as_bstr(), u64::MAX, None)
                .expect("no error")
                .is_none(),
            "a path below a file is missing, not an error"
        );

        let large = reader
            .read(b"plain.txt".as_bstr(), 3, None)
            .expect("readable")
            .expect("present");
        assert!(large.is_too_large(), "6 bytes exceed the limit of 3");
        assert!(
            large.side.id.is_null(),
            "too-large worktree files are not hashed"
        );
        assert_eq!(large.side.size, 6, "the on-disk size is reported");

        let back = reader
            .convert_to_worktree(b"a\nb\n", b"crlf.txt".as_bstr())
            .expect("convertible");
        assert_eq!(back, b"a\r\nb\r\n", "autocrlf=true checks out CRLF");
        let git_form = reader
            .convert_to_git(&back, b"crlf.txt".as_bstr())
            .expect("convertible");
        assert_eq!(git_form, b"a\nb\n", "the round trip is lossless");
    }

    #[test]
    fn worktree_kinds_from_index_and_nested_repositories() {
        let repo = fixture::repo();
        repo.git(&["config", "core.fileMode", "false"]);
        repo.write("run.sh", b"#!/bin/sh\n");
        repo.write("plain.txt", b"plain\n");
        repo.git(&["add", "run.sh", "plain.txt"]);
        repo.git(&["update-index", "--chmod=+x", "run.sh"]);
        repo.git(&["init", "-q", "nested"]);
        std::fs::create_dir_all(repo.path.join("emptydir")).expect("temp dir is writable");
        let local = repo.open();
        let mut reader = WorktreeReader::new(&local).expect("worktree");

        let read = |reader: &mut WorktreeReader<'_>, rela: &str| {
            let rela = rela.as_bytes().as_bstr();
            let index_kind = index_entry(reader.index(), rela, Stage::Unconflicted)
                .expect("valid mode")
                .map(|entry| entry.kind);
            reader.read(rela, u64::MAX, index_kind).expect("readable")
        };
        assert_eq!(
            read(&mut reader, "run.sh").expect("present").side.kind,
            EntryKind::BlobExecutable,
            "core.fileMode=false: the executable bit comes from the index"
        );
        assert_eq!(
            read(&mut reader, "plain.txt").expect("present").side.kind,
            EntryKind::Blob,
            "a regular index entry stays a regular file"
        );
        let sub = read(&mut reader, "nested").expect("a nested repository");
        assert_eq!(
            sub.side.kind,
            EntryKind::Submodule,
            "it is a submodule side"
        );
        assert!(
            sub.data.is_none() && !sub.is_too_large(),
            "submodules are not read and are not 'too large'"
        );
        assert!(
            read(&mut reader, "emptydir").is_none(),
            "a plain directory is nothing trackable"
        );
    }

    /// A directory link at `link` to `target`: a symlink on Unix, a junction on Windows (`mklink /J` needs no
    /// privilege and leads anywhere, just like a symlink).
    fn dir_link(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("symlinks work");
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .expect("cmd runs");
            assert!(status.success(), "mklink /J creates a junction");
        }
    }

    #[test]
    fn path_behind_linked_directory_is_missing_and_not_writable() {
        let repo = fixture::repo();
        repo.write("real/f.txt", b"x\n");
        dir_link(&repo.path.join("real"), &repo.path.join("link"));
        let local = repo.open();
        let mut reader = WorktreeReader::new(&local).expect("worktree");
        assert!(
            reader
                .read(b"link/f.txt".as_bstr(), u64::MAX, None)
                .expect("not an error")
                .is_none(),
            "Git does not look through a linked directory: the path is missing"
        );
        assert_eq!(
            resolve_worktree_path(&repo.path, b"link/f.txt".as_bstr()).expect("inspectable"),
            WorktreePath::BehindLink,
            "the junction or symlink is detected on every platform"
        );
        let err = writable_worktree_path(&repo.path, b"link/f.txt".as_bstr())
            .expect_err("never written through a link");
        assert_eq!(
            err.kind(),
            crate::GitErrorKind::InvalidName,
            "a classified refusal: {err:?}"
        );
        assert_eq!(
            writable_worktree_path(&repo.path, b"link".as_bstr()).expect("the link itself"),
            repo.path.join("link"),
            "the last component may be a link (deleting it removes the link)"
        );
        assert_eq!(
            writable_worktree_path(&repo.path, b"real/new/deeper.txt".as_bstr())
                .expect("missing components are fine"),
            repo.path.join("real").join("new").join("deeper.txt"),
            "nothing below a missing directory can be a link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_side_is_its_target() {
        let repo = fixture::repo();
        repo.write("real/f.txt", b"x\n");
        std::os::unix::fs::symlink("real", repo.path.join("link")).expect("symlinks work");
        let local = repo.open();
        let mut reader = WorktreeReader::new(&local).expect("worktree");
        let link = reader
            .read(b"link".as_bstr(), u64::MAX, None)
            .expect("readable")
            .expect("present");
        assert_eq!(link.side.kind, EntryKind::Link, "the link itself");
        assert_eq!(
            link.data.as_deref(),
            Some(b"real".as_slice()),
            "a link's git form is its target"
        );
    }

    #[test]
    fn read_with_raw_returns_both_forms_of_one_read() {
        let repo = fixture::repo();
        repo.git(&["config", "core.autocrlf", "true"]);
        repo.write("crlf.txt", b"one\r\ntwo\n");
        repo.write("lf.txt", b"one\n");
        let local = repo.open();
        let mut reader = WorktreeReader::new(&local).expect("worktree");
        let side = reader
            .read_with_raw(b"crlf.txt".as_bstr(), u64::MAX, None)
            .expect("readable")
            .expect("present");
        assert_eq!(
            side.raw.as_deref(),
            Some(b"one\r\ntwo\n".as_slice()),
            "the bytes on disk"
        );
        assert_eq!(
            side.content.data.as_deref(),
            Some(b"one\ntwo\n".as_slice()),
            "the clean conversion"
        );
        let unchanged = reader
            .read_with_raw(b"lf.txt".as_bstr(), u64::MAX, None)
            .expect("readable")
            .expect("present");
        assert_eq!(
            unchanged.raw, unchanged.content.data,
            "a file no filter changes has equal forms"
        );
    }

    #[test]
    fn prepare_text_decides_binary_and_algorithm() {
        let repo = fixture::repo();
        repo.write("text.txt", b"a\n");
        repo.write("bin.dat", b"a\0b\n");
        repo.git(&["add", "."]);
        repo.git(&["config", "diff.algorithm", "histogram"]);
        let local = repo.open();
        let index = local.index().expect("index exists");
        let side = |path: &'static str| {
            let entry = index_entry(&index, path.as_bytes().as_bstr(), Stage::Unconflicted)
                .expect("valid")
                .expect("staged");
            Resource {
                id: entry.id,
                kind: entry.kind,
                path: path.as_bytes().as_bstr(),
                worktree: false,
            }
        };
        let text = prepare_text(&local, None, Some(side("text.txt"))).expect("prepared");
        assert!(!text.binary, "plain text");
        assert_eq!(
            text.algorithm,
            gix::diff::blob::Algorithm::Histogram,
            "diff.algorithm is honoured"
        );
        assert!(
            text.old_textconv.is_none() && text.new_textconv.is_none(),
            "no driver"
        );
        let binary = prepare_text(&local, Some(side("bin.dat")), None).expect("prepared");
        assert!(binary.binary, "a NUL byte makes it binary");
    }

    #[test]
    fn entry_lookups_by_byte_path() {
        let repo = fixture::repo();
        repo.write("dir/a.txt", b"a\n");
        repo.write("top.txt", b"top\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        repo.write("new.txt", b"new\n");
        repo.git(&["add", "-N", "new.txt"]);
        let local = repo.open();

        let head = head_entry(&local, b"dir/a.txt".as_bstr())
            .expect("readable")
            .expect("tracked");
        assert_eq!(
            head.id,
            oid(&repo.git(&["rev-parse", "HEAD:dir/a.txt"])),
            "nested tree lookup"
        );
        assert_eq!(head.kind, EntryKind::Blob, "a regular file");
        assert!(
            head_entry(&local, b"dir/zzz".as_bstr())
                .expect("readable")
                .is_none(),
            "absent path"
        );
        assert!(
            head_entry(&local, b"new.txt".as_bstr())
                .expect("readable")
                .is_none(),
            "not committed"
        );

        let index = local.index().expect("index exists");
        let ita = index_entry(&index, b"new.txt".as_bstr(), Stage::Unconflicted)
            .expect("valid mode")
            .expect("intent-to-add entry");
        assert!(ita.intent_to_add, "git add -N sets the flag");
        assert_eq!(
            ita.id,
            ObjectId::empty_blob(local.object_hash()),
            "its id is the empty blob"
        );
        assert!(!is_conflicted(&index, b"top.txt".as_bstr()), "stage 0 only");

        let blob = read_blob(&local, head, u64::MAX).expect("readable");
        assert_eq!(blob.data.as_deref(), Some(b"a\n".as_slice()), "blob bytes");
        let too_large = read_blob(&local, head, 1).expect("readable");
        assert!(
            too_large.is_too_large() && too_large.side.size == 2,
            "size from the header, data skipped"
        );

        let unborn = fixture::repo();
        assert!(
            head_entry(&unborn.open(), b"x".as_bstr())
                .expect("unborn is fine")
                .is_none(),
            "an unborn HEAD has no entries"
        );
    }

    #[test]
    fn conflicted_path_detected() {
        let repo = fixture::repo();
        repo.write("f.txt", b"base\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "base"]);
        repo.git(&["checkout", "-q", "-b", "other"]);
        repo.write("f.txt", b"theirs\n");
        repo.git(&["commit", "-q", "-am", "theirs"]);
        repo.git(&["checkout", "-q", "main"]);
        repo.write("f.txt", b"ours\n");
        repo.git(&["commit", "-q", "-am", "ours"]);
        repo.git_fail(&["merge", "-q", "other"]);
        let local = repo.open();
        let index = local.index().expect("index exists");
        assert!(
            is_conflicted(&index, b"f.txt".as_bstr()),
            "stages 1-3 exist"
        );
        let ours = index_entry(&index, b"f.txt".as_bstr(), Stage::Ours)
            .expect("valid")
            .expect("stage 2");
        let ours_blob = read_blob(
            &local,
            Side {
                id: ours.id,
                kind: ours.kind,
            },
            u64::MAX,
        )
        .expect("readable");
        assert_eq!(
            ours_blob.data.as_deref(),
            Some(b"ours\n".as_slice()),
            "stage 2 is ours"
        );
    }
}
