//! The captured repository state and its snapshot-tree format (spec 17 §1.1, §2.2.1).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, bail};
use bstr::{BString, ByteSlice as _};
use gix::{object::tree::EntryKind, refs::FullName};

use crate::{
    meta::{FORMAT_VERSION, bytes::Bytes},
    types::{InProgress, RefDivergence, RefValueInfo, Scope, SkippedFile},
};

/// Bit of [`FileState::index_flags`]: the entry was added with `git add -N` (`INTENT_TO_ADD`).
pub(crate) const FLAG_INTENT_TO_ADD: u16 = 1 << 0;
/// Bit of [`FileState::index_flags`]: `git update-index --skip-worktree` (`SKIP_WORKTREE`).
pub(crate) const FLAG_SKIP_WORKTREE: u16 = 1 << 1;
/// Bit of [`FileState::index_flags`]: `git update-index --assume-unchanged` (`ASSUME_VALID`).
pub(crate) const FLAG_ASSUME_UNCHANGED: u16 = 1 << 2;

/// Map the index entry `flags` to the [`FileState::index_flags`] bits (0 if none of the captured flags is set).
pub(crate) fn index_flag_bits(flags: gix::index::entry::Flags) -> u16 {
    use gix::index::entry::Flags;
    let mut bits = 0;
    if flags.contains(Flags::INTENT_TO_ADD) {
        bits |= FLAG_INTENT_TO_ADD;
    }
    if flags.contains(Flags::SKIP_WORKTREE) {
        bits |= FLAG_SKIP_WORKTREE;
    }
    if flags.contains(Flags::ASSUME_VALID) {
        bits |= FLAG_ASSUME_UNCHANGED;
    }
    bits
}

/// Map [`FileState::index_flags`] bits back to index entry flags.
pub(crate) fn index_flags_from_bits(bits: u16) -> gix::index::entry::Flags {
    use gix::index::entry::Flags;
    let mut flags = Flags::empty();
    if bits & FLAG_INTENT_TO_ADD != 0 {
        flags |= Flags::INTENT_TO_ADD;
    }
    if bits & FLAG_SKIP_WORKTREE != 0 {
        flags |= Flags::SKIP_WORKTREE;
    }
    if bits & FLAG_ASSUME_UNCHANGED != 0 {
        flags |= Flags::ASSUME_VALID;
    }
    flags
}

/// What `HEAD` was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HeadState {
    /// `HEAD` points to an existing ref.
    Symbolic(FullName),
    /// `HEAD` points to an object.
    Detached(gix::ObjectId),
    /// `HEAD` points to a ref that does not exist.
    Unborn(FullName),
}

impl HeadState {
    /// The target to write into `HEAD` (`Unborn(n)` => `Symbolic(n)`).
    pub(crate) fn to_target(&self) -> gix::refs::Target {
        match self {
            HeadState::Symbolic(name) | HeadState::Unborn(name) => {
                gix::refs::Target::Symbolic(name.clone())
            }
            HeadState::Detached(id) => gix::refs::Target::Object(*id),
        }
    }

    /// The value for display.
    pub(crate) fn info(&self) -> RefValueInfo {
        match self {
            HeadState::Symbolic(name) => RefValueInfo::Symbolic(name.clone()),
            HeadState::Detached(id) => RefValueInfo::Object(*id),
            HeadState::Unborn(name) => RefValueInfo::Unborn(name.clone()),
        }
    }
}

/// The value of a captured ref.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RefValue {
    /// Points to an object (a tag object for annotated tags).
    Direct(gix::ObjectId),
    /// Points to another ref (rare under `refs/heads` and `refs/tags`).
    Symbolic(FullName),
}

impl RefValue {
    /// The target to write into the ref.
    pub(crate) fn to_target(&self) -> gix::refs::Target {
        match self {
            RefValue::Direct(id) => gix::refs::Target::Object(*id),
            RefValue::Symbolic(name) => gix::refs::Target::Symbolic(name.clone()),
        }
    }

    /// The value for display.
    pub(crate) fn info(&self) -> RefValueInfo {
        match self {
            RefValue::Direct(id) => RefValueInfo::Object(*id),
            RefValue::Symbolic(name) => RefValueInfo::Symbolic(name.clone()),
        }
    }
}

