//! Snapshot-based undo/redo (operation log) for GitCthulhu.
//!
//! Before each mutating operation the GUI asks the oplog to capture the repository state ([`Oplog::prepare()`]);
//! after it, [`Oplog::finish()`] captures again and appends one entry (before + after) to a commit chain under
//! `refs/gitcthulhu/oplog/main` (or `refs/gitcthulhu/oplog/linked/<id>` for a linked worktree). Undo puts back the
//! before-state of the newest undoable entry, redo reverses the undo; both are entries themselves.
//!
//! Writing functions take `&mut but_core::sync::RepoExclusive` (the repository's exclusive guard, keyed by the
//! common dir); reading functions take no lock and see a consistent snapshot of the oplog ref.
#![deny(missing_docs)]

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use bstr::BString;
use but_core::sync::RepoExclusive;
use gix::refs::{FullName, FullNameRef};

mod capture;
mod error;
mod fail_point;
mod git_runner;
mod meta;
mod prune;
mod record;
mod restore;
mod stacks;
mod state;
mod storage;
mod types;

pub use crate::{
    error::{OplogError, UnsupportedReason},
    git_runner::{GitRunner, SystemGit},
    types::*,
};

/// Test-only hooks (feature `test-hooks`): fail points and direct access to the stack replay.
#[cfg(feature = "test-hooks")]
pub mod testing {
    pub use crate::{
        fail_point::{FailPoint, arm, arm_action, disarm_all},
        stacks::testing::{Replayed, classify_entry, replay_roles},
    };

    /// Capture `repo` like `oplog.prepare()` would, write the snapshot tree and read it back. Returns the tree id and
    /// whether the state read back equals the captured one.
    pub fn capture_round_trip(
        oplog: &crate::Oplog,
        repo: &gix::Repository,
        scope: crate::Scope,
        untracked: &crate::UntrackedCapture,
    ) -> Result<(gix::ObjectId, bool), crate::OplogError> {
        let state = crate::capture::capture(repo, scope, untracked, &oplog.limits())?;
        let tree = state.write_tree(repo)?;
        let read = crate::state::CapturedState::read_tree(repo, tree)?;
        Ok((tree, read == state))
    }
}

use crate::{
    capture::CaptureLimits,
    meta::{EntryMeta, FORMAT_VERSION},
    stacks::Stacks,
    storage::{Chain, StoredEntry},
};

/// File name of the restore journal in [`Oplog::journal_dir()`] (§3.4).
pub(crate) const JOURNAL_FILE: &str = "restore.json";
/// File name of the index backup in [`Oplog::journal_dir()`] (§3.3 C2).
pub(crate) const INDEX_BACKUP_FILE: &str = "index.backup";

/// Configuration of an [`Oplog`].
#[derive(Clone)]
pub struct OplogOptions {
    /// When `false`, `prepare` returns `Ok(None)` and nothing is recorded. Undo/redo still work on existing entries.
    pub enabled: bool,
    /// Which entries are kept.
    pub retention: RetentionPolicy,
    /// Untracked files larger than this are not captured (default 50 MiB).
    pub untracked_file_limit: u64,
    /// The captured untracked files of one snapshot total at most this many bytes, smallest first (default 512 MiB).
    pub untracked_total_limit: u64,
    /// When `false`, untracked contents are never captured (every `UntrackedCapture` acts as `None`; requested paths
    /// are reported as skipped with `SkipReason::CaptureDisabled`). Default true (§1.3).
    pub capture_untracked: bool,
    /// Runs the git executable for `--quit` and `stash store`. Default: [`SystemGit`]. The GUI injects an adapter
    /// over its Git runtime, so the user's Git path and environment rules apply.
    pub git: Arc<dyn GitRunner>,
    /// Written into each entry's metadata.
    pub app_version: String,
}

impl Default for OplogOptions {
    fn default() -> Self {
        OplogOptions {
            enabled: true,
            retention: RetentionPolicy::default(),
            untracked_file_limit: 50 * 1024 * 1024,
            untracked_total_limit: 512 * 1024 * 1024,
            capture_untracked: true,
            git: Arc::new(SystemGit),
            app_version: String::new(),
        }
    }
}

