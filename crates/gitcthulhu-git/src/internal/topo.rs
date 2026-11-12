//! The topological walk of the log (spec §3.1): `git rev-list --date-order` or `--topo-order`, children before
//! parents, over an in-memory commit table.
//!
//! This is Git's incremental algorithm (`revision.c`, `init_topo_walk`) as gix implements it, minus the parts the log
//! never uses (excluded commits). It replaces `gix_traverse::commit::Topo` because that one re-reads commits over and
//! over: every visit of a commit reads it, and again each of its parents to learn their commit times, in each of its
//! three passes. Without a commit-graph every reachable commit is visited by the in-degree pass before the first one
//! is returned, so that cost decided the first page (spec §9). Here a commit is read from the object database once,
//! and commits of the commit-graph are never read at all.
//!
//! Table: commits in the commit-graph are addressed by their graph position, so their walk state is a plain array
//! entry and their parents, commit times and generation numbers come from the graph. Every other commit (all of them
//! without a graph, the commits newer than the graph otherwise) gets an index after the graph's positions, a hash map
//! entry, and its parents and commit time decoded once. Such commits have generation number infinity, as in Git.
//!
//! Walk: an in-degree pass counts, for every commit down to the smallest generation needed so far, how many walked
//! children it has. A commit enters the output queue when its last child was returned; the output queue is ordered
//! by commit time (date order, ties first-in first-out like Git's `prio_queue`) or is a stack (topo order). Without a
//! commit-graph every generation is infinite, so the first in-degree pass covers all reachable commits: the full walk
//! that `git log --date-order` makes as well.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
    sync::{Arc, Condvar, Mutex, PoisonError},
};

use bstr::ByteSlice as _;
use gix::{
    ObjectId,
    commitgraph::{GENERATION_NUMBER_INFINITY, Graph, Position},
    objs::{Find as _, commit::ref_iter::Token},
};
use smallvec::SmallVec;

use crate::{CancelToken, GitError, LogOrder, Result, internal::errors::gix_error};

/// A commit of the table: its graph position, or `graph_len + n` for the n-th commit outside the graph.
pub(crate) type Node = u32;

/// Parents of one commit, as nodes.
type NodeParents = SmallVec<[Node; 4]>;

/// How often the build looks at the cancel token, in commits.
const CANCEL_CHECK_INTERVAL: usize = 256;

/// An annotated tag of an annotated tag of ... is peeled at most this deep (a guard against corrupt objects).
const MAX_PEEL_DEPTH: usize = 32;

/// A commit outside the commit-graph.
struct Extra {
    id: ObjectId,
    /// Committer time; valid once `parents` is set.
    time: i64,
    /// `(start, len)` in [`CommitTable::edges`]; `None` until the commit was read.
    parents: Option<(u32, u32)>,
}

/// Commits by node: parents, commit time and generation, read once.
pub(crate) struct CommitTable {
    odb: gix::OdbHandle,
    graph: Option<Graph>,
    graph_len: u32,
    /// Commits whose parents a shallow clone cut off; they are walked without parents (as Git's grafts do).
    shallow: Arc<HashSet<ObjectId>>,
    buf: Vec<u8>,
    extra: Vec<Extra>,
    extra_index: gix::hashtable::HashMap<ObjectId, Node>,
    /// Parent lists of the commits outside the graph.
    edges: Vec<Node>,
}

impl CommitTable {
    /// A table over `repo`'s objects, using `graph` for the commits it contains.
    ///
    /// The object database handle has no object cache: every commit is read once, so caching would only copy.
    pub(crate) fn new(
        repo: &gix::Repository,
        graph: Option<Graph>,
        shallow: Arc<HashSet<ObjectId>>,
    ) -> Self {
        let mut odb = repo.objects.clone();
        odb.unset_object_cache();
        let graph_len = graph.as_ref().map_or(0, Graph::num_commits);
        CommitTable {
            odb,
            graph,
            graph_len,
            shallow,
            buf: Vec::new(),
            extra: Vec::new(),
            extra_index: Default::default(),
            edges: Vec::new(),
        }
    }

    /// Number of nodes so far (the whole graph plus the commits outside it seen so far).
    fn len(&self) -> usize {
        self.graph_len as usize + self.extra.len()
    }

