//! Repository state detection (WP-B1): HEAD, the operation in progress and its details (spec §1.10).
//!
//! A pure read. Everything is read from the per-worktree git directory, so linked worktrees report their own state.

use std::path::Path;

use bstr::ByteSlice;
use gix::{ObjectId, refs::FullName};
use smallvec::SmallVec;

use crate::{
    BisectState, FixupFlag, GitError, HeadState, InProgress, MergeState, RebaseBackend,
    RebaseState, RebaseStopReason, Repo, RepoFlags, RepoKind, RepoState, Result, SequencerKind,
    SequencerState, TodoAction, TodoLine, internal::errors::gix_error,
};

/// HEAD, the operation in progress and its details.
///
/// Errors: `Io` (unreadable state files), `Other` (gix failures).
pub fn repo_state(repo: &Repo) -> Result<RepoState> {
    let local = repo.local();
    let git_dir = repo.git_dir();
    let head = head_state(&local)?;
    let in_progress = local.state().map(InProgress::from);
    let conflicted_paths = conflicted_paths(&local)?;

    let merge = read_merge(git_dir)?;
    let sequencer = read_sequencer(git_dir, &local)?;
    let rebase = read_rebase(git_dir, &local, conflicted_paths > 0)?;
    let bisect = read_bisect(git_dir)?;

    let flags = RepoFlags {
        bare: local.is_bare(),
        shallow: local.is_shallow().map_err(gix_error)?,
        linked_worktree: matches!(repo.kind(), RepoKind::LinkedWorktree { .. }),
        sparse: local
            .config_snapshot()
            .boolean("core.sparseCheckout")
            .unwrap_or(false),
        has_submodules: repo
            .workdir()
            .is_some_and(|workdir| workdir.join(".gitmodules").is_file()),
        commit_graph: {
            let info = repo.common_dir().join("objects").join("info");
            info.join("commit-graph").is_file() || info.join("commit-graphs").is_dir()
        },
    };
    Ok(RepoState {
        head,
        in_progress,
        merge,
        sequencer,
        rebase,
        bisect,
        conflicted_paths,
        flags,
    })
}

/// The rebase in progress in `repo`'s own git directory (what [`repo_state`] reports as `rebase`), without reading
/// anything else. `None` when no rebase runs, and for `git am` (which shares `rebase-apply/`).
pub(crate) fn rebase_details(repo: &Repo) -> Result<Option<RebaseState>> {
    let local = repo.local();
    read_rebase(repo.git_dir(), &local, has_conflicts(repo)?)
}

/// Cheap: scans the index for stage != 0.
///
/// A bare repository has no index and therefore no conflicts.
pub fn has_conflicts(repo: &Repo) -> Result<bool> {
    let local = repo.local();
    if local.is_bare() {
        return Ok(false);
    }
    let index = local.index_or_empty().map_err(gix_error)?;
    Ok(index
        .entries()
        .iter()
        .any(|entry| entry.flags.stage_raw() != 0))
}

fn head_state(local: &gix::Repository) -> Result<HeadState> {
    let head = local.head().map_err(gix_error)?;
    Ok(match &head.kind {
        gix::head::Kind::Unborn(name) => HeadState::Unborn { name: name.clone() },
        gix::head::Kind::Detached { target, .. } => HeadState::Detached { target: *target },
        gix::head::Kind::Symbolic(reference) => {
            let name = reference.name.clone();
            let target = local.head_id().map_err(gix_error)?.detach();
            HeadState::Branch { name, target }
        }
    })
}

/// Number of distinct paths with unmerged entries.
fn conflicted_paths(local: &gix::Repository) -> Result<usize> {
    if local.is_bare() {
        return Ok(0);
    }
    let index = local.index_or_empty().map_err(gix_error)?;
    // Entries are sorted by (path, stage), so the stages of one path are adjacent.
    let mut count = 0;
    let mut last = None;
    for entry in index.entries() {
        if entry.flags.stage_raw() != 0 {
            let path = entry.path(&index);
            if last != Some(path) {
                count += 1;
                last = Some(path);
            }
        }
    }
    Ok(count)
}

// ───────────────────────────── file helpers ─────────────────────────────

/// The file's content, `None` if it does not exist.
fn read_opt(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        // A directory where a file is expected (or the reverse) is "not there" for our purposes.
        Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => Ok(None),
        Err(err) => Err(GitError::io(path, err)),
    }
}

