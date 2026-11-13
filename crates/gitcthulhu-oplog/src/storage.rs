//! The commit chain under `refs/gitcthulhu/oplog/*` (spec 17 §2.1-§2.3).

use std::collections::BTreeSet;

use anyhow::Context as _;
use bstr::{BString, ByteSlice as _};
use gix::{
    object::tree::EntryKind,
    refs::{
        FullNameRef, Target,
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
    },
};

use crate::{
    OplogError,
    meta::EntryMeta,
    state::{CapturedState, HeadState, RefValue, peel_to_commit},
};

/// Name and email of the entry commits' author and committer (never signed, never needs `user.name`).
pub(crate) const SIGNATURE_NAME: &str = "GitCthulhu";
/// See [`SIGNATURE_NAME`].
pub(crate) const SIGNATURE_EMAIL: &str = "gitcthulhu@localhost";
/// The trailer line of every entry commit message: `GitCthulhu-Oplog: <seq> <kind>`.
const TRAILER: &str = "GitCthulhu-Oplog: ";

/// Names in the tree of an entry commit.
mod names {
    pub const META: &str = "meta.json";
    pub const BEFORE: &str = "before";
    pub const AFTER: &str = "after";
    pub const KEEP_TAGS: &str = "keep-tags";
}

/// One parsed entry of the chain.
#[derive(Clone, Debug)]
pub(crate) struct StoredEntry {
    /// The entry commit.
    pub commit: gix::ObjectId,
    pub meta: EntryMeta,
    /// The `before/` snapshot tree (absent for markers and barriers).
    pub before: Option<gix::ObjectId>,
    /// The `after/` snapshot tree (absent for markers, barriers and failed after-captures).
    pub after: Option<gix::ObjectId>,
}

/// The parsed chain of one oplog ref.
#[derive(Clone, Debug, Default)]
pub(crate) struct Chain {
    /// The commit the ref pointed to, `None` if it doesn't exist.
    pub head: Option<gix::ObjectId>,
    /// Oldest → newest.
    pub entries: Vec<StoredEntry>,
}

impl Chain {
    /// The entry with `seq`.
    pub(crate) fn entry(&self, seq: u64) -> Option<&StoredEntry> {
        self.entries
            .binary_search_by_key(&seq, |e| e.meta.seq)
            .ok()
            .map(|idx| &self.entries[idx])
    }
}

/// The current value of the oplog ref.
pub(crate) fn read_head(
    repo: &gix::Repository,
    ref_name: &FullNameRef,
) -> Result<Option<gix::ObjectId>, OplogError> {
    let Some(reference) = repo
        .try_find_reference(ref_name.as_bstr())
        .map_err(anyhow::Error::from)?
    else {
        return Ok(None);
    };
    match reference.inner.target {
        Target::Object(id) => Ok(Some(id)),
        Target::Symbolic(target) => Err(OplogError::Other(anyhow::anyhow!(
            "oplog ref {ref_name} is symbolic (points to {target}); it must point to a commit"
        ))),
    }
}

/// Load the chain ending in `head`. If the walk reaches `known.head`, the entries of `known` are reused instead of
/// being parsed again (the common case after an append).
pub(crate) fn load_chain_at(
    repo: &gix::Repository,
    head: gix::ObjectId,
    known: Option<&Chain>,
) -> Result<Chain, OplogError> {
    let mut newest_first = Vec::new();
    let mut cursor = Some(head);
    let mut base: &[StoredEntry] = &[];
    while let Some(id) = cursor {
        if let Some(known) = known
            && known.head == Some(id)
        {
            base = &known.entries;
            break;
        }
        let (entry, previous) = read_entry(repo, id)?;
        newest_first.push(entry);
        cursor = previous;
    }
    let mut entries = Vec::with_capacity(base.len() + newest_first.len());
    entries.extend_from_slice(base);
    entries.extend(newest_first.into_iter().rev());
    for pair in entries.windows(2) {
        if pair[1].meta.seq != pair[0].meta.seq + 1 {
            return Err(OplogError::Corrupt {
                commit: pair[1].commit,
                reason: format!(
                    "sequence number {} does not follow {}",
                    pair[1].meta.seq, pair[0].meta.seq
                ),
            });
        }
    }
    Ok(Chain {
        head: Some(head),
        entries,
    })
}

