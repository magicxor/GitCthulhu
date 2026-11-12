//! Blame and file-history reads (owned by G8 after G0b; spec 21 §2.4).

use super::{
    Against, BlameOptions, BlameResult, BlameRev, CancelToken, DiffTarget, FileDiff,
    FileHistoryEntry, FileHistoryPage, FileHistoryWalker, GitError, RelaPath, Repo,
};

/// Blame of `path` at `rev`, following renames.
pub fn blame(
    repo: &Repo,
    path: &RelaPath,
    rev: BlameRev,
    cancel: &CancelToken,
) -> Result<BlameResult, GitError> {
    let opts = BlameOptions {
        follow_renames: true,
        ..BlameOptions::default()
    };
    gitcthulhu_git::blame::blame(repo, path, rev, &opts, cancel)
}

/// Pages through the history of one file (from HEAD, following renames). `Send`.
pub struct HistoryPager {
    walker: FileHistoryWalker,
}

impl HistoryPager {
    /// A pager from HEAD that follows renames.
    pub fn new(repo: &Repo, path: &RelaPath) -> Result<Self, GitError> {
        Ok(Self {
            walker: FileHistoryWalker::new(repo, path, None, true)?,
        })
    }

    /// The next `n` commits that changed the file.
    pub fn next(&mut self, n: usize, cancel: &CancelToken) -> Result<FileHistoryPage, GitError> {
        self.walker.next_page(n, cancel)
    }
}

/// The diff a history entry made to its file: the commit against its first parent, at the file's path in that
/// commit (renames carry the previous path). `full_file` is Gitnuro's "view the complete file".
pub fn commit_diff(
    repo: &Repo,
    entry: &FileHistoryEntry,
    full_file: bool,
    cancel: &CancelToken,
) -> Result<FileDiff, GitError> {
    let target = DiffTarget::Commit {
        commit: entry.commit.id,
        against: Against::FirstParent,
        path: entry.path.clone(),
        previous_path: entry.previous_path.clone(),
    };
    super::diff::file_diff(repo, &target, full_file, cancel)
}