fn read_text(path: &Path) -> Result<Option<String>> {
    Ok(read_opt(path)?.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}

fn read_trimmed(path: &Path) -> Result<Option<String>> {
    Ok(read_text(path)?
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty()))
}

fn read_id(path: &Path) -> Result<Option<ObjectId>> {
    Ok(read_trimmed(path)?.and_then(|text| ObjectId::from_hex(text.as_bytes()).ok()))
}

fn read_full_name(path: &Path) -> Result<Option<FullName>> {
    Ok(read_trimmed(path)?
        .filter(|name| name != "detached HEAD")
        .and_then(|name| FullName::try_from(name.as_str()).ok()))
}

fn read_number(path: &Path) -> Result<Option<u32>> {
    Ok(read_trimmed(path)?.and_then(|text| text.parse().ok()))
}

fn read_todo(path: &Path, local: &gix::Repository) -> Result<Vec<TodoLine>> {
    let Some(text) = read_text(path)? else {
        return Ok(Vec::new());
    };
    Ok(text
        .lines()
        .filter_map(parse_todo_line)
        .filter(|line| line.action != TodoAction::Comment)
        .map(|line| resolve(line, local))
        .collect())
}

/// Resolve an abbreviated id; an unknown or ambiguous one leaves `commit` unset.
fn resolve(mut line: TodoLine, local: &gix::Repository) -> TodoLine {
    if line.commit.is_none()
        && let Some(abbrev) = line.abbrev.as_deref()
        && is_hex(abbrev)
        && let Ok(id) = local.rev_parse_single(abbrev.as_bytes().as_bstr())
    {
        line.commit = Some(id.detach());
    }
    line
}

// ───────────────────────────── merge / sequencer ─────────────────────────────

fn read_merge(git_dir: &Path) -> Result<Option<MergeState>> {
    let Some(heads) = read_text(&git_dir.join("MERGE_HEAD"))? else {
        return Ok(None);
    };
    let heads: SmallVec<[ObjectId; 1]> = heads
        .lines()
        .filter_map(|line| ObjectId::from_hex(line.trim().as_bytes()).ok())
        .collect();
    Ok(Some(MergeState {
        heads,
        message: read_text(&git_dir.join("MERGE_MSG"))?,
        no_ff: read_text(&git_dir.join("MERGE_MODE"))?
            .is_some_and(|mode| mode.split_whitespace().any(|word| word == "no-ff")),
        autostash: git_dir.join("MERGE_AUTOSTASH").is_file(),
    }))
}

fn read_sequencer(git_dir: &Path, local: &gix::Repository) -> Result<Option<SequencerState>> {
    let cherry_pick = read_id(&git_dir.join("CHERRY_PICK_HEAD"))?;
    let revert = read_id(&git_dir.join("REVERT_HEAD"))?;
    let todo_path = git_dir.join("sequencer").join("todo");
    let todo = if todo_path.is_file() {
        Some(read_todo(&todo_path, local)?)
    } else {
        None
    };
    if cherry_pick.is_none()
        && revert.is_none()
        && todo.is_none()
        && !git_dir.join("CHERRY_PICK_HEAD").exists()
        && !git_dir.join("REVERT_HEAD").exists()
    {
        return Ok(None);
    }
    let kind = if cherry_pick.is_some() || git_dir.join("CHERRY_PICK_HEAD").exists() {
        SequencerKind::CherryPick
    } else if revert.is_some() || git_dir.join("REVERT_HEAD").exists() {
        SequencerKind::Revert
    } else {
        match todo.as_ref().and_then(|todo| todo.first()) {
            Some(TodoLine {
                action: TodoAction::Revert,
                ..
            }) => SequencerKind::Revert,
            _ => SequencerKind::CherryPick,
        }
    };
    let current = cherry_pick.or(revert);
    // While a step is stopped its own line is still the head of the todo; "remaining" excludes it.
    let mut remaining = todo.unwrap_or_default();
    if let Some(current) = current
        && remaining
            .first()
            .is_some_and(|first| first.commit == Some(current))
    {
        remaining.remove(0);
    }
    Ok(Some(SequencerState {
        kind,
        current,
        remaining,
        message: read_text(&git_dir.join("MERGE_MSG"))?,
    }))
}

// ───────────────────────────── rebase ─────────────────────────────