    /// The commit id of `node`.
    pub(crate) fn id(&self, node: Node) -> ObjectId {
        match self.graph_node(node) {
            Some(graph) => graph.id_at(Position(node)).to_owned(),
            None => self.extra[(node - self.graph_len) as usize].id,
        }
    }

    fn graph_node(&self, node: Node) -> Option<&Graph> {
        self.graph.as_ref().filter(|_| node < self.graph_len)
    }

    /// The node of a tip: `Ok(None)` if `id` is missing or not a commit. With `peel`, annotated tags are followed to
    /// what they finally point to.
    pub(crate) fn tip(&mut self, id: ObjectId, peel: bool) -> Result<Option<Node>> {
        let mut id = id;
        for _ in 0..MAX_PEEL_DEPTH {
            if let Some(node) = self.known(&id) {
                // A commit seen as somebody's parent is not read yet; read it now, so a broken one is an error here.
                self.load(node)?;
                return Ok(Some(node));
            }
            enum Found {
                Commit(SmallVec<[ObjectId; 2]>, i64),
                Tag(ObjectId),
                Nothing,
            }
            let mut buf = std::mem::take(&mut self.buf);
            let found = match self.odb.try_find(&id, &mut buf).map_err(gix_error) {
                Err(err) => Err(err),
                Ok(None) => Ok(Found::Nothing),
                Ok(Some(data)) => match data.kind {
                    gix::object::Kind::Commit => parse_commit(data.data, data.object_hash, &id)
                        .map(|(parents, time)| Found::Commit(parents, time)),
                    // Like a failed peel in Git, a tag that cannot be decoded is no tip.
                    gix::object::Kind::Tag if peel => Ok(gix::objs::TagRefIter::from_bytes(
                        data.data,
                        data.object_hash,
                    )
                    .target_id()
                    .map_or(Found::Nothing, Found::Tag)),
                    _ => Ok(Found::Nothing),
                },
            };
            self.buf = buf;
            match found? {
                Found::Commit(parents, time) => {
                    return Ok(Some(self.insert_loaded(id, parents, time)));
                }
                Found::Tag(target) => id = target,
                Found::Nothing => return Ok(None),
            }
        }
        Ok(None)
    }

    /// The node of `id` if it is in the graph or was seen before.
    fn known(&self, id: &ObjectId) -> Option<Node> {
        if let Some(graph) = &self.graph
            && let Some(Position(pos)) = graph.lookup(id)
        {
            return Some(pos);
        }
        self.extra_index.get(id).copied()
    }

    /// The node of the commit `id`, created unread if it is new.
    fn node_of(&mut self, id: &ObjectId) -> Node {
        if let Some(node) = self.known(id) {
            return node;
        }
        let node = self.len() as Node;
        self.extra.push(Extra {
            id: *id,
            time: 0,
            parents: None,
        });
        self.extra_index.insert(*id, node);
        node
    }

    fn insert_loaded(&mut self, id: ObjectId, parents: SmallVec<[ObjectId; 2]>, time: i64) -> Node {
        let node = self.node_of(&id);
        self.set_loaded(node, &parents, time);
        node
    }

    fn set_loaded(&mut self, node: Node, parents: &[ObjectId], time: i64) {
        let cut = self
            .shallow
            .contains(&self.extra[(node - self.graph_len) as usize].id);
        let start = self.edges.len() as u32;
        if !cut {
            for parent in parents {
                let parent = self.node_of(parent);
                self.edges.push(parent);
            }
        }
        let len = self.edges.len() as u32 - start;
        let extra = &mut self.extra[(node - self.graph_len) as usize];
        extra.time = time;
        extra.parents = Some((start, len));
    }

    /// Read `node` from the object database if it is outside the graph and not read yet.
    fn load(&mut self, node: Node) -> Result<()> {
        if node < self.graph_len {
            return Ok(());
        }
        let extra = &self.extra[(node - self.graph_len) as usize];
        if extra.parents.is_some() {
            return Ok(());
        }
        let id = extra.id;
        let mut buf = std::mem::take(&mut self.buf);
        let parsed = match self.odb.try_find(&id, &mut buf).map_err(gix_error) {
            Err(err) => Err(err),
            Ok(Some(data)) if data.kind == gix::object::Kind::Commit => {
                parse_commit(data.data, data.object_hash, &id)
            }
            Ok(_) => Err(GitError::NotFound {
                what: "commit",
                name: id.to_string(),
            }),
        };
        self.buf = buf;
        let (parents, time) = parsed?;
        self.set_loaded(node, &parents, time);
        Ok(())
    }

