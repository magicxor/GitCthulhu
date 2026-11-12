//! Commit log walking and search (WP-B4, spec §2.5 `log.rs`, §3.1).
//!
//! The log is a `gix_traverse` topological walk in date order (`git rev-list --date-order`) or topo order, which
//! guarantees every child before its parents, the precondition of the lane layout in [`crate::graph`]. The walk is
//! built once per [`LogWalker`] (on a detached thread, because the build is not interruptible) and then delivers
//! pages incrementally.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    future::Future as _,
    sync::{Arc, mpsc},
    task::{Context, Wake, Waker},
};

use bstr::{BStr, ByteSlice};
use gix::{
    ObjectId,
    objs::{Find as _, commit::ref_iter::Token},
    traverse::commit::{Parents, Topo, topo},
};
use smallvec::SmallVec;

use crate::{
    CancelToken, CommitInfo, GitError, GitTime, LogOptions, LogOrder, LogPage, LogTips, Person,
    Repo, Result, SearchQuery, Signature,
};

impl Default for LogOptions {
    fn default() -> Self {
        LogOptions {
            tips: LogTips::AllRefs {
                remotes: true,
                tags: true,
                stashes: true,
            },
            order: LogOrder::Date,
            first_parent: false,
            mailmap: true,
        }
    }
}

/// How often long loops look at the cancel token.
const CANCEL_CHECK_INTERVAL: usize = 256;

/// The predicate that hides stash helper commits from the walk.
type HidePredicate = Box<dyn FnMut(&gix::oid) -> bool + Send>;

/// An incremental walk over history, producing pages of commits in walk order (children before parents).
///
/// `Send`, not `Sync`: the GUI keeps it in a `Mutex` and drives it from background tasks.
pub struct LogWalker {
    repo: gix::Repository,
    topo: Topo<WalkObjects, HidePredicate>,
    first_parent: bool,
    stash_ids: HashSet<ObjectId>,
    shallow: Arc<HashSet<ObjectId>>,
    decoder: CommitDecoder,
    /// Commits decoded but not yet returned: a page interrupted by cancellation or an error keeps its commits here,
    /// so the next page starts with them and pagination never loses rows.
    pending: Vec<CommitInfo>,
    /// The walk's next item, read ahead so `is_exhausted()` is exact after a full page.
    peeked: Option<gix::Result<ObjectId>>,
    /// The topological walk returned its last commit.
    topo_done: bool,
    loaded: usize,
}

impl LogWalker {
    /// Start a walk. The Topo build (up to seconds without a commit-graph) runs on a detached compute thread (§1.4);
    /// on cancel this returns `Err(Cancelled)` immediately and the build result is dropped when it finishes.
    ///
    /// Tips: `LogTips::AllRefs` walks HEAD (if born), local branches, and optionally remote-tracking branches, tags
    /// (peeled; tags of non-commits are skipped) and every stash in the `refs/stash` reflog. Other namespaces, such as
    /// the oplog's `refs/gitcthulhu/*`, are never walked. `LogTips::Explicit` walks exactly the given commits.
    ///
    /// Errors: `NotFound` for an explicit tip that is not a commit in the repository, `Cancelled`, `Gix`.
    pub fn new(repo: &Repo, opts: &LogOptions, cancel: &CancelToken) -> Result<LogWalker> {
        let local = repo.local();
        let opts = opts.clone();
        let token = cancel.clone();
        run_detached("gitcthulhu-log-walk", cancel, move || {
            LogWalker::build(local, &opts, &token)
        })
    }