impl std::fmt::Debug for OplogOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OplogOptions")
            .field("enabled", &self.enabled)
            .field("retention", &self.retention)
            .field("untracked_file_limit", &self.untracked_file_limit)
            .field("untracked_total_limit", &self.untracked_total_limit)
            .field("capture_untracked", &self.capture_untracked)
            .field("app_version", &self.app_version)
            .finish_non_exhaustive()
    }
}

/// How many entries the log keeps (§2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// At most this many entries (default 500).
    pub max_entries: usize,
    /// Entries older than this are dropped (default 30 days).
    pub max_age: std::time::Duration,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy {
            max_entries: 500,
            max_age: std::time::Duration::from_secs(30 * 24 * 60 * 60),
        }
    }
}

/// Undo/redo log of one worktree. Holds no `gix::Repository`; every call takes the caller's per-thread repository,
/// which must belong to the worktree the handle was opened for (checked: `repo.git_dir()` equality, else
/// [`OplogError::WrongRepository`]).
///
/// `Send + Sync`; internally caches the parsed chain keyed by the oplog ref's current commit id.
pub struct Oplog {
    git_dir: PathBuf,
    git_dir_canonical: PathBuf,
    ref_name: FullName,
    is_linked: bool,
    worktree_path: String,
    options: OplogOptions,
    cache: parking_lot::Mutex<Option<Arc<(Chain, Stacks)>>>,
    /// The highest sequence number seen, so that a chain started after `clear()` gets larger ids.
    last_seq: AtomicU64,
}

// The handle is shared between the GUI's threads (documented as `Send + Sync`).
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Oplog>();
};

fn canonical(path: &Path) -> PathBuf {
    gix::path::realpath(path).unwrap_or_else(|_| path.to_owned())
}

