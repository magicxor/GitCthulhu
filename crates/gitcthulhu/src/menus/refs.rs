//! Branch, remote-branch, tag, stash, remote and submodule menus (owned by G3; 07 §6.6, 09 §4.2-4.5, 08 §2.13),
//! used by the side panel and by the log's chips (G4). Items and their order are Gitnuro's; the builders call
//! `menus::extend` at every slot of spec 21 §2.13.2.
//!
//! **What a [`MenuTarget`] must carry** (the `*_target` constructors fill it from the model):
//! - local and remote branch: `branch` = the full ref (`refs/heads/x`, `refs/remotes/origin/x`); a local branch's
//!   `upstream` = its configured merge ref (`branch.<x>.merge`, `refs/heads/y` on the remote), `commit` = its tip;
//! - tag: `branch` = the full tag ref, `commit` = its commit (looked up in the model when absent);
//! - stash: `commit` = the stash commit;
//! - remote: `path` = the remote's name;
//! - submodule: `path` = the submodule's path.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::{App, Context, Window};

use super::{MenuItem, MenuKind, MenuSlot, MenuTarget, extend};
use crate::backend::prompt::ConfirmRequest;
use crate::backend::{
    ByteSlice as _, FullName, HeadState, LocalBranch, ObjectId, RelaPath, RemoteBranch, StashInfo,
    SubmoduleInfo, SubmoduleState,
};
use crate::icons::AppIcon;
use crate::notifications::ToastKind;
use crate::tab::ops::branch::short_name;
use crate::tab::{RepoTab, TabCtx};

type TabCall = Rc<dyn Fn(&mut RepoTab, &mut Window, &mut Context<RepoTab>)>;

/// An action that runs `f` on the tab of `tab` (the menu closes first, then the call runs).
fn on_tab(
    id: &'static str,
    label: impl Into<gpui_kit::SharedString>,
    tab: &TabCtx,
    f: impl Fn(&mut RepoTab, &mut Window, &mut Context<RepoTab>) + 'static,
) -> MenuItem {
    let tab = tab.clone();
    let f: TabCall = Rc::new(f);
    MenuItem::action(id, label, move |window, cx| {
        let f = f.clone();
        tab.with_tab(window, cx, move |t, w, c| f(t, w, c));
    })
}

/// Adds a separator unless the list is empty or already ends in one.
fn separator(items: &mut Vec<MenuItem>) {
    if matches!(items.last(), Some(MenuItem::Action { .. })) {
        items.push(MenuItem::Separator);
    }
}

/// Removes a trailing separator.
fn trim_separator(items: &mut Vec<MenuItem>) {
    if matches!(items.last(), Some(MenuItem::Separator)) {
        items.pop();
    }
}

/// The branch HEAD is on, when it is a real branch with commits (merge, rebase, push and pull need one).
fn head_branch(target: &MenuTarget, cx: &App) -> Option<FullName> {
    let model = target.tab.model.read(cx);
    match &model.refs.value.as_ref()?.head {
        HeadState::Branch { name, .. } => Some(name.clone()),
        _ => None,
    }
}

// ---- targets ---------------------------------------------------------------------------------------------------

/// The menu target of a local branch.
pub fn local_branch_target(tab: &TabCtx, branch: &LocalBranch) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::LocalBranch,
        tab: tab.clone(),
        branch: Some(branch.full.clone()),
        upstream: branch.upstream.as_ref().map(|u| u.merge.clone()),
        commit: Some(branch.target),
        path: None,
        is_current: branch.is_head,
        checked_out_elsewhere: branch.checked_out_in.is_some(),
    }
}

/// The menu target of a remote-tracking branch.
pub fn remote_branch_target(tab: &TabCtx, branch: &RemoteBranch) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::RemoteBranch,
        tab: tab.clone(),
        branch: Some(branch.full.clone()),
        upstream: None,
        commit: Some(branch.target),
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    }
}

