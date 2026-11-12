//! Blame and file-history reads (owned by G8 after G0b; spec 21 §2.4).

use super::{
    BlameOptions, BlameResult, BlameRev, CancelToken, FileHistoryPage, FileHistoryWalker, GitError,
    RelaPath, Repo,
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
