//! Author dialogs (G10): Author settings (09 §3.10), opened from the bottom bar, and Commit author identity
//! (09 §3.11), asked before a commit when Git has no identity.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div,
};

use super::IconBasedDialog;
use super::register::common::{self, FocusOnce};
use crate::backend::present::ErrorDialogData;
use crate::backend::{ConfigScope, Identity, IdentityInfo, PartialIdentity, git, loaders};
use crate::icons::{AppIcon, icon};
use crate::metrics::dialogs::{AUTHOR_LABEL_W, AUTHOR_PAD_H, AUTHOR_ROW_W, IDENTITY_W};
use crate::metrics::{self, dp};
use crate::tab::TabCtx;
use crate::tab::ops::OpKind;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::{CheckboxText, PrimaryButton, observed};

/// What the Commit author identity dialog decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityChoice {
    /// Use this identity for one commit (`CommitOptions::identity_override`).
    Override(Identity),
    /// The identity was saved (`save_author`); commit normally.
    Saved,
}

/// Element ids of the author dialogs.
pub mod ids {
    /// The Author settings content.
    pub const AUTHOR_DIALOG: &str = "author-dialog";
    /// Global name field.
    pub const AUTHOR_GLOBAL_NAME: &str = "author-global-name";
    /// Global e-mail field.
    pub const AUTHOR_GLOBAL_EMAIL: &str = "author-global-email";
    /// Repository name field.
    pub const AUTHOR_REPO_NAME: &str = "author-repo-name";
    /// Repository e-mail field.
    pub const AUTHOR_REPO_EMAIL: &str = "author-repo-email";
    /// The "Repository-level values will override global values" row.
    pub const AUTHOR_OVERRIDE_NOTE: &str = "author-override-note";
    /// The Commit author identity content.
    pub const IDENTITY_DIALOG: &str = "identity-dialog";
    /// Its name field.
    pub const IDENTITY_NAME: &str = "identity-name";
    /// Its e-mail field.
    pub const IDENTITY_EMAIL: &str = "identity-email";
    /// Its "Save identity" checkbox.
    pub const IDENTITY_SAVE: &str = "identity-save";
}

/// `Some(identity)` for what the user typed; `None` (unset the keys) when both fields are empty.
pub fn identity_of(name: &str, email: &str) -> Option<Identity> {
    let (name, email) = (name.trim(), email.trim());
    (!name.is_empty() || !email.is_empty()).then(|| Identity {
        name: name.to_owned(),
        email: email.to_owned(),
    })
}

fn texts_of(identity: &PartialIdentity) -> (String, String) {
    (
        identity.name.clone().unwrap_or_default(),
        identity.email.clone().unwrap_or_default(),
    )
}

/// The scopes whose identity differs from what the dialog loaded, as the edits `save_authors` takes.
pub fn changed_scopes(
    initial_global: &(String, String),
    global: (&str, &str),
    initial_local: Option<&(String, String)>,
    local: Option<(&str, &str)>,
) -> Vec<(ConfigScope, Option<Identity>)> {
    let same = |initial: &(String, String), now: (&str, &str)| {
        initial.0.trim() == now.0.trim() && initial.1.trim() == now.1.trim()
    };
    let mut edits = Vec::new();
    if !same(initial_global, global) {
        edits.push((ConfigScope::Global, identity_of(global.0, global.1)));
    }
    if let (Some(initial), Some(now)) = (initial_local, local)
        && !same(initial, now)
    {
        edits.push((ConfigScope::Local, identity_of(now.0, now.1)));
    }
    edits
}

// ------------------------------------------------------------------------------------------------ Author settings

/// The content of the Author settings dialog.
struct AuthorSettingsDialog {
    tab: Option<TabCtx>,
    global_name: Entity<OutlinedField>,
    global_email: Entity<OutlinedField>,
    repo_name: Entity<OutlinedField>,
    repo_email: Entity<OutlinedField>,
    initial_global: (String, String),
    initial_local: (String, String),
    /// The user edited a field (a late load must not overwrite it).
    touched: bool,
    focus: FocusOnce,
    _subscriptions: Vec<Subscription>,
    _load: Option<gpui_kit::Task<()>>,
}

