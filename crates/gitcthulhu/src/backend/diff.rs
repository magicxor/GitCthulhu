//! Diff reads (owned by G7a after G0b; spec 21 §2.4, §4.1).

use std::sync::Arc;

use super::{CancelToken, DiffOptions, DiffTarget, FileDiff, GitError, ObjectId, Repo};

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
