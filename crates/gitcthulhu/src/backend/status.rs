//! Status-pane reads and writes (owned by G5 after G0b; spec 21 §2.4).

use std::io::{BufRead as _, BufReader};

use super::{ByteSlice as _, GitError, RelaPath, Repo};

/// The HEAD commit's full message, for the Amend prefill (`log::head_message`).
pub fn head_message(repo: &Repo) -> Result<Option<String>, GitError> {
    gitcthulhu_git::log::head_message(repo)
}

/// Whether the index has unmerged entries (`state::has_conflicts`).
pub fn has_conflicts(repo: &Repo) -> Result<bool, GitError> {
    gitcthulhu_git::state::has_conflicts(repo)
}

/// Persists the commit-box draft (`commit::save_draft_message`; the caller debounces by 1 s).
pub fn save_draft(repo: &Repo, message: Option<&str>) -> Result<(), GitError> {
    gitcthulhu_git::commit::save_draft_message(repo, message)
}

/// Whether the worktree file still contains a conflict marker (`<<<<<<< ` at the start of a line, 20 §2.6 resolve).
/// `false` when the file cannot be read.
pub fn file_has_conflict_markers(repo: &Repo, path: &RelaPath) -> bool {
    let Some(workdir) = repo.workdir() else {
        return false;
    };
    let Ok(relative) = path.to_path() else {
        return false;
    };
    let Ok(file) = std::fs::File::open(workdir.join(relative)) else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) if line.starts_with(b"<<<<<<< ") => return true,
            Ok(_) => {}
        }
    }
}
