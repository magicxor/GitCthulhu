//! Blob and worktree reads in git form (WP-B0d), shared by `diff.rs` and `stage.rs`.
//!
//! - Tree, HEAD and index entry lookups by byte path (no lossy conversions).
//! - [`read_blob`]: an object side, size-checked from the object header before anything is read.
//! - [`WorktreeReader`]: a worktree side converted to git form (clean filters, `core.autocrlf`, `eol`,
//!   `working-tree-encoding`) through the repository's filter pipeline, hashed with `gix::objs::compute_hash` so the
//!   id equals what `git hash-object <path>` prints. It also converts back to worktree form for discards.
//!
//! Sizes are checked before reading (`find_header` / `metadata().len()`), so a side above the limit costs no read.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use bstr::BStr;
use gix::{ObjectId, index::entry::Stage};

use crate::{BlobSide, EntryKind, GitError, Result, Side};

/// One side of a diff plus the bytes the line engine needs.
#[derive(Debug, Clone)]
pub(crate) struct SideContent {
    /// Id, kind, git-form size (worktree size when not read) and origin. The id is null only for a worktree file
    /// above the size limit (not hashed) and for a worktree submodule (its commit comes from the status).
    pub side: BlobSide,
    /// The git-form bytes. `None` above the size limit and for submodules.
    pub data: Option<Vec<u8>>,
}

