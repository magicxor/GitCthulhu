//! File and commit diffs (WP-B2). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use std::sync::Arc;

use bstr::BString;
use gix::ObjectId;

use crate::{
    Against, CancelToken, CommitDetails, CommitFile, CommitFilesOptions, DiffOptions, DiffTarget,
    ExportPatch, FileDiff, PatchText, PatchTextOptions, Repo, Result, WhitespaceMode,
    internal::not_implemented,
};

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions {
            context_lines: 3,
            full_file: false,
            whitespace: WhitespaceMode::None,
            inline_highlights: true,
            max_file_bytes: 50 * 1024 * 1024,
            max_lines: 1_000_000,
            max_image_bytes: 20 * 1024 * 1024,
            algorithm: None,
        }
    }
}

/// The diff of one file.
pub fn file_diff(
    repo: &Repo,
    target: &DiffTarget,
    opts: &DiffOptions,
    cancel: &CancelToken,
) -> Result<FileDiff> {
    not_implemented("diff::file_diff")
}

/// A commit with its message and changed files.
pub fn commit_details(
    repo: &Repo,
    commit: ObjectId,
    opts: CommitFilesOptions,
) -> Result<CommitDetails> {
    not_implemented("diff::commit_details")
}

/// The files a commit changed relative to `against`.
pub fn commit_files(
    repo: &Repo,
    commit: ObjectId,
    against: Against,
    opts: CommitFilesOptions,
) -> Result<Arc<Vec<CommitFile>>> {
    not_implemented("diff::commit_files")
}

/// Compare any two commits/trees ("compare branches", GitKraken). `old = None` => empty tree.
pub fn tree_files(
    repo: &Repo,
    old: Option<ObjectId>,
    new: ObjectId,
    line_stats: bool,
) -> Result<Vec<CommitFile>> {
    not_implemented("diff::tree_files")
}

/// Budgeted staged diff for AI (16 A7): index vs HEAD.
pub fn staged_patch_text(
    repo: &Repo,
    opts: &PatchTextOptions,
    cancel: &CancelToken,
) -> Result<PatchText> {
    not_implemented("diff::staged_patch_text")
}

/// Exact git patch text for "Copy as patch" / "Save patch".
pub fn export_patch(repo: &Repo, what: &ExportPatch, cancel: &CancelToken) -> Result<BString> {
    not_implemented("diff::export_patch")
}

/// Raw blob bytes (image previews, "open file at revision"), refusing above `max_bytes` (TooLarge).
pub fn read_blob(repo: &Repo, id: ObjectId, max_bytes: u64) -> Result<Arc<[u8]>> {
    not_implemented("diff::read_blob")
}