    fn build(repo: gix::Repository, opts: &LogOptions, cancel: &CancelToken) -> Result<LogWalker> {
        let shallow: HashSet<ObjectId> = repo
            .shallow_commits()?
            .map(|commits| commits.iter().copied().collect())
            .unwrap_or_default();
        let shallow = Arc::new(shallow);
        let (tips, stash_ids) = collect_tips(&repo, &opts.tips, cancel)?;

        // The index and untracked commits of stashes are parents 2.. of the stash commit; they are never shown.
        let mut helpers = HashSet::new();
        let mut buf = Vec::new();
        for stash in &stash_ids {
            let commit = find_commit_iter(&repo, stash, &mut buf)?;
            helpers.extend(commit.parent_ids().skip(1));
        }
        helpers.retain(|id| !stash_ids.contains(id));

        // Git ignores the commit-graph in shallow repositories (its parent lists would contradict the cut).
        let commit_graph = if shallow.is_empty() {
            match repo.commit_graph_if_enabled() {
                Ok(graph) => graph,
                Err(err) => {
                    tracing::warn!(%err, "Ignoring an unreadable commit-graph; the log walk will be slower");
                    None
                }
            }
        } else {
            None
        };

        cancel.check()?;
        let hide: HidePredicate = Box::new(move |id| !helpers.contains(id));
        let objects = WalkObjects {
            odb: repo.objects.clone(),
            shallow: shallow.clone(),
        };
        let topo = topo::Builder::from_iters(objects, tips, None::<Vec<ObjectId>>)
            .with_predicate(hide)
            .sorting(match opts.order {
                LogOrder::Date => topo::Sorting::DateOrder,
                LogOrder::Topo => topo::Sorting::TopoOrder,
            })
            .parents(if opts.first_parent {
                Parents::First
            } else {
                Parents::All
            })
            .with_commit_graph(commit_graph)
            .build()?;
        let decoder = CommitDecoder::new(opts.mailmap.then(|| repo.open_mailmap()));
        Ok(LogWalker {
            repo,
            topo,
            first_parent: opts.first_parent,
            stash_ids,
            shallow,
            decoder,
            pending: Vec::new(),
            peeked: None,
            topo_done: false,
            loaded: 0,
        })
    }

    /// `opts.first_parent` (for `GraphBuilder::new`).
    pub fn first_parent(&self) -> bool {
        self.first_parent
    }

    /// Next up to `n` commits in walk order. Cancel is checked every 256 commits.
    ///
    /// `Cancelled` (or an error) loses nothing: commits already read are returned by the next call.
    pub fn next_page(&mut self, n: usize, cancel: &CancelToken) -> Result<LogPage> {
        let mut commits = std::mem::take(&mut self.pending);
        if commits.len() > n {
            self.pending = commits.split_off(n);
        }
        let mut read = 0usize;
        while commits.len() < n {
            if read.is_multiple_of(CANCEL_CHECK_INTERVAL) && cancel.is_cancelled() {
                self.pending = commits;
                return Err(GitError::Cancelled);
            }
            read += 1;
            let id = match self.next_id() {
                None => break,
                Some(Ok(id)) => id,
                Some(Err(err)) => {
                    self.pending = commits;
                    return Err(err.into());
                }
            };
            match self.decoder.decode(&self.repo, id, false) {
                Ok((info, _)) => commits.push(info),
                Err(err) => {
                    // A retry starts with the same commit instead of skipping it.
                    self.peeked = Some(Ok(id));
                    self.pending = commits;
                    return Err(err);
                }
            }
        }
        if self.pending.is_empty() && self.peeked.is_none() && !self.topo_done {
            self.peeked = self.next_id();
        }
        self.loaded += commits.len();
        Ok(LogPage {
            commits,
            exhausted: self.is_exhausted(),
        })
    }

    /// How many commits were returned so far.
    pub fn loaded(&self) -> usize {
        self.loaded
    }

    /// The walk has no more commits.
    pub fn is_exhausted(&self) -> bool {
        self.topo_done && self.peeked.is_none() && self.pending.is_empty()
    }

    /// The stash commits among the walked tips.
    pub fn stash_ids(&self) -> &HashSet<ObjectId> {
        &self.stash_ids
    }

    /// Whether `id` is a commit whose parents are cut off by a shallow clone.
    pub fn is_shallow_boundary(&self, id: &ObjectId) -> bool {
        self.shallow.contains(id)
    }