/// Parse the entry commit `id`; also return the previous entry commit, if any.
fn read_entry(
    repo: &gix::Repository,
    id: gix::ObjectId,
) -> Result<(StoredEntry, Option<gix::ObjectId>), OplogError> {
    let corrupt = |reason: String| OplogError::Corrupt { commit: id, reason };
    let commit = repo
        .find_commit(id)
        .map_err(|err| corrupt(format!("cannot read the commit: {err}")))?;
    let decoded = commit
        .decode()
        .map_err(|err| corrupt(format!("cannot decode the commit: {err}")))?;
    let tree_id = decoded.tree();
    let first_parent = decoded.parents().next();
    let tree = repo
        .find_tree(tree_id)
        .map_err(|err| corrupt(format!("cannot read its tree: {err}")))?;
    let meta_id = tree
        .find_entry(names::META)
        .ok_or_else(|| corrupt(format!("its tree has no {}", names::META)))?
        .object_id();
    let blob = repo
        .find_blob(meta_id)
        .map_err(|err| corrupt(format!("cannot read {}: {err}", names::META)))?;
    let meta = EntryMeta::from_json(&blob.data)
        .map_err(|err| corrupt(format!("unreadable {}: {err}", names::META)))?;
    let before = tree.find_entry(names::BEFORE).map(|e| e.object_id());
    let after = tree.find_entry(names::AFTER).map(|e| e.object_id());
    let previous = match first_parent {
        Some(parent) if is_previous_entry(repo, parent, meta.seq) => Some(parent),
        _ => None,
    };
    Ok((
        StoredEntry {
            commit: id,
            meta,
            before,
            after,
        },
        previous,
    ))
}

/// `true` if `parent` is the oplog entry with sequence number `seq - 1` (and not a kept commit of the first entry).
fn is_previous_entry(repo: &gix::Repository, parent: gix::ObjectId, seq: u64) -> bool {
    let Some(expected) = seq.checked_sub(1) else {
        return false;
    };
    let Ok(commit) = repo.find_commit(parent) else {
        return false;
    };
    let Ok(decoded) = commit.decode() else {
        return false;
    };
    if !decoded
        .author()
        .is_ok_and(|author| author.email == SIGNATURE_EMAIL)
    {
        return false;
    }
    decoded.message.lines().any(|line| {
        line.strip_prefix(TRAILER.as_bytes())
            .and_then(|rest| rest.split_str(" ").next())
            .and_then(|n| n.to_str().ok()?.parse::<u64>().ok())
            == Some(expected)
    })
}

/// The commits and annotated tag objects an entry keeps alive (§2.3).
#[derive(Debug, Default)]
pub(crate) struct KeptObjects {
    /// Sorted, deduplicated commits: the extra parents of the entry commit.
    pub commits: Vec<gix::ObjectId>,
    /// Annotated tag objects and their raw bytes, stored as `keep-tags/<hex>` blobs.
    pub tags: Vec<(gix::ObjectId, Vec<u8>)>,
}

/// The objects that must stay reachable for undo and redo between `before` and `after` (§2.3): both old and new
/// targets of every ref that differs (incl. a detached `HEAD`), peeled to commits, plus the stash commits that
/// appear in only one of the two stash logs. With `after == None` (unknown after-state), every ref target of
/// `before` is kept.
pub(crate) fn kept_objects(
    repo: &gix::Repository,
    before: &CapturedState,
    after: Option<&CapturedState>,
) -> KeptObjects {
    let mut targets = BTreeSet::new();
    let detached = |head: &HeadState| match head {
        HeadState::Detached(id) => Some(*id),
        HeadState::Symbolic(_) | HeadState::Unborn(_) => None,
    };
    match after {
        Some(after) => {
            let names: BTreeSet<_> = before.refs.keys().chain(after.refs.keys()).collect();
            for name in names {
                let (b, a) = (before.refs.get(name), after.refs.get(name));
                if b != a {
                    for value in [b, a].into_iter().flatten() {
                        if let RefValue::Direct(id) = value {
                            targets.insert(*id);
                        }
                    }
                }
            }
            if before.head != after.head {
                targets.extend(detached(&before.head));
                targets.extend(detached(&after.head));
            }
            let lines = |s: &CapturedState| stash_log_commits(s.stash_log.as_ref());
            let (b, a) = (lines(before), lines(after));
            targets.extend(b.symmetric_difference(&a).copied());
        }
        None => {
            targets.extend(before.refs.values().filter_map(|v| match v {
                RefValue::Direct(id) => Some(*id),
                RefValue::Symbolic(_) => None,
            }));
            targets.extend(detached(&before.head));
            targets.extend(stash_log_commits(before.stash_log.as_ref()));
        }
    }

    let mut commits = BTreeSet::new();
    let mut tags = Vec::new();
    for id in targets {
        let Ok(object) = repo.find_object(id) else {
            continue;
        };
        match object.kind {
            gix::object::Kind::Commit => {
                commits.insert(id);
            }
            gix::object::Kind::Tag => {
                // Keep every tag object of a tag-of-tag chain, then the commit it peels to (if any).
                let mut current = object;
                while current.kind == gix::object::Kind::Tag {
                    let target = current.to_tag_ref_iter().target_id().ok();
                    tags.push((current.id, current.data.clone()));
                    let Some(target) = target else { break };
                    let Ok(next) = repo.find_object(target) else {
                        break;
                    };
                    current = next;
                }
                if let Some(commit) = peel_to_commit(repo, id) {
                    commits.insert(commit);
                }
            }
            gix::object::Kind::Tree | gix::object::Kind::Blob => {}
        }
    }
    tags.sort_by_key(|(id, _)| *id);
    tags.dedup_by_key(|(id, _)| *id);
    KeptObjects {
        commits: commits.into_iter().collect(),
        tags,
    }
}

