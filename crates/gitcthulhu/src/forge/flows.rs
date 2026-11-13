//! The sign-in dialogs of the Accounts page (spec 21 §5.4): token dialogs for every forge (the token is masked, the
//! help text names the required scopes, 15 §2) and the GitHub device-flow dialog (64 dp `Github` icon, code in Mono
//! 20 with Copy, [Open browser], a spinner, a 15 minute timeout, Cancel).
//!
//! Every call to a forge runs in `backend::bg`; a failed sign-in keeps the dialog open with the reason.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div,
};

use crate::backend::CancelToken;
use crate::backend::Sensitive;
use crate::backend::forge::{
    self as backend_forge, AccountInfo, DeviceCodeInfo, DevicePoll, Failure, FailureKind,
    NewAccount,
};
use crate::dialogs::IconBasedDialog;
use crate::dialogs::register::common::{self, FocusOnce};
use crate::icons::AppIcon;
use crate::metrics::dp;
use crate::notifications::ToastKind;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::{CircularProgress, IconButton, TextLink, observed};

/// The content width of the sign-in dialogs.
const CONTENT_W: f32 = 420.;
/// The code expires after 15 minutes (the dialog gives up then, whatever GitHub says).
pub const DEVICE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Element ids of the sign-in dialogs.
pub mod ids {
    /// The token dialog.
    pub const TOKEN_DIALOG: &str = "forge-token-dialog";
    /// The first field of a token dialog: the server URL or the email.
    pub const TOKEN_FIRST: &str = "forge-token-first";
    /// The masked token field.
    pub const TOKEN_FIELD: &str = "forge-token-field";
    /// The help text.
    pub const TOKEN_HELP: &str = "forge-token-help";
    /// The link that opens the forge's token page.
    pub const TOKEN_LINK: &str = "forge-token-link";
    /// The error banner of a failed sign-in.
    pub const TOKEN_ERROR: &str = "forge-token-error";
    /// The device-flow dialog.
    pub const DEVICE_DIALOG: &str = "forge-device-dialog";
    /// The code of the device flow.
    pub const DEVICE_CODE: &str = "forge-device-code";
    /// The Copy button next to the code.
    pub const DEVICE_COPY: &str = "forge-device-copy";
    /// The spinner.
    pub const DEVICE_PROGRESS: &str = "forge-device-progress";
    /// The status line under the code.
    pub const DEVICE_STATUS: &str = "forge-device-status";
}

/// Which token dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenFlow {
    /// github.com.
    GitHub,
    /// A GitHub Enterprise Server instance.
    GitHubEnterprise,
    /// gitlab.com.
    GitLab,
    /// A self-hosted GitLab.
    GitLabSelfHosted,
    /// Bitbucket Cloud.
    Bitbucket,
}