    /// The next commit of the walk, after the read-ahead one.
    fn next_id(&mut self) -> Option<gix::Result<ObjectId>> {
        if let Some(peeked) = self.peeked.take() {
            return Some(peeked);
        }
        if self.topo_done {
            return None;
        }
        match self.topo.next() {
            Some(item) => Some(item.map(|info| info.id)),
            None => {
                self.topo_done = true;
                None
            }
        }
    }
}

/// Walk all tips (or the given ones) and report matching commit ids in walk order, calling `on_match` as they are
/// found. Stops after `limit` matches. Used for "search all history" (the in-memory search over loaded rows is
/// GUI-side, 05 §2.13).
///
/// Matching is a substring search, case-insensitive unless `query.case_sensitive`, over the full message, the author
/// and committer name and email, as selected; `in_hash` matches a hex prefix of the full id (always
/// case-insensitive). An empty query matches nothing. Returns the number of matches.
pub fn search(
    repo: &Repo,
    query: &SearchQuery,
    tips: &LogTips,
    limit: usize,
    cancel: &CancelToken,
    on_match: &mut dyn FnMut(&CommitInfo),
) -> Result<usize> {
    let matcher = Matcher::new(query);
    if limit == 0 || matcher.is_empty() {
        return Ok(0);
    }
    let opts = LogOptions {
        tips: tips.clone(),
        order: LogOrder::Date,
        first_parent: false,
        mailmap: true,
    };
    let mut walker = LogWalker::new(repo, &opts, cancel)?;
    let mut found = 0;
    let mut read = 0usize;
    while let Some(id) = walker.next_id() {
        if read.is_multiple_of(CANCEL_CHECK_INTERVAL) {
            cancel.check()?;
        }
        read += 1;
        let id = id?;
        let hash_match = matcher.matches_hash(&id);
        if !hash_match && !matcher.needs_commit() {
            continue;
        }
        let (info, message) = walker
            .decoder
            .decode(&walker.repo, id, matcher.in_message)?;
        if hash_match || matcher.matches_commit(&info, message.as_deref()) {
            on_match(&info);
            found += 1;
            if found >= limit {
                break;
            }
        }
    }
    Ok(found)
}

/// Full messages, oldest first, of `range` (`base..head`), for AI PR text (16 A9).
///
/// At most `limit` messages: the newest `limit` commits of the range. Errors: `NotFound` if `head` is not a commit.
pub fn commit_messages(
    repo: &Repo,
    base: Option<ObjectId>,
    head: ObjectId,
    limit: usize,
) -> Result<Vec<String>> {
    let local = repo.local();
    let mut buf = Vec::new();
    // Resolve `head` first, so a missing commit is a `NotFound` and not a walk error.
    find_commit_iter(&local, &head, &mut buf)?;
    let walk = local
        .rev_walk([head])
        .with_hidden(base)
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ))
        .all()?;
    let mut messages = Vec::new();
    for info in walk.take(limit) {
        let info = info?;
        messages.push(full_message(&local, &info.id, &mut buf)?);
    }
    messages.reverse();
    Ok(messages)
}

/// Message of HEAD (amend prefill, 05 §2.14). `None` for unborn.
pub fn head_message(repo: &Repo) -> Result<Option<String>> {
    let local = repo.local();
    let head = local.head()?;
    let Some(id) = head.id() else {
        return Ok(None);
    };
    let id = id.detach();
    Ok(Some(full_message(&local, &id, &mut Vec::new())?))
}

/// Full decoded messages of the given commits, same order (Gitnuro's log filter matches the full message of loaded
/// commits, 08 §2.3). The GUI calls it in chunks of <= 5 000 ids for the loaded rows and caches the result. Cancel is
/// checked every 256 commits. Unknown ids => empty string.
pub fn messages(repo: &Repo, ids: &[ObjectId], cancel: &CancelToken) -> Result<Vec<Box<str>>> {
    let local = repo.local();
    let mut buf = Vec::new();
    let mut out = Vec::with_capacity(ids.len());
    for (n, id) in ids.iter().enumerate() {
        if n.is_multiple_of(CANCEL_CHECK_INTERVAL) {
            cancel.check()?;
        }
        let message = match full_message(&local, id, &mut buf) {
            Ok(message) => message,
            Err(GitError::NotFound { .. }) => String::new(),
            Err(err) => return Err(err),
        };
        out.push(message.into_boxed_str());
    }
    Ok(out)
}

