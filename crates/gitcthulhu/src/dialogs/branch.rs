//! Branch dialogs (G10): New branch (09 §3.2), Rename branch (3.3), Change upstream (3.7) and Choose another
//! local name (spec 21 §2.9). The operations themselves are catalogue methods of `tab/ops/branch.rs`.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div,
};

use super::register::common::{self, PrefilledFieldDialog};
use super::{IconBasedDialog, SingleFieldConfig, SingleTextFieldDialog};
use crate::backend::{ByteSlice as _, FullName, RefList, StartPoint};
use crate::icons::AppIcon;
use crate::metrics::dialogs::UPSTREAM_W;
use crate::metrics::dp;
use crate::tab::TabCtx;
use crate::theme::ActiveTokens as _;
use crate::widgets::dropdown::{FilterDropdown, FilterDropdownEvent};
use crate::widgets::observed;

/// Element ids of the branch dialogs.
pub mod ids {
    /// The Change-upstream dialog content.
    pub const UPSTREAM_DIALOG: &str = "upstream-dialog";
    /// The remote dropdown.
    pub const UPSTREAM_REMOTE: &str = "upstream-remote";
    /// The branch dropdown.
    pub const UPSTREAM_BRANCH: &str = "upstream-branch";
    /// The AI "Suggest" link of the New branch dialog.
    pub const SUGGEST: &str = "branch-suggest";
    /// The reason a suggestion failed.
    pub const SUGGEST_ERROR: &str = "branch-suggest-error";
}

/// The short name of `full` (`refs/heads/feature/x` -> `feature/x`).
fn short_name(full: &FullName) -> String {
    full.as_ref().shorten().to_str_lossy().into_owned()
}

/// The configuration of the New branch dialog; `on_accept` receives the trimmed name.
pub fn new_branch_config(
    on_accept: impl Fn(String, &mut Window, &mut App) + 'static,
) -> SingleFieldConfig {
    let mut config = SingleFieldConfig::new(
        AppIcon::Branch,
        "New branch",
        "Create branch",
        move |name, window, cx| on_accept(name.trim().to_owned(), window, cx),
    );
    config.subtitle = Some("Create a new branch and check it out".into());
    config
}

/// Opens the New branch dialog for a branch starting at `start`. Accepting creates the branch and checks it out.
pub fn open_new_branch(window: &mut Window, cx: &mut App, tab: TabCtx, start: StartPoint) {
    let (create_tab, create_start) = (tab.clone(), start.clone());
    #[cfg_attr(not(feature = "services"), allow(unused_mut))]
    let mut config = new_branch_config(move |name, window, cx| {
        let start = create_start.clone();
        create_tab.with_tab(window, cx, move |t, window, cx| {
            t.create_branch(name, start, true, window, cx);
        });
    });
    #[cfg(feature = "services")]
    {
        config.extra = Some(Rc::new(move |field, _, cx| {
            // Only while an AI provider is ready (16 A8/A9); the link fills the name field and never creates.
            crate::ai::is_ready(cx).then(|| {
                cx.new(|_| suggest::SuggestLink::new(field.clone(), tab.clone(), start.clone()))
                    .into()
            })
        }));
    }
    #[cfg(not(feature = "services"))]
    let _ = (tab, start);
    SingleTextFieldDialog::open(window, cx, config);
}

/// The AI "Suggest" link of the New branch dialog (16 A8/A9).
#[cfg(feature = "services")]
mod suggest {
    use gpui_kit::{
        App, Context, Entity, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
        Window, div,
    };

    use super::ids;
    use crate::ai::{AiHandle, BranchNameSource};
    use crate::backend::ai::AiFailure;
    use crate::backend::{HeadState, StartPoint};
    use crate::dialogs::register::common;
    use crate::metrics::dp;
    use crate::tab::TabCtx;
    use crate::theme::ActiveTokens as _;
    use crate::widgets::field::OutlinedField;
    use crate::widgets::{TextLink, observed};

    /// The link below the name field: asks the AI provider for a name and puts it into the field.
    pub(super) struct SuggestLink {
        field: Entity<OutlinedField>,
        tab: TabCtx,
        start: StartPoint,
        /// The running request; dropping the dialog drops it and cancels the request.
        running: Option<AiHandle>,
        error: Option<SharedString>,
    }

    impl SuggestLink {
        pub(super) fn new(field: Entity<OutlinedField>, tab: TabCtx, start: StartPoint) -> Self {
            Self {
                field,
                tab,
                start,
                running: None,
                error: None,
            }
        }