fn read_rebase(
    git_dir: &Path,
    local: &gix::Repository,
    has_conflicts: bool,
) -> Result<Option<RebaseState>> {
    let merge_dir = git_dir.join("rebase-merge");
    let apply_dir = git_dir.join("rebase-apply");
    if apply_dir.join("applying").exists() {
        // `git am`: not a rebase.
        return Ok(None);
    }
    // The commit whose application is being resolved; Git removes it when the step is finished.
    let rebase_head = read_id(&git_dir.join("REBASE_HEAD"))?;
    let started_by_us = git_dir
        .join("gitcthulhu")
        .join("rebase")
        .join("plan.json")
        .is_file();
    if merge_dir.is_dir() {
        let done = read_todo(&merge_dir.join("done"), local)?;
        let todo = read_todo(&merge_dir.join("git-rebase-todo"), local)?;
        let stopped_sha = read_trimmed(&merge_dir.join("stopped-sha"))?;
        let stopped_at = rebase_head
            .or_else(|| {
                stopped_sha
                    .as_deref()
                    .and_then(|sha| resolve_abbrev(local, sha))
            })
            .or_else(|| done.last().and_then(|line| line.commit));
        let amend = read_id(&merge_dir.join("amend"))?;
        let stop_reason = if has_conflicts {
            RebaseStopReason::Conflict
        } else if merge_dir.join("amend").exists() {
            RebaseStopReason::Edit
        } else if let Some(last) = done.last() {
            match last.action {
                TodoAction::Break => RebaseStopReason::Break,
                TodoAction::Exec => RebaseStopReason::ExecFailed,
                _ if stopped_sha.is_some() => RebaseStopReason::ResolvedNeedsContinue,
                _ => RebaseStopReason::Unknown,
            }
        } else if stopped_sha.is_some() {
            RebaseStopReason::ResolvedNeedsContinue
        } else {
            RebaseStopReason::Unknown
        };
        let step = match (
            read_number(&merge_dir.join("msgnum"))?,
            read_number(&merge_dir.join("end"))?,
        ) {
            (Some(current), Some(end)) => Some((current, end)),
            _ => None,
        };
        return Ok(Some(RebaseState {
            backend: RebaseBackend::Merge,
            interactive: merge_dir.join("interactive").exists(),
            head_name: read_full_name(&merge_dir.join("head-name"))?,
            onto: read_id(&merge_dir.join("onto"))?,
            orig_head: read_id(&merge_dir.join("orig-head"))?,
            step,
            done,
            todo,
            stopped_at,
            amend,
            stop_reason,
            message: read_text(&merge_dir.join("message"))?,
            started_by_us,
        }));
    }
    if apply_dir.is_dir() {
        let step = match (
            read_number(&apply_dir.join("next"))?,
            read_number(&apply_dir.join("last"))?,
        ) {
            (Some(next), Some(last)) => Some((next, last)),
            _ => None,
        };
        return Ok(Some(RebaseState {
            backend: RebaseBackend::Apply,
            interactive: false,
            head_name: read_full_name(&apply_dir.join("head-name"))?,
            onto: read_id(&apply_dir.join("onto"))?,
            orig_head: read_id(&apply_dir.join("orig-head"))?,
            step,
            done: Vec::new(),
            todo: Vec::new(),
            stopped_at: rebase_head,
            amend: None,
            stop_reason: if has_conflicts {
                RebaseStopReason::Conflict
            } else if apply_dir.join("next").exists() {
                RebaseStopReason::ResolvedNeedsContinue
            } else {
                RebaseStopReason::Unknown
            },
            message: None,
            started_by_us,
        }));
    }
    Ok(None)
}

fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn resolve_abbrev(local: &gix::Repository, abbrev: &str) -> Option<ObjectId> {
    if !is_hex(abbrev) {
        return None;
    }
    local
        .rev_parse_single(abbrev.as_bytes().as_bstr())
        .ok()
        .map(|id| id.detach())
}

// ───────────────────────────── bisect ─────────────────────────────

fn read_bisect(git_dir: &Path) -> Result<Option<BisectState>> {
    let start = read_trimmed(&git_dir.join("BISECT_START"))?;
    if start.is_none() && !git_dir.join("BISECT_LOG").exists() {
        return Ok(None);
    }
    let terms = read_text(&git_dir.join("BISECT_TERMS"))?
        .map(|text| {
            let mut lines = text.lines().map(|line| line.trim().to_owned());
            (lines.next(), lines.next())
        })
        .and_then(|(bad, good)| {
            Some((
                bad.filter(|t| !t.is_empty())?,
                good.filter(|t| !t.is_empty())?,
            ))
        })
        .unwrap_or_else(|| ("bad".to_owned(), "good".to_owned()));
    Ok(Some(BisectState { terms, start }))
}