/// Single commit info (jump-to-commit, tooltips), with the mailmap applied like the log does by default.
///
/// Errors: `NotFound` if `id` is not a commit in the repository.
pub fn commit_info(repo: &Repo, id: ObjectId) -> Result<CommitInfo> {
    let local = repo.local();
    let mut decoder = CommitDecoder::new(Some(local.open_mailmap()));
    Ok(decoder.decode(&local, id, false)?.0)
}

// ───────────────────────────── tips ─────────────────────────────

/// The walk's tips in the order HEAD, local branches, remote branches, tags, stashes (newest first), and the set of
/// stash commits among them.
fn collect_tips(
    repo: &gix::Repository,
    tips: &LogTips,
    cancel: &CancelToken,
) -> Result<(Vec<ObjectId>, HashSet<ObjectId>)> {
    let (remotes, tags, stashes) = match tips {
        LogTips::Explicit(ids) => {
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                if !is_commit(repo, id)? {
                    return Err(not_found_commit(id));
                }
                if !out.contains(id) {
                    out.push(*id);
                }
            }
            return Ok((out, HashSet::new()));
        }
        LogTips::AllRefs {
            remotes,
            tags,
            stashes,
        } => (*remotes, *tags, *stashes),
    };

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |id: ObjectId, out: &mut Vec<ObjectId>| {
        if seen.insert(id) {
            out.push(id);
        }
    };
    if let Some(head) = repo.head()?.id() {
        let head = head.detach();
        if is_commit(repo, &head)? {
            add(head, &mut out);
        }
    }
    let platform = repo.references()?;
    let mut namespaces = vec![platform.local_branches()?];
    if remotes {
        namespaces.push(platform.remote_branches()?);
    }
    if tags {
        namespaces.push(platform.tags()?);
    }
    let mut read = 0usize;
    for refs in namespaces {
        for reference in refs {
            if read.is_multiple_of(CANCEL_CHECK_INTERVAL) {
                cancel.check()?;
            }
            read += 1;
            // Like Git, a broken or dangling ref is skipped rather than failing the whole log.
            let mut reference = match reference {
                Ok(reference) => reference,
                Err(err) => {
                    tracing::warn!(%err, "Ignoring an unreadable reference in the log walk");
                    continue;
                }
            };
            let Ok(id) = reference.peel_to_id() else {
                tracing::debug!(name = %reference.name().as_bstr(), "Ignoring a dangling reference");
                continue;
            };
            let id = id.detach();
            if is_commit(repo, &id)? {
                add(id, &mut out);
            }
        }
    }

    let mut stash_ids = HashSet::new();
    if stashes {
        for id in stash_commits(repo)? {
            if is_commit(repo, &id)? {
                stash_ids.insert(id);
                add(id, &mut out);
            }
        }
    }
    Ok((out, stash_ids))
}

/// Every stash in the `refs/stash` reflog, newest first (plus the ref's own target if the reflog lacks it).
fn stash_commits(repo: &gix::Repository) -> Result<Vec<ObjectId>> {
    let Some(stash) = repo.try_find_reference("refs/stash")? else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    let mut log = stash.log_iter();
    let lines = log
        .all()
        .map_err(|err| GitError::io(repo.common_dir().join("logs/refs/stash"), err))?;
    if let Some(lines) = lines {
        for line in lines {
            match line {
                Ok(line) => {
                    let id = line.new_oid();
                    if !id.is_null() {
                        ids.push(id);
                    }
                }
                Err(err) => tracing::warn!(%err, "Ignoring an unreadable line of the stash reflog"),
            }
        }
    }
    ids.reverse();
    if let Some(target) = stash.try_id().map(|id| id.detach())
        && !ids.contains(&target)
    {
        ids.insert(0, target);
    }
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
    Ok(ids)
}