impl Oplog {
    /// Validate `repo` (non-bare, files ref backend, no sparse checkout) and compute the worktree key.
    /// Does not write anything. Errors: Unsupported(..).
    pub fn open(repo: &gix::Repository, options: OplogOptions) -> Result<Oplog, OplogError> {
        capture::check_supported(repo)?;
        let git_dir = repo.git_dir().to_owned();
        let git_dir_canonical = canonical(&git_dir);
        let is_linked = git_dir_canonical != canonical(repo.common_dir());
        let ref_name = if is_linked {
            let id = git_dir
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    OplogError::Other(anyhow::anyhow!(
                        "the git dir '{}' of the linked worktree has no valid name",
                        git_dir.display()
                    ))
                })?;
            linked_ref_name(id)?
        } else {
            FullName::try_from("refs/gitcthulhu/oplog/main")
                .expect("the main oplog ref name is valid")
        };
        let workdir = repo
            .workdir()
            .ok_or(OplogError::Unsupported(UnsupportedReason::BareRepository))?;
        let worktree_path = gix::path::into_bstr(canonical(workdir))
            .map(|path| gix::path::to_unix_separators_on_windows(path).to_string())
            .map_err(|err| OplogError::Other(anyhow::Error::from(err)))?;
        Ok(Oplog {
            git_dir,
            git_dir_canonical,
            ref_name,
            is_linked,
            worktree_path,
            options,
            cache: parking_lot::Mutex::new(None),
            last_seq: AtomicU64::new(0),
        })
    }

    /// The options of this handle.
    pub fn options(&self) -> &OplogOptions {
        &self.options
    }

    /// Replace the options of this handle (e.g. after the user changed the Settings > Undo page).
    pub fn set_options(&mut self, options: OplogOptions) {
        self.options = options;
    }

    /// The full name of the oplog ref, e.g. `refs/gitcthulhu/oplog/main`.
    pub fn ref_name(&self) -> &FullNameRef {
        self.ref_name.as_ref()
    }

    // ── recording (exclusive guard held by the caller) ─────────────────────────────────────────

    /// Capture the before-state for `op` (untracked contents per `op.untracked`). `Ok(None)` if recording is disabled.
    ///
    /// Errors: InterruptedRestore, Unsupported, Capture, FileLocked, RepositoryLocked, Io. Any error except
    /// InterruptedRestore: the caller runs the operation unrecorded; the stacks are untouched (§1.2 notes).
    pub fn prepare(
        &self,
        repo: &gix::Repository,
        op: OperationDescriptor,
        perm: &mut RepoExclusive,
    ) -> Result<Option<PendingOperation>, OplogError> {
        record::prepare(self, repo, op, perm)
    }

    /// Capture the after-state (same `UntrackedCapture`) and append the entry. Returns `Ok(None)` when the after-state
    /// is equivalent to the before-state (no-op or failed-without-effect operation: nothing to undo). Classifies the
    /// entry (§4.3). If the after-capture fails, the entry is still appended with `after_captured = false` and
    /// `Ok(Some(id))`.
    pub fn finish(
        &self,
        repo: &gix::Repository,
        pending: PendingOperation,
        result: OperationResult,
        perm: &mut RepoExclusive,
    ) -> Result<Option<EntryId>, OplogError> {
        record::finish(self, repo, pending, result, perm)
    }

    /// Record a non-undoable network effect (push, remote delete). Does not change the undo/redo stacks.
    pub fn record_marker(
        &self,
        repo: &gix::Repository,
        marker: NetworkMarker,
        perm: &mut RepoExclusive,
    ) -> Result<EntryId, OplogError> {
        record::record_marker(self, repo, marker, perm)
    }

    /// Record that history was paused ("record undo history" turned off and on again). Clears both stacks.
    /// Not used for capture failures (those record nothing, §1.2).
    pub fn record_barrier(
        &self,
        repo: &gix::Repository,
        kind: OperationKind,
        title: String,
        reason: String,
        perm: &mut RepoExclusive,
    ) -> Result<EntryId, OplogError> {
        record::record_barrier(self, repo, kind, title, reason, perm)
    }

    // ── reading (no lock; consistent snapshot of the ref) ─────────────────────────────────────

    /// Cheap (metadata only, cached): what Undo/Redo would do now. Never captures.
    pub fn state(&self, repo: &gix::Repository) -> Result<UndoRedoState, OplogError> {
        self.check_repo(repo)?;
        let chain_and_stacks = self.chain(repo)?;
        let (chain, stacks) = &*chain_and_stacks;
        let info = |item: &stacks::StackItem, is_undo: bool| -> StackItemInfo {
            let label = chain.entry(item.label.0).map(|e| &e.meta);
            let ends_in_progress = if is_undo {
                let expected = chain
                    .entry(item.expected.0)
                    .and_then(|e| e.meta.after_in_progress);
                let target = chain
                    .entry(item.reverse.0)
                    .and_then(|e| e.meta.before_in_progress);
                expected.filter(|_| target.is_none())
            } else {
                None
            };
            StackItemInfo {
                reverses: item.reverse,
                operation: item.label,
                kind: label.map_or(OperationKind::Unknown, |m| m.kind),
                title: label.map(|m| m.title.clone()).unwrap_or_default(),
                finished_at_ms: label.map_or(0, |m| m.finished_at_ms),
                ends_in_progress,
            }
        };
        Ok(UndoRedoState {
            undo: stacks.undo.last().map(|item| info(item, true)),
            redo: stacks.redo.last().map(|item| info(item, false)),
            undo_depth: stacks.undo.len(),
            redo_depth: stacks.redo.len(),
            interrupted_restore: restore::interrupted_restore(self, repo)?.is_some(),
        })
    }

    /// `true` if Undo would do something now.
    pub fn can_undo(&self, repo: &gix::Repository) -> Result<bool, OplogError> {
        self.check_repo(repo)?;
        Ok(!self.chain(repo)?.1.undo.is_empty())
    }

    /// `true` if Redo would do something now.
    pub fn can_redo(&self, repo: &gix::Repository) -> Result<bool, OplogError> {
        self.check_repo(repo)?;
        Ok(!self.chain(repo)?.1.redo.is_empty())
    }

    /// Newest first. `PageRequest::before` is an exclusive cursor (an `EntryId` from a previous page).
    pub fn list(&self, repo: &gix::Repository, page: PageRequest) -> Result<Page, OplogError> {
        self.check_repo(repo)?;
        let chain_and_stacks = self.chain(repo)?;
        let (chain, stacks) = &*chain_and_stacks;
        let limit = page.limit.clamp(1, 200);
        let mut candidates = chain
            .entries
            .iter()
            .rev()
            .filter(|e| page.before.is_none_or(|before| e.meta.seq < before.0))
            .filter(|e| {
                page.include_bookkeeping
                    || !matches!(e.meta.role, EntryRole::Undo { .. } | EntryRole::Redo { .. })
            });
        let mut entries = Vec::with_capacity(limit);
        for stored in candidates.by_ref().take(limit) {
            entries.push(self.to_entry(repo, stored, stacks)?);
        }
        let next = if candidates.next().is_some() {
            entries.last().map(|e| e.id)
        } else {
            None
        };
        Ok(Page { entries, next })
    }

    /// The entry with `id`, if it exists.
    pub fn entry(&self, repo: &gix::Repository, id: EntryId) -> Result<Option<Entry>, OplogError> {
        self.check_repo(repo)?;
        let chain_and_stacks = self.chain(repo)?;
        let (chain, stacks) = &*chain_and_stacks;
        chain
            .entry(id.0)
            .map(|stored| self.to_entry(repo, stored, stacks))
            .transpose()
    }

    /// The interrupted restore that must be finished or rolled back first, if any.
    pub fn interrupted_restore(
        &self,
        repo: &gix::Repository,
    ) -> Result<Option<InterruptedRestore>, OplogError> {
        self.check_repo(repo)?;
        restore::interrupted_restore(self, repo)
    }

    /// Which of the given untracked `paths` (worktree-relative; directories expand to the untracked files below them)
    /// a capture would skip, and why: size > untracked_file_limit (TooLarge), unreadable (Unreadable), capture disabled
    /// (CaptureDisabled), an untracked nested repository (NestedRepository, never captured). `OverBudget` is not
    /// predicted (the budget depends on the set; reported by `prepare`).
    /// stat + open probe only; no hashing, no lock. The GUI calls it before taking the guard (§4.4).
    pub fn preview_skipped(
        &self,
        repo: &gix::Repository,
        paths: &[BString],
    ) -> Result<Vec<SkippedFile>, OplogError> {
        self.check_repo(repo)?;
        capture::preview_skipped(repo, paths, &self.limits()).map_err(OplogError::Other)
    }

    // ── restoring (exclusive guard held by the caller; never across a dialog) ─────────────────

    /// Undo the top of the undo stack. Returns `NeedsConfirmation` without writing anything when the repository
    /// diverged from the expected state; call again with the token after the user agreed.
    pub fn undo(
        &self,
        repo: &gix::Repository,
        options: RestoreOptions,
        perm: &mut RepoExclusive,
    ) -> Result<UndoOutcome, OplogError> {
        self.check_repo(repo)?;
        restore::undo(self, repo, options, perm)
    }

    /// Redo the top of the redo stack; see [`Self::undo()`].
    pub fn redo(
        &self,
        repo: &gix::Repository,
        options: RestoreOptions,
        perm: &mut RepoExclusive,
    ) -> Result<UndoOutcome, OplogError> {
        self.check_repo(repo)?;
        restore::redo(self, repo, options, perm)
    }

    /// Restore the repository to the before- or after-state of any entry with a snapshot. Always asks for
    /// confirmation first (`ConfirmReason::ArbitraryRestore`). Recorded as an undoable `RestoreTo` entry.
    pub fn restore_to(
        &self,
        repo: &gix::Repository,
        target: RestoreTarget,
        options: RestoreOptions,
        perm: &mut RepoExclusive,
    ) -> Result<UndoOutcome, OplogError> {
        self.check_repo(repo)?;
        restore::restore_to(self, repo, target, options, perm)
    }

    /// Finish or roll back an interrupted restore (§3.6).
    pub fn recover(
        &self,
        repo: &gix::Repository,
        how: Recovery,
        perm: &mut RepoExclusive,
    ) -> Result<(), OplogError> {
        self.check_repo(repo)?;
        restore::recover(self, repo, how, perm)
    }

    // ── maintenance ──────────────────────────────────────────────────────────────────────────

    /// Apply `policy` (or the configured one) now.
    pub fn prune(
        &self,
        repo: &gix::Repository,
        policy: Option<RetentionPolicy>,
        _perm: &mut RepoExclusive,
    ) -> Result<PruneReport, OplogError> {
        self.check_repo(repo)?;
        let policy = policy.unwrap_or(self.options.retention);
        let chain_and_stacks = self.chain(repo)?;
        prune::prune(
            repo,
            self.ref_name(),
            &chain_and_stacks.0,
            &policy,
            storage::now_ms(),
        )
    }

    /// Delete the whole history of this worktree (ref, journal, backup). Objects become unreachable.
    pub fn clear(
        &self,
        repo: &gix::Repository,
        _perm: &mut RepoExclusive,
    ) -> Result<(), OplogError> {
        self.check_repo(repo)?;
        // Remember the highest sequence number so that the next chain starts above it.
        self.chain(repo)?;
        storage::delete_ref(repo, self.ref_name())?;
        storage::remove_journal_files(&self.journal_dir(repo))?;
        *self.cache.lock() = None;
        Ok(())
    }

    /// Delete `refs/gitcthulhu/oplog/linked/<worktree_id>` of a linked worktree that was removed or pruned (its git
    /// dir, journal included, is already gone). `repo` may be any worktree of the repository. Missing ref => Ok.
    pub fn clear_linked_worktree(
        repo: &gix::Repository,
        worktree_id: &str,
        _perm: &mut RepoExclusive,
    ) -> Result<(), OplogError> {
        let name = linked_ref_name(worktree_id)?;
        storage::delete_ref(repo, name.as_ref())
    }
}

