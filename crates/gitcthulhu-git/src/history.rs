//! File history (WP-B9): the commits that touched one path, with Git's default history simplification and optional
//! rename following, as an incremental walk.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet},
    num::NonZeroUsize,
    sync::Arc,
};

use bstr::{BStr, BString, ByteSlice as _};
use gix::{ObjectId, object::tree::EntryMode};
use lru::LruCache;
use smallvec::SmallVec;

use crate::{
    CancelToken, ChangeKind, CommitInfo, FileHistoryEntry, FileHistoryPage, GitError, GitTime,
    Person, RelaPath, Repo, Result, Signature,
};

/// How many commits are processed between two looks at the cancel token.
const CANCEL_CHECK_INTERVAL: usize = 256;

/// The `limit` of the rename tracking done to follow a file (like Git's `diff.renameLimit` default).
const RENAME_LIMIT: usize = 1000;

/// How many `(commit, path)` entry lookups are remembered: every commit is looked at once as itself and once for
/// each of its children.
const ENTRY_CACHE: usize = 8192;

/// The identity of a tree entry for the purpose of "did this commit change the file".
type EntryKey = Option<(ObjectId, EntryMode)>;

/// A commit waiting to be examined, ordered by committer time (newest first), then insertion order.
struct Queued {
    time: i64,
    seq: Reverse<u64>,
    id: ObjectId,
    path: BString,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.seq == other.seq
    }
}

impl Eq for Queued {}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.time, self.seq).cmp(&(other.time, other.seq))
    }
}

/// Parents and committer time of a commit.
struct CommitMeta {
    time: i64,
    parents: SmallVec<[ObjectId; 2]>,
}

/// An incremental walk over the commits that touched one path.
///
/// Owns its own thread-local repository and is `Send`; the GUI keeps it in a `Mutex` and drives it from background
/// tasks.
pub struct FileHistoryWalker {
    repo: gix::Repository,
    graph: Option<gix::commitgraph::Graph>,
    /// The boundary commits of a shallow repository: their parents are not in the object database, so they are
    /// treated as roots (like `git log` does).
    shallow: HashSet<ObjectId>,
    follow: bool,
    queue: BinaryHeap<Queued>,
    seen: HashSet<(ObjectId, BString)>,
    next_seq: u64,
    entries: LruCache<(ObjectId, BString), EntryKey>,
    people: HashMap<(BString, BString), Arc<Person>>,
    visited: usize,
}

impl FileHistoryWalker {
    /// `start = None` => HEAD. `follow` = rename following like `git log --follow` (one path at a time).
    ///
    /// An unborn HEAD yields an empty, exhausted walk.
    ///
    /// Errors: `NotFound` if `start` is not a commit of the repository, `Gix` on repository failures.
    pub fn new(
        repo: &Repo,
        path: &RelaPath,
        start: Option<ObjectId>,
        follow: bool,
    ) -> Result<FileHistoryWalker> {
        let local = repo.local();
        let start = match start {
            Some(id) => {
                local.find_commit(id).map_err(|_| GitError::NotFound {
                    what: "commit",
                    name: id.to_string(),
                })?;
                Some(id)
            }
            None => local.head_id().ok().map(gix::Id::detach),
        };
        let graph = local
            .commit_graph_if_enabled()
            .map_err(|err| GitError::Other(anyhow::anyhow!(err)))?;
        let shallow = local
            .shallow_commits()
            .map_err(|err| GitError::Other(anyhow::anyhow!(err)))?
            .map(|commits| commits.iter().copied().collect())
            .unwrap_or_default();
        let mut walker = FileHistoryWalker {
            repo: local,
            graph,
            shallow,
            follow,
            queue: BinaryHeap::new(),
            seen: HashSet::new(),
            next_seq: 0,
            entries: LruCache::new(
                NonZeroUsize::new(ENTRY_CACHE).expect("the cache size is not zero"),
            ),
            people: HashMap::new(),
            visited: 0,
        };
        if let Some(start) = start {
            walker.enqueue(start, path.clone())?;
        }
        Ok(walker)
    }

