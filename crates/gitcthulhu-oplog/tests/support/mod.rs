//! Shared fixtures of the oplog integration tests: throw-away repositories built with the `git` executable only
//! (no shell scripts, so everything runs on Windows where `bash` may resolve to WSL's shim).
#![allow(dead_code)] // Every test binary includes this module and uses a different part of it.

use std::{
    path::{Path, PathBuf},
    process::Output,
};

use bstr::{BString, ByteSlice as _};
use but_core::sync::RepoExclusiveGuard;
use gitcthulhu_oplog::{
    EntryId, OperationDescriptor, OperationKind, OperationResult, Oplog, OplogError, OplogOptions,
    PageRequest, Scope, UntrackedCapture,
};

/// The temp directory in the form Git reports paths in: `TEMP` may be an 8.3 short path on Windows
/// (`C:/Users/RUNNER~1/...` on hosted runners) and is a symlink into `/private/var` on macOS, while Git
/// prints worktree paths by the real one.
fn canonical_temp_dir() -> PathBuf {
    let temp = std::env::temp_dir();
    let Ok(canonical) = std::fs::canonicalize(&temp) else {
        return temp;
    };
    // Without the verbatim prefix Windows adds, which Git does not understand.
    match canonical
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
    {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ if cfg!(windows) => temp,
        _ => canonical,
    }
}

/// A temporary repository created with `git init -b main`.
pub struct Fixture {
    tmp: std::sync::Arc<tempfile::TempDir>,
    /// The worktree.
    pub path: PathBuf,
}

impl Fixture {
    /// An empty repository (unborn `main`) with repo-local identity, `commit.gpgsign=false`, `core.autocrlf=false`.
    pub fn empty() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("oplog")
            .tempdir_in(canonical_temp_dir())
            .expect("a temporary directory can be created");
        let path = tmp.path().join("repo");
        std::fs::create_dir_all(&path).expect("the repository directory can be created");
        let fx = Fixture {
            tmp: std::sync::Arc::new(tmp),
            path,
        };
        fx.git(&["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Test User"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("advice.detachedHead", "false"),
        ] {
            fx.git(&["config", key, value]);
        }
        fx
    }

    /// A repository with one commit `initial` that adds `a.txt`.
    pub fn new() -> Self {
        let fx = Self::empty();
        fx.write("a.txt", "a\n");
        fx.git(&["add", "a.txt"]);
        fx.git(&["commit", "-q", "-m", "initial"]);
        fx
    }

    /// Another worktree (or a moved copy) of this fixture's repository at `path`, sharing the temporary directory.
    pub fn worktree(&self, path: PathBuf) -> Fixture {
        Fixture {
            tmp: self.tmp.clone(),
            path,
        }
    }

    /// The temporary directory that contains the repository (for siblings like clones and remotes).
    pub fn tmp(&self) -> &Path {
        self.tmp.path()
    }

    /// Write `contents` to the worktree-relative `path`, creating parent directories.
    pub fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
        let path = self.path.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent directories can be created");
        }
        std::fs::write(path, contents).expect("the file can be written");
    }

    /// Run `git <args>` in the worktree; panic on failure; return stdout.
    pub fn git(&self, args: &[&str]) -> String {
        git_in(&self.path, args)
    }

    /// Run `git <args>` in the worktree and return the output, whatever the exit status.
    pub fn git_output(&self, args: &[&str]) -> Output {
        git_output_in(&self.path, args, &[])
    }

    /// Run `git <args>` with extra environment variables; panic on failure.
    pub fn git_env(&self, args: &[&str], env: &[(&str, &str)]) -> String {
        let out = git_output_in(&self.path, args, env);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            out.stderr.as_bstr()
        );
        out.stdout.to_str_lossy().into_owned()
    }

    /// `git rev-parse <rev>`.
    pub fn rev(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev]).trim().to_owned()
    }

    /// Commit `path` with `contents` and `message`.
    pub fn commit_file(&self, path: &str, contents: &str, message: &str) {
        self.write(path, contents);
        self.git(&["add", "--", path]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// A freshly opened repository handle.
    pub fn repo(&self) -> gix::Repository {
        but_testsupport::open_repo(&self.path).expect("the fixture repository can be opened")
    }

    /// An oplog handle with default options.
    pub fn oplog(&self) -> Oplog {
        self.oplog_with(OplogOptions::default())
    }

    /// An oplog handle with `options`.
    pub fn oplog_with(&self, options: OplogOptions) -> Oplog {
        Oplog::open(&self.repo(), options).expect("the fixture repository is supported")
    }

    /// The repository's exclusive guard, as the GUI takes it (keyed by the common dir).
    pub fn guard(&self) -> RepoExclusiveGuard {
        let repo = self.repo();
        but_core::sync::exclusive_repo_access(repo.common_dir(), None)
    }
}

/// Run `git <args>` in `dir`; panic on failure; return stdout.
pub fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = git_output_in(dir, args, &[]);
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}{}",
        dir.display(),
        out.stdout.as_bstr(),
        out.stderr.as_bstr()
    );
    out.stdout.to_str_lossy().into_owned()
}

/// Run `git <args>` in `dir` with extra `env`, returning the output whatever the exit status.
pub fn git_output_in(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = but_testsupport::git_at_dir(dir);
    cmd.args(args);
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().expect("git can be spawned")
}

