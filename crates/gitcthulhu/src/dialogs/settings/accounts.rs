//! The Accounts settings page (D0.1; owned by X4; spec 21 §5.4): three groups (GitHub, GitLab, Bitbucket), each a
//! group row with the sign-in buttons on the right and one row per account (32 dp avatar, name and kind, validity
//! text, [Check] [Forget]). The page is a view over the app-wide `forge::AccountsModel`: it reads the list, never
//! the network.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Global, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div,
};

use super::SettingRow;
use crate::backend::forge::{AccountInfo, Forge};
use crate::forge::accounts::{AccountRow, AccountsModel, LoadState, RowValidity};
use crate::forge::flows::{TokenFlow, open_device_dialog, open_token_dialog};
use crate::metrics::dp;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::avatar::Avatar;
use crate::widgets::{SecondaryButton, observed};

/// Element ids of the page (tests, QA).
pub mod ids {
    use gpui_kit::SharedString;

    use super::Forge;

    /// The page.
    pub const PAGE: &str = "accounts-page";
    /// "Sign in with browser" (GitHub; only when the build has an OAuth client id).
    pub const GITHUB_BROWSER: &str = "accounts-github-browser";
    /// "Add token" of GitHub.
    pub const GITHUB_TOKEN: &str = "accounts-github-token";
    /// "Add Enterprise…".
    pub const GITHUB_ENTERPRISE: &str = "accounts-github-enterprise";
    /// "Add token" of GitLab.
    pub const GITLAB_TOKEN: &str = "accounts-gitlab-token";
    /// "Add self-hosted…".
    pub const GITLAB_SELF_HOSTED: &str = "accounts-gitlab-selfhosted";
    /// "Add API token" of Bitbucket.
    pub const BITBUCKET_TOKEN: &str = "accounts-bitbucket-token";
    /// The message under the groups when the accounts cannot be read.
    pub const NOTE: &str = "accounts-note";

    /// The group row of `forge`.
    pub fn group(forge: Forge) -> &'static str {
        match forge {
            Forge::GitHub => "github",
            Forge::GitLab => "gitlab",
            Forge::Bitbucket | Forge::Azure => "bitbucket",
        }
    }

    /// The row of an account.
    pub fn row(forge: Forge, name: &str) -> SharedString {
        format!("accounts-row-{}-{name}", group(forge)).into()
    }

    /// The avatar of an account row.
    pub fn avatar(forge: Forge, name: &str) -> SharedString {
        format!("accounts-avatar-{}-{name}", group(forge)).into()
    }

    /// The validity text of an account row.
    pub fn validity(forge: Forge, name: &str) -> SharedString {
        format!("accounts-validity-{}-{name}", group(forge)).into()
    }

    /// The [Check] button of an account row.
    pub fn check(forge: Forge, name: &str) -> SharedString {
        format!("accounts-check-{}-{name}", group(forge)).into()
    }

    /// The [Forget] button of an account row.
    pub fn forget(forge: Forge, name: &str) -> SharedString {
        format!("accounts-forget-{}-{name}", group(forge)).into()
    }
}

/// Size of the avatar of an account row (21 §5.4).
pub const AVATAR_SIZE: f32 = 32.;
/// Left indent of the account rows under their group row.
const ROW_INDENT: f32 = 16.;
/// Gap between the controls of a row.
const CONTROL_GAP: f32 = 8.;

struct PageGlobal(Entity<AccountsPage>);

impl Global for PageGlobal {}

/// The page content: the view over the accounts model (created on first use, then kept).
pub fn page(window: &mut Window, cx: &mut App) -> AnyElement {
    let _ = window;
    let existing = cx.try_global::<PageGlobal>().map(|g| g.0.clone());
    let view = match existing {
        Some(view) => view,
        None => {
            crate::forge::accounts::install(cx);
            let Some(model) = crate::forge::accounts::installed(cx) else {
                return div().into_any_element();
            };
            let view = cx.new(|cx| AccountsPage::new(model, cx));
            cx.set_global(PageGlobal(view.clone()));
            view
        }
    };
    view.into_any_element()
}

struct AccountsPage {
    model: Entity<AccountsModel>,
    _subscription: Subscription,
}