/// The menu target of a tag (`full` = `refs/tags/x`, `target` = its commit).
pub fn tag_target(tab: &TabCtx, full: FullName, target: ObjectId) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::Tag,
        tab: tab.clone(),
        branch: Some(full),
        upstream: None,
        commit: Some(target),
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    }
}

/// The menu target of a stash.
pub fn stash_target(tab: &TabCtx, stash: &StashInfo) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::Stash,
        tab: tab.clone(),
        branch: None,
        upstream: None,
        commit: Some(stash.id),
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    }
}

/// The menu target of a remote (`path` holds its name).
pub fn remote_target(tab: &TabCtx, name: &str) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::Remote,
        tab: tab.clone(),
        branch: None,
        upstream: None,
        commit: None,
        path: Some(RelaPath::from(name)),
        is_current: false,
        checked_out_elsewhere: false,
    }
}

/// The menu target of a submodule.
pub fn submodule_target(tab: &TabCtx, submodule: &SubmoduleInfo) -> MenuTarget {
    MenuTarget {
        kind: MenuKind::Submodule,
        tab: tab.clone(),
        branch: None,
        upstream: None,
        commit: None,
        path: Some(submodule.path.clone()),
        is_current: false,
        checked_out_elsewhere: false,
    }
}

// ---- shared actions --------------------------------------------------------------------------------------------

/// Copies `text` to the clipboard and confirms with `"<text>" copied to clipboard` (07 §6.6). Under test the text
/// is recorded instead (the test platform has no clipboard).
pub(crate) fn copy_with_toast(cx: &mut App, tab: &TabCtx, text: &str) {
    #[cfg(test)]
    tests_support::COPIED.with(|c| c.borrow_mut().push(text.to_owned()));
    #[cfg(not(test))]
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_owned()));
    crate::app_view::show_toast(
        cx,
        Some(tab.id),
        ToastKind::Positive,
        format!("\"{text}\" copied to clipboard"),
        None,
    );
}

/// Opens the submodule at `path` (relative to the repository's working directory) in a new, selected tab.
pub(crate) fn open_submodule_in_tab(
    tab: &TabCtx,
    path: &RelaPath,
    window: &mut Window,
    cx: &mut App,
) {
    let base = tab
        .backend
        .handle()
        .and_then(|h| h.workdir())
        .unwrap_or_default();
    let full: PathBuf = base.join(path.to_path_lossy());
    #[cfg(test)]
    {
        let _ = (&window, &cx);
        tests_support::OPENED.with(|o| o.borrow_mut().push(full.clone()));
    }
    #[cfg(not(test))]
    if let Some(view) = cx
        .try_global::<crate::app_view::AppViewGlobal>()
        .and_then(|g| g.0.upgrade())
    {
        view.update(cx, |app, cx| app.open_repository(full, true, window, cx));
    }
    #[cfg(not(test))]
    let _ = window;
}

/// The "Delete submodule" confirmation: deleting removes the submodule's working directory with every change in it.
fn confirm_delete_submodule(tab: &TabCtx, path: &RelaPath, window: &mut Window, cx: &mut App) {
    let shown = path.to_str_lossy().into_owned();
    let mut request = ConfirmRequest::new(
        "Delete submodule",
        format!(
            "Delete the submodule '{shown}'? Its working directory is removed, including any changes in it."
        ),
    );
    request.confirm_label = "Delete".into();
    request.destructive = true;
    let (tab, path) = (tab.clone(), path.clone());
    crate::dialogs::confirm::open_with(
        window,
        cx,
        request,
        Box::new(move |yes, window, cx| {
            if yes {
                tab.with_tab(window, cx, move |t, w, c| {
                    t.delete_submodule(path, w, c);
                });
            }
        }),
    );
}

// ---- builders --------------------------------------------------------------------------------------------------