fn is_commit(repo: &gix::Repository, id: &ObjectId) -> Result<bool> {
    match repo.try_find_header(*id)? {
        Some(header) => Ok(header.kind() == gix::object::Kind::Commit),
        None => Ok(false),
    }
}

// ───────────────────────────── objects ─────────────────────────────

/// The object access of the topological walk: the repository's object database, with the parents of shallow
/// boundary commits cut off (as Git's shallow grafts do), so the walk never asks for commits a shallow clone lacks.
#[derive(Clone)]
struct WalkObjects {
    odb: gix::OdbHandle,
    shallow: Arc<HashSet<ObjectId>>,
}

impl gix::objs::Find for WalkObjects {
    fn try_find<'a>(
        &self,
        id: &gix::oid,
        buffer: &'a mut Vec<u8>,
    ) -> gix::Result<Option<gix::objs::Data<'a>>> {
        if self.shallow.is_empty() || !self.shallow.contains(id) {
            return self.odb.try_find(id, buffer);
        }
        let (kind, object_hash, stripped) = match self.odb.try_find(id, buffer)? {
            None => return Ok(None),
            Some(data) => (
                data.kind,
                data.object_hash,
                (data.kind == gix::object::Kind::Commit).then(|| without_parents(data.data)),
            ),
        };
        match stripped {
            Some(stripped) => {
                *buffer = stripped;
                Ok(Some(gix::objs::Data::new(buffer, kind, object_hash)))
            }
            // Only commits are listed as shallow; anything else is returned as stored.
            None => self.odb.try_find(id, buffer),
        }
    }
}

/// `commit` without its `parent` header lines.
fn without_parents(commit: &[u8]) -> Vec<u8> {
    let header_end = commit.find(b"\n\n").map_or(commit.len(), |pos| pos + 1);
    let (header, rest) = commit.split_at(header_end);
    let mut out = Vec::with_capacity(commit.len());
    for line in header.split_inclusive(|&b| b == b'\n') {
        if !line.starts_with(b"parent ") {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(rest);
    out
}

fn not_found_commit(id: &ObjectId) -> GitError {
    GitError::NotFound {
        what: "commit",
        name: id.to_string(),
    }
}

/// The commit `id` as token iterator; `NotFound` if it is missing or not a commit.
fn find_commit_iter<'a>(
    repo: &gix::Repository,
    id: &ObjectId,
    buf: &'a mut Vec<u8>,
) -> Result<gix::objs::CommitRefIter<'a>> {
    repo.objects
        .try_find(id, buf)?
        .and_then(|data| data.try_into_commit_iter())
        .ok_or_else(|| not_found_commit(id))
}

/// The full message of commit `id`, decoded with its `encoding` header, trailing whitespace removed.
fn full_message(repo: &gix::Repository, id: &ObjectId, buf: &mut Vec<u8>) -> Result<String> {
    let mut encoding = None;
    let mut message: &BStr = b"".as_bstr();
    for token in find_commit_iter(repo, id, buf)? {
        match token? {
            Token::Encoding(label) => encoding = Some(label),
            Token::Message(text) => message = text,
            Token::Tree { .. }
            | Token::Parent { .. }
            | Token::Author { .. }
            | Token::Committer { .. }
            | Token::ExtraHeader(_) => {}
        }
    }
    Ok(decode_text(message, encoding).trim_end().to_owned())
}

/// `bytes` decoded with the commit's `encoding` header (Git's `i18n.commitEncoding`), else as lossy UTF-8.
fn decode_text<'a>(bytes: &'a [u8], encoding: Option<&BStr>) -> Cow<'a, str> {
    if let Some(encoding) = encoding
        .and_then(|label| encoding_rs::Encoding::for_label(label.trim()))
        .filter(|encoding| *encoding != encoding_rs::UTF_8)
    {
        return encoding.decode_without_bom_handling(bytes).0;
    }
    String::from_utf8_lossy(bytes)
}

