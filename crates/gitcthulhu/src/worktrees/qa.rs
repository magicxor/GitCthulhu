//! QA scenario verbs of the worktree feature (registered from `worktrees::register`, feature `qa` only). They call
//! the same tab operations and menu builders as the GUI; nothing injects OS input.
//!
//! | Verb | Effect |
//! |---|---|
//! | `expand worktrees section` | expands the side panel's Worktrees section |
//! | `worktree add <path> existing <branch> \| new <name> [from <ref>] \| detach <ref\|head> [lock [reason]]` | `git worktree add` |
//! | `worktree lock <name> [reason]`, `worktree unlock <name>`, `worktree remove <name> [force]`, `worktree prune` | the other operations |
//! | `worktree vanish <name>` | deletes a linked worktree's directory (sandbox only) so it becomes prunable |
//! | `open worktree <name>` | opens the worktree in a new tab |
//! | `open worktree menu <name> [at x y]`, `open worktrees menu [at x y]` | the row and header menus |
//! | `assert worktree <name> [current\|locked\|unlocked\|prunable\|detached\|branch <b>]`, `assert worktree count <n>` | checks |

use std::path::{Path, PathBuf};

use gpui_kit::{App, Window, point, px};

use super::{checked_out_branch, find_by_name, open_in_new_tab};
use crate::app_view::AppViewGlobal;
use crate::backend::{
    FullName, HeadState, ObjectId, StartPoint, WorktreeAddOptions, WorktreeInfo, WorktreeTarget,
};
use crate::menus;
use crate::menus::worktree as worktree_menus;
use crate::qa::verbs::{Args, StepResult, Verb, register_verb};
use crate::tab::TabCtx;
use crate::tab::ui::{RepoUiEvent, SideExpansion};

fn ctx_of(cx: &App) -> Result<TabCtx, String> {
    let view = cx
        .try_global::<AppViewGlobal>()
        .and_then(|g| g.0.upgrade())
        .ok_or_else(|| "the window content is gone".to_owned())?;
    let view = view.read(cx);
    let tab = view
        .active_tab()
        .ok_or_else(|| "the selected tab is not a repository (open one first)".to_owned())?;
    Ok(tab.read(cx).ctx().clone())
}

/// Scenarios only write below the sandbox: a new worktree's directory and a vanished one must lie inside it.
fn inside_sandbox(args: &Args, path: &Path) -> Result<(), String> {
    if crate::qa::gate::is_inside(args.sandbox(), path) {
        Ok(())
    } else {
        Err(format!(
            "{} is outside the QA sandbox {} (give an absolute path such as $REPOS/<name>)",
            path.display(),
            args.sandbox().display()
        ))
    }
}

fn worktree_named(ctx: &TabCtx, cx: &App, name: &str) -> Result<WorktreeInfo, String> {
    find_by_name(ctx, name, cx).ok_or_else(|| format!("no worktree named '{name}'"))
}

/// A branch, remote branch (`origin/main`) or tag by its short name: the full ref and the commit it points at.
fn resolve_ref(ctx: &TabCtx, cx: &App, name: &str) -> Result<(FullName, ObjectId), String> {
    let model = ctx.model.read(cx);
    let refs = model
        .refs
        .value
        .as_ref()
        .ok_or_else(|| "the refs are not loaded".to_owned())?;
    if name.eq_ignore_ascii_case("head") {
        return match &refs.head {
            HeadState::Branch { name, target } => Ok((name.clone(), *target)),
            HeadState::Detached { target } => FullName::try_from("HEAD")
                .map(|f| (f, *target))
                .map_err(|e| e.to_string()),
            HeadState::Unborn { .. } => Err("HEAD has no commit".to_owned()),
        };
    }
    if let Some(b) = refs.local.iter().find(|b| b.name == name) {
        return Ok((b.full.clone(), b.target));
    }
    for remote in &refs.remotes {
        if let Some(b) = remote
            .branches
            .iter()
            .find(|b| format!("{}/{}", b.remote, b.name) == name)
        {
            return Ok((b.full.clone(), b.target));
        }
    }
    refs.tags
        .iter()
        .find(|t| t.name == name)
        .map(|t| (t.full.clone(), t.target))
        .ok_or_else(|| format!("no branch or tag named '{name}'"))
}