/// The local-branch menu (slots `AfterCheckout`, `AfterRenameGroup`): Checkout, Merge and Rebase (when HEAD is a
/// branch), Rename, Change default upstream, Delete (not the current branch) and Copy branch name.
pub fn local_branch(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    let Some(full) = target.branch.clone() else {
        extend(cx, MenuSlot::AfterCheckout, target, &mut items);
        extend(cx, MenuSlot::AfterRenameGroup, target, &mut items);
        trim_separator(&mut items);
        return items;
    };
    let tab = &target.tab;
    let head = head_branch(target, cx);
    let rev = short_name(&full);

    if !target.is_current {
        let f = full.clone();
        items.push(
            on_tab("branch-checkout", "Checkout branch", tab, move |t, w, c| {
                t.checkout_branch(f.clone(), w, c);
            })
            .with_icon(AppIcon::Start),
        );
    }
    extend(cx, MenuSlot::AfterCheckout, target, &mut items);
    if !target.is_current && head.is_some() {
        let r = rev.clone();
        items.push(on_tab(
            "branch-merge",
            "Merge branch",
            tab,
            move |t, w, c| {
                t.merge(r.clone(), w, c);
            },
        ));
        let r = rev.clone();
        items.push(on_tab(
            "branch-rebase",
            "Rebase branch",
            tab,
            move |t, w, c| {
                t.rebase(r.clone(), w, c);
            },
        ));
        // 08 §2.13: this separator exists only with Merge/Rebase (a detached HEAD goes Checkout -> Rename).
        separator(&mut items);
    }

    let (t1, f1) = (tab.clone(), full.clone());
    items.push(
        MenuItem::action("branch-rename", "Rename branch", move |window, cx| {
            crate::dialogs::branch::open_rename_branch(window, cx, t1.clone(), f1.clone());
        })
        .with_icon(AppIcon::Edit),
    );
    let (t2, f2) = (tab.clone(), full.clone());
    items.push(MenuItem::action(
        "branch-upstream",
        "Change default upstream branch",
        move |window, cx| {
            crate::dialogs::branch::open_change_upstream(window, cx, t2.clone(), f2.clone());
        },
    ));
    extend(cx, MenuSlot::AfterRenameGroup, target, &mut items);
    separator(&mut items);

    if !target.is_current {
        items.push(
            on_tab("branch-delete", "Delete branch", tab, move |t, w, c| {
                t.delete_branch(full.clone(), false, w, c);
            })
            .with_icon(AppIcon::Delete),
        );
        separator(&mut items);
    }
    push_copy_name(&mut items, tab, rev);
    items
}

fn push_copy_name(items: &mut Vec<MenuItem>, tab: &TabCtx, name: String) {
    let tab = tab.clone();
    items.push(
        MenuItem::action("branch-copy-name", "Copy branch name", move |_, cx| {
            copy_with_toast(cx, &tab, &name);
        })
        .with_icon(AppIcon::Copy),
    );
}

