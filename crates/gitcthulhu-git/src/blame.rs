//! Blame (WP-B9): attribute every line of a file, at a commit or in the worktree, to the commit that introduced it.
//!
//! The engine is `gix::blame::file`. It has no interrupt flag, so it runs on a detached compute thread; the calling
//! function waits for the result *or* the cancel token and returns [`GitError::Cancelled`] at once (the thread
//! finishes on its own and its result is dropped, except that it still fills the cache). If gix fails, `git blame
//! --porcelain` is tried and parsed.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use bstr::{BString, ByteSlice as _};
use gix::ObjectId;
use lru::LruCache;

use crate::{
    BlameChunk, BlameCommit, BlameOptions, BlameResult, BlameRev, CancelToken, GitError, OpContext,
    RelaPath, Repo, Result,
    cli::{CommandKind, GitCommand},
    internal::errors::gix_error,
    log::CommitDecoder,
};

/// How many results of `BlameRev::Commit` are remembered.
const CACHE_ENTRIES: usize = 8;

/// How often the waiting caller looks at the cancel token.
const CANCEL_POLL: Duration = Duration::from_millis(20);

/// NUL bytes in the first this many bytes make a file binary (Git's own heuristic).
const BINARY_SNIFF_LEN: usize = 8000;

/// What decides whether two blames are the same.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    common_dir: std::path::PathBuf,
    commit: ObjectId,
    path: RelaPath,
    follow_renames: bool,
    since: Option<(i64, i32)>,
    line_range: Option<Range<u32>>,
}

type Cache = Mutex<LruCache<CacheKey, Arc<BlameResult>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(CACHE_ENTRIES).expect("the cache size is not zero"),
        ))
    })
}

static COMPUTATIONS: AtomicU64 = AtomicU64::new(0);

/// How many blames were computed (not served from the cache) in this process. For tests.
#[doc(hidden)]
pub fn computations() -> u64 {
    COMPUTATIONS.load(Ordering::SeqCst)
}

/// Attribute every line of `path` at `rev` to a commit.
///
/// - `BlameRev::Commit(id)`: the file as of that commit. Results are cached (LRU of 8).
/// - `BlameRev::Worktree`: the file in the worktree, in the form Git would store it. Lines that are not committed
///   yet become chunks with `commit: None`. A path that is not in `HEAD` but in the worktree has all lines
///   uncommitted.
///
/// Errors: `NotFound` (no such file at that revision, or in the worktree), `Unsupported` (binary file),
/// `BareRepository` (`Worktree` without a work tree), `Cancelled`, `Io`, `Gix`.
pub fn blame(
    repo: &Repo,
    path: &RelaPath,
    rev: BlameRev,
    opts: &BlameOptions,
    cancel: &CancelToken,
) -> Result<BlameResult> {
    cancel.check()?;
    let key = match rev {
        BlameRev::Commit(commit) => Some(CacheKey {
            common_dir: repo.common_dir().to_owned(),
            commit,
            path: path.clone(),
            follow_renames: opts.follow_renames,
            since: opts
                .since
                .map(|since| (since.seconds, since.offset_minutes)),
            line_range: opts.line_range.clone(),
        }),
        BlameRev::Worktree => None,
    };
    if let Some(key) = &key
        && let Some(hit) = lock(cache()).get(key)
    {
        return Ok(BlameResult::clone(hit));
    }

    let (tx, rx) = mpsc::channel();
    {
        let (repo, path, opts, cancel) = (repo.clone(), path.clone(), opts.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("gitcthulhu-blame".into())
            .spawn(move || {
                let result = compute(&repo, &path, rev, &opts, &cancel);
                if let (Some(key), Ok(result)) = (key, &result) {
                    lock(cache()).put(key, Arc::new(result.clone()));
                }
                // The receiver is gone if the caller was cancelled.
                tx.send(result).ok();
            })
            .map_err(|err| {
                GitError::Other(anyhow::Error::new(err).context("Failed to start the blame thread"))
            })?;
    }
    loop {
        match rx.recv_timeout(CANCEL_POLL) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => cancel.check()?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(GitError::Other(anyhow::anyhow!(
                    "The blame computation stopped unexpectedly"
                )));
            }
        }
    }
}