/// The index and worktree part of a `Scope::Full` snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileState {
    /// `HEAD^{tree}`, the empty tree when `HEAD` is unborn.
    pub head_tree: gix::ObjectId,
    /// The effective worktree tree (= `head_tree` when clean).
    pub worktree: gix::ObjectId,
    /// The effective stage-0 index tree (= `head_tree` when there is no index change).
    pub index: gix::ObjectId,
    /// But-core format `<path>/<stage>`; `None` without conflicts.
    pub index_conflicts: Option<gix::ObjectId>,
    /// Stage-0 entries with at least one of the [`FLAG_INTENT_TO_ADD`], [`FLAG_SKIP_WORKTREE`],
    /// [`FLAG_ASSUME_UNCHANGED`] bits.
    pub index_flags: BTreeMap<BString, u16>,
}

impl FileState {
    /// head_tree = worktree = index = `tree`, no conflicts, no flags (the carry base after an external HEAD move, §3.3 B2).
    pub(crate) fn clean(tree: gix::ObjectId) -> FileState {
        FileState {
            head_tree: tree,
            worktree: tree,
            index: tree,
            index_conflicts: None,
            index_flags: BTreeMap::new(),
        }
    }
}

/// One snapshot of the repository state (spec 17 §1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedState {
    pub scope: Scope,
    pub head: HeadState,
    /// Every `refs/heads/*`, `refs/tags/*` and `refs/stash`.
    pub refs: BTreeMap<FullName, RefValue>,
    /// Raw bytes of `<common-dir>/logs/refs/stash`.
    pub stash_log: Option<BString>,
    /// `branch.<name>.*` of the captured local branches: short name -> `(key, value)` in file order.
    pub branch_config: BTreeMap<BString, Vec<(BString, BString)>>,
    pub in_progress: Option<InProgress>,
    /// `Some` for `Scope::Full`.
    pub files: Option<FileState>,
    pub skipped_untracked: Vec<SkippedFile>,
    pub skipped_untracked_truncated: bool,
}

/// Names of the entries of a snapshot tree.
mod names {
    pub const STATE: &str = "state.json";
    pub const REFS: &str = "refs";
    pub const STASH_LOG: &str = "stash-log";
    pub const FILES: &str = "files";
    pub const HEAD_TREE: &str = "head-tree";
    pub const WORKTREE: &str = "worktree";
    pub const INDEX: &str = "index";
    pub const INDEX_CONFLICTS: &str = "index-conflicts";
}

/// `state.json` of a snapshot tree. Byte strings use [`Bytes`] so paths and names stay byte-exact; maps whose
/// keys are byte strings are stored as lists of pairs, because JSON object keys must be valid UTF-8.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotStateV1 {
    version: u32,
    scope: Scope,
    head: HeadJson,
    #[serde(default)]
    symbolic_refs: Vec<(Bytes, Bytes)>,
    in_progress: Option<InProgress>,
    #[serde(default)]
    branch_config: Vec<(Bytes, Vec<(Bytes, Bytes)>)>,
    #[serde(default)]
    index_flags: Vec<(Bytes, u16)>,
    #[serde(default)]
    skipped_untracked: Vec<SkippedFile>,
    #[serde(default)]
    skipped_untracked_truncated: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum HeadJson {
    Symbolic(Bytes),
    Detached(String),
    Unborn(Bytes),
}

fn full_name(name: BString) -> anyhow::Result<FullName> {
    FullName::try_from(name.clone()).with_context(|| format!("invalid ref name '{name}'"))
}