/// `refs/gitcthulhu/oplog/linked/<id>`; `id` must be a single path component.
fn linked_ref_name(id: &str) -> Result<FullName, OplogError> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']) {
        return Err(OplogError::Other(anyhow::anyhow!(
            "'{id}' is not a valid linked worktree id"
        )));
    }
    FullName::try_from(format!("refs/gitcthulhu/oplog/linked/{id}")).map_err(|err| {
        OplogError::Other(anyhow::anyhow!(
            "'{id}' is not a valid linked worktree id: {err}"
        ))
    })
}

/// Internal interfaces used by recording and by the restore engine.
impl Oplog {
    /// The parsed chain and its replayed stacks, cached by the oplog ref's current commit id. After an append only
    /// the new entries are parsed.
    pub(crate) fn chain(&self, repo: &gix::Repository) -> Result<Arc<(Chain, Stacks)>, OplogError> {
        let head = storage::read_head(repo, self.ref_name())?;
        let cached = self.cache.lock().clone();
        if let Some(cached) = &cached
            && cached.0.head == head
        {
            return Ok(Arc::clone(cached));
        }
        let chain = match head {
            None => Chain::default(),
            Some(head) => storage::load_chain_at(repo, head, cached.as_ref().map(|c| &c.0))?,
        };
        let linked_workdir = self.is_linked.then_some(self.worktree_path.as_str());
        let stacks = stacks::replay(chain.entries.iter().map(|e| &e.meta), linked_workdir);
        if let Some(last) = chain.entries.last() {
            self.last_seq.fetch_max(last.meta.seq, Ordering::Relaxed);
        }
        let fresh = Arc::new((chain, stacks));
        *self.cache.lock() = Some(Arc::clone(&fresh));
        Ok(fresh)
    }