    /// Generation number and commit time of `node`.
    fn gen_and_time(&mut self, node: Node) -> Result<(u32, i64)> {
        if let Some(graph) = self.graph_node(node) {
            let commit = graph.commit_at(Position(node));
            return Ok((commit.generation(), commit.committer_timestamp() as i64));
        }
        self.load(node)?;
        Ok((
            GENERATION_NUMBER_INFINITY,
            self.extra[(node - self.graph_len) as usize].time,
        ))
    }

    /// Read every commit reachable from `tips` that is not read yet, on several threads (only first parents are
    /// followed with `first_only`).
    ///
    /// Without a commit-graph the first in-degree pass needs every reachable commit, and reading them one by one is
    /// what the pass costs (§9). A history has as many independent lines to read as it has branches open at a time,
    /// so the threads take those in parallel. This is only a prefetch: whatever it does not read (a commit that fails
    /// to read, a cancelled run) is read by the walk itself when it needs it, with the walk's error handling.
    fn prefetch(&mut self, tips: &[Node], first_only: bool, cancel: &CancelToken) -> Result<()> {
        let mut start = Vec::new();
        let mut parents = NodeParents::new();
        for &tip in tips {
            self.parents(tip, first_only, &mut parents)?;
            for &parent in &parents {
                if parent >= self.graph_len
                    && self.extra[(parent - self.graph_len) as usize]
                        .parents
                        .is_none()
                {
                    start.push(self.id(parent));
                }
            }
        }
        start.sort_unstable();
        start.dedup();
        if start.is_empty() {
            return Ok(());
        }
        let threads = std::thread::available_parallelism()
            .map_or(4, std::num::NonZeroUsize::get)
            .min(MAX_PREFETCH_THREADS);
        // Visited: what is read already and what is queued. Not every known commit: with `first_only`, a tip's second
        // parent is known but unread, and may be on another tip's first-parent line, which must not stop there.
        let mut visited: Vec<gix::hashtable::HashSet<ObjectId>> =
            (0..VISITED_SHARDS).map(|_| Default::default()).collect();
        let loaded = self
            .extra
            .iter()
            .filter(|extra| extra.parents.is_some())
            .map(|extra| &extra.id);
        for id in loaded.chain(&start) {
            visited[usize::from(id.as_bytes()[0]) % VISITED_SHARDS].insert(*id);
        }
        let shared = Prefetch {
            queue: Mutex::new(PrefetchQueue {
                ids: start,
                busy: 0,
                stop: false,
            }),
            wake: Condvar::new(),
            visited: visited.into_iter().map(Mutex::new).collect(),
            shallow: &self.shallow,
            first_only,
            cancel,
        };
        let found: Vec<Vec<Prefetched>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    let odb = self.odb.clone();
                    let shared = &shared;
                    scope.spawn(move || shared.run(&odb))
                })
                .collect();
            // A thread that panicked read nothing that counts; the walk reads it.
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .collect()
        });
        cancel.check()?;
        for (id, parents, time) in found.into_iter().flatten() {
            let node = self.node_of(&id);
            if self.extra[(node - self.graph_len) as usize]
                .parents
                .is_none()
            {
                self.set_loaded(node, &parents, time);
            }
        }
        Ok(())
    }

    /// The parents of `node` (only the first with `first_only`), in order.
    fn parents(&mut self, node: Node, first_only: bool, out: &mut NodeParents) -> Result<()> {
        out.clear();
        if let Some(graph) = self.graph_node(node) {
            for parent in graph.commit_at(Position(node)).iter_parents() {
                out.push(parent.map_err(gix_error)?.0);
                if first_only {
                    break;
                }
            }
            return Ok(());
        }
        self.load(node)?;
        let (start, len) = self.extra[(node - self.graph_len) as usize]
            .parents
            .unwrap_or_default();
        let len = if first_only { len.min(1) } else { len };
        out.extend_from_slice(&self.edges[start as usize..(start + len) as usize]);
        Ok(())
    }
}

/// A commit read by a prefetch thread: id, parents, committer time.
type Prefetched = (ObjectId, SmallVec<[ObjectId; 2]>, i64);