fn add(window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let path = PathBuf::from(args.text(0, "path")?);
    inside_sandbox(args, &path)?;
    let kind = args.text(1, "existing | new | detach")?.to_lowercase();
    let ctx = ctx_of(cx)?;
    let words: Vec<String> = (2..args.len()).filter_map(|ix| args.opt(ix)).collect();
    let lock_at = words.iter().position(|w| w.eq_ignore_ascii_case("lock"));
    let lock_reason = lock_at.map(|ix| words[ix + 1..].join(" "));
    let words = &words[..lock_at.unwrap_or(words.len())];
    let target = match kind.as_str() {
        "existing" => {
            let name = words.first().ok_or("missing argument: branch")?;
            let (full, _) = resolve_ref(&ctx, cx, name)?;
            WorktreeTarget::ExistingBranch(full)
        }
        "new" => {
            let name = words
                .first()
                .ok_or("missing argument: new branch name")?
                .clone();
            let start = match words.get(1).map(String::as_str) {
                Some("from") => {
                    let from = words.get(2).ok_or("missing argument: start point")?;
                    if from.eq_ignore_ascii_case("head") {
                        StartPoint::Head
                    } else {
                        StartPoint::Ref(resolve_ref(&ctx, cx, from)?.0)
                    }
                }
                _ => StartPoint::Head,
            };
            WorktreeTarget::NewBranch { name, start }
        }
        "detach" => {
            let name = words.first().ok_or("missing argument: ref")?;
            WorktreeTarget::Detached(resolve_ref(&ctx, cx, name)?.1)
        }
        other => {
            return Err(format!(
                "unknown target '{other}' (existing | new | detach)"
            ));
        }
    };
    let opts = WorktreeAddOptions {
        path: path.clone(),
        target,
        lock_reason,
    };
    ctx.with_tab(window, cx, move |tab, window, cx| {
        tab.add_worktree(opts, window, cx);
    });
    Ok(path.display().to_string())
}

fn on_named(
    window: &mut Window,
    cx: &mut App,
    args: &Args,
    f: impl FnOnce(
        &mut crate::tab::RepoTab,
        PathBuf,
        &mut Window,
        &mut gpui_kit::Context<crate::tab::RepoTab>,
    ) + 'static,
) -> StepResult {
    let name = args.text(0, "worktree name")?;
    let ctx = ctx_of(cx)?;
    let info = worktree_named(&ctx, cx, &name)?;
    let path = info.path;
    let shown = path.display().to_string();
    ctx.with_tab(window, cx, move |tab, window, cx| f(tab, path, window, cx));
    Ok(shown)
}

fn at(args: &Args, from: usize) -> Result<gpui_kit::Point<gpui_kit::Pixels>, String> {
    match args.opt(from).as_deref() {
        Some("at") => {
            let x: f32 = args.number(from + 1, "x")?;
            let y: f32 = args.number(from + 2, "y")?;
            Ok(point(px(x), px(y)))
        }
        _ => Ok(point(px(120.), px(300.))),
    }
}

fn assert_worktree(_: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let ctx = ctx_of(cx)?;
    let name = args.text(0, "worktree name")?;
    let info = worktree_named(&ctx, cx, &name)?;
    let expect = args.opt(1).map(|w| w.to_lowercase());
    let branch = checked_out_branch(&info).map(|f| crate::tab::ops::branch::short_name(&f));
    let ok = match expect.as_deref() {
        None => true,
        Some("current") => info.is_current,
        Some("locked") => info.locked.is_some(),
        Some("unlocked") => info.locked.is_none(),
        Some("prunable") => info.prunable,
        Some("detached") => matches!(info.head, Some(HeadState::Detached { .. })),
        Some("branch") => {
            let want = args.text(2, "branch")?;
            branch.as_deref() == Some(want.as_str())
        }
        Some(other) => return Err(format!("unknown check '{other}'")),
    };
    if ok {
        Ok(format!("{name}: ok"))
    } else {
        Err(format!(
            "worktree {name}: expected {}, found locked={:?} prunable={} current={} branch={branch:?}",
            expect.unwrap_or_default(),
            info.locked,
            info.prunable,
            info.is_current
        ))
    }
}