impl AuthorSettingsDialog {
    fn apply_info(&mut self, info: &IdentityInfo, window: &mut Window, cx: &mut Context<Self>) {
        self.initial_global = texts_of(&info.global);
        self.initial_local = texts_of(&info.local);
        let (gn, ge) = self.initial_global.clone();
        let (ln, le) = self.initial_local.clone();
        for (field, text) in [
            (&self.global_name, gn),
            (&self.global_email, ge),
            (&self.repo_name, ln),
            (&self.repo_email, le),
        ] {
            // A late load fills every field the user has not typed into yet. Leaving such a field empty would
            // make Save treat the loaded value as erased and unset it.
            if !self.touched || common::text_of(field, cx).is_empty() {
                field.update(cx, |f, cx| f.set_text(&text, window, cx));
            }
        }
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let global_name = common::text_of(&self.global_name, cx);
        let global_email = common::text_of(&self.global_email, cx);
        let repo_name = common::text_of(&self.repo_name, cx);
        let repo_email = common::text_of(&self.repo_email, cx);
        window.close_dialog(cx);
        let edits = changed_scopes(
            &self.initial_global,
            (&global_name, &global_email),
            self.tab.as_ref().map(|_| &self.initial_local),
            self.tab.as_ref().map(|_| (&*repo_name, &*repo_email)),
        );
        if edits.is_empty() {
            return;
        }
        match &self.tab {
            Some(tab) => tab.with_tab(window, cx, move |t, window, cx| {
                t.save_authors(edits, None, window, cx);
            }),
            None => save_global_without_tab(edits, cx),
        }
    }
}

/// Without a repository tab (the welcome page) the global identity is written directly, off the UI thread.
fn save_global_without_tab(edits: Vec<(ConfigScope, Option<Identity>)>, cx: &mut App) {
    let task = crate::backend::bg(cx, move || {
        for (scope, identity) in &edits {
            git::config::set_identity(None, *scope, identity.as_ref())?;
        }
        Ok::<(), crate::backend::GitError>(())
    });
    cx.spawn(async move |cx| {
        if let Err(err) = task.await {
            cx.update(|cx| {
                let mut data =
                    ErrorDialogData::new(OpKind::SaveAuthor.error_title(), err.to_string());
                data.details = err.stderr().map(SharedString::from);
                crate::app_view::show_error(cx, data);
            });
        }
    })
    .detach();
}

fn author_row(
    label: &str,
    field: &Entity<OutlinedField>,
    tokens: &crate::theme::Tokens,
) -> impl IntoElement {
    div()
        .w(dp(AUTHOR_ROW_W))
        .py(dp(8.))
        .flex()
        .flex_row()
        .items_center()
        .child(
            div()
                .flex_none()
                .w(dp(AUTHOR_LABEL_W))
                .pr(dp(16.))
                .text_body1()
                .text_color(tokens.on_background)
                .child(label.to_owned()),
        )
        .child(div().flex_1().child(field.clone()))
}