impl AccountsPage {
    fn new(model: Entity<AccountsModel>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            _subscription: subscription,
        }
    }

    fn group_row(&self, forge: Forge, device_flow: bool, cx: &mut Context<Self>) -> AnyElement {
        let _ = cx;
        let (title, subtitle) = match forge {
            Forge::GitHub => ("GitHub", "github.com and GitHub Enterprise Server"),
            Forge::GitLab => ("GitLab", "gitlab.com and self-hosted GitLab"),
            Forge::Bitbucket | Forge::Azure => ("Bitbucket", "bitbucket.org"),
        };
        let button = |id: &'static str, label: &'static str, flow: TokenFlow| {
            SecondaryButton::new(id, label).on_click(move |window, cx| {
                open_token_dialog(window, cx, flow);
            })
        };
        let controls = match forge {
            Forge::GitHub => div()
                .flex()
                .flex_row()
                .gap(dp(CONTROL_GAP))
                .when(device_flow, |row| {
                    row.child(
                        SecondaryButton::new(ids::GITHUB_BROWSER, "Sign in with browser")
                            .on_click(open_device_dialog),
                    )
                })
                .child(button(ids::GITHUB_TOKEN, "Add token", TokenFlow::GitHub))
                .child(button(
                    ids::GITHUB_ENTERPRISE,
                    "Add Enterprise…",
                    TokenFlow::GitHubEnterprise,
                )),
            Forge::GitLab => div()
                .flex()
                .flex_row()
                .gap(dp(CONTROL_GAP))
                .child(button(ids::GITLAB_TOKEN, "Add token", TokenFlow::GitLab))
                .child(button(
                    ids::GITLAB_SELF_HOSTED,
                    "Add self-hosted…",
                    TokenFlow::GitLabSelfHosted,
                )),
            Forge::Bitbucket | Forge::Azure => div().flex().flex_row().child(button(
                ids::BITBUCKET_TOKEN,
                "Add API token",
                TokenFlow::Bitbucket,
            )),
        };
        SettingRow::new(ids::group(forge), title)
            .subtitle(subtitle)
            .control(controls)
            .into_any_element()
    }

    fn account_row(&self, row: &AccountRow, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let info: &AccountInfo = &row.info;
        let (forge, name) = (info.forge, info.name.clone());
        let validity_color = match row.validity {
            RowValidity::Valid => tokens.add_file,
            RowValidity::Invalid | RowValidity::NoCredentials | RowValidity::Failed(_) => {
                tokens.error
            }
            RowValidity::Unchecked | RowValidity::Checking => tokens.on_background_secondary,
        };
        let model = self.model.downgrade();
        let id = info.id.clone();
        let check = {
            let (model, id) = (model.clone(), id.clone());
            SecondaryButton::new(ids::check(forge, &name), "Check").on_click(move |_, cx| {
                let id = id.clone();
                model.update(cx, |model, cx| model.check(&id, cx)).ok();
            })
        };
        let forget = {
            SecondaryButton::new(ids::forget(forge, &name), "Forget").on_click(move |_, cx| {
                let id = id.clone();
                model.update(cx, |model, cx| model.forget(&id, cx)).ok();
            })
        };
        observed(ids::row(forge, &name))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .py(dp(4.))
            .pl(dp(ROW_INDENT))
            .child(
                observed(ids::avatar(forge, &name))
                    .flex_none()
                    .mr(dp(12.))
                    .child(Avatar::new(name.clone(), "", AVATAR_SIZE)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_body1()
                            .text_color(tokens.on_background)
                            .child(SharedString::from(name.clone())),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(tokens.on_background_secondary)
                            .child(SharedString::from(info.caption.clone())),
                    ),
            )
            .child(
                observed(ids::validity(forge, &name))
                    .flex_none()
                    .px(dp(CONTROL_GAP))
                    .text_body2()
                    .text_color(validity_color)
                    .child(SharedString::from(row.validity.text().to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .gap(dp(CONTROL_GAP))
                    .child(check)
                    .child(forget),
            )
            .into_any_element()
    }
}

impl Render for AccountsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        // Loading is a side effect: do it after this frame.
        let model = self.model.clone();
        window.defer(cx, move |_, cx| {
            model.update(cx, |model, cx| model.ensure_fresh(cx));
        });
        let device_flow = crate::app::services(cx)
            .is_some_and(|services| crate::backend::forge::device_flow_available(&services));
        let (rows, state) = {
            let model = self.model.read(cx);
            (model.rows().to_vec(), model.state().clone())
        };
        let mut children: Vec<AnyElement> = Vec::new();
        for forge in [Forge::GitHub, Forge::GitLab, Forge::Bitbucket] {
            children.push(self.group_row(forge, device_flow, cx));
            for row in rows.iter().filter(|row| row.info.forge == forge) {
                children.push(self.account_row(row, cx));
            }
        }
        let note = match &state {
            LoadState::Failed(message) => {
                Some(format!("The accounts could not be read: {message}"))
            }
            LoadState::Loading if rows.is_empty() => Some("Loading the accounts…".to_owned()),
            _ => None,
        };
        observed(ids::PAGE)
            .w_full()
            .flex()
            .flex_col()
            .children(children)
            .when_some(note, |page, note| {
                page.child(
                    observed(ids::NOTE)
                        .py(dp(8.))
                        .text_body2()
                        .text_color(tokens.on_background_secondary)
                        .child(note),
                )
            })
    }
}