/// The first non-blank line of `message`, trimmed, and whether anything but whitespace follows it.
fn summary_line(message: &[u8]) -> (&[u8], bool) {
    let mut lines = message.split(|&b| b == b'\n');
    let summary = lines
        .by_ref()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let has_body = lines.any(|line| !line.trim().is_empty());
    (summary, has_body)
}

/// Decodes commits into [`CommitInfo`], interning persons (after the mailmap) so that every distinct raw
/// name/email pair is resolved and allocated once per walker.
struct CommitDecoder {
    buf: Vec<u8>,
    key: Vec<u8>,
    people: HashMap<Box<[u8]>, Arc<Person>>,
    mailmap: Option<gix::mailmap::Snapshot>,
}

impl CommitDecoder {
    fn new(mailmap: Option<gix::mailmap::Snapshot>) -> Self {
        CommitDecoder {
            buf: Vec::new(),
            key: Vec::new(),
            people: HashMap::new(),
            mailmap,
        }
    }

    /// `id` as [`CommitInfo`], plus its full decoded message if `with_message`.
    fn decode(
        &mut self,
        repo: &gix::Repository,
        id: ObjectId,
        with_message: bool,
    ) -> Result<(CommitInfo, Option<String>)> {
        let CommitDecoder {
            buf,
            key,
            people,
            mailmap,
        } = self;
        let mut parents = SmallVec::new();
        let mut author = None;
        let mut committer = None;
        let mut encoding = None;
        let mut message: &BStr = b"".as_bstr();
        for token in find_commit_iter(repo, &id, buf)? {
            match token? {
                Token::Parent { id } => parents.push(id),
                Token::Author { signature } => author = Some(signature.trim()),
                Token::Committer { signature } => committer = Some(signature.trim()),
                Token::Encoding(label) => encoding = Some(label),
                Token::Message(text) => message = text,
                Token::Tree { .. } | Token::ExtraHeader(_) => {}
            }
        }
        let mut signature = |raw: Option<gix::actor::SignatureRef<'_>>| {
            let raw = raw.unwrap_or_default();
            let time = raw.time().unwrap_or_default();
            Signature {
                person: intern_person(people, key, mailmap.as_ref(), raw),
                time: GitTime {
                    seconds: time.seconds,
                    offset_minutes: time.offset / 60,
                },
            }
        };
        let author = signature(author);
        let committer = signature(committer);
        let (summary, has_body) = summary_line(message);
        let info = CommitInfo {
            id,
            parents,
            author,
            committer,
            summary: decode_text(summary, encoding).into(),
            has_body,
        };
        let full = with_message.then(|| decode_text(message, encoding).trim_end().to_owned());
        Ok((info, full))
    }
}

/// The interned (and mailmapped) person for the raw signature `raw`.
fn intern_person(
    people: &mut HashMap<Box<[u8]>, Arc<Person>>,
    key: &mut Vec<u8>,
    mailmap: Option<&gix::mailmap::Snapshot>,
    raw: gix::actor::SignatureRef<'_>,
) -> Arc<Person> {
    key.clear();
    key.extend_from_slice(raw.name);
    key.push(0);
    key.extend_from_slice(raw.email);
    if let Some(person) = people.get(key.as_slice()) {
        return person.clone();
    }
    let mapped = mailmap.and_then(|mailmap| mailmap.try_resolve_ref(raw));
    let person = Arc::new(Person {
        name: mapped
            .as_ref()
            .and_then(|m| m.name)
            .unwrap_or(raw.name)
            .to_owned(),
        email: mapped
            .as_ref()
            .and_then(|m| m.email)
            .unwrap_or(raw.email)
            .to_owned(),
    });
    people.insert(key.as_slice().into(), person.clone());
    person
}

// ───────────────────────────── search ─────────────────────────────