        /// What the name is suggested from: the staged changes when branching from HEAD with some, else the
        /// summary of the commit the branch starts at.
        fn source(&self, cx: &App) -> Option<BranchNameSource> {
            let model = self.tab.model.read(cx);
            let staged = model
                .status
                .value
                .as_ref()
                .is_some_and(|status| !status.staged.is_empty());
            if staged
                && matches!(self.start, StartPoint::Head)
                && let Some(repo) = self.tab.backend.repo()
            {
                return Some(BranchNameSource::Staged(repo.clone()));
            }
            let id = match &self.start {
                StartPoint::Commit(id) => *id,
                // Also on a detached HEAD (no local branch is the head then).
                StartPoint::Head => match &model.refs.value.as_ref()?.head {
                    HeadState::Branch { target, .. } | HeadState::Detached { target } => *target,
                    HeadState::Unborn { .. } => return None,
                },
                StartPoint::Ref(name) => {
                    let refs = model.refs.value.as_ref()?;
                    refs.local
                        .iter()
                        .find(|branch| &branch.full == name)
                        .map(|branch| branch.target)
                        .or_else(|| {
                            refs.remotes
                                .iter()
                                .flat_map(|remote| &remote.branches)
                                .find(|branch| &branch.full == name)
                                .map(|branch| branch.target)
                        })?
                }
            };
            let summary = model
                .log
                .value
                .as_ref()?
                .rows
                .iter()
                .filter_map(|row| row.commit.as_ref())
                .find(|commit| commit.id == id)?
                .summary
                .to_string();
            Some(BranchNameSource::CommitMessage(summary))
        }

        fn suggest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            if self.running.is_some() {
                return;
            }
            self.error = None;
            let Some(source) = self.source(cx) else {
                self.error = Some("There is nothing to suggest a name from".into());
                cx.notify();
                return;
            };
            let this = cx.weak_entity();
            let handle =
                crate::ai::suggest_branch_name(window, cx, source, move |result, window, cx| {
                    this.update(cx, |link, cx| {
                        link.running = None;
                        match result {
                            Ok(name) if !name.trim().is_empty() => link
                                .field
                                .update(cx, |field, cx| field.set_text(name.trim(), window, cx)),
                            Ok(_) => link.error = Some("The AI provider suggested no name".into()),
                            Err(AiFailure::Cancelled) => {}
                            Err(failure) => link.error = Some(failure.message().into()),
                        }
                        cx.notify();
                    })
                    .ok();
                });
            self.running = Some(handle);
            cx.notify();
        }
    }

    impl Drop for SuggestLink {
        fn drop(&mut self) {
            if let Some(handle) = &self.running {
                handle.cancel();
            }
        }
    }

    impl Render for SuggestLink {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let tokens = *cx.tokens();
            let this = cx.weak_entity();
            let label = if self.running.is_some() {
                "Suggesting…"
            } else {
                "Suggest a name"
            };
            div()
                .flex()
                .flex_col()
                .child(div().pt(dp(8.)).flex().flex_row().justify_end().child(
                    TextLink::new(ids::SUGGEST, label).on_click(move |window, cx| {
                        this.update(cx, |link, cx| link.suggest(window, cx)).ok();
                    }),
                ))
                .children(self.error.clone().map(|error| {
                    observed(ids::SUGGEST_ERROR)
                        .pt(dp(8.))
                        .child(common::error_banner(error, &tokens))
                }))
        }
    }
}

/// The configuration of the Rename branch dialog for `branch`; `on_accept` receives the trimmed new name (only
/// when it differs from the current one).
pub fn rename_branch_config(
    branch: &FullName,
    on_accept: impl Fn(String, &mut Window, &mut App) + 'static,
) -> SingleFieldConfig {
    let current = short_name(branch);
    let unchanged = current.clone();
    let mut config = SingleFieldConfig::new(
        AppIcon::Branch,
        "Rename branch",
        "Rename branch",
        move |name, window, cx| {
            let name = name.trim().to_owned();
            if name != unchanged {
                on_accept(name, window, cx);
            }
        },
    );
    config.subtitle = Some(format!("Set a new name to the branch \"{current}\"").into());
    config.initial_text = current;
    config
}