/// Prefetch threads at most; more do not help against the object database.
const MAX_PREFETCH_THREADS: usize = 8;
/// The visited set of the prefetch is split by the first byte of the id, so threads rarely wait for each other.
const VISITED_SHARDS: usize = 64;

/// The work of the prefetch threads: ids to read, and how many threads are reading.
struct PrefetchQueue {
    ids: Vec<ObjectId>,
    busy: usize,
    stop: bool,
}

/// State shared by the prefetch threads.
struct Prefetch<'a> {
    queue: Mutex<PrefetchQueue>,
    wake: Condvar,
    visited: Vec<Mutex<gix::hashtable::HashSet<ObjectId>>>,
    shallow: &'a HashSet<ObjectId>,
    first_only: bool,
    cancel: &'a CancelToken,
}

impl Prefetch<'_> {
    /// Mark `id` as visited; `true` if it was new.
    fn visit(&self, id: ObjectId) -> bool {
        let shard = usize::from(id.as_bytes()[0]) % VISITED_SHARDS;
        self.visited[shard]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id)
    }

    /// The next id to read, or `None` when all work is done (or stopped).
    fn take(&self) -> Option<ObjectId> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if queue.stop {
                return None;
            }
            if let Some(id) = queue.ids.pop() {
                queue.busy += 1;
                return Some(id);
            }
            if queue.busy == 0 {
                // Nothing queued and nobody can queue more: done. Wake the others so they see it too.
                queue.stop = true;
                self.wake.notify_all();
                return None;
            }
            queue = self
                .wake
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn give(&self, id: ObjectId) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ids
            .push(id);
        self.wake.notify_one();
    }

    fn done_with_one(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.busy -= 1;
        if queue.busy == 0 && queue.ids.is_empty() {
            self.wake.notify_all();
        }
    }

    fn stop(&self) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stop = true;
        self.wake.notify_all();
    }

    /// One thread: take an id, follow its first parent on this thread and hand the other parents to idle threads,
    /// so a linear history costs no hand-over per commit.
    fn run(&self, odb: &gix::OdbHandle) -> Vec<Prefetched> {
        let mut out = Vec::new();
        let mut buf = Vec::new();
        while let Some(start) = self.take() {
            let _in_progress = InProgress(self);
            let mut next = Some(start);
            while let Some(id) = next.take() {
                if out.len().is_multiple_of(CANCEL_CHECK_INTERVAL) && self.cancel.is_cancelled() {
                    self.stop();
                    break;
                }
                // A commit that cannot be read is left to the walk, which reports the error where it matters.
                let Ok(Some(data)) = odb.try_find(&id, &mut buf) else {
                    continue;
                };
                if data.kind != gix::object::Kind::Commit {
                    continue;
                }
                let Ok((parents, time)) = parse_commit(data.data, data.object_hash, &id) else {
                    continue;
                };
                if !self.shallow.contains(&id) {
                    let followed = if self.first_only {
                        &parents[..parents.len().min(1)]
                    } else {
                        &parents[..]
                    };
                    for parent in followed {
                        if self.visit(*parent) {
                            if next.is_none() {
                                next = Some(*parent);
                            } else {
                                self.give(*parent);
                            }
                        }
                    }
                }
                out.push((id, parents, time));
            }
        }
        out
    }
}

/// A taken id is in progress until this is dropped, also when its thread panics: the other threads then never wait
/// for work that cannot come.
struct InProgress<'p, 'a>(&'p Prefetch<'a>);

impl Drop for InProgress<'_, '_> {
    fn drop(&mut self) {
        self.0.done_with_one();
    }
}

/// Parents and committer time of the commit `data`.
fn parse_commit(
    data: &[u8],
    hash: gix::hash::Kind,
    id: &ObjectId,
) -> Result<(SmallVec<[ObjectId; 2]>, i64)> {
    if let Some(parsed) = parse_commit_fast(data, hash) {
        return Ok(parsed);
    }
    let mut parents = SmallVec::new();
    for token in gix::objs::CommitRefIter::from_bytes(data, hash) {
        match token.map_err(gix_error)? {
            Token::Tree { .. } | Token::Author { .. } => {}
            Token::Parent { id } => parents.push(id),
            Token::Committer { signature } => return Ok((parents, signature.seconds())),
            Token::Encoding(_) | Token::ExtraHeader(_) | Token::Message(_) => break,
        }
    }
    Err(GitError::Other(anyhow::anyhow!(
        "The commit {id} has no committer"
    )))
}