impl TokenFlow {
    /// The dialog title.
    pub fn title(self) -> &'static str {
        match self {
            TokenFlow::GitHub => "Add a GitHub token",
            TokenFlow::GitHubEnterprise => "Add a GitHub Enterprise server",
            TokenFlow::GitLab => "Add a GitLab token",
            TokenFlow::GitLabSelfHosted => "Add a self-hosted GitLab server",
            TokenFlow::Bitbucket => "Add a Bitbucket API token",
        }
    }

    fn icon(self) -> AppIcon {
        match self {
            TokenFlow::GitHub | TokenFlow::GitHubEnterprise => AppIcon::Github,
            TokenFlow::GitLab | TokenFlow::GitLabSelfHosted => AppIcon::Server,
            TokenFlow::Bitbucket => AppIcon::Password,
        }
    }

    /// The label and hint of the first field, if the flow has one.
    pub fn first_field(self) -> Option<(&'static str, &'static str)> {
        match self {
            TokenFlow::GitHubEnterprise => Some(("API URL", "https://ghe.example.com/api/v3")),
            TokenFlow::GitLabSelfHosted => Some(("Server URL", "https://gitlab.example.com")),
            TokenFlow::Bitbucket => Some(("Atlassian account email", "you@example.com")),
            TokenFlow::GitHub | TokenFlow::GitLab => None,
        }
    }

    /// What the token needs (15 §2).
    pub fn help(self) -> &'static str {
        match self {
            TokenFlow::GitHub => {
                "Create a personal access token (classic) with the \"repo\" scope. The token is stored in your system keychain."
            }
            TokenFlow::GitHubEnterprise => {
                "Enter the API address of your server and a personal access token (classic) with the \"repo\" scope. The token is stored in your system keychain."
            }
            TokenFlow::GitLab => {
                "Create a personal access token with the \"api\" scope. The token is stored in your system keychain."
            }
            TokenFlow::GitLabSelfHosted => {
                "Enter the address of your GitLab server and a personal access token with the \"api\" scope. The token is stored in your system keychain."
            }
            TokenFlow::Bitbucket => {
                "Create an Atlassian API token with the scopes read:user:bitbucket, read:repository:bitbucket, read:pullrequest:bitbucket and write:pullrequest:bitbucket. The token is stored in your system keychain."
            }
        }
    }

    /// The forge's token page: link text and URL.
    pub fn link(self) -> Option<(&'static str, &'static str)> {
        match self {
            TokenFlow::GitHub => Some((
                "Create a token on github.com",
                "https://github.com/settings/tokens/new?scopes=repo&description=GitCthulhu",
            )),
            TokenFlow::GitLab => Some((
                "Create a token on gitlab.com",
                "https://gitlab.com/-/user_settings/personal_access_tokens?name=GitCthulhu&scopes=api",
            )),
            TokenFlow::Bitbucket => Some((
                "Create a token on Atlassian",
                "https://id.atlassian.com/manage-profile/security/api-tokens",
            )),
            TokenFlow::GitHubEnterprise | TokenFlow::GitLabSelfHosted => None,
        }
    }

    /// The account to add from the dialog's texts.
    pub fn account(self, first: Option<String>, token: Sensitive) -> NewAccount {
        let first = first.unwrap_or_default();
        match self {
            TokenFlow::GitHub => NewAccount::GitHubToken { token },
            TokenFlow::GitHubEnterprise => NewAccount::GitHubEnterprise {
                api_url: first,
                token,
            },
            TokenFlow::GitLab => NewAccount::GitLabToken { token },
            TokenFlow::GitLabSelfHosted => NewAccount::GitLabSelfHosted { url: first, token },
            TokenFlow::Bitbucket => NewAccount::BitbucketToken {
                email: first,
                token,
            },
        }
    }
}

/// What a failed sign-in tells the user.
pub fn describe_failure(failure: &Failure) -> String {
    match failure.kind {
        FailureKind::Auth => {
            "The forge rejected this token. Check that it is correct, not expired and has the required scopes."
                .to_owned()
        }
        FailureKind::Offline => format!("The forge could not be reached. {}", failure.message),
        _ => failure.message.clone(),
    }
}