/// The remote-branch menu (slot `AfterPushPullGroup`): Checkout, Merge and Rebase (when HEAD is a branch), Push
/// the current branch to it, Pull it into the current branch, Delete remote branch and Copy branch name.
pub fn remote_branch(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    let Some(full) = target.branch.clone() else {
        extend(cx, MenuSlot::AfterPushPullGroup, target, &mut items);
        trim_separator(&mut items);
        return items;
    };
    let tab = &target.tab;
    let head = head_branch(target, cx);
    let rev = short_name(&full);

    let f = full.clone();
    items.push(
        on_tab("branch-checkout", "Checkout branch", tab, move |t, w, c| {
            t.checkout_remote_branch(f.clone(), None, w, c);
        })
        .with_icon(AppIcon::Start),
    );
    if head.is_some() {
        let r = rev.clone();
        items.push(on_tab(
            "branch-merge",
            "Merge branch",
            tab,
            move |t, w, c| {
                t.merge(r.clone(), w, c);
            },
        ));
        let r = rev.clone();
        items.push(on_tab(
            "branch-rebase",
            "Rebase branch",
            tab,
            move |t, w, c| {
                t.rebase(r.clone(), w, c);
            },
        ));
        // 08 §2.13: the separators exist only with these groups (a detached HEAD goes Checkout -> Delete).
        separator(&mut items);
    }

    if let Some(current) = head {
        let (local, remote) = (current.clone(), full.clone());
        items.push(on_tab(
            "branch-push-to",
            format!("Push \"{}\" to \"{rev}\"", short_name(&current)),
            tab,
            move |t, w, c| {
                t.push_branch_to(local.clone(), remote.clone(), false, w, c);
            },
        ));
        let remote = full.clone();
        items.push(on_tab(
            "branch-pull-from",
            format!("Pull \"{rev}\" to \"{}\"", short_name(&current)),
            tab,
            move |t, w, c| {
                t.pull_from(remote.clone(), w, c);
            },
        ));
        separator(&mut items);
    }
    // The extenders' items end with their own separator.
    extend(cx, MenuSlot::AfterPushPullGroup, target, &mut items);

    let located = remote_and_branch(target, &full, cx);
    // 08 §2.13: "Copy branch name" copies the simple name, without the remote prefix.
    let simple = located.as_ref().map_or_else(
        || {
            rev.split_once('/')
                .map_or(rev.clone(), |(_, b)| b.to_owned())
        },
        |(_, branch)| branch.clone(),
    );
    if let Some((remote, branch)) = located {
        items.push(
            on_tab(
                "branch-delete-remote",
                "Delete remote branch",
                tab,
                move |t, w, c| {
                    t.delete_remote_branch(remote.clone(), branch.clone(), w, c);
                },
            )
            .with_icon(AppIcon::Delete),
        );
        separator(&mut items);
    }
    push_copy_name(&mut items, tab, simple);
    items
}

/// The remote name and the branch name on that remote of a remote-tracking ref, from the loaded refs.
fn remote_and_branch(target: &MenuTarget, full: &FullName, cx: &App) -> Option<(String, String)> {
    let model = target.tab.model.read(cx);
    let refs = model.refs.value.as_ref()?;
    refs.remotes
        .iter()
        .flat_map(|r| &r.branches)
        .find(|b| &b.full == full)
        .map(|b| (b.remote.clone(), b.name.clone()))
}

/// The tag menu (slot `End`): Checkout tag's commit and Delete tag.
pub fn tag(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if let Some(full) = target.branch.clone() {
        let tab = &target.tab;
        let commit = target.commit.or_else(|| {
            let model = tab.model.read(cx);
            let refs = model.refs.value.as_ref()?;
            refs.tags.iter().find(|t| t.full == full).map(|t| t.target)
        });
        if let Some(commit) = commit {
            let f = full.clone();
            items.push(
                on_tab(
                    "tag-checkout",
                    "Checkout tag's commit",
                    tab,
                    move |t, w, c| {
                        t.checkout_tag(f.clone(), commit, w, c);
                    },
                )
                .with_icon(AppIcon::Start),
            );
        }
        let name = short_name(&full);
        items.push(
            on_tab("tag-delete", "Delete tag", tab, move |t, w, c| {
                t.delete_tag(name.clone(), w, c);
            })
            .with_icon(AppIcon::Delete),
        );
    }
    extend(cx, MenuSlot::End, target, &mut items);
    trim_separator(&mut items);
    items
}