/// The `new_oid` of every line of a stash reflog.
fn stash_log_commits(log: Option<&BString>) -> BTreeSet<gix::ObjectId> {
    let Some(log) = log else {
        return BTreeSet::new();
    };
    log.lines()
        .filter_map(|line| {
            let mut fields = line.splitn_str(3, " ");
            fields.next()?;
            gix::ObjectId::from_hex(fields.next()?).ok()
        })
        .collect()
}

/// The fixed author/committer of entry commits, at the current time.
pub(crate) fn signature() -> gix::actor::Signature {
    gix::actor::Signature {
        name: SIGNATURE_NAME.into(),
        email: SIGNATURE_EMAIL.into(),
        time: gix::date::Time {
            seconds: now_ms() / 1000,
            offset: 0,
        },
    }
}

/// Milliseconds since the unix epoch.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Append an entry to the chain of `ref_name`, which must currently point to `parent` (`None`: must not exist).
///
/// Writes the snapshot trees of `before`/`after`, computes the kept objects (§2.3) and moves the ref with a guard.
/// Returns `ConcurrentModification` if the ref moved; the caller re-reads the chain and retries.
pub(crate) fn append(
    repo: &gix::Repository,
    ref_name: &FullNameRef,
    parent: Option<gix::ObjectId>,
    meta: &EntryMeta,
    before: Option<&CapturedState>,
    after: Option<&CapturedState>,
) -> Result<gix::ObjectId, OplogError> {
    let commit = write_entry_commit(repo, parent, meta, before, after)?;
    update_ref(repo, ref_name, parent, commit)?;
    Ok(commit)
}

fn write_entry_commit(
    repo: &gix::Repository,
    parent: Option<gix::ObjectId>,
    meta: &EntryMeta,
    before: Option<&CapturedState>,
    after: Option<&CapturedState>,
) -> anyhow::Result<gix::ObjectId> {
    let mut editor = repo.empty_tree().edit()?.detach();
    let meta_json = serde_json::to_vec_pretty(meta)?;
    editor.upsert(
        [names::META],
        EntryKind::Blob,
        repo.write_blob(meta_json)?.detach(),
    )?;
    if let Some(before) = before {
        editor.upsert([names::BEFORE], EntryKind::Tree, before.write_tree(repo)?)?;
    }
    if let Some(after) = after {
        editor.upsert([names::AFTER], EntryKind::Tree, after.write_tree(repo)?)?;
    }
    let kept = match before {
        Some(before) => {
            // Without an after-state, compare with the refs as they are now (cheap, no status walk).
            let current;
            let after = match after {
                Some(after) => Some(after),
                None => {
                    current = crate::capture::capture(
                        repo,
                        crate::types::Scope::RefsOnly,
                        &crate::types::UntrackedCapture::None,
                        &crate::capture::CaptureLimits {
                            untracked_file_limit: 0,
                            untracked_total_limit: 0,
                            capture_untracked: false,
                        },
                    )
                    .ok();
                    current.as_ref()
                }
            };
            kept_objects(repo, before, after)
        }
        None => KeptObjects::default(),
    };
    for (tag, data) in &kept.tags {
        let blob = repo.write_blob(data)?.detach();
        editor.upsert([names::KEEP_TAGS, &tag.to_string()], EntryKind::Blob, blob)?;
    }
    let tree = editor.write(|tree| repo.write_object(tree).map(gix::Id::detach))?;

    let mut parents: Vec<gix::ObjectId> = parent.into_iter().collect();
    parents.extend(kept.commits);
    let signature = signature();
    let title = meta.title.replace(['\r', '\n'], " ");
    let message = format!("{title}\n\n{TRAILER}{} {}\n", meta.seq, meta.kind.as_str());
    let commit = gix::objs::Commit {
        tree,
        parents: parents.into(),
        author: signature.clone(),
        committer: signature,
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    Ok(repo.write_object(&commit)?.detach())
}

/// Rewrite the entry commit `old` onto `new_parent` (pruning, §2.5): same tree, message, author and committer;
/// parents `[new_parent] ++ extra parents of old`. `old_has_previous` tells whether `old`'s first parent is the
/// previous entry (then it is dropped) or a kept commit (the oldest entry of a chain).
pub(crate) fn rewrite_entry_commit(
    repo: &gix::Repository,
    old: gix::ObjectId,
    old_has_previous: bool,
    new_parent: Option<gix::ObjectId>,
) -> anyhow::Result<gix::ObjectId> {
    let commit = repo.find_commit(old)?;
    let mut decoded: gix::objs::Commit = commit.decode()?.try_into()?;
    let extras = if old_has_previous {
        decoded.parents.iter().skip(1).copied().collect::<Vec<_>>()
    } else {
        decoded.parents.to_vec()
    };
    let mut parents: Vec<gix::ObjectId> = new_parent.into_iter().collect();
    parents.extend(extras);
    decoded.parents = parents.into();
    Ok(repo.write_object(&decoded)?.detach())
}

/// Move `ref_name` from `expected` (`None`: must not exist) to `new` without writing a reflog (a reflog would keep
/// pruned entries alive). A guard failure yields `ConcurrentModification`, a held lock `RepositoryLocked`.
pub(crate) fn update_ref(
    repo: &gix::Repository,
    ref_name: &FullNameRef,
    expected: Option<gix::ObjectId>,
    new: gix::ObjectId,
) -> Result<(), OplogError> {
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "gitcthulhu: oplog".into(),
            },
            expected: match expected {
                Some(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
                None => PreviousValue::MustNotExist,
            },
            new: Target::Object(new),
        },
        name: ref_name.to_owned(),
        deref: false,
    };
    edit_without_reflog(repo, ref_name, expected, edit)
}