fn lock(cache: &Cache) -> std::sync::MutexGuard<'_, LruCache<CacheKey, Arc<BlameResult>>> {
    // A panic while holding the lock cannot leave the cache in a state worse than "missing entries".
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A run of lines attributed to one commit, in the raw form both engines produce.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawEntry {
    /// 0-based first line in the blamed file.
    start: u32,
    /// 0-based first line in the source file.
    source_start: u32,
    /// Number of lines.
    len: u32,
    /// `None` = not committed yet.
    commit: Option<ObjectId>,
    /// The name of the source file if it differs from the blamed path.
    source_name: Option<BString>,
}

/// The file content and who wrote which lines.
struct Attribution {
    content: Vec<u8>,
    entries: Vec<RawEntry>,
}

fn other(err: impl std::fmt::Display) -> GitError {
    GitError::Other(anyhow::anyhow!("{err}"))
}

fn not_found(path: &RelaPath) -> GitError {
    GitError::NotFound {
        what: "file",
        name: path.to_string(),
    }
}

fn compute(
    repo: &Repo,
    path: &RelaPath,
    rev: BlameRev,
    opts: &BlameOptions,
    cancel: &CancelToken,
) -> Result<BlameResult> {
    COMPUTATIONS.fetch_add(1, Ordering::SeqCst);
    let local = repo.local();
    let attribution = match gix_blame(&local, repo, path, rev, opts) {
        Ok(attribution) => attribution,
        Err(
            err @ (GitError::NotFound { .. }
            | GitError::Unsupported { .. }
            | GitError::BareRepository),
        ) => {
            return Err(err);
        }
        Err(err) => {
            tracing::debug!(%err, "gix blame failed, trying `git blame`");
            match cli_blame(repo, path, rev, opts, cancel) {
                Ok(attribution) => attribution,
                // The CLI could not do better: report what gix said.
                Err(_) => return Err(err),
            }
        }
    };
    build_result(&local, path, rev, attribution)
}

/// The file content as of `rev`, and what `gix::blame::file` says about it.
fn gix_blame(
    local: &gix::Repository,
    repo: &Repo,
    path: &RelaPath,
    rev: BlameRev,
    opts: &BlameOptions,
) -> Result<Attribution> {
    // `start` is `None` if the file has no history to blame.
    let (start, content) = match rev {
        BlameRev::Commit(id) => {
            let content = blob_at(local, id, path.as_ref())?.ok_or_else(|| not_found(path))?;
            (Some(gix::blame::Start::Commit(id)), content)
        }
        BlameRev::Worktree => {
            let content = worktree_content(local, repo, path)?;
            let head = local.head_id().ok().map(gix::Id::detach);
            match head {
                Some(head) if blob_at(local, head, path.as_ref())?.is_some() => (
                    Some(gix::blame::Start::Contents {
                        first_suspect: head,
                        contents: content.clone().into(),
                    }),
                    content,
                ),
                // Not in HEAD (or no HEAD yet): every line is new.
                _ => (None, content),
            }
        }
    };
    if is_binary(&content) {
        return Err(GitError::Unsupported {
            what: "blame of binary file",
        });
    }
    let total = count_lines(&content);
    let wanted = clamp_range(opts.line_range.as_ref(), total);
    if wanted.is_empty() {
        return Ok(Attribution {
            content,
            entries: Vec::new(),
        });
    }
    let Some(start) = start else {
        return Ok(Attribution {
            content,
            entries: vec![RawEntry {
                start: wanted.start,
                source_start: wanted.start,
                len: wanted.end - wanted.start,
                commit: None,
                source_name: None,
            }],
        });
    };

    let ranges = if opts.line_range.is_some() {
        gix::blame::BlameRanges::PartialFile(vec![wanted])
    } else {
        gix::blame::BlameRanges::default()
    };
    let options = gix::blame::Options {
        diff_algorithm: local.diff_algorithm().map_err(gix_error)?,
        ranges,
        since: opts
            .since
            .map(|since| gix::date::Time::new(since.seconds, since.offset_minutes * 60)),
        rewrites: opts.follow_renames.then(gix::diff::Rewrites::default),
        debug_track_path: false,
    };
    let shallow: HashSet<ObjectId> = local
        .shallow_commits()
        .map_err(gix_error)?
        .map(|commits| commits.iter().copied().collect())
        .unwrap_or_default();
    // The commit-graph would hand out the parents a shallow boundary commit must not have.
    let graph = if shallow.is_empty() {
        local.commit_graph_if_enabled().map_err(gix_error)?
    } else {
        None
    };
    let mut resource_cache = local
        .diff_resource_cache_for_tree_diff()
        .map_err(gix_error)?;
    let outcome = gix::blame::file(
        ShallowAware {
            odb: &local.objects,
            shallow,
        },
        start,
        graph,
        &mut resource_cache,
        path.as_bstr(),
        options,
    )
    .map_err(gix_error)?;
    let entries = outcome
        .entries
        .into_iter()
        .map(|entry| RawEntry {
            start: entry.start_in_blamed_file,
            source_start: entry.start_in_source_file,
            len: entry.len.get(),
            commit: (!entry.commit_id.is_null()).then_some(entry.commit_id),
            source_name: entry.source_file_name.filter(|name| name != path),
        })
        .collect();
    Ok(Attribution { content, entries })
}

