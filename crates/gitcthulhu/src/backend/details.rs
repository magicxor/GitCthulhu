//! Commit-details reads (owned by G6 after G0b; spec 21 §2.4).

use super::{CommitDetails, CommitFilesOptions, GitError, ObjectId, Repo};

/// A commit with its message and changed files (no line stats); `stash` appends the untracked files of a stash.
pub fn commit_details(repo: &Repo, id: ObjectId, stash: bool) -> Result<CommitDetails, GitError> {
    gitcthulhu_git::diff::commit_details(
        repo,
        id,
        CommitFilesOptions {
            line_stats: false,
            stash,
        },
    )
}