impl CapturedState {
    /// Write this state as a snapshot tree (§2.2.1) and return its id.
    ///
    /// Uses a detached tree editor: the gitlinks under `refs/` may point to objects of any kind, and ref-name
    /// components need no path validation.
    pub(crate) fn write_tree(&self, repo: &gix::Repository) -> anyhow::Result<gix::ObjectId> {
        let mut symbolic_refs = Vec::new();
        let mut editor = repo.empty_tree().edit()?.detach();
        for (name, value) in &self.refs {
            match value {
                RefValue::Direct(id) => {
                    if !name.as_bstr().starts_with(b"refs/") {
                        bail!("captured ref '{name}' is not below refs/");
                    }
                    // `refs/heads/main` is stored as `refs/` + `heads/main`.
                    editor.upsert(name.as_bstr().split_str("/"), EntryKind::Commit, *id)?;
                }
                RefValue::Symbolic(target) => symbolic_refs.push((
                    Bytes(name.as_bstr().to_owned()),
                    Bytes(target.as_bstr().to_owned()),
                )),
            }
        }
        if let Some(log) = &self.stash_log {
            let id = repo.write_blob(log.as_slice())?.detach();
            editor.upsert([names::STASH_LOG], EntryKind::Blob, id)?;
        }
        let mut index_flags = Vec::new();
        if let Some(files) = &self.files {
            if files.head_tree.is_empty_tree() {
                // Git knows the empty tree implicitly, but fsck and other tools expect referenced trees to exist.
                repo.write_object(gix::objs::Tree::empty())?;
            }
            for (name, id) in [
                (names::HEAD_TREE, files.head_tree),
                (names::WORKTREE, files.worktree),
                (names::INDEX, files.index),
            ] {
                editor.upsert([names::FILES, name], EntryKind::Tree, id)?;
            }
            if let Some(id) = files.index_conflicts {
                editor.upsert([names::FILES, names::INDEX_CONFLICTS], EntryKind::Tree, id)?;
            }
            index_flags = files
                .index_flags
                .iter()
                .map(|(path, bits)| (Bytes(path.clone()), *bits))
                .collect();
        }
        let state = SnapshotStateV1 {
            version: FORMAT_VERSION,
            scope: self.scope,
            head: match &self.head {
                HeadState::Symbolic(name) => HeadJson::Symbolic(Bytes(name.as_bstr().to_owned())),
                HeadState::Detached(id) => HeadJson::Detached(id.to_string()),
                HeadState::Unborn(name) => HeadJson::Unborn(Bytes(name.as_bstr().to_owned())),
            },
            symbolic_refs,
            in_progress: self.in_progress,
            branch_config: self
                .branch_config
                .iter()
                .map(|(branch, values)| {
                    (
                        Bytes(branch.clone()),
                        values
                            .iter()
                            .map(|(k, v)| (Bytes(k.clone()), Bytes(v.clone())))
                            .collect(),
                    )
                })
                .collect(),
            index_flags,
            skipped_untracked: self.skipped_untracked.clone(),
            skipped_untracked_truncated: self.skipped_untracked_truncated,
        };
        let json = serde_json::to_vec_pretty(&state)?;
        let id = repo.write_blob(json)?.detach();
        editor.upsert([names::STATE], EntryKind::Blob, id)?;
        let tree = editor.write(|tree| repo.write_object(tree).map(gix::Id::detach))?;
        Ok(tree)
    }

