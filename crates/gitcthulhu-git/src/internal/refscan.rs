//! Listing the references of one namespace (`refs/heads/`, `refs/remotes/`, `refs/tags/`) fast, for `refs::list` and
//! the log's tips.
//!
//! gix reads loose references one file after the other. A file open costs 50-200 µs on Windows (on-access scanning
//! included), which is what makes 10k loose references take one to two seconds, and gix itself adds nothing
//! measurable on top of the file reads. So this module enumerates a namespace with one directory walk and reads the
//! loose files on a few threads at once. Packed references come from gix's cached `packed-refs` buffer; a loose
//! reference wins over a packed one of the same name, as in Git. Nothing here looks at objects.
//!
//! Repositories that read references through a namespace (`GIT_NAMESPACE`) or with `core.precomposeUnicode` use gix's
//! own iteration, which implements both.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
};

use bstr::{BString, ByteVec as _};
use gix::{
    ObjectId,
    refs::{FullName, Target},
};

use crate::{GitError, Result, internal::errors::gix_error};

/// A namespace of references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefNamespace {
    /// `refs/heads/`
    LocalBranches,
    /// `refs/remotes/`
    RemoteBranches,
    /// `refs/tags/`
    Tags,
}

impl RefNamespace {
    fn prefix(self) -> &'static str {
        match self {
            RefNamespace::LocalBranches => "refs/heads/",
            RefNamespace::RemoteBranches => "refs/remotes/",
            RefNamespace::Tags => "refs/tags/",
        }
    }
}

/// A reference as stored: its name and what it points to, without peeling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScannedRef {
    pub(crate) name: FullName,
    pub(crate) target: Target,
}

impl ScannedRef {
    /// The object the reference points to; `None` if it is symbolic and cannot be resolved.
    ///
    /// A direct reference is taken as it is. Looking it up again in gix to `peel_to_id` it would check `packed-refs`
    /// on disk every time (about 50 µs each, 0.5 s for 10k references), so only symbolic ones (rare in these
    /// namespaces) are resolved that way, which also peels annotated tags they lead to.
    pub(crate) fn resolve(&self, repo: &gix::Repository) -> Option<ObjectId> {
        match &self.target {
            Target::Object(id) => Some(*id),
            Target::Symbolic(_) => repo
                .find_reference(self.name.as_ref())
                .ok()?
                .peel_to_id()
                .ok()
                .map(gix::Id::detach),
        }
    }
}

/// Loose reference files up to this count are read on the calling thread; starting threads costs more than it saves.
const PARALLEL_MIN_FILES: usize = 128;
/// Each reading thread gets at least this many files.
const FILES_PER_THREAD: usize = 64;
/// More threads do not help: the file system serialises opens beyond that.
const MAX_THREADS: usize = 8;

/// Every reference of `namespace`, sorted by name.
///
/// An item is an error for a reference that cannot be read or parsed (the caller decides whether to skip it); an
/// error of the whole result means the namespace could not be enumerated or `packed-refs` could not be read.
pub(crate) fn scan(
    repo: &gix::Repository,
    namespace: RefNamespace,
) -> Result<Vec<Result<ScannedRef>>> {
    if repo.refs.namespace.is_some() || repo.refs.precompose_unicode {
        return scan_with_gix(repo, namespace);
    }
    let prefix = namespace.prefix();

    // Loose first, packed second: `git pack-refs` writes `packed-refs` before it deletes the loose files, so a
    // reference that moves from loose to packed while we read is found in one of the two.
    let mut paths = Vec::new();
    collect_loose_paths(&repo.common_dir().join(prefix), prefix, &mut paths)?;
    if repo.git_dir() != repo.common_dir() {
        // A linked worktree may hold references of its own (gix and Git read them first).
        let mut own = Vec::new();
        collect_loose_paths(&repo.git_dir().join(prefix), prefix, &mut own)?;
        own.sort_by(|a, b| a.1.cmp(&b.1));
        paths.retain(|(_, name)| own.binary_search_by(|(_, other)| other.cmp(name)).is_err());
        paths.extend(own);
    }
    paths.sort_by(|a, b| a.1.cmp(&b.1));
    let loose = read_loose(paths, repo.object_hash());

    let mut packed = Vec::new();
    if let Some(buffer) = repo.refs.cached_packed_buffer().map_err(gix_error)? {
        for reference in buffer.iter_prefixed(prefix.into()).map_err(gix_error)? {
            packed.push(reference.map_err(gix_error).map(|reference| ScannedRef {
                name: reference.name.to_owned(),
                target: Target::Object(reference.target()),
            }));
        }
    }
    Ok(merge(loose, packed))
}

