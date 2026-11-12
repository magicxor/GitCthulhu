//! Diff reads (owned by G7a; spec 21 §2.4, §4.1). Blocking: call them inside a `ReadSlot` or `backend::bg`.

use std::sync::Arc;

use super::{
    CancelToken, CommitFilesOptions, DiffOptions, DiffTarget, FileDiff, GitError, ObjectId, Repo,
};

/// Image previews read at most this many bytes per blob; above it `read_blob` fails with `TooLarge`.
pub const MAX_BLOB_BYTES: u64 = 20 * 1024 * 1024;

/// The diff of one file with the GUI defaults (`full_file` = "View the complete file").
pub fn file_diff(
    repo: &Repo,
    target: &DiffTarget,
    full_file: bool,
    cancel: &CancelToken,
) -> Result<FileDiff, GitError> {
    let opts = DiffOptions {
        full_file,
        ..DiffOptions::default()
    };
    gitcthulhu_git::diff::file_diff(repo, target, &opts, cancel)
}

/// A blob's bytes, at most [`MAX_BLOB_BYTES`] (`GitErrorKind::TooLarge` above, `NotFound` when missing).
pub fn read_blob(repo: &Repo, id: ObjectId) -> Result<Arc<[u8]>, GitError> {
    gitcthulhu_git::diff::read_blob(repo, id, MAX_BLOB_BYTES)
}

/// Completes a commit-file target that came without rename information (`RepoUi::diff_target` cannot know
/// renames, 32 T13): the commit's file list (rename detection, cached by the backend and shared with the
/// commit-changes pane) supplies the previous path, and for a stash (`stash = true`) the untracked-files commit
/// of files that only exist there. Uncommitted targets, targets that already carry a previous path and failed
/// lookups are returned unchanged.
pub fn resolve_target(repo: &Repo, target: &DiffTarget, stash: bool) -> DiffTarget {
    let DiffTarget::Commit {
        commit,
        against,
        path,
        previous_path: None,
    } = target
    else {
        return target.clone();
    };
    let opts = CommitFilesOptions {
        line_stats: false,
        stash,
    };
    match gitcthulhu_git::diff::commit_files(repo, *commit, *against, opts) {
        Ok(files) => files
            .iter()
            .find(|f| f.path == *path)
            .map(|f| f.target.clone())
            .unwrap_or_else(|| target.clone()),
        Err(err) => {
            tracing::debug!("commit files for rename lookup failed: {err}");
            target.clone()
        }
    }
}

/// The diff the diff view shows: [`resolve_target`], then [`file_diff`].
pub fn load(
    repo: &Repo,
    target: &DiffTarget,
    stash: bool,
    full_file: bool,
    cancel: &CancelToken,
) -> Result<FileDiff, GitError> {
    let target = resolve_target(repo, target, stash);
    file_diff(repo, &target, full_file, cancel)
}