impl SideContent {
    /// `true` if the side was not read because it exceeds the size limit it was read with.
    pub fn is_too_large(&self) -> bool {
        self.data.is_none() && self.side.kind != EntryKind::Submodule
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

/// Read the object side `side`. Submodules are not read. If the object is larger than `max_bytes` (known from the
/// object header), its data is `None`.
pub(crate) fn read_blob(repo: &gix::Repository, side: Side, max_bytes: u64) -> Result<SideContent> {
    let blob_side = |size| BlobSide {
        id: side.id,
        kind: side.kind,
        size,
        from_worktree: false,
    };
    if side.kind == EntryKind::Submodule {
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

/// Reads worktree files in git form. Create one per operation; it holds the filter pipeline (with its attribute
/// stack) and the index that configured it.
pub(crate) struct WorktreeReader<'repo> {
    repo: &'repo gix::Repository,
    pipeline: gix::filter::Pipeline<'repo>,
    index: gix::worktree::IndexPersistedOrInMemory,
    path_check: gix::status::plumbing::SymlinkCheck,
    /// `core.fileMode`: if `false`, the executable bit comes from the index entry.
    executable_bit: bool,
}

impl<'repo> WorktreeReader<'repo> {
    /// Set up the filter pipeline for `repo`'s worktree. `BareRepository` without a worktree.
    pub fn new(repo: &'repo gix::Repository) -> Result<Self> {
        let workdir = repo.workdir().ok_or(GitError::BareRepository)?.to_owned();
        let (pipeline, index) = repo.filter_pipeline(None)?;
        Ok(WorktreeReader {
            repo,
            pipeline,
            index,
            path_check: gix::status::plumbing::SymlinkCheck::new(workdir),
            executable_bit: repo.filesystem_options()?.executable_bit,
        })
    }

    /// The index the pipeline was configured with (read when the reader was created).
    pub fn index(&self) -> &gix::index::State {
        &self.index
    }

    /// The absolute path of `rela`, verified not to lead through a symbolic link (on Unix; Windows only joins).
    pub fn path(&mut self, rela: &BStr) -> Result<PathBuf> {
        let display = || PathBuf::from(rela.to_string());
        self.path_check
            .verified_path_allow_nonexisting(rela)
            .map(|p| p.into_owned())
            .map_err(|err| GitError::io(display(), err))
    }

    /// Read the worktree file at `rela` in git form. `None` if nothing trackable is there (missing, or a plain
    /// directory). A nested repository is a submodule side without data. If the file on disk is larger than
    /// `max_bytes`, it is neither read nor hashed (null id, `data: None`).
    ///
    /// The kind is a symlink for symbolic links, executable if `core.fileMode` is true and the file has an executable
    /// bit, or (with `core.fileMode=false`) if its index entry is executable.
    pub fn read(&mut self, rela: &BStr, max_bytes: u64) -> Result<Option<SideContent>> {
        let path = match self.path_check.verified_path_allow_nonexisting(rela) {
            Ok(path) => path.into_owned(),
            // Git does not look behind a symbolic link either (`has_symlinked_leading_path`): the tracked file is gone.
            Err(err) if is_missing(&err) || is_behind_symlink(&err) => return Ok(None),
            Err(err) => return Err(GitError::io(PathBuf::from(rela.to_string()), err)),
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
        let null = ObjectId::null(self.repo.object_hash());
        if md.is_dir() {
            return Ok(path.join(".git").exists().then(|| SideContent {
                side: side(null, EntryKind::Submodule, 0),
                data: None,
            }));
        }
        let kind = if md.is_symlink() {
            EntryKind::Link
        } else if self.executable_bit {
            if md.is_executable() {
                EntryKind::BlobExecutable
            } else {
                EntryKind::Blob
            }
        } else {
            match index_entry(&self.index, rela, Stage::Unconflicted)?.map(|e| e.kind) {
                // Without symlink support Git checks links out as plain files holding the target.
                Some(kind @ (EntryKind::BlobExecutable | EntryKind::Link)) => kind,
                Some(EntryKind::Blob | EntryKind::Submodule | EntryKind::Tree) | None => {
                    EntryKind::Blob
                }
            }
        };
        if md.len() > max_bytes {
            return Ok(Some(SideContent {
                side: side(null, kind, md.len()),
                data: None,
            }));
        }
        let mut data = Vec::with_capacity(md.len() as usize);
        if let Err(err) = but_core::worktree::worktree_file_to_git_in_buf(
            &mut data,
            &md,
            rela,
            &path,
            &mut self.pipeline,
            &self.index,
        ) {
            return match err.downcast::<std::io::Error>() {
                Ok(io) if is_missing(&io) => Ok(None),
                Ok(io) => Err(GitError::io(path, io)),
                Err(err) => Err(GitError::Other(err.context(format!(
                    "could not convert '{}' to git form",
                    path.display()
                )))),
            };
        }
        let id = hash_blob(self.repo, &data)?;
        Ok(Some(SideContent {
            side: side(id, kind, data.len() as u64),
            data: Some(data),
        }))
    }

    /// Convert raw worktree bytes of `rela` to git form (`convert_to_git`). Used for the discard round-trip check.
    pub fn convert_to_git(&mut self, raw: &[u8], rela: &BStr) -> Result<Vec<u8>> {
        let path = rela_to_path(rela)?;
        let mut out = Vec::with_capacity(raw.len());
        self.pipeline
            .convert_to_git(raw, &path, &self.index)?
            .read_to_end(&mut out)
            .map_err(|err| GitError::io(path.into_owned(), err))?;
        Ok(out)
    }

    /// Convert git-form bytes of `rela` to worktree form (`convert_to_worktree`, delayed filters forbidden).
    pub fn convert_to_worktree(&mut self, git_form: &[u8], rela: &BStr) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(git_form.len());
        self.pipeline
            .convert_to_worktree(git_form, rela, Default::default())?
            .read_to_end(&mut out)
            .map_err(|err| GitError::io(PathBuf::from(rela.to_string()), err))?;
        Ok(out)
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

/// The error of `SymlinkCheck` for a path whose leading directories contain a symbolic link (Unix only; gix does not
/// export a predicate, so the stable message is matched).
fn is_behind_symlink(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::Other
        && err.to_string().contains("Cannot step through symlink")
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
            .read(b"crlf.txt".as_bstr(), u64::MAX)
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
            .read(b"plain.txt".as_bstr(), u64::MAX)
            .expect("readable")
            .expect("present");
        assert_eq!(
            plain.side.id,
            oid(&repo.git(&["hash-object", "plain.txt"])),
            "same id as git hash-object"
        );

        assert!(
            reader
                .read(b"missing.txt".as_bstr(), u64::MAX)
                .expect("no error")
                .is_none(),
            "missing is None"
        );
        assert!(
            reader
                .read(b"plain.txt/below".as_bstr(), u64::MAX)
                .expect("no error")
                .is_none(),
            "a path below a file is missing, not an error"
        );

        let large = reader
            .read(b"plain.txt".as_bstr(), 3)
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
            reader
                .read(rela.as_bytes().as_bstr(), u64::MAX)
                .expect("readable")
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

    #[cfg(unix)]
    #[test]
    fn path_behind_symlinked_directory_is_missing() {
        let repo = fixture::repo();
        repo.write("real/f.txt", b"x\n");
        std::os::unix::fs::symlink("real", repo.path.join("link")).expect("symlinks work");
        let local = repo.open();
        let mut reader = WorktreeReader::new(&local).expect("worktree");
        assert!(
            reader
                .read(b"link/f.txt".as_bstr(), u64::MAX)
                .expect("not an error")
                .is_none(),
            "Git does not look through a symlinked directory: the path is missing"
        );
        let link = reader
            .read(b"link".as_bstr(), u64::MAX)
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
