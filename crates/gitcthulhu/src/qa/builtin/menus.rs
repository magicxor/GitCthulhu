//! `open context menu <target>`: builds the menu of a target with the app's own menu builders and opens it.

use gpui_kit::{App, Pixels, Point, Window, point, px};

use super::active_ctx;
use super::commits::{find_commit, local_branch, log_row, remote_branch, stash, tag, tracking};
use crate::backend::RelaPath;
use crate::menus::{self, MenuItem, MenuKind, MenuTarget};
use crate::qa::verbs::{Args, Verb, register_verb};
use crate::tab::TabCtx;

/// Registers the verbs of this file.
pub(super) fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "open context menu",
            "open context menu <target> [at <x> <y>]",
            "Opens a context menu. Targets: log-row <N>, commit <prefix|summary>, branch <name>, remote-branch <remote/name>, \
             tag <name>, stash <index>, remote <name>, submodule <path>, status-file staged|unstaged <path>, \
             commit-file <path>, diff-line, toolbar pull|push|stash",
            open_context_menu,
        ),
    );
}

fn open_context_menu(window: &mut Window, cx: &mut App, args: &Args) -> Result<String, String> {
    let ctx = active_ctx(cx)?;
    let words = args.raw();
    // A trailing `at <x> <y>` moves the menu.
    let (words, at) = match words.iter().position(|w| w.eq_ignore_ascii_case("at")) {
        Some(ix) if words.len() >= ix + 3 => {
            let x = words[ix + 1]
                .parse::<f32>()
                .map_err(|_| "x must be a number".to_owned())?;
            let y = words[ix + 2]
                .parse::<f32>()
                .map_err(|_| "y must be a number".to_owned())?;
            (&words[..ix], point(px(x), px(y)))
        }
        _ => (words, point(px(320.0), px(240.0))),
    };
    let kind = words
        .first()
        .ok_or_else(|| "missing argument: menu target".to_owned())?
        .to_lowercase();
    let rest = words[1..].join(" ");
    let items = build_items(&ctx, cx, &kind, &rest, words)?;
    if items.is_empty() {
        return Err(format!(
            "the {kind} menu has no items (not implemented yet?)"
        ));
    }
    let count = items.len();
    if kind == "toolbar" {
        menus::open_dropdown(window, cx, at, items, true);
    } else {
        menus::open_context_menu(window, cx, at, items);
    }
    Ok(format!("{count} items at {}", describe(at)))
}

fn describe(at: Point<Pixels>) -> String {
    format!("{},{}", at.x.as_f32(), at.y.as_f32())
}

fn base(kind: MenuKind, ctx: &TabCtx) -> MenuTarget {
    MenuTarget {
        kind,
        tab: ctx.clone(),
        branch: None,
        upstream: None,
        commit: None,
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    }
}

fn build_items(
    ctx: &TabCtx,
    cx: &App,
    kind: &str,
    rest: &str,
    words: &[String],
) -> Result<Vec<MenuItem>, String> {
    let model = ctx.model.read(cx);
    Ok(match kind {
        "log-row" => {
            let visible: usize = rest
                .parse()
                .map_err(|_| "log-row needs a row number".to_owned())?;
            let (_, row) = log_row(model, visible)?;
            let commit = row
                .commit
                .as_ref()
                .ok_or_else(|| "the uncommitted row has no commit menu".to_owned())?;
            let mut target = base(MenuKind::Commit, ctx);
            target.commit = Some(commit.id);
            menus::log::commit(&target, cx)
        }
        "commit" => {
            let (id, _) = find_commit(model, rest)?;
            let mut target = base(MenuKind::Commit, ctx);
            target.commit = Some(id);
            menus::log::commit(&target, cx)
        }
        "branch" => {
            let branch = local_branch(model, rest)?;
            let mut target = base(MenuKind::LocalBranch, ctx);
            target.upstream = tracking(&branch);
            target.is_current = branch.is_head;
            target.checked_out_elsewhere = branch.checked_out_in.is_some();
            target.commit = Some(branch.target);
            target.branch = Some(branch.full);
            menus::refs::local_branch(&target, cx)
        }
        "remote-branch" => {
            let branch = remote_branch(model, rest)?;
            let mut target = base(MenuKind::RemoteBranch, ctx);
            target.commit = Some(branch.target);
            target.branch = Some(branch.full);
            menus::refs::remote_branch(&target, cx)
        }
        "tag" => {
            let tag = tag(model, rest)?;
            let mut target = base(MenuKind::Tag, ctx);
            target.commit = Some(tag.target);
            target.branch = Some(tag.full);
            menus::refs::tag(&target, cx)
        }
        "stash" => {
            let index: usize = rest
                .parse()
                .map_err(|_| "stash needs an index".to_owned())?;
            let stash = stash(model, index)?;
            let mut target = base(MenuKind::Stash, ctx);
            target.commit = Some(stash.id);
            menus::refs::stash(&target, cx)
        }
        "remote" => {
            let known = model
                .refs
                .value
                .as_ref()
                .is_some_and(|refs| refs.remotes.iter().any(|r| r.name == rest));
            if !known {
                return Err(format!("no remote '{rest}'"));
            }
            let mut target = base(MenuKind::Remote, ctx);
            target.path = Some(RelaPath::from(rest));
            menus::refs::remote(&target, cx)
        }
        "submodule" => {
            let mut target = base(MenuKind::Submodule, ctx);
            target.path = Some(RelaPath::from(rest));
            menus::refs::submodule(&target, cx)
        }
        "status-file" => {
            let path = words.get(2..).map(|w| w.join(" ")).unwrap_or_default();
            if !matches!(
                words.get(1).map(|w| w.to_lowercase()).as_deref(),
                Some("staged" | "unstaged")
            ) || path.is_empty()
            {
                return Err("status-file needs staged|unstaged and a path".into());
            }
            let mut target = base(MenuKind::StatusFile, ctx);
            target.path = Some(RelaPath::from(path.as_str()));
            menus::files::status_file(&target, cx)
        }
        "commit-file" => {
            let mut target = base(MenuKind::CommitFile, ctx);
            target.path = Some(RelaPath::from(rest));
            menus::commit_files::commit_file(&target, cx)
        }
        "diff-line" => {
            let target = base(MenuKind::DiffLine, ctx);
            menus::diff_line::diff_line(&target, cx)
        }
        "toolbar" => match rest.to_lowercase().as_str() {
            "pull" => menus::toolbar::pull_menu(ctx, cx),
            "push" => menus::toolbar::push_menu(ctx, cx),
            "stash" => menus::toolbar::stash_menu(ctx, cx),
            other => {
                return Err(format!(
                    "unknown toolbar menu '{other}' (pull, push or stash)"
                ));
            }
        },
        other => return Err(format!("unknown menu target '{other}'")),
    })
}