/// Opens the Rename branch dialog (the current name is prefilled and selected).
pub fn open_rename_branch(window: &mut Window, cx: &mut App, tab: TabCtx, branch: FullName) {
    let target = branch.clone();
    let config = rename_branch_config(&branch, move |name, window, cx| {
        let target = target.clone();
        tab.with_tab(window, cx, move |t, window, cx| {
            t.rename_branch(target, name, window, cx);
        });
    });
    PrefilledFieldDialog::open(window, cx, config);
}

/// The "Choose another local name" single-field dialog (checkout of a remote branch whose name exists); calls
/// `on_accept` with the chosen name.
pub fn choose_local_name(
    window: &mut Window,
    cx: &mut App,
    suggested: String,
    on_accept: Rc<dyn Fn(String, &mut Window, &mut App)>,
) {
    let mut config = SingleFieldConfig::new(
        AppIcon::Branch,
        "Choose another local name",
        "Checkout",
        move |name, window, cx| on_accept(name.trim().to_owned(), window, cx),
    );
    config.subtitle = Some(
        format!("A local branch named \"{suggested}\" already exists. Choose another name for the new local branch.")
            .into(),
    );
    config.initial_text = suggested;
    PrefilledFieldDialog::open(window, cx, config);
}

/// A remote of the Change-upstream dialog with its selectable branches.
#[derive(Clone, Debug, PartialEq)]
struct RemoteChoice {
    name: String,
    /// `(simple name, full remote-tracking ref)`, `HEAD` excluded.
    branches: Vec<(String, FullName)>,
}

fn remote_choices(refs: &RefList) -> Vec<RemoteChoice> {
    refs.remotes
        .iter()
        .map(|remote| RemoteChoice {
            name: remote.name.clone(),
            branches: remote
                .branches
                .iter()
                .filter(|b| b.name != "HEAD")
                .map(|b| (b.name.clone(), b.full.clone()))
                .collect(),
        })
        .collect()
}

/// The preselection of the Change-upstream dialog: the index of the tracking remote and of its branch.
fn preselect(
    refs: &RefList,
    remotes: &[RemoteChoice],
    branch: &FullName,
) -> (Option<usize>, Option<usize>) {
    let Some(upstream) = refs
        .local
        .iter()
        .find(|b| &b.full == branch)
        .and_then(|b| b.upstream.as_ref())
    else {
        return (None, None);
    };
    let Some(remote_ix) = remotes.iter().position(|r| r.name == upstream.remote) else {
        return (None, None);
    };
    let merged = short_name(&upstream.merge);
    let branch_ix = remotes[remote_ix]
        .branches
        .iter()
        .position(|(name, _)| *name == merged);
    (Some(remote_ix), branch_ix)
}

fn names(items: impl Iterator<Item = String>) -> Vec<SharedString> {
    items.map(SharedString::from).collect()
}

/// The content of the Change-upstream dialog.
struct UpstreamDialog {
    tab: TabCtx,
    branch: FullName,
    remotes: Vec<RemoteChoice>,
    remote_dd: Entity<FilterDropdown>,
    branch_dd: Entity<FilterDropdown>,
    remote_sel: Option<usize>,
    branch_sel: Option<usize>,
    /// The current tracking remote and branch (indices), restored when its remote is chosen again.
    tracking: (Option<usize>, Option<usize>),
    /// The caller's completion callback; taken by Accept (`true` when the operation started) or by the drop of the
    /// dialog (`false`: Cancel, Escape, any other way of closing it).
    on_done: Option<OnDone>,
    _subscriptions: [Subscription; 2],
}

/// Receives whether the dialog was accepted and the upstream operation started. Runs without a window: it only
/// records the answer.
pub type OnDone = Box<dyn FnOnce(bool)>;

impl Drop for UpstreamDialog {
    fn drop(&mut self) {
        if let Some(done) = self.on_done.take() {
            done(false);
        }
    }
}

impl UpstreamDialog {
    fn branch_options(&self) -> Vec<SharedString> {
        match self.remote_sel.and_then(|ix| self.remotes.get(ix)) {
            Some(remote) => names(remote.branches.iter().map(|(name, _)| name.clone())),
            None => Vec::new(),
        }
    }

    fn selected_ref(&self) -> Option<FullName> {
        let remote = self.remotes.get(self.remote_sel?)?;
        remote
            .branches
            .get(self.branch_sel?)
            .map(|(_, full)| full.clone())
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(upstream) = self.selected_ref() else {
            return;
        };
        window.close_dialog(cx);
        let branch = self.branch.clone();
        let started = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = started.clone();
        self.tab.with_tab(window, cx, move |t, window, cx| {
            flag.set(t.set_upstream(branch, Some(upstream), window, cx));
        });
        // An operation that was refused (the tab is busy) or a tab that is gone starts nothing to wait for.
        if let Some(done) = self.on_done.take() {
            done(started.get());
        }
    }
}