/// Registers the verbs of the worktree feature.
pub(super) fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "expand worktrees section",
            "expand worktrees section",
            "Expands the Worktrees section of the side panel",
            |_, cx, _| {
                let ctx = ctx_of(cx)?;
                ctx.ui.update(cx, |ui, cx| {
                    ui.side_expanded = SideExpansion {
                        worktrees: true,
                        ..ui.side_expanded
                    };
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
            "worktree add",
            "worktree add <path> existing <branch> | new <name> [from <ref>] | detach <ref|head> [lock [reason]]",
            "Adds a linked worktree (`git worktree add`); refs are short names of branches, remote branches or tags",
            add,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "worktree lock",
            "worktree lock <name> [reason]",
            "Locks the linked worktree with that directory name",
            |window, cx, args| {
                let reason = Some(args.rest(1)).filter(|r| !r.is_empty());
                on_named(window, cx, args, move |tab, path, window, cx| {
                    tab.lock_worktree(path, reason, window, cx);
                })
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "worktree unlock",
            "worktree unlock <name>",
            "Unlocks the linked worktree with that directory name",
            |window, cx, args| {
                on_named(window, cx, args, |tab, path, window, cx| {
                    tab.unlock_worktree(path, window, cx);
                })
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "worktree remove",
            "worktree remove <name> [force]",
            "Removes the linked worktree (`git worktree remove`; without force a dirty worktree asks for confirmation)",
            |window, cx, args| {
                let force = args.has_flag(1, "force");
                on_named(window, cx, args, move |tab, path, window, cx| {
                    tab.remove_worktree(path, force, window, cx);
                })
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "worktree prune",
            "worktree prune",
            "Prunes the worktrees whose directory is gone",
            |window, cx, _| {
                let ctx = ctx_of(cx)?;
                ctx.with_tab(window, cx, |tab, window, cx| {
                    tab.prune_worktrees(window, cx);
                });
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "worktree vanish",
            "worktree vanish <name>",
            "Deletes the directory of a linked worktree inside the sandbox (it then shows as prunable)",
            |window, cx, args| {
                let name = args.text(0, "worktree name")?;
                let ctx = ctx_of(cx)?;
                let info = worktree_named(&ctx, cx, &name)?;
                if info.id.is_none() {
                    return Err("the main worktree is never deleted".into());
                }
                inside_sandbox(args, &info.path)?;
                std::fs::remove_dir_all(&info.path).map_err(|e| e.to_string())?;
                ctx.with_tab(window, cx, |tab, _, cx| {
                    tab.reload(crate::backend::RefreshSet::WORKTREES, cx);
                });
                Ok(info.path.display().to_string())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open worktree menu",
            "open worktree menu <name> [at <x> <y>]",
            "Opens the context menu of a worktree row",
            |window, cx, args| {
                let ctx = ctx_of(cx)?;
                let info = worktree_named(&ctx, cx, &args.text(0, "worktree name")?)?;
                let target = worktree_menus::row_target(&ctx, &info);
                let items = worktree_menus::worktree_row(&target, cx);
                if items.is_empty() {
                    return Err("the worktree menu has no items".into());
                }
                let count = items.len();
                menus::open_context_menu(window, cx, at(args, 1)?, items);
                Ok(format!("{count} items"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open worktrees menu",
            "open worktrees menu [at <x> <y>]",
            "Opens the context menu of the Worktrees header",
            |window, cx, args| {
                let ctx = ctx_of(cx)?;
                let items =
                    worktree_menus::worktrees_header(&worktree_menus::header_target(&ctx), cx);
                let count = items.len();
                menus::open_context_menu(window, cx, at(args, 0)?, items);
                Ok(format!("{count} items"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open worktree",
            "open worktree <name>",
            "Opens the worktree in a new tab (selects its tab when it is already open)",
            |window, cx, args| {
                let ctx = ctx_of(cx)?;
                let info = worktree_named(&ctx, cx, &args.text(0, "worktree name")?)?;
                let shown = info.path.display().to_string();
                open_in_new_tab(window, cx, info.path);
                Ok(shown)
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "assert worktree",
            "assert worktree <name> [current|locked|unlocked|prunable|detached|branch <b>]",
            "Fails unless the worktree with that directory name exists and has the property",
            assert_worktree,
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert worktree count",
            "assert worktree count <n>",
            "Fails unless the repository has exactly n worktrees (the main one included)",
            |_, cx, args| {
                let want: usize = args.number(0, "count")?;
                let ctx = ctx_of(cx)?;
                let have = ctx
                    .model
                    .read(cx)
                    .worktrees
                    .value
                    .as_ref()
                    .map_or(0, |list| list.len());
                if have == want {
                    Ok(format!("{have} worktrees"))
                } else {
                    Err(format!("expected {want} worktrees, found {have}"))
                }
            },
        )
        .without_settle(),
    );
}
