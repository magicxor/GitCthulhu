use std::{
    borrow::Cow,
    collections::{BTreeSet, HashSet},
    fs::FileType,
    ops::ControlFlow,
    path::{Path, PathBuf},
};

use gix::bstr::BStr;
use notify::RecursiveMode;

#[cfg(test)]
mod tests;

/// Traverse all unignored or tracked directories in `worktree_path` and pass them to `visit_dir(path, notify-mode)`,
/// which can decide to stop the iteration by returning `ControlFlow::Break`.
/// `git_dir` is used to select specific directories for recursive watching first,
/// and other contained `.git` directories are watched similarly, as are the `linked_git_dirs`
/// of linked worktrees.
#[tracing::instrument(skip(repo, visit_dir), level = "debug", err)]
pub(crate) fn compute_watch_plan_for_repo(
    repo: &gix::Repository,
    worktree_path: &Path,
    git_dir: &Path,
    linked_git_dirs: &[&Path],
    mut visit_dir: impl FnMut(&Path, notify::RecursiveMode) -> anyhow::Result<ControlFlow<()>>,
) -> anyhow::Result<()> {
    let index = repo.index_or_empty()?;
    let icase_acc = build_index_icase_accelerator_if_needed(repo, &index);
    let mut excludes = repo.excludes(
        &index,
        None,
        gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
    )?;

    let mut seen = HashSet::new();
    let mut stack = Vec::new();

    // Prioritize watches for git-dir paths so we still get critical git events even when we hit
    // OS watch limits while adding worktree watches.
    if visit_dir(worktree_path, RecursiveMode::NonRecursive)?.is_break() {
        return Ok(());
    }
    if emit_git_dir_watches(git_dir, &mut visit_dir)?.is_break() {
        return Ok(());
    }
    for linked_git_dir in linked_git_dirs.iter().filter(|dir| dir.is_dir()) {
        if emit_git_dir_watches(linked_git_dir, &mut visit_dir)?.is_break() {
            return Ok(());
        }
    }

    seen.insert(worktree_path.to_owned());
    push_child_dirs_sorted(worktree_path, &mut stack);
    while let Some(dir) = stack.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        if dir.starts_with(git_dir) {
            continue;
        }
        let Ok(relative) = dir.strip_prefix(worktree_path) else {
            continue;
        };

        if !relative.as_os_str().is_empty() {
            if relative
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(gix::discover::DOT_GIT_DIR))
            {
                if emit_git_dir_watches(&dir, &mut visit_dir)?.is_break() {
                    continue;
                }
                if dir
                    .ancestors()
                    .filter_map(|d| d.file_name().and_then(|f| f.to_str()))
                    .any(|name| name.eq_ignore_ascii_case(gix::discover::DOT_GIT_DIR))
                {
                    continue;
                }
            }
            let is_excluded = excludes
                .at_path(relative, Some(gix::index::entry::Mode::DIR))
                .is_ok_and(|platform| platform.is_excluded());
            if is_excluded
                && !is_tracked_in_index(
                    to_repo_relative_path(relative)?.as_ref(),
                    true,
                    &index,
                    icase_acc.as_ref(),
                )
            {
                continue;
            }
        }
        if visit_dir(&dir, RecursiveMode::NonRecursive)?.is_break() {
            return Ok(());
        }

        push_child_dirs_sorted(&dir, &mut stack);
    }

    Ok(())
}

/// Emit the watches of a git dir: the non-recursive git dir itself (`HEAD`, `index`, `packed-refs`, `config`,
/// `FETCH_HEAD` and the `*_HEAD` files of running operations, and the creation of `rebase-merge/`, `rebase-apply/`,
/// `sequencer/`), the reflogs we care about, all refs recursively, the directories of running operations if they
/// exist, linked worktree registrations and the repositories of submodules (never their objects).
pub(crate) fn emit_git_dir_watches(
    git_dir: &Path,
    visit_dir: &mut impl FnMut(&Path, RecursiveMode) -> anyhow::Result<ControlFlow<()>>,
) -> anyhow::Result<ControlFlow<()>> {
    // Non-recursive to not pick up objects.
    if visit_dir(git_dir, RecursiveMode::NonRecursive)?.is_break() {
        return Ok(ControlFlow::Break(()));
    }
    // Non-recursive: `logs/HEAD` changes whenever the checked-out branch moves, and `logs/refs/stash` is the reflog
    // of the stash.
    let logs_dir = git_dir.join("logs");
    if logs_dir.is_dir() {
        if visit_dir(&logs_dir, RecursiveMode::NonRecursive)?.is_break() {
            return Ok(ControlFlow::Break(()));
        }
        let logs_refs_dir = logs_dir.join("refs");
        if logs_refs_dir.is_dir()
            && visit_dir(&logs_refs_dir, RecursiveMode::NonRecursive)?.is_break()
        {
            return Ok(ControlFlow::Break(()));
        }
    }
    // Branches, remote-tracking branches, tags, the stash and our own `refs/gitcthulhu/*`. Recursive, which also
    // covers directories created later.
    let refs_dir = git_dir.join("refs");
    if refs_dir.is_dir() && visit_dir(&refs_dir, RecursiveMode::Recursive)?.is_break() {
        return Ok(ControlFlow::Break(()));
    }
    // Directories that exist while an operation runs. If they are created later, the dynamic watch path adds them.
    for name in crate::file_monitor::STATE_DIRS {
        let dir = git_dir.join(name);
        if dir.is_dir() && visit_dir(&dir, RecursiveMode::NonRecursive)?.is_break() {
            return Ok(ControlFlow::Break(()));
        }
    }
    // Watch where linked worktrees are registered so their addition and removal is detected.
    let linked_worktrees_dir = git_dir.join(crate::LINKED_WORKTREES_DIR);
    if linked_worktrees_dir.is_dir()
        && visit_dir(&linked_worktrees_dir, RecursiveMode::NonRecursive)?.is_break()
    {
        return Ok(ControlFlow::Break(()));
    }
    // Submodule repositories: `modules/`, each `modules/<name>/` (its `HEAD` and `index`) and its refs. Their
    // object databases are deliberately not watched: a fetch in a submodule would flood the debouncer.
    let modules_dir = git_dir.join(crate::MODULES_DIR);
    if modules_dir.is_dir() {
        if visit_dir(&modules_dir, RecursiveMode::NonRecursive)?.is_break() {
            return Ok(ControlFlow::Break(()));
        }
        return emit_module_watches(&modules_dir, 0, visit_dir);
    }
    Ok(ControlFlow::Continue(()))
}