/// The stash menu (slot `End`): Apply, Pop and Drop stash.
pub fn stash(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if let Some(id) = target.commit {
        let tab = &target.tab;
        items.push(
            on_tab("stash-apply", "Apply stash", tab, move |t, w, c| {
                t.apply_stash(id, w, c);
            })
            .with_icon(AppIcon::ApplyStash),
        );
        items.push(
            on_tab("stash-pop", "Pop stash", tab, move |t, w, c| {
                t.pop_stash(id, w, c);
            })
            .with_icon(AppIcon::ApplyStash),
        );
        items.push(
            on_tab("stash-drop", "Drop stash", tab, move |t, w, c| {
                t.drop_stash(id, w, c);
            })
            .with_icon(AppIcon::Delete),
        );
    }
    extend(cx, MenuSlot::End, target, &mut items);
    trim_separator(&mut items);
    items
}

/// The remote menu (slot `End`): Edit, Delete, then Fetch branches.
pub fn remote(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if let Some(name) = target.path.as_ref().map(|p| p.to_str_lossy().into_owned()) {
        let tab = &target.tab;
        let (t, n) = (tab.clone(), name.clone());
        items.push(
            MenuItem::action("remote-edit", "Edit", move |window, cx| {
                crate::dialogs::remote::open_edit_remote(window, cx, t.clone(), n.clone());
            })
            .with_icon(AppIcon::Edit),
        );
        let n = name.clone();
        items.push(
            on_tab("remote-delete", "Delete", tab, move |t, w, c| {
                t.delete_remote(n.clone(), w, c);
            })
            .with_icon(AppIcon::Delete),
        );
        separator(&mut items);
        items.push(on_tab(
            "remote-fetch",
            "Fetch branches",
            tab,
            move |t, w, c| {
                t.fetch_remote(name.clone(), w, c);
            },
        ));
    }
    extend(cx, MenuSlot::End, target, &mut items);
    trim_separator(&mut items);
    items
}

/// The submodule menu (slot `End`): Initialize (uninitialized) or Open in new tab, Sync and Update (valid), then
/// Delete submodule.
pub fn submodule(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if let Some(path) = target.path.clone() {
        let tab = &target.tab;
        let state = {
            let model = tab.model.read(cx);
            model
                .submodules
                .value
                .as_ref()
                .and_then(|subs| subs.iter().find(|s| s.path == path).map(|s| s.state))
        };
        let valid = matches!(
            state,
            Some(SubmoduleState::Initialized | SubmoduleState::CheckedOut)
        );
        if state == Some(SubmoduleState::Uninitialized) {
            let p = path.clone();
            items.push(on_tab(
                "submodule-init",
                "Initialize submodule",
                tab,
                move |t, w, c| {
                    t.init_submodule(p.clone(), w, c);
                },
            ));
        }
        if valid {
            let (t1, p1) = (tab.clone(), path.clone());
            items.push(
                MenuItem::action(
                    "submodule-open",
                    "Open submodule in new tab",
                    move |window, cx| open_submodule_in_tab(&t1, &p1, window, cx),
                )
                .with_icon(AppIcon::Open),
            );
            let p = path.clone();
            items.push(
                on_tab("submodule-sync", "Sync submodule", tab, move |t, w, c| {
                    t.sync_submodule(p.clone(), w, c);
                })
                .with_icon(AppIcon::Refresh),
            );
            let p = path.clone();
            items.push(
                on_tab(
                    "submodule-update",
                    "Update submodule",
                    tab,
                    move |t, w, c| {
                        t.update_submodule(p.clone(), w, c);
                    },
                )
                .with_icon(AppIcon::UpdateSubmodule),
            );
        }
        separator(&mut items);
        let t = tab.clone();
        items.push(
            MenuItem::action("submodule-delete", "Delete submodule", move |window, cx| {
                confirm_delete_submodule(&t, &path, window, cx);
            })
            .with_icon(AppIcon::Delete),
        );
    }
    extend(cx, MenuSlot::End, target, &mut items);
    trim_separator(&mut items);
    items
}

/// Recorders that stand in for the clipboard and the app view under test.
#[cfg(test)]
pub(crate) mod tests_support {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        /// Texts "copied" to the clipboard.
        pub static COPIED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Paths "opened" in new tabs.
        pub static OPENED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }
}