/// The namespace through gix's iteration (namespaces, precomposed unicode).
fn scan_with_gix(
    repo: &gix::Repository,
    namespace: RefNamespace,
) -> Result<Vec<Result<ScannedRef>>> {
    let platform = repo.references().map_err(gix_error)?;
    let iter = match namespace {
        RefNamespace::LocalBranches => platform.local_branches(),
        RefNamespace::RemoteBranches => platform.remote_branches(),
        RefNamespace::Tags => platform.tags(),
    }
    .map_err(gix_error)?;
    Ok(iter
        .map(|reference| {
            reference
                .map(|reference| ScannedRef {
                    name: reference.name().to_owned(),
                    target: reference.target().into_owned(),
                })
                .map_err(gix_error)
        })
        .collect())
}

/// Every file below `dir` whose path relative to the common dir is a valid reference name, as `(path, name)`.
/// A missing directory has no references.
fn collect_loose_paths(dir: &Path, prefix: &str, out: &mut Vec<(PathBuf, FullName)>) -> Result<()> {
    let mut pending = vec![(dir.to_owned(), BString::from(prefix))];
    while let Some((dir, name_prefix)) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if is_gone(&err) => continue,
            Err(err) => return Err(GitError::io(dir, err)),
        };
        for entry in entries {
            let entry = entry.map_err(|err| GitError::io(&dir, err))?;
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let file_name = entry.file_name();
            // Names are bytes on Unix (as in gix); on Windows a name that is no UTF-8 cannot be a reference.
            let Ok(file_name) = gix::path::os_str_into_bstr(&file_name) else {
                continue;
            };
            let mut name = name_prefix.clone();
            name.push_str(file_name);
            if file_type.is_dir() {
                name.push(b'/');
                pending.push((entry.path(), name));
            } else if file_type.is_file() {
                // Lock files and other names Git would refuse are not references (gix skips them too).
                if let Ok(name) = FullName::try_from(name) {
                    out.push((entry.path(), name));
                }
            }
        }
    }
    Ok(())
}

/// A loose reference file: its name and what it holds, or why it could not be read.
type Loose = (FullName, Result<Target>);

/// Read and parse the loose files, in order; files that vanished meanwhile are dropped.
fn read_loose(paths: Vec<(PathBuf, FullName)>, hash: gix::hash::Kind) -> Vec<Loose> {
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(MAX_THREADS)
        .min(paths.len() / FILES_PER_THREAD)
        .max(1);
    if paths.len() < PARALLEL_MIN_FILES || threads == 1 {
        return read_chunk(paths, hash);
    }
    let chunk_len = paths.len().div_ceil(threads);
    let mut chunks = Vec::with_capacity(threads);
    let mut rest = paths;
    while rest.len() > chunk_len {
        let tail = rest.split_off(chunk_len);
        chunks.push(rest);
        rest = tail;
    }
    chunks.push(rest);
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                // The names, to report every file of a chunk whose thread died.
                let names: Vec<FullName> = chunk.iter().map(|(_, name)| name.clone()).collect();
                (names, scope.spawn(move || read_chunk(chunk, hash)))
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|(names, handle)| {
                handle.join().unwrap_or_else(|_| {
                    names
                        .into_iter()
                        .map(|name| {
                            let err = GitError::Other(anyhow::anyhow!(
                                "The thread reading the loose reference {name} panicked"
                            ));
                            (name, Err(err))
                        })
                        .collect()
                })
            })
            .collect()
    })
}

fn read_chunk(paths: Vec<(PathBuf, FullName)>, hash: gix::hash::Kind) -> Vec<Loose> {
    let mut buf = Vec::with_capacity(128);
    paths
        .into_iter()
        .filter_map(|(path, name)| read_one(&path, name, hash, &mut buf))
        .collect()
}

/// `None` if the file is gone (deleted or packed since the directory was listed).
fn read_one(
    path: &Path,
    name: FullName,
    hash: gix::hash::Kind,
    buf: &mut Vec<u8>,
) -> Option<Loose> {
    buf.clear();
    if let Err(err) = std::fs::File::open(path).and_then(|mut file| file.read_to_end(buf)) {
        return (!is_gone(&err)).then(|| (name, Err(GitError::io(path, err))));
    }
    let target = gix::refs::file::loose::Reference::try_from_path(name.clone(), buf, hash)
        .map(|reference| reference.target)
        .map_err(gix_error);
    Some((name, target))
}

