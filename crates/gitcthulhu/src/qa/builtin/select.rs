//! Selection verbs: log rows, refs, files and the views opened from them.

use gpui_kit::{App, Modifiers, Window};

use super::active_ctx;
use super::commits::{find_commit, local_branch, log_row, remote_branch, stash, tag};
use crate::backend::{BlameRev, ByteSlice as _, RelaPath};
use crate::qa::verbs::{Args, Verb, register_verb};
use crate::tab::TabCtx;
use crate::tab::ui::{FileList, RepoUiEvent, Selection, SideExpansion};

/// Registers the verbs of this file.
pub(super) fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "expand side sections",
            "expand side sections",
            "Expands every section of the side panel (Local branches, Remotes, Tags, Stashes, Submodules) and each remote",
            |_, cx, _| {
                let ctx = active_ctx(cx)?;
                ctx.ui.update(cx, |ui, cx| {
                    ui.side_expanded = SideExpansion {
                        local: true,
                        remotes: true,
                        tags: true,
                        stashes: true,
                        submodules: true,
                        worktrees: ui.side_expanded.worktrees,
                    };
                    ui.remotes_collapsed.clear();
                    cx.emit(RepoUiEvent::TogglesChanged);
                    cx.notify();
                });
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open rebase editor",
            "open rebase editor <base commit: hex prefix | summary text>",
            "Opens the interactive rebase editor for the commits above the base (the log menu's \"Rebase interactive\")",
            |_, cx, args| {
                let query = args.rest(0);
                if query.is_empty() {
                    return Err("missing argument: base commit".into());
                }
                let ctx = active_ctx(cx)?;
                let (base, _) = find_commit(ctx.model.read(cx), &query)?;
                crate::rebase_interactive::open(&ctx, Some(base), cx);
                Ok(base.to_hex_with_len(7).to_string())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select uncommitted row",
            "select uncommitted row",
            "Selects the uncommitted-changes row of the log (the status pane shows on the right)",
            |_, cx, _| {
                let ctx = active_ctx(cx)?;
                ctx.ui
                    .update(cx, |ui, cx| ui.select(Selection::UncommittedChanges, cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select log row",
            "select log row <N>",
            "Selects the N-th visible row of the log, counting from 0 (row 0 is the uncommitted row while it is shown)",
            |_, cx, args| {
                let visible: usize = args.number(0, "row number")?;
                let ctx = active_ctx(cx)?;
                let selection = {
                    let model = ctx.model.read(cx);
                    let (_, row) = log_row(model, visible)?;
                    match &row.commit {
                        None => Selection::UncommittedChanges,
                        Some(commit) => Selection::Commit {
                            id: commit.id,
                            is_stash: commit.is_stash,
                            scroll_to: true,
                        },
                    }
                };
                let label = describe(&selection);
                ctx.ui.update(cx, |ui, cx| ui.select(selection, cx));
                Ok(label)
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select commit",
            "select commit <hex prefix | summary text>",
            "Selects the first loaded commit with that id prefix or that text in its summary",
            |_, cx, args| {
                let query = args.rest(0);
                if query.is_empty() {
                    return Err("missing argument: id prefix or summary text".into());
                }
                let ctx = active_ctx(cx)?;
                let (id, is_stash) = find_commit(ctx.model.read(cx), &query)?;
                ctx.ui.update(cx, |ui, cx| {
                    ui.select(
                        Selection::Commit {
                            id,
                            is_stash,
                            scroll_to: true,
                        },
                        cx,
                    )
                });
                Ok(id.to_string())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select branch",
            "select branch <name>",
            "Selects a local branch (or, as remote/name, a remote-tracking branch) like a click in the side panel",
            |_, cx, args| {
                let name = args.text(0, "branch name")?;
                let ctx = active_ctx(cx)?;
                let selection = {
                    let model = ctx.model.read(cx);
                    match local_branch(model, &name) {
                        Ok(b) => Selection::Branch {
                            full: b.full,
                            target: b.target,
                        },
                        Err(local_error) => match remote_branch(model, &name) {
                            Ok(b) => Selection::Branch {
                                full: b.full,
                                target: b.target,
                            },
                            Err(_) => return Err(local_error),
                        },
                    }
                };
                ctx.ui.update(cx, |ui, cx| ui.select(selection, cx));
                Ok(name)
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select tag",
            "select tag <name>",
            "Selects a tag like a click in the side panel",
            |_, cx, args| {
                let name = args.text(0, "tag name")?;
                let ctx = active_ctx(cx)?;
                let selection = {
                    let t = tag(ctx.model.read(cx), &name)?;
                    Selection::Tag {
                        full: t.full,
                        target: t.target,
                    }
                };
                ctx.ui.update(cx, |ui, cx| ui.select(selection, cx));
                Ok(name)
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select stash",
            "select stash <index>",
            "Selects a stash by its index (stash@{index})",
            |_, cx, args| {
                let index: usize = args.number(0, "stash index")?;
                let ctx = active_ctx(cx)?;
                let selection = {
                    let s = stash(ctx.model.read(cx), index)?;
                    Selection::Stash {
                        id: s.id,
                        index: s.index,
                    }
                };
                ctx.ui.update(cx, |ui, cx| ui.select(selection, cx));
                Ok(format!("stash@{{{index}}}"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select file",
            "select file staged|unstaged|commit <path>",
            "Selects a file in the staged, unstaged or commit-files list (a single selected file opens its diff)",
            |_, cx, args| {
                let list = parse_list(&args.text(0, "staged, unstaged or commit")?)?;
                let path = args.rest(1);
                if path.is_empty() {
                    return Err("missing argument: file path".into());
                }
                select_file(cx, list, &path)
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "clear file selection",
            "clear file selection",
            "Clears the file selection (the diff closes)",
            |_, cx, _| {
                let ctx = active_ctx(cx)?;
                ctx.ui.update(cx, |ui, cx| ui.clear_files(cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open diff",
            "open diff [staged|unstaged|commit] [path]",
            "Opens the diff of a file: the given one, else the first unstaged file, else the first staged file",
            open_diff,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open blame",
            "open blame <path>",
            "Opens the blame view of a file (worktree version)",
            |_, cx, args| {
                let path = RelaPath::from(args.text(0, "file path")?.as_str());
                let ctx = active_ctx(cx)?;
                crate::blame::open(&ctx, path, BlameRev::Worktree, cx);
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open history",
            "open history <path>",
            "Opens the file history view of a file",
            |_, cx, args| {
                let path = RelaPath::from(args.text(0, "file path")?.as_str());
                let ctx = active_ctx(cx)?;
                crate::file_history::open(&ctx, path, cx);
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "close view",
            "close view",
            "Closes the most recently opened closeable view (Escape): search, diff, blame, history",
            |_, cx, _| {
                let ctx = active_ctx(cx)?;
                let closed = ctx.ui.update(cx, |ui, cx| ui.close_last_view(cx));
                Ok(if closed { "closed" } else { "nothing to close" }.to_owned())
            },
        ),
    );
}

/// A short description of a selection for the report.
fn describe(selection: &Selection) -> String {
    match selection {
        Selection::None => "none".into(),
        Selection::UncommittedChanges => "uncommitted changes".into(),
        Selection::Commit { id, .. } => format!("commit {id}"),
        Selection::Branch { full, .. } => format!("branch {}", full.as_bstr()),
        Selection::Tag { full, .. } => format!("tag {}", full.as_bstr()),
        Selection::Stash { index, .. } => format!("stash@{{{index}}}"),
    }
}

/// `staged`, `unstaged`, `commit`.
fn parse_list(word: &str) -> Result<FileList, String> {
    match word.to_lowercase().as_str() {
        "staged" => Ok(FileList::Staged),
        "unstaged" => Ok(FileList::Unstaged),
        "commit" | "commit-files" => Ok(FileList::CommitFiles),
        other => Err(format!(
            "unknown file list '{other}' (staged, unstaged or commit)"
        )),
    }
}

/// Clicks `path` in `list` (a plain click: single selection).
fn select_file(cx: &mut App, list: FileList, path: &str) -> Result<String, String> {
    let ctx = active_ctx(cx)?;
    check_file(&ctx, cx, list, path)?;
    let rela = RelaPath::from(path);
    let uncommitted = matches!(list, FileList::Staged | FileList::Unstaged);
    ctx.ui.update(cx, |ui, cx| {
        if uncommitted && ui.selection != Selection::UncommittedChanges {
            ui.select(Selection::UncommittedChanges, cx);
        }
        ui.click_file(
            list,
            rela.clone(),
            Modifiers::default(),
            std::slice::from_ref(&rela),
            cx,
        );
    });
    Ok(format!("{list:?} {path}"))
}

/// The status lists are known here: a typo in the path fails the step instead of selecting nothing.
fn check_file(ctx: &TabCtx, cx: &App, list: FileList, path: &str) -> Result<(), String> {
    let model = ctx.model.read(cx);
    let status = || {
        model
            .status
            .value
            .clone()
            .ok_or_else(|| "the status is not loaded".to_owned())
    };
    let entries = match list {
        FileList::Staged => status()?.staged.clone(),
        FileList::Unstaged => status()?.unstaged.clone(),
        FileList::CommitFiles => {
            return if ctx.ui.read(cx).selection.commit().is_some() {
                Ok(())
            } else {
                Err("select a commit first".to_owned())
            };
        }
    };
    if entries.iter().any(|e| e.path.to_str_lossy() == path) {
        Ok(())
    } else {
        let known: Vec<String> = entries
            .iter()
            .map(|e| e.path.to_str_lossy().into_owned())
            .collect();
        Err(format!(
            "{path} is not in the {list:?} list (have: {})",
            known.join(", ")
        ))
    }
}

/// `open diff`.
fn open_diff(_: &mut Window, cx: &mut App, args: &Args) -> Result<String, String> {
    if let Some(word) = args.opt(0) {
        let list = parse_list(&word)?;
        let path = args.rest(1);
        if path.is_empty() {
            return Err("missing argument: file path".into());
        }
        return select_file(cx, list, &path);
    }
    let ctx = active_ctx(cx)?;
    let first = {
        let model = ctx.model.read(cx);
        let status = model
            .status
            .value
            .clone()
            .ok_or_else(|| "the status is not loaded".to_owned())?;
        status
            .unstaged
            .first()
            .map(|e| (FileList::Unstaged, e.path.to_str_lossy().into_owned()))
            .or_else(|| {
                status
                    .staged
                    .first()
                    .map(|e| (FileList::Staged, e.path.to_str_lossy().into_owned()))
            })
    };
    let (list, path) = first.ok_or_else(|| "there is no changed file to open".to_owned())?;
    select_file(cx, list, &path)
}