    /// `<git_dir>/gitcthulhu/oplog`: the restore journal and index backup of this worktree.
    pub(crate) fn journal_dir(&self, repo: &gix::Repository) -> PathBuf {
        repo.git_dir().join("gitcthulhu").join("oplog")
    }

    /// The untracked-capture limits of the current options.
    pub(crate) fn limits(&self) -> CaptureLimits {
        CaptureLimits {
            untracked_file_limit: self.options.untracked_file_limit,
            untracked_total_limit: self.options.untracked_total_limit,
            capture_untracked: self.options.capture_untracked,
        }
    }

    /// The canonical workdir written into `EntryMeta::worktree_path`.
    pub(crate) fn worktree_path(&self) -> &str {
        &self.worktree_path
    }

    /// `true` if `git_dir` is the git dir this handle was opened for (ASCII case-insensitive on Windows, where the
    /// same directory may be opened with differently cased paths).
    pub(crate) fn is_own_git_dir(&self, git_dir: &Path) -> bool {
        let same = |a: &Path, b: &Path| {
            a == b || (cfg!(windows) && a.as_os_str().eq_ignore_ascii_case(b.as_os_str()))
        };
        same(git_dir, &self.git_dir) || same(&canonical(git_dir), &self.git_dir_canonical)
    }