/// An object database view in which the boundary commits of a shallow repository have no parents, like Git's own
/// view of them. Without it `gix::blame::file` fails on the first commit whose parent is not there.
struct ShallowAware<'a, T> {
    odb: &'a T,
    shallow: HashSet<ObjectId>,
}

impl<T: gix::objs::Find> gix::objs::Find for ShallowAware<'_, T> {
    fn try_find<'b>(
        &self,
        id: &gix::oid,
        buffer: &'b mut Vec<u8>,
    ) -> gix::Result<Option<gix::objs::Data<'b>>> {
        if !self.shallow.contains(id) {
            return self.odb.try_find(id, buffer);
        }
        let (kind, hash_kind, data) = match self.odb.try_find(id, &mut *buffer)? {
            None => return Ok(None),
            Some(found) => (found.kind, found.object_hash, found.data.to_vec()),
        };
        buffer.clear();
        if kind == gix::object::Kind::Commit {
            buffer.extend(without_parents(&data));
        } else {
            buffer.extend_from_slice(&data);
        }
        Ok(Some(gix::objs::Data::new(buffer, kind, hash_kind)))
    }
}

impl<T: gix::objs::FindHeader> gix::objs::FindHeader for ShallowAware<'_, T> {
    fn try_header(&self, id: &gix::oid) -> gix::Result<Option<gix::objs::Header>> {
        self.odb.try_header(id)
    }
}

/// A raw commit without its `parent` header lines.
fn without_parents(commit: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(commit.len());
    let mut in_header = true;
    for line in commit.split_inclusive(|byte| *byte == b'\n') {
        if in_header {
            if line == b"\n" {
                in_header = false;
            } else if line.starts_with(b"parent ") {
                continue;
            }
        }
        out.extend_from_slice(line);
    }
    out
}

/// The content of `path` in the tree of `commit`, `None` if it is not there or is not a file.
fn blob_at(
    local: &gix::Repository,
    commit: ObjectId,
    path: &bstr::BStr,
) -> Result<Option<Vec<u8>>> {
    let commit = local.find_commit(commit).map_err(|_| GitError::NotFound {
        what: "commit",
        name: commit.to_string(),
    })?;
    let tree = commit.tree().map_err(gix_error)?;
    let Some(entry) = tree
        .lookup_entry(path.split(|byte| *byte == b'/'))
        .map_err(gix_error)?
    else {
        return Ok(None);
    };
    if !(entry.mode().is_blob() || entry.mode().is_link()) {
        return Ok(None);
    }
    let blob = local.find_blob(entry.object_id()).map_err(gix_error)?;
    Ok(Some(blob.data.clone()))
}