    /// The next up to `n` entries, in committer-date order like `git log`'s default.
    ///
    /// With clock skew a parent can be listed before its child; the list has no graph, so this is harmless.
    /// Cancellation is checked every 256 commits.
    pub fn next_page(&mut self, n: usize, cancel: &CancelToken) -> Result<FileHistoryPage> {
        let mut page = Vec::new();
        cancel.check()?;
        while page.len() < n {
            let Some(next) = self.queue.pop() else {
                break;
            };
            if self.visited % CANCEL_CHECK_INTERVAL == CANCEL_CHECK_INTERVAL - 1 {
                cancel.check()?;
            }
            self.visited += 1;
            if let Some(entry) = self.examine(next.id, next.path)? {
                page.push(entry);
            }
        }
        Ok(FileHistoryPage {
            entries: page,
            exhausted: self.queue.is_empty(),
        })
    }

    /// How many commits were examined so far. Every `(commit, tracked path)` is examined at most once, which the
    /// tests check on nested merges.
    #[doc(hidden)]
    pub fn visited_commits(&self) -> usize {
        self.visited
    }

    /// Queue `(id, path)` unless it was queued before.
    fn enqueue(&mut self, id: ObjectId, path: BString) -> Result<()> {
        if !self.seen.insert((id, path.clone())) {
            return Ok(());
        }
        let time = self.meta(id)?.time;
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push(Queued {
            time,
            seq: Reverse(seq),
            id,
            path,
        });
        Ok(())
    }

    /// Committer time and parents, from the commit-graph if it has the commit. A shallow boundary commit has no
    /// parents.
    fn meta(&self, id: ObjectId) -> Result<CommitMeta> {
        let mut meta = self.meta_unfiltered(id)?;
        if self.shallow.contains(&id) {
            meta.parents.clear();
        }
        Ok(meta)
    }

    fn meta_unfiltered(&self, id: ObjectId) -> Result<CommitMeta> {
        if let Some(graph) = &self.graph
            && let Some(commit) = graph.commit_by_id(id)
        {
            let parents: Result<SmallVec<[ObjectId; 2]>> = commit
                .iter_parents()
                .map(|parent| {
                    parent
                        .map(|pos| graph.id_at(pos).to_owned())
                        .map_err(|err| GitError::Other(anyhow::anyhow!(err)))
                })
                .collect();
            return Ok(CommitMeta {
                time: commit.committer_timestamp() as i64,
                parents: parents?,
            });
        }
        let commit = self.repo.find_commit(id).map_err(GitError::Gix)?;
        let time = commit
            .committer()
            .map_err(|err| GitError::Other(anyhow::anyhow!(err)))?
            .seconds();
        Ok(CommitMeta {
            time,
            parents: commit.parent_ids().map(gix::Id::detach).collect(),
        })
    }

    /// The identity of the entry at `path` in the tree of `commit`.
    fn entry(&mut self, commit: ObjectId, path: &BStr) -> Result<EntryKey> {
        let key = (commit, path.to_owned());
        if let Some(found) = self.entries.get(&key) {
            return Ok(*found);
        }
        let tree = self
            .repo
            .find_commit(commit)
            .map_err(GitError::Gix)?
            .tree()
            .map_err(GitError::Gix)?;
        let found = tree
            .lookup_entry(path.split(|byte| *byte == b'/'))
            .map_err(|err| GitError::Other(anyhow::anyhow!(err)))?
            .map(|entry| (entry.object_id(), entry.mode()));
        self.entries.put(key, found);
        Ok(found)
    }

    /// Look at one commit for one tracked path: decide whether it belongs to the history and queue its parents.
    fn examine(&mut self, id: ObjectId, path: BString) -> Result<Option<FileHistoryEntry>> {
        let parents = self.meta(id)?.parents;
        let here = self.entry(id, path.as_ref())?;

        let mut parent_entries: SmallVec<[EntryKey; 2]> = SmallVec::new();
        for parent in &parents {
            parent_entries.push(self.entry(*parent, path.as_ref())?);
        }

        // Git's default simplification: a commit TREESAME to a parent is not part of the history, and for a merge
        // only the first such parent is followed.
        if let Some(same) = parent_entries.iter().position(|parent| *parent == here) {
            self.enqueue(parents[same], path)?;
            return Ok(None);
        }

        if parents.is_empty() && here.is_none() {
            // A root commit without the file has nothing to say about it.
            return Ok(None);
        }

        let mut previous_path = None;
        let mut enqueue_with: SmallVec<[(ObjectId, BString); 2]> = SmallVec::new();
        for (parent, parent_entry) in parents.iter().zip(&parent_entries) {
            let mut tracked = path.clone();
            if self.follow
                && here.is_some()
                && parent_entry.is_none()
                && let Some(source) = self.renamed_from(*parent, id, path.as_ref())?
            {
                if previous_path.is_none() {
                    previous_path = Some(source.clone());
                }
                tracked = source;
            }
            enqueue_with.push((*parent, tracked));
        }
        for (parent, tracked) in enqueue_with {
            self.enqueue(parent, tracked)?;
        }

        let kind = match (here, parent_entries.iter().flatten().next()) {
            (None, _) => ChangeKind::Deleted,
            (Some(_), None) if previous_path.is_some() => ChangeKind::Renamed,
            (Some(_), None) => ChangeKind::Added,
            (Some((_, mode)), Some((_, previous_mode))) => {
                if is_same_entry_type(mode, *previous_mode) {
                    ChangeKind::Modified
                } else {
                    ChangeKind::TypeChanged
                }
            }
        };
        let commit = self.commit_info(id)?;
        Ok(Some(FileHistoryEntry {
            commit,
            path,
            previous_path,
            kind,
        }))
    }