/// A prepared [`SearchQuery`].
struct Matcher {
    /// The text, lower-cased unless case-sensitive.
    needle: String,
    /// The text as lower-case hex prefix, if hash search is on and the text is hex.
    hex: Option<String>,
    case_sensitive: bool,
    in_message: bool,
    in_author: bool,
    in_committer: bool,
}

impl Matcher {
    fn new(query: &SearchQuery) -> Self {
        let text = query.text.trim();
        let hex =
            (query.in_hash && !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| text.to_ascii_lowercase());
        Matcher {
            needle: if query.case_sensitive {
                text.to_owned()
            } else {
                text.to_lowercase()
            },
            hex,
            case_sensitive: query.case_sensitive,
            in_message: query.in_message,
            in_author: query.in_author,
            in_committer: query.in_committer,
        }
    }

    /// Nothing can match.
    fn is_empty(&self) -> bool {
        self.needle.is_empty() || (self.hex.is_none() && !self.needs_commit())
    }

    /// Matching needs the decoded commit (not just its id).
    fn needs_commit(&self) -> bool {
        self.in_message || self.in_author || self.in_committer
    }

    fn matches_hash(&self, id: &ObjectId) -> bool {
        self.hex.as_deref().is_some_and(|hex| {
            // Formatted on the stack: this runs for every commit of a history-wide search.
            let mut buf = gix::hash::Kind::hex_buf();
            id.hex_to_buf(&mut buf).starts_with(hex)
        })
    }

    fn matches_commit(&self, info: &CommitInfo, message: Option<&str>) -> bool {
        let person = |person: &Person| {
            self.contains(&person.name.to_str_lossy())
                || self.contains(&person.email.to_str_lossy())
        };
        (self.in_message && message.is_some_and(|message| self.contains(message)))
            || (self.in_author && person(&info.author.person))
            || (self.in_committer && person(&info.committer.person))
    }

    fn contains(&self, haystack: &str) -> bool {
        if self.case_sensitive {
            haystack.contains(&self.needle)
        } else {
            haystack.to_lowercase().contains(&self.needle)
        }
    }
}

// ───────────────────────────── detached compute ─────────────────────────────

/// Run `work` on a detached thread and wait for its result or for `cancel`, whichever comes first (§1.4). On cancel
/// this returns `Err(Cancelled)` at once; the thread finishes on its own and its result is dropped.
fn run_detached<T: Send + 'static>(
    name: &str,
    cancel: &CancelToken,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    enum Message<T> {
        Done(Result<T>),
        Cancelled,
    }
    /// Turns the cancel token's wake-up into a channel message, so waiting needs no polling.
    struct CancelWake<T>(mpsc::Sender<Message<T>>);
    impl<T: Send + 'static> Wake for CancelWake<T> {
        fn wake(self: Arc<Self>) {
            self.0.send(Message::Cancelled).ok();
        }
    }

    cancel.check()?;
    let (tx, rx) = mpsc::channel();
    let worker_tx = tx.clone();
    let thread_name = name.to_owned();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| {
                    Err(GitError::Other(anyhow::anyhow!(
                        "The {thread_name} thread panicked"
                    )))
                });
            // The receiver is gone if the caller was cancelled; the result is dropped then.
            worker_tx.send(Message::Done(result)).ok();
        })
        .map_err(|err| {
            GitError::Other(anyhow::Error::new(err).context("Failed to start a background thread"))
        })?;

    let waker = Waker::from(Arc::new(CancelWake(tx)));
    let mut cancelled = std::pin::pin!(cancel.cancelled());
    if cancelled
        .as_mut()
        .poll(&mut Context::from_waker(&waker))
        .is_ready()
    {
        return Err(GitError::Cancelled);
    }
    match rx.recv() {
        Ok(Message::Done(result)) => result,
        Ok(Message::Cancelled) => Err(GitError::Cancelled),
        // Unreachable while `waker` holds a sender, but never hang or panic on it.
        Err(_) => Err(GitError::Other(anyhow::anyhow!(
            "The background thread ended without a result"
        ))),
    }
}