/// The worktree file in the form Git stores it (clean filters and line endings applied), raw if that fails.
fn worktree_content(local: &gix::Repository, repo: &Repo, path: &RelaPath) -> Result<Vec<u8>> {
    let workdir = repo.workdir().ok_or(GitError::BareRepository)?;
    let full = workdir.join(
        gix::path::from_bstr(path.as_bstr())
            .map_err(|_| not_found(path))?
            .as_ref(),
    );
    let metadata = std::fs::symlink_metadata(&full).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => not_found(path),
        _ => GitError::io(&full, err),
    })?;
    if metadata.is_dir() {
        return Err(not_found(path));
    }
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(&full).map_err(|err| GitError::io(&full, err))?;
        return Ok(gix::path::into_bstr(target)
            .map_err(gix_error)?
            .into_owned()
            .into());
    }
    let raw = std::fs::read(&full).map_err(|err| GitError::io(&full, err))?;
    let filtered = (|| -> Option<Vec<u8>> {
        use std::io::Read as _;
        let (mut pipeline, index) = local.filter_pipeline(None).ok()?;
        let rela = gix::path::from_bstr(path.as_bstr()).ok()?;
        let mut outcome = pipeline
            .convert_to_git(raw.as_slice(), &rela, &index)
            .ok()?;
        let mut converted = Vec::with_capacity(raw.len());
        outcome.read_to_end(&mut converted).ok()?;
        Some(converted)
    })();
    Ok(filtered.unwrap_or(raw))
}

/// `path` as a command-line argument.
fn native_path(path: &RelaPath) -> std::ffi::OsString {
    match gix::path::from_bstr(path.as_bstr()) {
        Ok(path) => path.as_os_str().to_owned(),
        Err(_) => path.to_str_lossy().as_ref().into(),
    }
}

fn is_binary(content: &[u8]) -> bool {
    content[..content.len().min(BINARY_SNIFF_LEN)].contains(&0)
}

/// Lines as the blame tokenizes them: `\n`-terminated, the last one possibly without terminator.
fn count_lines(content: &[u8]) -> u32 {
    let newlines = content.iter().filter(|byte| **byte == b'\n').count();
    let partial = usize::from(!content.is_empty() && !content.ends_with(b"\n"));
    (newlines + partial) as u32
}

fn clamp_range(range: Option<&Range<u32>>, total: u32) -> Range<u32> {
    match range {
        Some(range) => range.start.min(total)..range.end.min(total),
        None => 0..total,
    }
}

/// Run `git blame --porcelain` when gix could not do it.
fn cli_blame(
    repo: &Repo,
    path: &RelaPath,
    rev: BlameRev,
    opts: &BlameOptions,
    cancel: &CancelToken,
) -> Result<Attribution> {
    let mut command = GitCommand::new(repo, CommandKind::Read).args(["blame", "--porcelain"]);
    if opts.follow_renames {
        command = command.arg("-M");
    }
    if let Some(range) = &opts.line_range {
        if range.is_empty() {
            return Ok(Attribution {
                content: Vec::new(),
                entries: Vec::new(),
            });
        }
        command = command.arg(format!("-L{},{}", range.start + 1, range.end));
    }
    if let BlameRev::Commit(id) = rev {
        command = command.arg(id.to_string());
    }
    // `git blame` takes a file name, not a pathspec: the literal-pathspec rule of the other commands does not apply.
    command = command.arg("--").arg(native_path(path));
    let output = crate::cli::run(repo.runtime(), command, &OpContext::silent(cancel.clone()))?;
    if !output.success() {
        return Err(GitError::Other(anyhow::anyhow!(
            "git blame failed: {}",
            output.stderr.trim()
        )));
    }
    parse_porcelain(&output.stdout, path)
}