/// A descriptor of `kind` with `title`, `Scope::Full` and no untracked capture.
pub fn desc(kind: OperationKind, title: &str) -> OperationDescriptor {
    OperationDescriptor {
        kind,
        title: title.to_owned(),
        detail: None,
        scope: Scope::Full,
        untracked: UntrackedCapture::None,
    }
}

/// Like [`desc()`] with `Scope::RefsOnly`.
pub fn refs_only(kind: OperationKind, title: &str) -> OperationDescriptor {
    OperationDescriptor {
        scope: Scope::RefsOnly,
        ..desc(kind, title)
    }
}

/// Wrap `op` in `prepare` and `finish` exactly as the GUI does: one guard for prepare → operation → finish, the
/// operation's result decides `OperationResult`. Returns the recorded entry (`None` if nothing was recorded).
pub fn record(
    fx: &Fixture,
    oplog: &Oplog,
    desc: OperationDescriptor,
    op: impl FnOnce(&Fixture) -> Result<(), String>,
) -> Result<Option<EntryId>, OplogError> {
    let mut guard = fx.guard();
    let pending = oplog.prepare(&fx.repo(), desc, guard.write_permission())?;
    let result = op(fx);
    let Some(pending) = pending else {
        return Ok(None);
    };
    let result = match result {
        Ok(()) => OperationResult::Succeeded,
        Err(message) => OperationResult::Failed { message },
    };
    oplog.finish(&fx.repo(), pending, result, guard.write_permission())
}

/// [`record()`] of an operation that must succeed and must be recorded.
pub fn record_ok(
    fx: &Fixture,
    oplog: &Oplog,
    desc: OperationDescriptor,
    op: impl FnOnce(&Fixture),
) -> EntryId {
    record(fx, oplog, desc, |fx| {
        op(fx);
        Ok(())
    })
    .expect("recording works")
    .expect("the operation changed the repository, so an entry was recorded")
}

/// Record a commit of `path` with `contents`.
pub fn record_commit(
    fx: &Fixture,
    oplog: &Oplog,
    path: &str,
    contents: &str,
    message: &str,
) -> EntryId {
    record_ok(
        fx,
        oplog,
        desc(OperationKind::Commit, &format!("Commit \"{message}\"")),
        |fx| fx.commit_file(path, contents, message),
    )
}

/// Record the creation of branch `name` at `HEAD` (`RefsOnly`).
pub fn record_branch(fx: &Fixture, oplog: &Oplog, name: &str) -> EntryId {
    record_ok(
        fx,
        oplog,
        refs_only(
            OperationKind::CreateBranch,
            &format!("Create branch '{name}'"),
        ),
        |fx| {
            fx.git(&["branch", name]);
        },
    )
}

/// The ids of all entries, oldest first (bookkeeping included).
pub fn all_entries(fx: &Fixture, oplog: &Oplog) -> Vec<gitcthulhu_oplog::Entry> {
    let mut out = Vec::new();
    let mut before = None;
    loop {
        let page = oplog
            .list(
                &fx.repo(),
                PageRequest {
                    before,
                    limit: 200,
                    include_bookkeeping: true,
                },
            )
            .expect("listing works");
        out.extend(page.entries);
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    out.reverse();
    out
}

/// A canonical text of the repository state: refs, HEAD, index (incl. stages), stash list, status, and the hash of
/// every non-ignored worktree file.
pub fn fingerprint(fx: &Fixture) -> String {
    let mut out = String::new();
    out.push_str(&fx.git(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        "refs/heads",
        "refs/tags",
        "refs/stash",
    ]));
    let head = fx.git_output(&["symbolic-ref", "-q", "HEAD"]);
    if head.status.success() {
        out.push_str(&head.stdout.to_str_lossy());
    } else {
        out.push_str(&fx.git(&["rev-parse", "HEAD"]));
    }
    out.push_str(&fx.git(&["ls-files", "-s"]));
    out.push_str(&fx.git(&["stash", "list", "--format=%H %gs"]));
    out.push_str(
        &fx.git(&["status", "--porcelain=v2", "-z", "--untracked-files=all"])
            .replace('\0', "\n"),
    );
    let files = fx.git(&["ls-files", "-co", "--exclude-standard", "-z"]);
    let mut files: Vec<&str> = files.split('\0').filter(|f| !f.is_empty()).collect();
    files.sort_unstable();
    files.dedup();
    for file in files {
        let path = fx.path.join(file);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let id = gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::object::Kind::Blob, &bytes)
            .expect("hashing in memory works");
        out.push_str(&format!("{id} {file}\n"));
    }
    out
}

/// `true` if an object with the blob id of `contents` exists in the repository.
pub fn has_blob_of(fx: &Fixture, contents: &[u8]) -> bool {
    let id = gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::object::Kind::Blob, contents)
        .expect("hashing in memory works");
    fx.git_output(&["cat-file", "-e", &id.to_string()])
        .status
        .success()
}

/// The paths of `git ls-tree -r --name-only <treeish>`, one per line.
pub fn ls_tree(fx: &Fixture, treeish: &str) -> String {
    fx.git(&["ls-tree", "-r", "--name-only", treeish])
}

/// Parse `git show <rev>:<path>` as JSON.
pub fn show_json(fx: &Fixture, rev_path: &str) -> serde_json::Value {
    serde_json::from_str(&fx.git(&["show", rev_path])).expect("the blob is valid JSON")
}

/// `bstr` helper.
pub fn b(s: &str) -> BString {
    s.into()
}