    /// Read a snapshot tree written by [`Self::write_tree()`].
    pub(crate) fn read_tree(
        repo: &gix::Repository,
        tree: gix::ObjectId,
    ) -> anyhow::Result<CapturedState> {
        let tree = repo
            .find_tree(tree)
            .with_context(|| format!("snapshot tree {tree} is missing"))?;
        let state_id = tree
            .find_entry(names::STATE)
            .with_context(|| format!("snapshot tree {} has no {}", tree.id, names::STATE))?
            .object_id();
        let state_blob = repo.find_blob(state_id)?;
        let state: SnapshotStateV1 =
            serde_json::from_slice(&state_blob.data).with_context(|| {
                format!(
                    "snapshot tree {} has an unreadable {}",
                    tree.id,
                    names::STATE
                )
            })?;
        if state.version > FORMAT_VERSION {
            bail!(
                "snapshot tree {} has format version {}, this build reads up to {FORMAT_VERSION}",
                tree.id,
                state.version
            );
        }

        let mut refs = BTreeMap::new();
        if let Some(entry) = tree.find_entry(names::REFS) {
            let refs_tree = repo.find_tree(entry.object_id())?;
            let mut recorder = gix::traverse::tree::Recorder::default();
            refs_tree.traverse().breadthfirst(&mut recorder)?;
            for record in recorder.records {
                if record.mode.is_commit() {
                    let mut name = BString::from("refs/");
                    name.extend_from_slice(&record.filepath);
                    refs.insert(full_name(name)?, RefValue::Direct(record.oid));
                }
            }
        }
        for (name, target) in state.symbolic_refs {
            refs.insert(full_name(name.0)?, RefValue::Symbolic(full_name(target.0)?));
        }

        let stash_log = match tree.find_entry(names::STASH_LOG) {
            Some(entry) => Some(BString::from(
                repo.find_blob(entry.object_id())?.data.clone(),
            )),
            None => None,
        };

        let files = match state.scope {
            Scope::RefsOnly => None,
            Scope::Full => {
                let files_tree = repo.find_tree(
                    tree.find_entry(names::FILES)
                        .with_context(|| {
                            format!("full snapshot tree {} has no {}", tree.id, names::FILES)
                        })?
                        .object_id(),
                )?;
                let get = |name: &str| -> Option<gix::ObjectId> {
                    files_tree.find_entry(name).map(|e| e.object_id())
                };
                let missing = |name: &str| {
                    format!("snapshot tree {} has no {}/{name}", tree.id, names::FILES)
                };
                Some(FileState {
                    head_tree: get(names::HEAD_TREE).with_context(|| missing(names::HEAD_TREE))?,
                    worktree: get(names::WORKTREE).with_context(|| missing(names::WORKTREE))?,
                    index: get(names::INDEX).with_context(|| missing(names::INDEX))?,
                    index_conflicts: get(names::INDEX_CONFLICTS),
                    index_flags: state
                        .index_flags
                        .into_iter()
                        .map(|(path, bits)| (path.0, bits))
                        .collect(),
                })
            }
        };

        let head = match state.head {
            HeadJson::Symbolic(name) => HeadState::Symbolic(full_name(name.0)?),
            HeadJson::Unborn(name) => HeadState::Unborn(full_name(name.0)?),
            HeadJson::Detached(hex) => HeadState::Detached(
                gix::ObjectId::from_hex(hex.as_bytes())
                    .with_context(|| format!("invalid detached HEAD id '{hex}'"))?,
            ),
        };

        Ok(CapturedState {
            scope: state.scope,
            head,
            refs,
            stash_log,
            branch_config: state
                .branch_config
                .into_iter()
                .map(|(branch, values)| {
                    (
                        branch.0,
                        values.into_iter().map(|(k, v)| (k.0, v.0)).collect(),
                    )
                })
                .collect(),
            in_progress: state.in_progress,
            files,
            skipped_untracked: state.skipped_untracked,
            skipped_untracked_truncated: state.skipped_untracked_truncated,
        })
    }