// ───────────────────────────── todo lines ─────────────────────────────

/// Parse one line of a rebase/sequencer todo list; pure; `None` for blank lines.
///
/// `commit` is set only when the line carries a full-length object id; abbreviated ids stay in `abbrev` (they are
/// resolved by [`repo_state()`]). `rest` is what follows the command and its commit: the subject of a `pick`, the
/// label of `label`/`reset`/`merge`, the command of `exec`. Comment lines have action `Comment` and `rest` = the
/// whole line.
pub fn parse_todo_line(line: &str) -> Option<TodoLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw = line.trim_end_matches(['\r', '\n']).to_owned();
    if trimmed.starts_with('#') {
        return Some(TodoLine {
            action: TodoAction::Comment,
            commit: None,
            abbrev: None,
            rest: trimmed.to_owned(),
            raw,
        });
    }
    let (word, mut tail) = split_word(trimmed);
    let action = match word {
        "p" | "pick" => TodoAction::Pick,
        "revert" => TodoAction::Revert,
        "r" | "reword" => TodoAction::Reword,
        "e" | "edit" => TodoAction::Edit,
        "s" | "squash" => TodoAction::Squash,
        "f" | "fixup" => TodoAction::Fixup(FixupFlag::None),
        "x" | "exec" => TodoAction::Exec,
        "b" | "break" => TodoAction::Break,
        "d" | "drop" => TodoAction::Drop,
        "l" | "label" => TodoAction::Label,
        "t" | "reset" => TodoAction::Reset,
        "m" | "merge" => TodoAction::Merge,
        "u" | "update-ref" => TodoAction::UpdateRef,
        "noop" => TodoAction::Noop,
        other => TodoAction::Unknown(other.to_owned()),
    };
    let mut action = action;
    // Commands whose first argument is a commit.
    let takes_commit = matches!(
        action,
        TodoAction::Pick
            | TodoAction::Revert
            | TodoAction::Reword
            | TodoAction::Edit
            | TodoAction::Squash
            | TodoAction::Drop
            | TodoAction::Fixup(_)
    );
    let mut abbrev = None;
    if matches!(action, TodoAction::Fixup(_)) {
        let (flag, after) = split_word(tail);
        let flag = match flag {
            "-C" => Some(FixupFlag::UseMessage),
            "-c" => Some(FixupFlag::EditMessage),
            _ => None,
        };
        if let Some(flag) = flag {
            action = TodoAction::Fixup(flag);
            tail = after;
        }
    }
    if takes_commit {
        let (id, after) = split_word(tail);
        if !id.is_empty() {
            abbrev = Some(id.to_owned());
            tail = after;
        }
    } else if matches!(action, TodoAction::Merge) {
        // `merge [-C <commit> | -c <commit>] <label> [# <oneline>]`
        let (flag, after) = split_word(tail);
        if flag == "-C" || flag == "-c" {
            let (id, after) = split_word(after);
            if !id.is_empty() {
                abbrev = Some(id.to_owned());
                tail = after;
            }
        }
    }
    let commit = abbrev
        .as_deref()
        .filter(|id| matches!(id.len(), 40 | 64))
        .and_then(|id| ObjectId::from_hex(id.as_bytes()).ok());
    Some(TodoLine {
        action,
        commit,
        abbrev,
        rest: tail.trim().to_owned(),
        raw,
    })
}

/// `(first whitespace separated word, the rest with leading whitespace removed)`.
fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    match text.find(char::is_whitespace) {
        Some(end) => (&text[..end], text[end..].trim_start()),
        None => (text, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_line_commands() {
        let pick = parse_todo_line("pick abc1234 Subject line").expect("a pick line");
        assert_eq!(pick.action, TodoAction::Pick, "pick");
        assert_eq!(pick.abbrev.as_deref(), Some("abc1234"), "abbrev");
        assert_eq!(pick.rest, "Subject line", "subject");
        assert_eq!(pick.commit, None, "abbreviated ids are not resolved here");
        assert_eq!(
            parse_todo_line("p abc1234 s").expect("short form").action,
            TodoAction::Pick,
            "short command"
        );
        assert!(parse_todo_line("   ").is_none(), "blank");
    }
}