/// [`parse_commit`] for a commit in Git's canonical layout (`tree`, `parent`s, `author`, `committer`, as Git writes
/// them), without decoding the signatures; `None` for anything else, which gix's tolerant parser then reads. Without a
/// commit-graph every reachable commit goes through here, and gix's parser costs about a microsecond per commit.
fn parse_commit_fast(data: &[u8], hash: gix::hash::Kind) -> Option<(SmallVec<[ObjectId; 2]>, i64)> {
    let hex_len = hash.len_in_hex();
    let mut rest = data
        .strip_prefix(b"tree ")?
        .get(hex_len..)?
        .strip_prefix(b"\n")?;
    let mut parents = SmallVec::new();
    while let Some(line) = rest.strip_prefix(b"parent ") {
        parents.push(ObjectId::from_hex(line.get(..hex_len)?).ok()?);
        rest = line[hex_len..].strip_prefix(b"\n")?;
    }
    let author = rest.strip_prefix(b"author ")?;
    let committer = author[author.find_byte(b'\n')? + 1..].strip_prefix(b"committer ")?;
    let committer = &committer[..committer.find_byte(b'\n')?];
    // `<name> <<email>> <seconds> <offset>`
    let time = committer[committer.rfind_byte(b'>')? + 1..].trim_start();
    let seconds = &time[..time.find_byte(b' ')?];
    let (negative, digits) = match seconds.strip_prefix(b"-") {
        Some(digits) => (true, digits),
        None => (false, seconds),
    };
    if digits.is_empty() || digits.len() > 18 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = digits
        .iter()
        .fold(0i64, |value, digit| value * 10 + i64::from(digit - b'0'));
    Some((parents, if negative { -value } else { value }))
}

/// The output queue.
enum Queue {
    /// By commit time, newest first; ties in insertion order.
    Date(BinaryHeap<(i64, Reverse<u64>, Node)>),
    /// A stack.
    Topo(Vec<Node>),
}

/// The in-degree queue.
enum IndegreeQueue {
    /// `(generation, commit time, node)`, highest first.
    ByGeneration(BinaryHeap<(u32, i64, Node)>),
    /// Without a commit-graph every generation is infinite, so every queued commit is counted and the order does
    /// not matter: a stack is enough.
    Unordered(Vec<Node>),
}

/// The walk: commit ids in date or topo order, children before parents.
pub(crate) struct TopoWalk {
    table: CommitTable,
    first_parent: bool,
    /// 1 + the number of walked children not returned yet; 0 for commits not counted (or already returned).
    indegree: Vec<u32>,
    /// The commit is (or was) in the in-degree queue.
    counted: Vec<bool>,
    indegree_queue: IndegreeQueue,
    queue: Queue,
    /// Insertion counter of the date queue.
    seq: u64,
    /// The in-degrees are complete for every commit of at least this generation.
    min_gen: u32,
}

impl TopoWalk {
    /// Start the walk at `tips` (duplicates are ignored). Runs the first in-degree pass, which is the whole history
    /// without a commit-graph; checks `cancel` every 256 commits.
    pub(crate) fn new(
        table: CommitTable,
        tips: &[Node],
        order: LogOrder,
        first_parent: bool,
        cancel: &CancelToken,
    ) -> Result<TopoWalk> {
        let len = table.len();
        let has_graph = table.graph.is_some();
        let mut walk = TopoWalk {
            table,
            first_parent,
            indegree: vec![0; len],
            counted: vec![false; len],
            indegree_queue: if has_graph {
                IndegreeQueue::ByGeneration(BinaryHeap::new())
            } else {
                IndegreeQueue::Unordered(Vec::new())
            },
            queue: match order {
                LogOrder::Date => Queue::Date(BinaryHeap::new()),
                LogOrder::Topo => Queue::Topo(Vec::new()),
            },
            seq: 0,
            min_gen: GENERATION_NUMBER_INFINITY,
        };
        let mut unique = Vec::with_capacity(tips.len());
        let mut seen = HashSet::with_capacity(tips.len());
        for &tip in tips {
            if seen.insert(tip) {
                unique.push(tip);
            }
        }
        for &tip in &unique {
            walk.grow();
            walk.indegree[tip as usize] = 1;
            walk.counted[tip as usize] = true;
            let (generation, time) = walk.table.gen_and_time(tip)?;
            walk.min_gen = walk.min_gen.min(generation);
            walk.queue_for_count(generation, time, tip);
        }
        if !has_graph {
            walk.table.prefetch(&unique, first_parent, cancel)?;
        }
        walk.count_indegrees(walk.min_gen, Some(cancel))?;

        let mut ready = Vec::new();
        for &tip in &unique {
            if walk.indegree[tip as usize] == 1 {
                ready.push((walk.table.gen_and_time(tip)?.1, tip));
            }
        }
        match &mut walk.queue {
            Queue::Date(_) => {
                for (time, tip) in ready {
                    walk.push(time, tip);
                }
            }
            Queue::Topo(stack) => {
                // Like Git (`prepare_revision_walk` sorts the tips newest first, stable for equal times, and
                // `init_topo_walk` reverses the stack): the tips come out newest first, equal times in tip order.
                ready.sort_by_key(|(time, _)| Reverse(*time));
                stack.extend(ready.into_iter().rev().map(|(_, tip)| tip));
            }
        }
        Ok(walk)
    }