impl Render for AuthorSettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        self.focus.run(&self.global_name, false, window, cx);
        let has_repo = self.tab.is_some();
        let overrides = has_repo
            && (!common::text_of(&self.repo_name, cx).trim().is_empty()
                || !common::text_of(&self.repo_email, cx).trim().is_empty());
        let this = cx.weak_entity();
        observed(ids::AUTHOR_DIALOG)
            .w(dp(AUTHOR_ROW_W + 2. * AUTHOR_PAD_H))
            .flex()
            .flex_col()
            .child(
                div()
                    .px(dp(AUTHOR_PAD_H))
                    .flex()
                    .flex_col()
                    .child(div().py(dp(8.)).text_h4().child("Global settings"))
                    .child(author_row("Name", &self.global_name, &tokens))
                    .child(author_row("Email", &self.global_email, &tokens))
                    .when(has_repo, |col| {
                        col.child(
                            div()
                                .pt(dp(16.))
                                .pb(dp(8.))
                                .text_h4()
                                .child("Repository settings"),
                        )
                        .child(author_row("Name", &self.repo_name, &tokens))
                        .child(author_row("Email", &self.repo_email, &tokens))
                        .child(
                            observed(ids::AUTHOR_OVERRIDE_NOTE)
                                .opacity(if overrides { 1. } else { 0. })
                                .flex()
                                .flex_row()
                                .items_center()
                                .child(icon(AppIcon::Warning, metrics::ICON_SM, tokens.secondary))
                                .child(
                                    div().pl(dp(4.)).py(dp(8.)).text_body2().child(
                                        "Repository-level values will override global values",
                                    ),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .pt(dp(16.))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(
                        div().pr(dp(8.)).child(
                            PrimaryButton::new("dialog-cancel", "Cancel")
                                .cancel()
                                .on_click(|window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(PrimaryButton::new("dialog-primary", "Save data").on_click(
                        move |window, cx| {
                            this.update(cx, |d, cx| d.save(window, cx)).ok();
                        },
                    )),
            )
    }
}

/// Opens the Author settings dialog (global and, with a tab, repository identity).
pub fn open_author_settings(window: &mut Window, cx: &mut App, tab: Option<TabCtx>) {
    let global_name = common::new_field(ids::AUTHOR_GLOBAL_NAME, None, "", window, cx);
    let global_email = common::new_field(ids::AUTHOR_GLOBAL_EMAIL, None, "", window, cx);
    let repo_name = common::new_field(ids::AUTHOR_REPO_NAME, None, "", window, cx);
    let repo_email = common::new_field(ids::AUTHOR_REPO_EMAIL, None, "", window, cx);
    let known = tab
        .as_ref()
        .and_then(|tab| tab.model.read(cx).identity.value.clone());
    let repo = tab.as_ref().and_then(|tab| tab.backend.repo().cloned());
    let view = cx.new(|cx| {
        let mut subscriptions = Vec::new();
        for field in [&global_name, &global_email, &repo_name, &repo_email] {
            subscriptions.push(cx.subscribe(
                field,
                |this: &mut AuthorSettingsDialog, _, event: &FieldEvent, cx| {
                    if *event == FieldEvent::Changed {
                        this.touched = true;
                        cx.notify();
                    }
                },
            ));
        }
        AuthorSettingsDialog {
            tab: tab.clone(),
            global_name,
            global_email,
            repo_name,
            repo_email,
            initial_global: Default::default(),
            initial_local: Default::default(),
            touched: false,
            focus: FocusOnce::default(),
            _subscriptions: subscriptions,
            _load: None,
        }
    });
    match known {
        Some(info) => {
            view.update(cx, |d, cx| d.apply_info(&info, window, cx));
        }
        None if repo.is_some() || tab.is_none() => {
            // Not loaded yet (or the global identity only): read it in the background.
            let task = crate::backend::bg(cx, move || loaders::identity(repo.as_ref()));
            let load = view.update(cx, |_, cx| {
                cx.spawn_in(window, async move |this, cx| {
                    if let Ok(info) = task.await {
                        this.update_in(cx, |d, window, cx| d.apply_info(&info, window, cx))
                            .ok();
                    }
                })
            });
            view.update(cx, |d, _| d._load = Some(load));
        }
        None => {}
    }
    common::open_view(
        window,
        cx,
        AUTHOR_ROW_W + 2. * AUTHOR_PAD_H,
        if tab.is_some() { 420. } else { 260. },
        view,
    );
}

// ------------------------------------------------------------------------------------------- Commit identity

type OnDone = Rc<dyn Fn(IdentityChoice, &mut Window, &mut App)>;

/// The content of the Commit author identity dialog.
struct CommitIdentityDialog {
    tab: TabCtx,
    name: Entity<OutlinedField>,
    email: Entity<OutlinedField>,
    save: bool,
    on_done: OnDone,
    focus: FocusOnce,
    _subscriptions: [Subscription; 2],
}

impl CommitIdentityDialog {
    fn identity(&self, cx: &App) -> Option<Identity> {
        let name = common::text_of(&self.name, cx).trim().to_owned();
        let email = common::text_of(&self.email, cx).trim().to_owned();
        (!name.is_empty() && !email.is_empty()).then_some(Identity { name, email })
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(identity) = self.identity(cx) else {
            return;
        };
        window.close_dialog(cx);
        let on_done = self.on_done.clone();
        if self.save {
            // Save into the user's .gitconfig first; the commit continues once that is written.
            let saved = {
                let on_done = on_done.clone();
                Box::new(move |window: &mut Window, cx: &mut App| {
                    on_done(IdentityChoice::Saved, window, cx)
                })
            };
            self.tab.with_tab(window, cx, move |t, window, cx| {
                t.save_author_then(ConfigScope::Global, Some(identity), saved, window, cx);
            });
        } else {
            on_done(IdentityChoice::Override(identity), window, cx);
        }
    }
}

impl Render for CommitIdentityDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        self.focus.run(&self.name, false, window, cx);
        let this = cx.weak_entity();
        let toggle = this.clone();
        let labelled = |label: &str, field: &Entity<OutlinedField>| {
            div()
                .w(dp(IDENTITY_W))
                .py(dp(8.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .pb(dp(8.))
                        .child(common::form_label(label.to_owned(), &tokens)),
                )
                .child(field.clone())
        };
        let content = observed(ids::IDENTITY_DIALOG)
            .w(dp(IDENTITY_W))
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .pb(dp(8.))
                    .flex()
                    .justify_center()
                    .text_center()
                    .text_body2()
                    .semibold()
                    .child("What identity would you like to use for this commit?"),
            )
            .child(labelled("Name", &self.name))
            .child(labelled("Email", &self.email))
            .child(
                CheckboxText::new(
                    ids::IDENTITY_SAVE,
                    self.save,
                    "Save identity in the .gitconfig file",
                )
                .on_toggle(move |checked, _, cx| {
                    toggle
                        .update(cx, |d, cx| {
                            d.save = checked;
                            cx.notify();
                        })
                        .ok();
                }),
            );
        IconBasedDialog::new(AppIcon::Accounts, "Author identity", "Continue")
            .subtitle("Your Git configuration does not have a user identity set.")
            .primary_enabled(self.identity(cx).is_some())
            .content(content)
            .on_primary(move |window, cx| {
                this.update(cx, |d, cx| d.accept(window, cx)).ok();
            })
    }
}

/// Opens the Commit author identity dialog; `on_done` receives the decision (not called on Cancel).
pub fn open_commit_identity(window: &mut Window, cx: &mut App, tab: TabCtx, on_done: OnDone) {
    let known = tab
        .model
        .read(cx)
        .identity
        .value
        .as_ref()
        .map(|info| (info.global.clone(), info.local.clone()))
        .unwrap_or_default();
    // A half-configured identity is offered as it is (name without e-mail, ...); local values win over global ones.
    let name = known
        .1
        .name
        .clone()
        .or(known.0.name.clone())
        .unwrap_or_default();
    let email = known.1.email.clone().or(known.0.email).unwrap_or_default();
    let name_field = common::new_field(ids::IDENTITY_NAME, None, &name, window, cx);
    let email_field = common::new_field(ids::IDENTITY_EMAIL, None, &email, window, cx);
    let view = cx.new(|cx| {
        let on_name = cx.subscribe(&name_field, |_, _, _: &FieldEvent, cx| cx.notify());
        let on_email = cx.subscribe_in(
            &email_field,
            window,
            |this: &mut CommitIdentityDialog, _, event: &FieldEvent, window, cx| match event {
                FieldEvent::Changed => cx.notify(),
                FieldEvent::Enter => this.accept(window, cx),
            },
        );
        CommitIdentityDialog {
            tab,
            name: name_field,
            email: email_field,
            save: false,
            on_done,
            focus: FocusOnce::default(),
            _subscriptions: [on_name, on_email],
        }
    });
    common::open_view(window, cx, IDENTITY_W, 480., view);
}