    /// If `commit` added `path` as the destination of a rename seen from `parent`, the source path.
    fn renamed_from(
        &mut self,
        parent: ObjectId,
        commit: ObjectId,
        path: &BStr,
    ) -> Result<Option<BString>> {
        let other = |err: &dyn std::fmt::Display| GitError::Other(anyhow::anyhow!("{err}"));
        let old_tree = self
            .repo
            .find_commit(parent)
            .map_err(|err| other(&err))?
            .tree()
            .map_err(|err| other(&err))?;
        let new_tree = self
            .repo
            .find_commit(commit)
            .map_err(|err| other(&err))?
            .tree()
            .map_err(|err| other(&err))?;
        let rewrites = gix::diff::Rewrites {
            limit: RENAME_LIMIT,
            ..Default::default()
        };
        let mut options = gix::diff::Options::default();
        options.track_path().track_rewrites(Some(rewrites));
        let changes = self
            .repo
            .diff_tree_to_tree(&old_tree, &new_tree, options)
            .map_err(|err| other(&err))?;
        Ok(changes.into_iter().find_map(|change| match change {
            gix::object::tree::diff::ChangeDetached::Rewrite {
                source_location,
                location,
                copy: false,
                ..
            } if location.as_bstr() == path => Some(source_location),
            _ => None,
        }))
    }

    /// The summary of a commit, with authors and committers interned per walker.
    fn commit_info(&mut self, id: ObjectId) -> Result<CommitInfo> {
        let commit = self.repo.find_commit(id).map_err(GitError::Gix)?;
        commit_info(&commit, &mut self.people)
    }
}

/// Whether an executable-bit flip is the only difference between two entry modes.
fn is_same_entry_type(a: EntryMode, b: EntryMode) -> bool {
    a.kind() == b.kind() || (a.is_blob() && b.is_blob())
}

/// Build the [`CommitInfo`] of `commit`, interning people in `people`.
pub(crate) fn commit_info(
    commit: &gix::Commit<'_>,
    people: &mut HashMap<(BString, BString), Arc<Person>>,
) -> Result<CommitInfo> {
    let other = |err: &dyn std::fmt::Display| GitError::Other(anyhow::anyhow!("{err}"));
    let author = signature(commit.author().map_err(|err| other(&err))?, people);
    let committer = signature(commit.committer().map_err(|err| other(&err))?, people);
    let message = commit.message_raw_sloppy();
    let (summary, rest) = match message.find_byte(b'\n') {
        Some(end) => (&message[..end], &message[end + 1..]),
        None => (message, b"".as_bstr()),
    };
    Ok(CommitInfo {
        id: commit.id,
        parents: commit.parent_ids().map(gix::Id::detach).collect(),
        author,
        committer,
        summary: summary.to_str_lossy().trim_end().into(),
        has_body: !rest.trim().is_empty(),
    })
}

fn signature(
    raw: gix::actor::SignatureRef<'_>,
    people: &mut HashMap<(BString, BString), Arc<Person>>,
) -> Signature {
    let person = people
        .entry((raw.name.to_owned(), raw.email.to_owned()))
        .or_insert_with(|| {
            Arc::new(Person {
                name: raw.name.to_owned(),
                email: raw.email.to_owned(),
            })
        })
        .clone();
    let time = raw.time().unwrap_or_default();
    Signature {
        person,
        time: GitTime {
            seconds: time.seconds,
            offset_minutes: time.offset / 60,
        },
    }
}