    /// Fail with `WrongRepository` unless `repo` belongs to this handle's worktree.
    pub(crate) fn check_repo(&self, repo: &gix::Repository) -> Result<(), OplogError> {
        if self.is_own_git_dir(repo.git_dir()) {
            Ok(())
        } else {
            Err(OplogError::WrongRepository)
        }
    }

    /// Append one entry whose metadata `build` derives from the current chain, its stacks and the next sequence
    /// number. A concurrent append (guard failure) re-reads the chain and retries once, then fails with
    /// `ConcurrentModification`. Afterwards, prunes if the retention policy's amortised trigger fires (§2.5).
    pub(crate) fn append_entry(
        &self,
        repo: &gix::Repository,
        mut build: impl FnMut(&Chain, &Stacks, u64) -> EntryMeta,
        before: Option<&state::CapturedState>,
        after: Option<&state::CapturedState>,
    ) -> Result<EntryId, OplogError> {
        let mut attempt = 0;
        let id = loop {
            let chain_and_stacks = self.chain(repo)?;
            let (chain, stacks) = &*chain_and_stacks;
            let seq = match chain.entries.last() {
                Some(last) => last.meta.seq + 1,
                None => u64::try_from(storage::now_ms())
                    .unwrap_or(0)
                    .max(self.last_seq.load(Ordering::Relaxed) + 1),
            };
            let meta = build(chain, stacks, seq);
            debug_assert_eq!(meta.seq, seq, "the builder uses the given sequence number");
            debug_assert_eq!(meta.version, FORMAT_VERSION);
            match storage::append(repo, self.ref_name(), chain.head, &meta, before, after) {
                Ok(_commit) => break EntryId(seq),
                Err(OplogError::ConcurrentModification) if attempt == 0 => attempt += 1,
                Err(err) => return Err(err),
            }
        };
        self.prune_if_due(repo);
        Ok(id)
    }

    /// Prune when the amortised trigger fires; failures are logged, never returned (the entry is already recorded).
    fn prune_if_due(&self, repo: &gix::Repository) {
        let result = self.chain(repo).and_then(|chain_and_stacks| {
            let chain = &chain_and_stacks.0;
            let now = storage::now_ms();
            if prune::is_due(chain, &self.options.retention, now) {
                prune::prune(repo, self.ref_name(), chain, &self.options.retention, now).map(Some)
            } else {
                Ok(None)
            }
        });
        match result {
            Ok(Some(report)) => tracing::debug!(?report, "pruned the oplog"),
            Ok(None) => {}
            Err(err) => tracing::warn!(?err, "pruning the oplog failed"),
        }
    }

    /// Build the public [`Entry`] of `stored`; reads only the `state.json` of its before-snapshot.
    fn to_entry(
        &self,
        repo: &gix::Repository,
        stored: &StoredEntry,
        stacks: &Stacks,
    ) -> Result<Entry, OplogError> {
        let meta: &EntryMeta = &stored.meta;
        let (skipped_untracked, skipped_untracked_truncated) = match stored.before {
            Some(tree) => state::read_skipped(repo, tree).map_err(|err| OplogError::Corrupt {
                commit: stored.commit,
                reason: format!("{err:#}"),
            })?,
            None => (Vec::new(), false),
        };
        Ok(Entry {
            id: meta.id(),
            commit: stored.commit,
            role: meta.role.clone(),
            kind: meta.kind,
            title: meta.title.clone(),
            detail: meta.detail.clone(),
            scope: meta.scope,
            started_at_ms: meta.started_at_ms,
            finished_at_ms: meta.finished_at_ms,
            result: meta.result.clone(),
            refs_changed: meta
                .refs_changed
                .iter()
                .filter_map(|name| FullName::try_from(name.as_str()).ok())
                .collect(),
            before_in_progress: meta.before_in_progress,
            after_in_progress: meta.after_in_progress,
            after_captured: meta.after_captured,
            skipped_untracked,
            skipped_untracked_truncated,
            network: meta.network.clone(),
            status: stacks
                .status
                .get(&meta.id())
                .copied()
                .unwrap_or(EntryStatus::NotUndoable),
            restorable: meta.version <= FORMAT_VERSION,
        })
    }
}