/// Tells the accounts model about a new account and thanks the user.
fn signed_in(info: &AccountInfo, cx: &mut App) {
    if let Some(model) = super::accounts::installed(cx) {
        model.update(cx, |model, cx| model.added(cx));
    }
    crate::app_view::show_toast(
        cx,
        None,
        ToastKind::Positive,
        format!("Signed in to {} as {}", info.forge.label(), info.name),
        None,
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Token dialog
// ---------------------------------------------------------------------------------------------------------------

/// Opens the token dialog of `flow`.
pub fn open_token_dialog(window: &mut Window, cx: &mut App, flow: TokenFlow) {
    let view = cx.new(|cx| TokenDialog::new(flow, window, cx));
    #[cfg(all(test, feature = "services"))]
    cx.set_global(OpenToken(view.downgrade()));
    common::open_view(window, cx, CONTENT_W, 420., view);
}

/// The token dialog opened last (tests fill it through it).
#[cfg(all(test, feature = "services"))]
pub(crate) struct OpenToken(pub gpui_kit::WeakEntity<TokenDialog>);

#[cfg(all(test, feature = "services"))]
impl gpui_kit::Global for OpenToken {}

/// The device-flow dialog opened last (tests read its state through it).
#[cfg(all(test, feature = "services"))]
pub(crate) struct OpenDevice(pub gpui_kit::WeakEntity<DeviceDialog>);

#[cfg(all(test, feature = "services"))]
impl gpui_kit::Global for OpenDevice {}

pub(crate) struct TokenDialog {
    flow: TokenFlow,
    first: Option<Entity<OutlinedField>>,
    token: Entity<OutlinedField>,
    error: Option<SharedString>,
    busy: bool,
    focus: FocusOnce,
    task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl TokenDialog {
    fn new(flow: TokenFlow, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let first = flow.first_field().map(|(_, hint)| {
            cx.new(|cx| OutlinedField::new(ids::TOKEN_FIRST, window, cx).hint(hint, window, cx))
        });
        let token = cx.new(|cx| {
            OutlinedField::password(ids::TOKEN_FIELD, window, cx).hint("Token", window, cx)
        });
        let mut subscriptions = Vec::new();
        for field in first.iter().chain(std::iter::once(&token)) {
            subscriptions.push(cx.subscribe_in(
                field,
                window,
                |this: &mut TokenDialog, _, event: &FieldEvent, window, cx| match event {
                    FieldEvent::Changed => cx.notify(),
                    FieldEvent::Enter => this.submit(window, cx),
                },
            ));
        }
        Self {
            flow,
            first,
            token,
            error: None,
            busy: false,
            focus: FocusOnce::default(),
            task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Types into the fields (tests).
    #[cfg(all(test, feature = "services"))]
    pub(crate) fn fill(
        &mut self,
        first: Option<&str>,
        token: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let (Some(field), Some(text)) = (&self.first, first) {
            field.update(cx, |f, cx| f.set_text(text, window, cx));
        }
        self.token.update(cx, |f, cx| f.set_text(token, window, cx));
    }

    /// Whether the error banner shows (tests).
    #[cfg(all(test, feature = "services"))]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn can_submit(&self, cx: &App) -> bool {
        let filled = |field: &Entity<OutlinedField>| !common::text_of(field, cx).trim().is_empty();
        !self.busy && self.first.as_ref().is_none_or(filled) && filled(&self.token)
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        let Some(services) = crate::app::services(cx) else {
            self.error =
                Some("The integrations are not started yet. Try again in a moment.".into());
            cx.notify();
            return;
        };
        let token = Sensitive::new(common::text_of(&self.token, cx).trim().to_owned());
        let first = self
            .first
            .as_ref()
            .map(|field| common::text_of(field, cx).trim().to_owned());
        let account = self.flow.account(first, token);
        self.busy = true;
        self.error = None;
        let work = crate::backend::bg(cx, move || backend_forge::add_account(&services, account));
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |dialog, window, cx| match result {
                Ok(info) => {
                    window.close_dialog(cx);
                    signed_in(&info, cx);
                }
                Err(failure) => {
                    dialog.busy = false;
                    dialog.error = Some(describe_failure(&failure).into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for TokenDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let first_focus = self.first.clone().unwrap_or_else(|| self.token.clone());
        self.focus.run(&first_focus, false, window, cx);
        let this = cx.weak_entity();
        let flow = self.flow;
        let first_label = flow.first_field().map(|(label, _)| label);
        let can_submit = self.can_submit(cx);
        let content = div()
            .w(dp(CONTENT_W))
            .flex()
            .flex_col()
            .child(
                observed(ids::TOKEN_HELP)
                    .pb(dp(8.))
                    .text_body2()
                    .text_color(tokens.on_background_secondary)
                    .child(flow.help()),
            )
            .when_some(flow.link(), |col, (text, url)| {
                col.child(
                    div().pb(dp(8.)).flex().flex_row().child(
                        TextLink::new(ids::TOKEN_LINK, text)
                            .on_click(move |_, cx| common::open_url(cx, url)),
                    ),
                )
            })
            .when_some(
                self.first.clone().zip(first_label),
                |col, (field, label)| {
                    col.child(common::form_label(label, &tokens))
                        .child(div().py(dp(8.)).child(field))
                },
            )
            .child(common::form_label("Token", &tokens))
            .child(div().py(dp(8.)).child(self.token.clone()))
            .when_some(self.error.clone(), |col, message| {
                col.child(observed(ids::TOKEN_ERROR).child(common::error_banner(message, &tokens)))
            });
        let on_primary = this;
        observed(ids::TOKEN_DIALOG).w(dp(CONTENT_W)).child(
            IconBasedDialog::new(
                flow.icon(),
                flow.title(),
                if self.busy {
                    "Signing in…"
                } else {
                    "Sign in"
                },
            )
            .content(content)
            .primary_enabled(can_submit)
            .on_primary(move |window, cx| {
                on_primary
                    .update(cx, |dialog, cx| dialog.submit(window, cx))
                    .ok();
            }),
        )
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Device flow
// ---------------------------------------------------------------------------------------------------------------

/// The calls of the device flow. The default talks to GitHub through the services; tests install their own as a
/// global.
#[derive(Clone)]
pub struct DeviceFlowBackend {
    /// Asks for a code.
    pub start: Arc<dyn Fn() -> Result<DeviceCodeInfo, Failure> + Send + Sync>,
    /// Polls once.
    pub poll:
        Arc<dyn Fn(&DeviceCodeInfo, &CancelToken) -> Result<DevicePoll, Failure> + Send + Sync>,
}

impl gpui_kit::Global for DeviceFlowBackend {}

impl DeviceFlowBackend {
    fn real(services: crate::backend::services::ServicesHandle) -> Self {
        let for_poll = services.clone();
        Self {
            start: Arc::new(move || backend_forge::device_start(&services)),
            poll: Arc::new(move |code, cancel| backend_forge::device_poll(&for_poll, code, cancel)),
        }
    }
}

/// Where the device flow stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceState {
    /// Asking GitHub for a code.
    Starting,
    /// The user enters the code in the browser; the dialog polls.
    Waiting(DeviceCodeInfo),
    /// The flow failed or the code expired (message).
    Failed(String),
}

/// Opens the device-flow dialog (only offered when the build has an OAuth client id).
pub fn open_device_dialog(window: &mut Window, cx: &mut App) {
    let backend = match cx.try_global::<DeviceFlowBackend>().cloned() {
        Some(backend) => backend,
        None => match crate::app::services(cx) {
            Some(services) => DeviceFlowBackend::real(services),
            None => {
                crate::app_view::show_toast(
                    cx,
                    None,
                    ToastKind::Warning,
                    "The integrations are not started yet",
                    None,
                );
                return;
            }
        },
    };
    let view = cx.new(|cx| {
        let mut dialog = DeviceDialog {
            backend,
            state: DeviceState::Starting,
            cancel: CancelToken::new(),
            task: None,
        };
        dialog.start(window, cx);
        dialog
    });
    #[cfg(all(test, feature = "services"))]
    cx.set_global(OpenDevice(view.downgrade()));
    common::open_view(window, cx, CONTENT_W, 380., view);
}

pub(crate) struct DeviceDialog {
    backend: DeviceFlowBackend,
    state: DeviceState,
    cancel: CancelToken,
    task: Option<Task<()>>,
}

impl DeviceDialog {
    /// Where the flow stands (tests).
    #[cfg(all(test, feature = "services"))]
    pub(crate) fn state(&self) -> &DeviceState {
        &self.state
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.cancel = CancelToken::new();
        self.state = DeviceState::Starting;
        let backend = self.backend.clone();
        let cancel = self.cancel.clone();
        let start = {
            let start = backend.start.clone();
            crate::backend::bg(cx, move || start())
        };
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let code = match start.await {
                Ok(code) => code,
                Err(failure) => {
                    this.update(cx, |dialog, cx| {
                        dialog.state = DeviceState::Failed(describe_failure(&failure));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let shown = code.clone();
            if this
                .update(cx, |dialog, cx| {
                    dialog.state = DeviceState::Waiting(shown);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            let interval = Duration::from_secs(code.interval.max(1));
            let limit = DEVICE_TIMEOUT.min(Duration::from_secs(code.expires_in.max(1)));
            let mut waited = Duration::ZERO;
            loop {
                cx.background_executor().timer(interval).await;
                waited += interval;
                if cancel.is_cancelled() {
                    return;
                }
                let poll = backend.poll.clone();
                let (asked, token) = (code.clone(), cancel.clone());
                let result = cx
                    .background_executor()
                    .spawn(async move { poll(&asked, &token) })
                    .await;
                let done = this.update_in(cx, |dialog, window, cx| match result {
                    Ok(DevicePoll::Pending) if waited >= limit => {
                        dialog.state =
                            DeviceState::Failed("The code expired. Try signing in again.".into());
                        cx.notify();
                        true
                    }
                    Ok(DevicePoll::Pending) => false,
                    Ok(DevicePoll::SignedIn(info)) => {
                        window.close_dialog(cx);
                        signed_in(&info, cx);
                        true
                    }
                    Err(failure) if failure.kind == FailureKind::Cancelled => true,
                    Err(failure) => {
                        dialog.state = DeviceState::Failed(describe_failure(&failure));
                        cx.notify();
                        true
                    }
                });
                if done.unwrap_or(true) {
                    return;
                }
            }
        }));
        cx.notify();
    }
}

impl Drop for DeviceDialog {
    fn drop(&mut self) {
        // Closed by Esc or a click outside: a poll in flight stops at its next look at the token.
        self.cancel.cancel();
    }
}

impl Render for DeviceDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let this = cx.weak_entity();
        let cancel = self.cancel.clone();
        let (primary, enabled) = match &self.state {
            DeviceState::Starting => ("Open browser", false),
            DeviceState::Waiting(_) => ("Open browser", true),
            DeviceState::Failed(_) => ("Try again", true),
        };
        let content = div()
            .w(dp(CONTENT_W))
            .flex()
            .flex_col()
            .items_center()
            .child(match &self.state {
                DeviceState::Waiting(code) => {
                    let text = code.user_code.clone();
                    div()
                        .pb(dp(16.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .child(
                            observed(ids::DEVICE_CODE)
                                .pr(dp(8.))
                                .text_mono14()
                                .text_size(dp(20.))
                                .semibold()
                                .child(code.user_code.clone()),
                        )
                        .child(
                            IconButton::new(ids::DEVICE_COPY, AppIcon::Copy)
                                .tooltip("Copy the code")
                                .on_click(move |_, cx| common::copy_text(cx, &text)),
                        )
                        .into_any_element()
                }
                _ => div().into_any_element(),
            })
            .child(match &self.state {
                DeviceState::Failed(message) => observed(ids::DEVICE_STATUS)
                    .w_full()
                    .child(common::error_banner(message.clone(), &tokens))
                    .into_any_element(),
                DeviceState::Starting => observed(ids::DEVICE_STATUS)
                    .pb(dp(8.))
                    .text_body2()
                    .text_color(tokens.on_background_secondary)
                    .child("Asking GitHub for a code…")
                    .into_any_element(),
                DeviceState::Waiting(_) => observed(ids::DEVICE_STATUS)
                    .pb(dp(8.))
                    .text_body2()
                    .text_color(tokens.on_background_secondary)
                    .child("Waiting for you to authorize GitCthulhu in the browser…")
                    .into_any_element(),
            })
            .when(!matches!(self.state, DeviceState::Failed(_)), |col| {
                col.child(observed(ids::DEVICE_PROGRESS).child(CircularProgress::new()))
            });
        let state = self.state.clone();
        observed(ids::DEVICE_DIALOG).w(dp(CONTENT_W)).child(
            IconBasedDialog::new(AppIcon::Github, "Sign in to GitHub", primary)
                .subtitle("Enter this code at github.com/login/device")
                .content(content)
                .primary_enabled(enabled)
                .on_primary(move |window, cx| match &state {
                    DeviceState::Waiting(code) => {
                        common::open_url(cx, &code.verification_uri);
                    }
                    DeviceState::Failed(_) => {
                        this.update(cx, |dialog, cx| dialog.start(window, cx)).ok();
                    }
                    DeviceState::Starting => {}
                })
                .on_cancel(move |_, _| cancel.cancel()),
        )
    }
}
