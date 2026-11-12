//! Dialog verbs: `open dialog <name>` calls the dialog entry points the app itself uses, `open settings page`.

use gpui_kit::{App, Window};

use super::active_ctx;
use super::commits::{find_commit, first_commit, local_branch};
use crate::app_view;
use crate::backend::present::ErrorDialogData;
use crate::backend::prompt::ConfirmRequest;
use crate::backend::{FullName, StartPoint};
use crate::dialogs::choice::{self, ChoiceSpec};
use crate::dialogs::settings::SettingsPage;
use crate::dialogs::{self};
use crate::qa::monitor;
use crate::qa::verbs::{Args, Verb, register_verb};

/// Registers the verbs of this file.
pub(super) fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "open dialog",
            "open dialog <name> [args]",
            "Opens a dialog. Names: about, clone, settings, new-branch [branch], rename-branch <branch>, change-upstream <branch>, \
             new-tag [commit], new-remote, edit-remote <name>, new-submodule, stash, reset [commit], signoff, quick-actions, \
             author, add-worktree, pull-request <branch>, confirm <title> <message>, choice <title> <message> <option>..., \
             error <title> <message> [details]",
            open_dialog,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open settings page",
            "open settings page <name>",
            "Opens the Settings dialog at a page: appearance, layout, date-time, environment, branches, remote-actions, proxy, \
             authentication, security, accounts, ai, terminal, updates, logs",
            |window, cx, args| {
                let page = parse_page(&args.text(0, "page name")?)?;
                monitor::scenario_dialog(cx, true);
                dialogs::settings::open(window, cx, Some(page));
                Ok(format!("{page:?}"))
            },
        ),
    );
}

fn open_dialog(window: &mut Window, cx: &mut App, args: &Args) -> Result<String, String> {
    let name = args.text(0, "dialog name")?.to_lowercase();
    // Generic dialogs first: they need no repository.
    match name.as_str() {
        "about" => return opened(cx, name, dialogs::about::open, window),
        "clone" => return opened(cx, name, dialogs::clone::open, window),
        "settings" => {
            return opened(
                cx,
                name,
                |w, cx| dialogs::settings::open(w, cx, None),
                window,
            );
        }
        "confirm" => {
            let request = ConfirmRequest::new(args.text(1, "title")?, args.text(2, "message")?);
            monitor::scenario_dialog(cx, true);
            dialogs::confirm::open_with(window, cx, request, Box::new(|_, _, _| {}));
            return Ok(name);
        }
        "choice" => {
            let options: Vec<_> = args.raw()[3.min(args.len())..]
                .iter()
                .map(|o| o.clone().into())
                .collect();
            if options.is_empty() {
                return Err("choice needs at least one option".into());
            }
            let spec = ChoiceSpec {
                title: args.text(1, "title")?.into(),
                message: args.text(2, "message")?.into(),
                options,
                checkbox: None,
            };
            monitor::scenario_dialog(cx, true);
            choice::open(window, cx, spec, Box::new(|_, _, _| {}));
            return Ok(name);
        }
        "error" => {
            let mut data = ErrorDialogData::new(args.text(1, "title")?, args.text(2, "message")?);
            data.details = args.opt(3).map(Into::into);
            monitor::scenario_dialog(cx, true);
            app_view::show_error(cx, data);
            return Ok(name);
        }
        _ => {}
    }
    let ctx = active_ctx(cx)?;
    monitor::scenario_dialog(cx, true);
    let result = open_repo_dialog(window, cx, ctx, &name, args);
    if result.is_err() {
        // Nothing opened: a dialog that appears now is not the scenario's.
        monitor::scenario_dialog(cx, false);
    }
    result
}