/// Parse the output of `git blame --porcelain`.
///
/// Every line is announced by `<sha> <line in source> <line in result> [<count>]`; the first time a commit shows up
/// its header lines follow (`filename <path>` among them), then the line itself prefixed with a TAB.
fn parse_porcelain(output: &[u8], path: &RelaPath) -> Result<Attribution> {
    let mut content = Vec::new();
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut filenames: HashMap<ObjectId, BString> = HashMap::new();
    let mut current: Option<(Option<ObjectId>, u32, u32)> = None;
    for line in output.split_inclusive(|byte| *byte == b'\n') {
        if let Some(text) = line.strip_prefix(b"\t") {
            let Some((commit, source_line, final_line)) = current.take() else {
                return Err(other("git blame printed a line without a header"));
            };
            content.extend_from_slice(text);
            if !text.ends_with(b"\n") {
                content.push(b'\n');
            }
            let source_name = commit
                .and_then(|commit| filenames.get(&commit))
                .filter(|name| *name != path)
                .cloned();
            let extend = entries.last_mut().filter(|last| {
                last.commit == commit
                    && last.source_name == source_name
                    && last.start + last.len == final_line
                    && last.source_start + last.len == source_line
            });
            match extend {
                Some(last) => last.len += 1,
                None => entries.push(RawEntry {
                    start: final_line,
                    source_start: source_line,
                    len: 1,
                    commit,
                    source_name,
                }),
            }
            continue;
        }
        let line = line.trim_end_with(|c| c == '\n' || c == '\r');
        if let Some(name) = line.strip_prefix(b"filename ") {
            if let Some((Some(commit), _, _)) = current {
                filenames.insert(commit, name.into());
            }
            continue;
        }
        let mut fields = line.split(|byte| *byte == b' ');
        let (Some(sha), Some(source), Some(result)) = (fields.next(), fields.next(), fields.next())
        else {
            continue; // header lines such as `author ...`
        };
        let (Ok(id), Some(source), Some(result)) = (
            ObjectId::from_hex(sha),
            std::str::from_utf8(source)
                .ok()
                .and_then(|s| s.parse::<u32>().ok()),
            std::str::from_utf8(result)
                .ok()
                .and_then(|s| s.parse::<u32>().ok()),
        ) else {
            continue;
        };
        let commit = (!id.is_null()).then_some(id);
        current = Some((commit, source.saturating_sub(1), result.saturating_sub(1)));
    }
    Ok(Attribution { content, entries })
}

/// Turn the raw attribution into the public result: merge neighbours, look up the commits, decode the text.
fn build_result(
    local: &gix::Repository,
    path: &RelaPath,
    rev: BlameRev,
    attribution: Attribution,
) -> Result<BlameResult> {
    let Attribution {
        content,
        mut entries,
    } = attribution;
    entries.sort_by_key(|entry| entry.start);

    let mut decoder = CommitDecoder::new(None);
    let mut commits: Vec<BlameCommit> = Vec::new();
    let mut index_of: HashMap<(ObjectId, Option<BString>), u32> = HashMap::new();
    let mut chunks: Vec<BlameChunk> = Vec::new();
    let mut previous: Option<&RawEntry> = None;
    for entry in &entries {
        let commit = match entry.commit {
            None => None,
            Some(id) => {
                let key = (id, entry.source_name.clone());
                Some(match index_of.get(&key) {
                    Some(index) => *index,
                    None => {
                        let info = decoder.decode(local, id, false)?.0;
                        commits.push(BlameCommit {
                            id,
                            author: info.author,
                            summary: info.summary.into(),
                            source_path: entry.source_name.clone().unwrap_or_else(|| path.clone()),
                        });
                        let index = (commits.len() - 1) as u32;
                        index_of.insert(key, index);
                        index
                    }
                })
            }
        };
        let continues = previous.is_some_and(|last| {
            last.commit == entry.commit
                && last.source_name == entry.source_name
                && last.start + last.len == entry.start
                && last.source_start + last.len == entry.source_start
        });
        match (continues, chunks.last_mut()) {
            (true, Some(chunk)) => chunk.len += entry.len,
            _ => chunks.push(BlameChunk {
                start_line: entry.start,
                len: entry.len,
                commit,
                source_start_line: entry.source_start,
            }),
        }
        previous = Some(entry);
    }

    let (text, lines) = decode_lines(&content);
    Ok(BlameResult {
        path: path.clone(),
        rev,
        commits,
        chunks,
        lines,
        text,
    })
}