/// How deep submodule names with slashes (`modules/libs/foo`) and nested submodules (`modules/a/modules/b`) are
/// followed.
const MAX_MODULE_DEPTH: usize = 8;

/// Watch the directories below `dir` (a `modules/` directory, or a directory on the way to a submodule repository
/// when the submodule name contains slashes). A directory with a `HEAD` is a submodule repository: it is watched
/// non-recursively, its `refs/` recursively and its own `modules/` like this one. Nothing else in it, in particular
/// not its `objects/`.
fn emit_module_watches(
    dir: &Path,
    depth: usize,
    visit_dir: &mut impl FnMut(&Path, RecursiveMode) -> anyhow::Result<ControlFlow<()>>,
) -> anyhow::Result<ControlFlow<()>> {
    if depth >= MAX_MODULE_DEPTH {
        return Ok(ControlFlow::Continue(()));
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(ControlFlow::Continue(()));
    };
    let mut names = BTreeSet::new();
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(is_watchable_directory) {
            names.insert(entry.file_name());
        }
    }
    for name in names {
        let sub = dir.join(name);
        if visit_dir(&sub, RecursiveMode::NonRecursive)?.is_break() {
            return Ok(ControlFlow::Break(()));
        }
        if sub.join("HEAD").is_file() {
            let refs = sub.join("refs");
            if refs.is_dir() && visit_dir(&refs, RecursiveMode::Recursive)?.is_break() {
                return Ok(ControlFlow::Break(()));
            }
            let nested = sub.join(crate::MODULES_DIR);
            if nested.is_dir() {
                if visit_dir(&nested, RecursiveMode::NonRecursive)?.is_break() {
                    return Ok(ControlFlow::Break(()));
                }
                if emit_module_watches(&nested, depth + 1, visit_dir)?.is_break() {
                    return Ok(ControlFlow::Break(()));
                }
            }
        } else if emit_module_watches(&sub, depth + 1, visit_dir)?.is_break() {
            return Ok(ControlFlow::Break(()));
        }
    }
    Ok(ControlFlow::Continue(()))
}

fn push_child_dirs_sorted(dir: &Path, stack: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children = BTreeSet::new();
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if is_watchable_directory(file_type) {
            children.insert(entry.file_name());
        }
    }
    stack.extend(children.into_iter().map(|child_dir| dir.join(child_dir)));
}

pub(crate) fn is_watchable_directory(file_type: FileType) -> bool {
    file_type.is_dir() && !file_type.is_symlink()
}

pub(crate) fn build_index_icase_accelerator_if_needed<'index>(
    repo: &gix::Repository,
    index: &'index gix::index::State,
) -> Option<gix::index::AccelerateLookup<'index>> {
    repo.filesystem_options()
        .ok()
        .and_then(|opts| opts.ignore_case.then(|| index.prepare_icase_backing()))
}

/// `relative_path` is a `/` separated repo-relative path, which is considered a directory if
/// `is_dir` is `true`.
/// If `icase_acc` is set, the lookup will be case-insensitive.
pub(crate) fn is_tracked_in_index(
    relative_path: &BStr,
    is_dir: bool,
    index: &gix::index::State,
    icase_acc: Option<&gix::index::AccelerateLookup>,
) -> bool {
    if let Some(icase_acc) = icase_acc {
        if is_dir {
            index.path_is_directory_icase(relative_path, true, icase_acc)
        } else {
            index
                .entry_by_path_icase(relative_path, true, icase_acc)
                .is_some()
        }
    } else if is_dir {
        index.path_is_directory(relative_path)
    } else {
        index.entry_by_path(relative_path).is_some()
    }
}

pub(crate) fn to_repo_relative_path(path: &Path) -> gix::Result<Cow<'_, BStr>> {
    Ok(gix::path::to_unix_separators_on_windows(
        gix::path::into_bstr(path)?,
    ))
}