/// The dialogs of a repository tab.
fn open_repo_dialog(
    window: &mut Window,
    cx: &mut App,
    ctx: crate::tab::TabCtx,
    name: &str,
    args: &Args,
) -> Result<String, String> {
    let model = ctx.model.clone();
    let arg = args.opt(1);
    match name {
        "new-branch" => {
            let start = match arg {
                Some(branch) => StartPoint::Ref(local_branch(model.read(cx), &branch)?.full),
                None => StartPoint::Head,
            };
            dialogs::branch::open_new_branch(window, cx, ctx, start);
        }
        "rename-branch" | "change-upstream" => {
            let branch: FullName =
                local_branch(model.read(cx), &args.text(1, "branch name")?)?.full;
            if name == "rename-branch" {
                dialogs::branch::open_rename_branch(window, cx, ctx, branch);
            } else {
                dialogs::branch::open_change_upstream(window, cx, ctx, branch);
            }
        }
        "new-tag" => {
            let target = match arg {
                Some(query) => find_commit(model.read(cx), &query)?.0,
                None => first_commit(model.read(cx))?,
            };
            dialogs::tag::open_new_tag(window, cx, ctx, target);
        }
        "new-remote" => dialogs::remote::open_new_remote(window, cx, ctx),
        "edit-remote" => {
            dialogs::remote::open_edit_remote(window, cx, ctx, args.text(1, "remote name")?)
        }
        "new-submodule" => dialogs::submodule::open_new_submodule(window, cx, ctx),
        "stash" => dialogs::stash::open_stash_with_message(window, cx, ctx),
        "reset" => {
            let target = match arg {
                Some(query) => find_commit(model.read(cx), &query)?.0,
                None => first_commit(model.read(cx))?,
            };
            dialogs::reset::open(window, cx, ctx, target);
        }
        "signoff" => dialogs::signoff::open(window, cx, ctx),
        "quick-actions" => dialogs::quick_actions::open(window, cx, ctx),
        "author" => dialogs::author::open_author_settings(window, cx, Some(ctx)),
        "add-worktree" => dialogs::worktree::open_add_worktree(window, cx, ctx),
        "pull-request" => {
            let branch = local_branch(model.read(cx), &args.text(1, "branch name")?)?.full;
            dialogs::pull_request::open(window, cx, ctx, branch);
        }
        other => return Err(format!("unknown dialog '{other}'")),
    }
    Ok(name.to_owned())
}

/// Marks the dialog as the scenario's own and opens it.
fn opened(
    cx: &mut App,
    name: String,
    open: impl FnOnce(&mut Window, &mut App),
    window: &mut Window,
) -> Result<String, String> {
    monitor::scenario_dialog(cx, true);
    open(window, cx);
    Ok(name)
}

/// The settings page of a name.
fn parse_page(word: &str) -> Result<SettingsPage, String> {
    Ok(
        match word.to_lowercase().replace(['_', ' '], "-").as_str() {
            "appearance" => SettingsPage::Appearance,
            "layout" => SettingsPage::Layout,
            "date-time" | "datetime" | "date" => SettingsPage::DateTime,
            "environment" => SettingsPage::Environment,
            "branches" => SettingsPage::Branches,
            "remote-actions" => SettingsPage::RemoteActions,
            "proxy" => SettingsPage::Proxy,
            "authentication" => SettingsPage::Authentication,
            "security" => SettingsPage::Security,
            "accounts" => SettingsPage::Accounts,
            "ai" => SettingsPage::Ai,
            "terminal" => SettingsPage::Terminal,
            "updates" => SettingsPage::Updates,
            "logs" => SettingsPage::Logs,
            other => return Err(format!("unknown settings page '{other}'")),
        },
    )
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn settings_pages_are_found_by_name() {
        assert_eq!(
            parse_page("Appearance"),
            Ok(SettingsPage::Appearance),
            "case"
        );
        assert_eq!(parse_page("date time"), Ok(SettingsPage::DateTime), "space");
        assert_eq!(
            parse_page("remote_actions"),
            Ok(SettingsPage::RemoteActions),
            "underscore"
        );
        assert!(parse_page("nope").is_err(), "unknown pages fail");
    }
}