    /// Compare `self` (the expected state) with `current`: `HEAD`, the captured refs, and the stash log (reported
    /// as `refs/stash` when only the log differs). Sorted by name; `HEAD` is reported with the name `HEAD`.
    pub(crate) fn compare_refs(&self, current: &CapturedState) -> Vec<RefDivergence> {
        let mut out = Vec::new();
        if self.head != current.head {
            out.push(RefDivergence {
                name: head_name(),
                expected: Some(self.head.info()),
                actual: Some(current.head.info()),
            });
        }
        let names: BTreeSet<&FullName> = self.refs.keys().chain(current.refs.keys()).collect();
        let mut stash_reported = false;
        for name in names {
            let (expected, actual) = (self.refs.get(name), current.refs.get(name));
            if expected != actual {
                stash_reported |= name.as_bstr() == STASH_REF;
                out.push(RefDivergence {
                    name: name.clone(),
                    expected: expected.map(RefValue::info),
                    actual: actual.map(RefValue::info),
                });
            }
        }
        if !stash_reported && self.stash_log != current.stash_log {
            let value = |s: &CapturedState| s.refs.get(&stash_name()).map(RefValue::info);
            out.push(RefDivergence {
                name: stash_name(),
                expected: value(self),
                actual: value(current),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// `true` if both states are the same for undo purposes: refs, `HEAD`, stash log, files and in-progress state.
    /// Branch config and skipped files are not compared.
    pub(crate) fn equivalent(&self, other: &CapturedState) -> bool {
        self.head == other.head
            && self.refs == other.refs
            && self.stash_log == other.stash_log
            && self.files == other.files
            && self.in_progress == other.in_progress
    }

    /// Paths in files.worktree that are absent from files.index and files.index_conflicts (tree diff, additions only).
    /// Empty for RefsOnly.
    pub(crate) fn untracked_paths(&self, repo: &gix::Repository) -> anyhow::Result<Vec<BString>> {
        let Some(files) = &self.files else {
            return Ok(Vec::new());
        };
        if files.worktree == files.index {
            return Ok(Vec::new());
        }
        let index_tree = repo.find_tree(files.index)?;
        let worktree_tree = repo.find_tree(files.worktree)?;
        let changes = repo.diff_tree_to_tree(
            &index_tree,
            &worktree_tree,
            gix::diff::Options::default().with_rewrites(None),
        )?;
        let conflicted = match files.index_conflicts {
            Some(id) => conflict_paths(repo, id)?,
            None => BTreeSet::new(),
        };
        let mut out: Vec<BString> = changes
            .into_iter()
            .filter_map(|change| match change {
                gix::object::tree::diff::ChangeDetached::Addition {
                    location,
                    entry_mode,
                    ..
                } if !entry_mode.is_tree() && !conflicted.contains(&location) => Some(location),
                _ => None,
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// The commit `HEAD` points to, peeled through tag objects; `None` if unborn or not resolvable from the
    /// captured refs.
    pub(crate) fn head_commit(&self, repo: &gix::Repository) -> Option<gix::ObjectId> {
        let id = match &self.head {
            HeadState::Detached(id) => *id,
            HeadState::Symbolic(name) => match self.refs.get(name) {
                Some(RefValue::Direct(id)) => *id,
                Some(RefValue::Symbolic(_)) | None => return None,
            },
            HeadState::Unborn(_) => return None,
        };
        Some(peel_to_commit(repo, id).unwrap_or(id))
    }
}

/// Read only the skipped-untracked list of the snapshot tree `tree` (cheap: one small blob, no ref traversal).
pub(crate) fn read_skipped(
    repo: &gix::Repository,
    tree: gix::ObjectId,
) -> anyhow::Result<(Vec<SkippedFile>, bool)> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SkippedOnly {
        #[serde(default)]
        skipped_untracked: Vec<SkippedFile>,
        #[serde(default)]
        skipped_untracked_truncated: bool,
    }
    let tree = repo.find_tree(tree)?;
    let state_id = tree
        .find_entry(names::STATE)
        .with_context(|| format!("snapshot tree {} has no {}", tree.id, names::STATE))?
        .object_id();
    let blob = repo.find_blob(state_id)?;
    let state: SkippedOnly = serde_json::from_slice(&blob.data).with_context(|| {
        format!(
            "snapshot tree {} has an unreadable {}",
            tree.id,
            names::STATE
        )
    })?;
    Ok((state.skipped_untracked, state.skipped_untracked_truncated))
}

/// The paths of the conflicted entries in a but-core `index-conflicts` tree (`<path>/<stage>`).
fn conflict_paths(
    repo: &gix::Repository,
    conflicts: gix::ObjectId,
) -> anyhow::Result<BTreeSet<BString>> {
    let mut recorder = gix::traverse::tree::Recorder::default();
    repo.find_tree(conflicts)?
        .traverse()
        .breadthfirst(&mut recorder)?;
    Ok(recorder
        .records
        .into_iter()
        .filter(|r| !r.mode.is_tree())
        .filter_map(|r| {
            let (path, _stage) = r.filepath.rsplit_once_str("/")?;
            Some(BString::from(path))
        })
        .collect())
}

/// Peel `id` to a commit (following tag objects); `None` if it is not a commit-ish or missing.
pub(crate) fn peel_to_commit(repo: &gix::Repository, id: gix::ObjectId) -> Option<gix::ObjectId> {
    let object = repo.find_object(id).ok()?;
    let commit = object.peel_to_kind(gix::object::Kind::Commit).ok()?;
    Some(commit.id)
}

/// The full name of `refs/stash`.
pub(crate) const STASH_REF: &str = "refs/stash";

/// `refs/stash` as [`FullName`].
pub(crate) fn stash_name() -> FullName {
    FullName::try_from(STASH_REF).expect("refs/stash is a valid ref name")
}

/// `HEAD` as [`FullName`].
pub(crate) fn head_name() -> FullName {
    FullName::try_from("HEAD").expect("HEAD is a valid ref name")
}

/// Full ref names (incl. `HEAD`) whose values differ between `before` and `after`; `refs/stash` is also listed
/// when only the stash log differs. Sorted.
pub(crate) fn refs_changed(before: &CapturedState, after: &CapturedState) -> Vec<FullName> {
    before
        .compare_refs(after)
        .into_iter()
        .map(|d| d.name)
        .collect()
}
