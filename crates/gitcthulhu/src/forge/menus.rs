//! The forge items of the branch and commit context menus (menu extenders, spec 21 §2.13.2, §5.5). They appear only
//! when a forge with an API is detected for the tab.
//!
//! * Local branch, after the rename group: "Open pull request #N" when the branch has one, else "Create pull
//!   request…".
//! * Remote branch, after the push/pull group: "Open pull request #N" and "Checkout pull request #N" (the backend's
//!   forge PR checkout rule, `forge::checkout`), or nothing when the branch has none.
//! * Commit row, at the end: "Open commit in browser".

use gpui_kit::App;

use crate::backend::forge::PrState;
use crate::icons::AppIcon;
use crate::menus::{MenuItem, MenuKind, MenuSlot, MenuTarget, register_extender};

/// Element ids of the menu items (tests, QA).
pub mod ids {
    /// "Open pull request #N".
    pub const OPEN_PR: &str = "forge-open-pr";
    /// "Create pull request…".
    pub const CREATE_PR: &str = "forge-create-pr";
    /// "Checkout pull request #N".
    pub const CHECKOUT_PR: &str = "forge-checkout-pr";
    /// "Open commit in browser".
    pub const OPEN_COMMIT: &str = "forge-open-commit";
}

/// Registers the extenders.
pub fn register(cx: &mut App) {
    register_extender(
        cx,
        MenuKind::LocalBranch,
        MenuSlot::AfterRenameGroup,
        local_branch,
    );
    register_extender(
        cx,
        MenuKind::RemoteBranch,
        MenuSlot::AfterPushPullGroup,
        remote_branch,
    );
    register_extender(cx, MenuKind::Commit, MenuSlot::End, commit);
}

/// What the extenders need of the tab's forge, copied out of the controller so the click handlers own it.
struct Detected {
    handle: crate::backend::forge::RepoHandle,
    reviews: bool,
    account: bool,
}

fn detected(target: &MenuTarget, cx: &App) -> Option<Detected> {
    let controller = super::controller_of(target.tab.id, cx)?;
    let controller = controller.read(cx);
    let handle = controller.handle()?.clone();
    let facts = handle.facts();
    facts.forge?;
    Some(Detected {
        reviews: facts.reviews,
        account: controller.can_use_reviews(),
        handle,
    })
}

fn open_item(label: String, url: String, id: &'static str, icon: AppIcon) -> MenuItem {
    MenuItem::action(id, label, move |_, cx| {
        crate::dialogs::register::common::open_url(cx, &url);
    })
    .with_icon(icon)
}

pub(super) fn local_branch(target: &MenuTarget, items: &mut Vec<MenuItem>, cx: &App) {
    let Some(branch) = target.branch.clone() else {
        return;
    };
    let Some(found) = detected(target, cx) else {
        return;
    };
    if !found.reviews {
        return;
    }
    let facts = found.handle.facts();
    let review = super::controller_of(target.tab.id, cx)
        .and_then(|controller| controller.read(cx).badge(&branch).cloned());
    match review {
        Some(badge) => {
            if let Some(url) = badge.url.clone() {
                items.push(open_item(
                    format!(
                        "Open {} {}",
                        facts.unit_name,
                        super::badges::label_of(&badge)
                    ),
                    url,
                    ids::OPEN_PR,
                    AppIcon::PullRequest,
                ));
            }
        }
        None => {
            let tab = target.tab.clone();
            let has_account = found.account;
            items.push(
                MenuItem::action(
                    ids::CREATE_PR,
                    format!("Create {}…", facts.unit_name),
                    move |window, cx| {
                        if has_account {
                            crate::dialogs::pull_request::open(
                                window,
                                cx,
                                tab.clone(),
                                branch.clone(),
                            );
                        } else {
                            crate::forge::accounts::explain_missing_account(window, cx, &tab);
                        }
                    },
                )
                .with_icon(AppIcon::PullRequestCreate),
            );
        }
    }
}

pub(super) fn remote_branch(target: &MenuTarget, items: &mut Vec<MenuItem>, cx: &App) {
    let Some(branch) = target.branch.clone() else {
        return;
    };
    let Some(found) = detected(target, cx) else {
        return;
    };
    if !found.reviews {
        return;
    }
    let facts = found.handle.facts();
    let Some(badge) = super::controller_of(target.tab.id, cx)
        .and_then(|controller| controller.read(cx).badge(&branch).cloned())
    else {
        // No review yet: a branch of the push remote (not its default branch) can be proposed as it is.
        if creatable_remote_branch(target, &branch, facts, cx) {
            let tab = target.tab.clone();
            let has_account = found.account;
            items.push(
                MenuItem::action(
                    ids::CREATE_PR,
                    format!("Create {}…", facts.unit_name),
                    move |window, cx| {
                        if has_account {
                            crate::dialogs::pull_request::open(
                                window,
                                cx,
                                tab.clone(),
                                branch.clone(),
                            );
                        } else {
                            crate::forge::accounts::explain_missing_account(window, cx, &tab);
                        }
                    },
                )
                .with_icon(AppIcon::PullRequestCreate),
            );
        }
        return;
    };
    if let Some(url) = badge.url.clone() {
        items.push(open_item(
            format!(
                "Open {} {}",
                facts.unit_name,
                super::badges::label_of(&badge)
            ),
            url,
            ids::OPEN_PR,
            AppIcon::PullRequest,
        ));
    }
    if matches!(badge.state, PrState::Open | PrState::Draft) && found.account {
        let tab = target.tab.clone();
        let handle = found.handle.clone();
        let number = badge.number;
        items.push(
            MenuItem::action(
                ids::CHECKOUT_PR,
                format!(
                    "Checkout {} {}",
                    facts.unit_name,
                    super::badges::label_of(&badge)
                ),
                move |window, cx| {
                    super::checkout::start(&tab, handle.clone(), number, window, cx);
                },
            )
            .with_icon(AppIcon::PullRequest),
        );
    }
}

/// Whether a remote-tracking branch can head a new review: it belongs to the remote reviews are pushed from (the
/// forge names the head after that remote's owner) and is not that remote's default branch or `HEAD`.
fn creatable_remote_branch(
    target: &MenuTarget,
    branch: &crate::backend::FullName,
    facts: &crate::backend::forge::RepoFacts,
    cx: &App,
) -> bool {
    let Some(push_remote) = facts.push_remote.as_ref().or(facts.base_remote.as_ref()) else {
        return false;
    };
    let model = target.tab.model.read(cx);
    let Some(refs) = &model.refs.value else {
        return false;
    };
    refs.remotes
        .iter()
        .filter(|remote| &remote.name == push_remote)
        .any(|remote| {
            remote.branches.iter().any(|b| {
                &b.full == branch
                    && b.name != "HEAD"
                    && remote.head_branch.as_deref() != Some(b.name.as_str())
            })
        })
}

pub(super) fn commit(target: &MenuTarget, items: &mut Vec<MenuItem>, cx: &App) {
    let Some(id) = target.commit else {
        return;
    };
    let Some(found) = detected(target, cx) else {
        return;
    };
    if let Some(url) = found.handle.url_commit(&id.to_hex().to_string()) {
        items.push(open_item(
            "Open commit in browser".to_owned(),
            url,
            ids::OPEN_COMMIT,
            AppIcon::ExternalLink,
        ));
    }
}