/// Delete `ref_name` if it exists (any value).
pub(crate) fn delete_ref(repo: &gix::Repository, ref_name: &FullNameRef) -> Result<(), OplogError> {
    let Some(current) = read_head(repo, ref_name)? else {
        return Ok(());
    };
    let edit = RefEdit {
        change: Change::Delete {
            expected: PreviousValue::MustExistAndMatch(Target::Object(current)),
            log: RefLog::AndReference,
        },
        name: ref_name.to_owned(),
        deref: false,
    };
    edit_without_reflog(repo, ref_name, Some(current), edit)
}

fn edit_without_reflog(
    repo: &gix::Repository,
    ref_name: &FullNameRef,
    expected: Option<gix::ObjectId>,
    edit: RefEdit,
) -> Result<(), OplogError> {
    let mut store = repo.refs.clone();
    store.write_reflog = gix::refs::store::WriteReflog::Disable;
    let lock_mode =
        gix::lock::acquire::Fail::AfterDurationWithBackoff(std::time::Duration::from_secs(1));
    let result = store
        .transaction()
        .prepare([edit], lock_mode, lock_mode)
        .and_then(|t| t.commit(None));
    match result {
        Ok(_) => Ok(()),
        Err(err) => {
            if read_head(repo, ref_name)? != expected {
                return Err(OplogError::ConcurrentModification);
            }
            if let Ok(rela) = gix::path::from_bstr(ref_name.as_bstr()) {
                let mut lock = repo.common_dir().join(rela).into_os_string();
                lock.push(".lock");
                let lock = std::path::PathBuf::from(lock);
                if lock.is_file() {
                    return Err(OplogError::RepositoryLocked { path: lock });
                }
            }
            Err(OplogError::Other(
                anyhow::Error::from(err).context(format!("updating {ref_name}")),
            ))
        }
    }
}

/// Delete the restore journal directory content of a worktree: the journal and the index backup.
pub(crate) fn remove_journal_files(journal_dir: &std::path::Path) -> Result<(), OplogError> {
    for name in [crate::JOURNAL_FILE, crate::INDEX_BACKUP_FILE] {
        let path = journal_dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(OplogError::Other(
                    anyhow::Error::from(err).context(format!("removing '{}'", path.display())),
                ));
            }
        }
    }
    Ok(())
}

/// Read the raw bytes of a `keep-tags/<hex>` blob of entry commit `commit`, if present.
pub(crate) fn kept_tag(
    repo: &gix::Repository,
    commit: gix::ObjectId,
    tag: gix::ObjectId,
) -> anyhow::Result<Option<Vec<u8>>> {
    let tree = repo.find_commit(commit)?.tree()?;
    let path = format!("{}/{}", names::KEEP_TAGS, tag);
    let Some(entry) = tree
        .lookup_entry_by_path(&path)
        .with_context(|| format!("looking up {path}"))?
    else {
        return Ok(None);
    };
    Ok(Some(repo.find_blob(entry.object_id())?.data.clone()))
}