    /// Size the per-node state to the table.
    fn grow(&mut self) {
        let len = self.table.len();
        if self.indegree.len() < len {
            self.indegree.resize(len, 0);
            self.counted.resize(len, false);
        }
    }

    fn push(&mut self, time: i64, node: Node) {
        match &mut self.queue {
            Queue::Date(heap) => {
                heap.push((time, Reverse(self.seq), node));
                self.seq += 1;
            }
            Queue::Topo(stack) => stack.push(node),
        }
    }

    fn queue_for_count(&mut self, generation: u32, time: i64, node: Node) {
        match &mut self.indegree_queue {
            IndegreeQueue::ByGeneration(heap) => heap.push((generation, time, node)),
            IndegreeQueue::Unordered(stack) => stack.push(node),
        }
    }

    /// The next commit to count if its generation is at least `cutoff`.
    fn next_to_count(&mut self, cutoff: u32) -> Option<Node> {
        match &mut self.indegree_queue {
            IndegreeQueue::ByGeneration(heap) => match heap.peek() {
                Some(&(generation, _, _)) if generation >= cutoff => {
                    heap.pop().map(|(_, _, node)| node)
                }
                _ => None,
            },
            IndegreeQueue::Unordered(stack) => stack.pop(),
        }
    }

    /// Count in-degrees for every queued commit of generation `cutoff` or more.
    fn count_indegrees(&mut self, cutoff: u32, cancel: Option<&CancelToken>) -> Result<()> {
        let mut parents = NodeParents::new();
        let mut steps = 0usize;
        while let Some(node) = self.next_to_count(cutoff) {
            if let Some(cancel) = cancel
                && steps.is_multiple_of(CANCEL_CHECK_INTERVAL)
            {
                cancel.check()?;
            }
            steps += 1;
            self.table.parents(node, self.first_parent, &mut parents)?;
            self.grow();
            for &parent in &parents {
                let indegree = &mut self.indegree[parent as usize];
                *indegree = if *indegree == 0 { 2 } else { *indegree + 1 };
                if !self.counted[parent as usize] {
                    self.counted[parent as usize] = true;
                    let (generation, time) = self.table.gen_and_time(parent)?;
                    self.queue_for_count(generation, time, parent);
                }
            }
        }
        Ok(())
    }

    /// The next commit, or `None` at the end.
    fn step(&mut self) -> Option<Result<Node>> {
        let node = match &mut self.queue {
            Queue::Date(heap) => heap.pop().map(|(_, _, node)| node),
            Queue::Topo(stack) => stack.pop(),
        }?;
        Some(self.expand(node).map(|()| node))
    }

    /// Return `node`: release its parents whose last child it was.
    fn expand(&mut self, node: Node) -> Result<()> {
        self.indegree[node as usize] = 0;
        let mut parents = NodeParents::new();
        self.table.parents(node, self.first_parent, &mut parents)?;
        self.grow();
        for &parent in &parents {
            let (generation, time) = self.table.gen_and_time(parent)?;
            if generation < self.min_gen {
                self.min_gen = generation;
                self.count_indegrees(generation, None)?;
            }
            let indegree = &mut self.indegree[parent as usize];
            if *indegree < 2 {
                return Err(GitError::Other(anyhow::anyhow!(
                    "The log walk lost track of the children of commit {}",
                    self.table.id(parent)
                )));
            }
            *indegree -= 1;
            if *indegree == 1 {
                self.push(time, parent);
            }
        }
        Ok(())
    }
}