fn is_gone(err: &std::io::Error) -> bool {
    // A directory that turned into a file (or the reverse) between listing and reading is gone as well.
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// Merge the name-sorted loose and packed lists; a loose reference, even an unreadable one, hides the packed one of
/// the same name (as in gix and Git). Unreadable packed lines are kept where they are.
fn merge(loose: Vec<Loose>, packed: Vec<Result<ScannedRef>>) -> Vec<Result<ScannedRef>> {
    let mut out = Vec::with_capacity(loose.len() + packed.len());
    let mut packed = packed.into_iter().peekable();
    for (name, target) in loose {
        while let Some(next) = packed.peek() {
            match next {
                Ok(other) if other.name > name => break,
                Ok(other) if other.name == name => {
                    packed.next();
                }
                _ => out.extend(packed.next()),
            }
        }
        out.push(target.map(|target| ScannedRef { name, target }));
    }
    out.extend(packed);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::content::fixture;

    /// The references of `namespace` as gix lists them, as `(name, target)` sorted by name.
    fn with_gix(repo: &gix::Repository, namespace: RefNamespace) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = scan_with_gix(repo, namespace)
            .expect("gix lists the namespace")
            .into_iter()
            .map(|item| {
                let item = item.expect("every reference parses");
                (item.name.to_string(), format!("{:?}", item.target))
            })
            .collect();
        out.sort();
        out
    }

    fn with_scan(repo: &gix::Repository, namespace: RefNamespace) -> Vec<(String, String)> {
        scan(repo, namespace)
            .expect("the namespace is listed")
            .into_iter()
            .map(|item| {
                let item = item.expect("every reference parses");
                (item.name.to_string(), format!("{:?}", item.target))
            })
            .collect()
    }

    #[test]
    fn scan_equals_gix_for_loose_packed_overridden_nested_and_symbolic_refs() {
        let repo = fixture::repo();
        repo.write("f", "1\n");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-q", "-m", "one"]);
        let one = repo.git(&["rev-parse", "HEAD"]);
        repo.write("f", "2\n");
        repo.git(&["commit", "-q", "-am", "two"]);
        for name in ["packed", "a-c", "a/b", "deep/er/est", "over"] {
            repo.git(&["branch", name, &one]);
        }
        repo.git(&["tag", "light", &one]);
        repo.git(&["tag", "-a", "-m", "annotated", "annotated"]);
        repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        repo.git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ]);
        repo.git(&["pack-refs", "--all"]);
        // Loose again after packing: newer than the packed value, which must lose.
        repo.git(&["update-ref", "refs/heads/over", "HEAD"]);
        repo.git(&["branch", "loose-only"]);
        repo.git(&["tag", "loose-tag"]);
        // A lock file and an invalid name are not references.
        let heads = repo.path.join(".git/refs/heads");
        std::fs::write(heads.join("locked.lock"), format!("{one}\n")).expect("writable");
        std::fs::write(heads.join("bad..name"), format!("{one}\n")).expect("writable");

        let gix_repo = gix::open(&repo.path).expect("the repository opens");
        for namespace in [
            RefNamespace::LocalBranches,
            RefNamespace::RemoteBranches,
            RefNamespace::Tags,
        ] {
            let scanned = with_scan(&gix_repo, namespace);
            let mut sorted = scanned.clone();
            sorted.sort();
            assert_eq!(scanned, sorted, "{namespace:?} is sorted by name");
            assert_eq!(
                scanned,
                with_gix(&gix_repo, namespace),
                "{namespace:?} equals gix"
            );
        }
        let heads = with_scan(&gix_repo, RefNamespace::LocalBranches);
        let head = repo.git(&["rev-parse", "HEAD"]);
        assert!(
            heads
                .iter()
                .any(|(name, target)| name == "refs/heads/over" && target.contains(&head)),
            "the loose value wins over the packed one: {heads:?}"
        );
        assert!(
            !heads
                .iter()
                .any(|(name, _)| name.contains("lock") || name.contains("..")),
            "lock files and invalid names are skipped: {heads:?}"
        );
    }

    #[test]
    fn many_loose_refs_are_read_in_parallel_in_order() {
        let repo = fixture::repo();
        repo.write("f", "1\n");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-q", "-m", "one"]);
        let head = repo.git(&["rev-parse", "HEAD"]);
        let dir = repo.path.join(".git/refs/heads/many");
        std::fs::create_dir_all(&dir).expect("writable");
        for i in 0..(PARALLEL_MIN_FILES * 3) {
            std::fs::write(dir.join(format!("b{i}")), format!("{head}\n")).expect("writable");
        }
        // A broken file is reported as an error item, the others are still listed.
        std::fs::write(dir.join("broken"), "not a reference\n").expect("writable");

        let gix_repo = gix::open(&repo.path).expect("the repository opens");
        let items = scan(&gix_repo, RefNamespace::LocalBranches).expect("listed");
        assert_eq!(
            items.len(),
            PARALLEL_MIN_FILES * 3 + 2,
            "main, the many and the broken one"
        );
        assert_eq!(
            items.iter().filter(|item| item.is_err()).count(),
            1,
            "one broken ref"
        );
        let names: Vec<String> = items
            .iter()
            .filter_map(|item| item.as_ref().ok())
            .map(|item| item.name.to_string())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "sorted by name");
    }

    #[test]
    fn missing_namespace_is_empty() {
        let repo = fixture::repo();
        let gix_repo = gix::open(&repo.path).expect("the repository opens");
        assert!(
            scan(&gix_repo, RefNamespace::RemoteBranches)
                .expect("listed")
                .is_empty(),
            "no remotes directory, no references"
        );
    }
}