/// The content decoded line by line without line endings, and the byte range of every line in the text.
///
/// UTF-8 is used if the content is valid UTF-8, else the encoding is guessed.
fn decode_lines(content: &[u8]) -> (String, Vec<Range<u32>>) {
    let encoding = if std::str::from_utf8(content).is_ok() {
        encoding_rs::UTF_8
    } else {
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(
            &content[..content.len().min(64 * 1024)],
            content.len() <= 64 * 1024,
        );
        detector.guess(None, true)
    };
    let mut text = String::with_capacity(content.len());
    let mut lines = Vec::new();
    for line in content.split_inclusive(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let (decoded, _) = encoding.decode_without_bom_handling(line);
        let start = text.len() as u32;
        text.push_str(&decoded);
        lines.push(start..text.len() as u32);
    }
    (text, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const NULL: &str = "0000000000000000000000000000000000000000";

    fn id(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).expect("valid hex")
    }

    #[test]
    fn fallback_cli_parser_porcelain() {
        let output = format!(
            "{A} 1 1 2\nauthor X\nauthor-mail <x@example.com>\nauthor-time 1\nauthor-tz +0000\n\
             summary first\nfilename old name.txt\n\tfirst line\n\
             {A} 2 2\n\tsecond line\n\
             {B} 1 3 1\nauthor Y\nsummary other\nfilename a.txt\n\tthird line\n\
             {NULL} 4 4 1\nauthor Not Committed Yet\nfilename a.txt\n\tfourth, no newline"
        );
        let attribution =
            parse_porcelain(output.as_bytes(), &"a.txt".into()).expect("the output parses");
        assert_eq!(
            attribution.content, b"first line\nsecond line\nthird line\nfourth, no newline\n",
            "content is reassembled from the tab-prefixed lines"
        );
        assert_eq!(
            attribution.entries,
            [
                RawEntry {
                    start: 0,
                    source_start: 0,
                    len: 2,
                    commit: Some(id(A)),
                    source_name: Some("old name.txt".into()),
                },
                RawEntry {
                    start: 2,
                    source_start: 0,
                    len: 1,
                    commit: Some(id(B)),
                    source_name: None,
                },
                RawEntry {
                    start: 3,
                    source_start: 3,
                    len: 1,
                    commit: None,
                    source_name: None,
                },
            ],
            "consecutive lines of one commit form one entry, renamed sources are named, the null id is uncommitted"
        );
    }

    #[test]
    fn shallow_boundary_commits_lose_their_parents() {
        let commit = format!(
            "tree {A}\nparent {A}\nparent {B}\nauthor X <x> 1 +0000\ncommitter X <x> 1 +0000\n\nparent {B} in the message\n"
        );
        let expected = format!(
            "tree {A}\nauthor X <x> 1 +0000\ncommitter X <x> 1 +0000\n\nparent {B} in the message\n"
        );
        assert_eq!(
            without_parents(commit.as_bytes()).as_bstr(),
            expected.as_bytes().as_bstr(),
            "only the parent headers go, the message stays untouched"
        );
    }

    #[test]
    fn lines_are_counted_like_the_tokenizer() {
        assert_eq!(count_lines(b""), 0, "empty file");
        assert_eq!(count_lines(b"a\nb\n"), 2, "terminated lines");
        assert_eq!(count_lines(b"a\nb"), 2, "last line without terminator");
        assert_eq!(count_lines(b"\n"), 1, "a single empty line");
    }

    #[test]
    fn decoding_strips_line_endings_and_keeps_ranges() {
        let (text, lines) = decode_lines(b"ab\r\ncd\nef");
        assert_eq!(text, "abcdef", "no line endings in the text");
        assert_eq!(lines, [0..2, 2..4, 4..6], "byte ranges per line");
    }

    #[test]
    fn decoding_guesses_legacy_encodings() {
        let (text, lines) = decode_lines(b"caf\xe9\nna\xefve\n");
        assert_eq!(lines.len(), 2, "two lines");
        assert!(
            text.contains("caf") && !text.contains('\u{fffd}'),
            "no replacement characters: {text:?}"
        );
    }

    #[test]
    fn line_ranges_are_clamped() {
        assert_eq!(clamp_range(None, 5), 0..5, "no range is everything");
        assert_eq!(clamp_range(Some(&(2..9)), 5), 2..5, "the end is clamped");
        assert!(
            clamp_range(Some(&(7..9)), 5).is_empty(),
            "past the end is empty"
        );
    }
}