impl Render for UpstreamDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let this = cx.weak_entity();
        let enabled = self.selected_ref().is_some();
        let content = observed(ids::UPSTREAM_DIALOG)
            .w(dp(UPSTREAM_W))
            .flex()
            .flex_col()
            .gap(dp(8.))
            .child(
                div()
                    .pt(dp(8.))
                    .child(common::form_label("Remote", &tokens)),
            )
            .child(self.remote_dd.clone())
            .child(
                div()
                    .pt(dp(8.))
                    .child(common::form_label("Branch", &tokens)),
            )
            .child(self.branch_dd.clone());
        IconBasedDialog::new(AppIcon::Branch, "Change upstream branch", "Accept")
            .subtitle("Set the upstream remote branch")
            .primary_enabled(enabled)
            .on_primary(move |window, cx| {
                this.update(cx, |d, cx| d.accept(window, cx)).ok();
            })
            .content(content)
    }
}

/// Opens the Change upstream dialog of `branch`.
pub fn open_change_upstream(window: &mut Window, cx: &mut App, tab: TabCtx, branch: FullName) {
    open_change_upstream_then(window, cx, tab, branch, Box::new(|_| {}));
}

/// Opens the Change upstream dialog of `branch`; `on_done` learns exactly once whether it was accepted and the
/// upstream operation started, or not (closed without a choice: Cancel, Escape; or the operation was refused).
pub fn open_change_upstream_then(
    window: &mut Window,
    cx: &mut App,
    tab: TabCtx,
    branch: FullName,
    on_done: OnDone,
) {
    let refs = tab.model.read(cx).refs.value.clone();
    let (remotes, remote_sel, branch_sel) = match refs.as_deref() {
        Some(refs) => {
            let remotes = remote_choices(refs);
            let (remote_sel, branch_sel) = preselect(refs, &remotes, &branch);
            (remotes, remote_sel, branch_sel)
        }
        None => (Vec::new(), None, None),
    };
    let remote_names = names(remotes.iter().map(|r| r.name.clone()));
    let branch_names = remote_sel
        .and_then(|ix| remotes.get(ix))
        .map(|r| names(r.branches.iter().map(|(name, _)| name.clone())))
        .unwrap_or_default();
    let remote_dd = cx.new(|cx| {
        let mut dd =
            FilterDropdown::new(ids::UPSTREAM_REMOTE, remote_names, remote_sel, window, cx);
        dd.set_width(UPSTREAM_W, cx);
        dd
    });
    let branch_dd = cx.new(|cx| {
        let mut dd =
            FilterDropdown::new(ids::UPSTREAM_BRANCH, branch_names, branch_sel, window, cx);
        dd.set_width(UPSTREAM_W, cx);
        dd
    });
    let view = cx.new(|cx| {
        let on_remote = cx.subscribe(
            &remote_dd,
            |this: &mut UpstreamDialog, _, event: &FilterDropdownEvent, cx| {
                let FilterDropdownEvent::Selected(ix) = event;
                if this.remote_sel == Some(*ix) {
                    return;
                }
                this.remote_sel = Some(*ix);
                // Changing the remote resets the branch: the tracking branch if it is on this remote, else none.
                this.branch_sel = match this.tracking {
                    (Some(remote), Some(branch)) if remote == *ix => Some(branch),
                    _ => None,
                };
                let options = this.branch_options();
                let selected = this.branch_sel;
                this.branch_dd
                    .update(cx, |dd, cx| dd.set_options(options, selected, cx));
                cx.notify();
            },
        );
        let on_branch = cx.subscribe(
            &branch_dd,
            |this: &mut UpstreamDialog, _, event: &FilterDropdownEvent, cx| {
                let FilterDropdownEvent::Selected(ix) = event;
                this.branch_sel = Some(*ix);
                cx.notify();
            },
        );
        UpstreamDialog {
            tab,
            branch,
            remotes,
            remote_dd,
            branch_dd,
            remote_sel,
            branch_sel,
            tracking: (remote_sel, branch_sel),
            on_done: Some(on_done),
            _subscriptions: [on_remote, on_branch],
        }
    });
    common::open_view(window, cx, UPSTREAM_W, 420., view);
}