impl Iterator for TopoWalk {
    type Item = Result<ObjectId>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.step()?.map(|node| self.table.id(node)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::content::fixture;

    /// A history with merges, equal commit times and a commit with the same parent twice.
    fn history() -> fixture::TestRepo {
        let repo = fixture::repo();
        let commit = |repo: &fixture::TestRepo, msg: &str, time: i64, parents: &[&str]| -> String {
            let tree = repo.git(&["write-tree"]);
            let mut args = vec!["commit-tree", tree.as_str(), "-m", msg];
            for parent in parents {
                args.extend(["-p", parent]);
            }
            let date = format!("{} +0000", 1_600_000_000 + time);
            repo.git_env(
                &args,
                &[("GIT_COMMITTER_DATE", &date), ("GIT_AUTHOR_DATE", &date)],
            )
        };
        let base = commit(&repo, "base", 1000, &[]);
        let a1 = commit(&repo, "a1", 1010, &[&base]);
        let b1 = commit(&repo, "b1", 1010, &[&base]);
        let a2 = commit(&repo, "a2", 1020, &[&a1]);
        let b2 = commit(&repo, "b2", 1005, &[&b1]); // clock skew: older than its parent
        let merge = commit(&repo, "merge", 1030, &[&a2, &b2]);
        let twice = commit(&repo, "twice", 1040, &[&merge, &merge]);
        let side = commit(&repo, "side", 1040, &[&a1]);
        repo.git(&["update-ref", "refs/heads/main", &twice]);
        repo.git(&["update-ref", "refs/heads/side", &side]);
        repo.git(&["update-ref", "refs/heads/b", &b2]);
        repo
    }

    fn walk(
        repo: &gix::Repository,
        graph: bool,
        order: LogOrder,
        first_parent: bool,
    ) -> Vec<ObjectId> {
        let graph = graph.then(|| repo.commit_graph().expect("a commit-graph"));
        let mut table = CommitTable::new(repo, graph, Arc::default());
        let mut tips = Vec::new();
        for name in ["refs/heads/main", "refs/heads/side", "refs/heads/b"] {
            let id = repo.find_reference(name).expect("ref").id().detach();
            tips.extend(table.tip(id, true).expect("readable"));
        }
        TopoWalk::new(table, &tips, order, first_parent, &CancelToken::new())
            .expect("the walk starts")
            .collect::<Result<Vec<_>>>()
            .expect("the walk completes")
    }

    fn oracle(repo: &fixture::TestRepo, args: &[&str]) -> Vec<ObjectId> {
        let mut full = vec!["rev-list"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["main", "side", "b"]);
        repo.git(&full)
            .lines()
            .map(|line| ObjectId::from_hex(line.as_bytes()).expect("an id"))
            .collect()
    }

    #[test]
    fn date_topo_and_first_parent_orders_equal_git_with_and_without_commit_graph() {
        let repo = history();
        let gix_repo = repo.open();
        let date = oracle(&repo, &["--date-order"]);
        let topo = oracle(&repo, &["--topo-order"]);
        let first = oracle(&repo, &["--date-order", "--first-parent"]);
        assert_eq!(
            walk(&gix_repo, false, LogOrder::Date, false),
            date,
            "date order"
        );
        assert_eq!(
            walk(&gix_repo, false, LogOrder::Topo, false),
            topo,
            "topo order"
        );
        assert_eq!(
            walk(&gix_repo, false, LogOrder::Date, true),
            first,
            "first parent"
        );

        repo.git(&["commit-graph", "write", "--reachable"]);
        let gix_repo = repo.open();
        assert_eq!(
            walk(&gix_repo, true, LogOrder::Date, false),
            date,
            "date order, graph"
        );
        assert_eq!(
            walk(&gix_repo, true, LogOrder::Topo, false),
            topo,
            "topo order, graph"
        );
        assert_eq!(
            walk(&gix_repo, true, LogOrder::Date, true),
            first,
            "first parent, graph"
        );
    }

    #[test]
    fn tips_peel_tags_and_skip_other_objects() {
        let repo = history();
        repo.git(&["tag", "-a", "-m", "t", "annotated", "side"]);
        repo.git(&["tag", "-a", "-m", "t2", "nested", "annotated"]);
        let gix_repo = repo.open();
        let mut table = CommitTable::new(&gix_repo, None, Arc::default());
        let id =
            |rev: &str| ObjectId::from_hex(repo.git(&["rev-parse", rev]).as_bytes()).expect("id");
        let side = table
            .tip(id("side"), false)
            .expect("readable")
            .expect("a commit");
        assert_eq!(table.id(side), id("side"));
        let nested = id("nested");
        assert_eq!(
            table.tip(nested, true).expect("readable"),
            Some(side),
            "a tag of a tag peels to the commit"
        );
        assert_eq!(
            table.tip(nested, false).expect("readable"),
            None,
            "without peeling a tag is no commit"
        );
        assert_eq!(
            table.tip(id("main^{tree}"), true).expect("readable"),
            None,
            "a tree is skipped"
        );
        assert_eq!(
            table
                .tip(ObjectId::null(gix::hash::Kind::Sha1), true)
                .expect("readable"),
            None,
            "a missing object is skipped"
        );
    }

    /// `parse_commit` with gix's parser only, as a reference for the fast path.
    fn parse_with_gix(data: &[u8]) -> Option<(SmallVec<[ObjectId; 2]>, i64)> {
        let mut parents = SmallVec::new();
        for token in gix::objs::CommitRefIter::from_bytes(data, gix::hash::Kind::Sha1) {
            match token.ok()? {
                Token::Parent { id } => parents.push(id),
                Token::Committer { signature } => return Some((parents, signature.seconds())),
                _ => {}
            }
        }
        None
    }

    #[test]
    fn fast_commit_parse_equals_gix_or_declines() {
        let tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
        let p1 = "1111111111111111111111111111111111111111";
        let p2 = "2222222222222222222222222222222222222222";
        let canonical = [
            format!(
                "tree {tree}\nauthor A <a@x> 1 +0000\ncommitter C <c@x> 1600000000 +0100\n\nroot\n"
            ),
            format!(
                "tree {tree}\nparent {p1}\nparent {p2}\nauthor A <a@x> 1 +0000\ncommitter C O <c@x> 1600000001 -0230\n\nmerge\n"
            ),
            format!(
                "tree {tree}\nparent {p1}\nauthor A <a@x> 1 +0000\ncommitter C <c@x> -5 +0000\n\nbefore 1970\n"
            ),
            format!(
                "tree {tree}\nparent {p1}\nauthor A <a@x> 1 +0000\ncommitter Name <with> brackets> <c@x> 42 +0000\nencoding ISO-8859-1\n\nm\n"
            ),
        ];
        for commit in &canonical {
            let fast = parse_commit_fast(commit.as_bytes(), gix::hash::Kind::Sha1);
            assert!(
                fast.is_some(),
                "the canonical layout takes the fast path: {commit}"
            );
            assert_eq!(fast, parse_with_gix(commit.as_bytes()), "{commit}");
        }
        let unusual = [
            // A header gix tolerates between the parents and the author.
            format!(
                "tree {tree}\nparent {p1}\nmergetag x\nauthor A <a@x> 1 +0000\ncommitter C <c@x> 7 +0000\n\nm\n"
            ),
            // No time at all.
            format!("tree {tree}\nauthor A <a@x> 1 +0000\ncommitter C <c@x>\n\nm\n"),
            // A truncated parent id.
            format!(
                "tree {tree}\nparent 1111\nauthor A <a@x> 1 +0000\ncommitter C <c@x> 7 +0000\n\nm\n"
            ),
        ];
        for commit in &unusual {
            assert_eq!(
                parse_commit_fast(commit.as_bytes(), gix::hash::Kind::Sha1),
                None,
                "anything but the canonical layout is left to gix: {commit}"
            );
            let id = ObjectId::null(gix::hash::Kind::Sha1);
            assert_eq!(
                parse_commit(commit.as_bytes(), gix::hash::Kind::Sha1, &id).ok(),
                parse_with_gix(commit.as_bytes()),
                "the fallback is gix's parser: {commit}"
            );
        }
    }
}
