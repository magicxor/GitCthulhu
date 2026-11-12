//! Forge accounts (D0.1; owned by X4; spec 21 §5.4, §2.8): the app-wide [`AccountsModel`] the Accounts settings page
//! and the per-tab controllers share, the HTTPS offer of the credential dialog and the "no account" explanation.
//!
//! The model never touches the network or the keychain on the UI thread: loading the list, checking a token,
//! signing in and forgetting an account run in `backend::bg`.

use std::time::{Duration, Instant};

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Task, Window,
};

use crate::backend::forge::{self as backend_forge, AccountId, AccountInfo, Failure, Validity};
use crate::backend::{CredentialRequest, PromptKind, Sensitive};
use crate::notifications::{ToastAction, ToastKind};
use crate::tab::TabCtx;

/// How long a loaded list stays fresh before the Accounts page loads it (and checks the tokens) again.
const FRESH_FOR: Duration = Duration::from_secs(60);

/// What a row of the Accounts page says about the token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowValidity {
    /// Not asked yet.
    Unchecked,
    /// The forge is being asked.
    Checking,
    /// The token works.
    Valid,
    /// The forge rejected the token (or could not be reached).
    Invalid,
    /// The token is gone from the keychain.
    NoCredentials,
    /// The check itself failed (message).
    Failed(String),
}

impl RowValidity {
    /// The text of the row.
    pub fn text(&self) -> &str {
        match self {
            RowValidity::Unchecked => "Not checked",
            RowValidity::Checking => "Checking…",
            RowValidity::Valid => "Valid",
            RowValidity::Invalid => "Invalid",
            RowValidity::NoCredentials => "No token stored",
            RowValidity::Failed(message) => message,
        }
    }
}

/// An account of the list and how its token stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRow {
    /// The account.
    pub info: AccountInfo,
    /// The last check.
    pub validity: RowValidity,
}

/// Where the list stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// Never loaded.
    NotLoaded,
    /// Loading.
    Loading,
    /// Loaded.
    Loaded,
    /// Loading failed (message).
    Failed(String),
}

/// Emitted when the set of accounts changed (a sign-in or a sign-out): controllers detect their forge again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountsEvent {
    /// An account was added or removed.
    Changed,
}

/// The app-wide accounts list.
pub struct AccountsModel {
    rows: Vec<AccountRow>,
    state: LoadState,
    loaded_at: Option<Instant>,
    /// The list was injected (QA scenarios, tests) and never comes from the services.
    seeded: bool,
    tasks: Vec<Task<()>>,
}

impl EventEmitter<AccountsEvent> for AccountsModel {}

struct AccountsGlobal(Entity<AccountsModel>);

impl Global for AccountsGlobal {}

/// Installs the model (once, at startup).
pub fn install(cx: &mut App) {
    if cx.has_global::<AccountsGlobal>() {
        return;
    }
    let model = cx.new(|_| AccountsModel::new());
    cx.set_global(AccountsGlobal(model));
}

/// The app-wide model, once installed.
pub fn installed(cx: &App) -> Option<Entity<AccountsModel>> {
    cx.try_global::<AccountsGlobal>().map(|g| g.0.clone())
}

impl AccountsModel {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            state: LoadState::NotLoaded,
            loaded_at: None,
            seeded: false,
            tasks: Vec::new(),
        }
    }

    /// The accounts.
    pub fn rows(&self) -> &[AccountRow] {
        &self.rows
    }

    /// Where the list stands.
    pub fn state(&self) -> &LoadState {
        &self.state
    }

    /// Whether the list was injected instead of read from the services.
    pub fn is_seeded(&self) -> bool {
        self.seeded
    }

    /// Replaces the list with `rows` and never reads the services (QA scenarios and tests).
    pub fn seed(&mut self, rows: Vec<AccountRow>, cx: &mut Context<Self>) {
        self.rows = rows;
        self.state = LoadState::Loaded;
        self.loaded_at = Some(Instant::now());
        self.seeded = true;
        cx.notify();
    }

    /// Loads the list when it was never loaded or is older than a minute, then checks every token. Called by the
    /// Accounts page on every render: cheap when nothing is due.
    pub fn ensure_fresh(&mut self, cx: &mut Context<Self>) {
        if self.seeded || self.state == LoadState::Loading {
            return;
        }
        // A failed load is asked again once the freshness window has passed, like a loaded list.
        let due = self.state == LoadState::NotLoaded
            || self.loaded_at.is_none_or(|at| at.elapsed() > FRESH_FOR);
        if due {
            self.reload(true, cx);
        } else if self.state == LoadState::Loaded
            && self
                .rows
                .iter()
                .any(|row| row.validity == RowValidity::Unchecked)
        {
            // A sign-in just added an account: its token has not been asked about yet.
            self.check_all(cx);
        }
    }

    /// Reads the account list (no network, no keychain); `check` then asks the forge about every token.
    pub fn reload(&mut self, check: bool, cx: &mut Context<Self>) {
        if self.seeded {
            return;
        }
        let Some(services) = crate::app::services(cx) else {
            self.state = LoadState::Failed("The integrations are not started".into());
            self.loaded_at = Some(Instant::now());
            cx.notify();
            return;
        };
        self.state = LoadState::Loading;
        let task = crate::backend::bg(cx, move || backend_forge::accounts(&services));
        let load = cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |model, cx| {
                model.loaded_at = Some(Instant::now());
                match result {
                    Ok(accounts) => {
                        let previous = std::mem::take(&mut model.rows);
                        model.rows = accounts
                            .into_iter()
                            .map(|info| {
                                let validity = previous
                                    .iter()
                                    .find(|row| row.info.id == info.id)
                                    .map_or(RowValidity::Unchecked, |row| row.validity.clone());
                                AccountRow { info, validity }
                            })
                            .collect();
                        model.state = LoadState::Loaded;
                        if check {
                            model.check_all(cx);
                        }
                    }
                    Err(failure) => model.state = LoadState::Failed(failure.message),
                }
                cx.notify();
            })
            .ok();
        });
        self.tasks.push(load);
    }

    /// Asks the forge about every token that is not being checked, one after the other.
    pub fn check_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<AccountId> = self
            .rows
            .iter()
            .filter(|row| row.validity != RowValidity::Checking)
            .map(|row| row.info.id.clone())
            .collect();
        // Without the services nothing can be asked: the rows keep what they say instead of "Checking…" forever.
        let Some(services) = crate::app::services(cx) else {
            return;
        };
        for id in &ids {
            self.set_validity(id, RowValidity::Checking);
        }
        let task = cx.spawn(async move |this, cx| {
            for id in ids {
                let services = services.clone();
                let asked = id.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { backend_forge::check_account(&services, &asked) })
                    .await;
                let alive = this
                    .update(cx, |model, cx| {
                        model.set_validity(&id, validity_of(result));
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Asks the forge about one account's token.
    pub fn check(&mut self, id: &AccountId, cx: &mut Context<Self>) {
        let Some(services) = crate::app::services(cx) else {
            return;
        };
        self.set_validity(id, RowValidity::Checking);
        let asked = id.clone();
        let task = crate::backend::bg(cx, move || backend_forge::check_account(&services, &asked));
        let id = id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |model, cx| {
                model.set_validity(&id, validity_of(result));
                cx.notify();
            })
            .ok();
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Removes an account with its token; the list is read again afterwards.
    pub fn forget(&mut self, id: &AccountId, cx: &mut Context<Self>) {
        if self.seeded {
            self.rows.retain(|row| &row.info.id != id);
            cx.emit(AccountsEvent::Changed);
            cx.notify();
            return;
        }
        let Some(services) = crate::app::services(cx) else {
            return;
        };
        let asked = id.clone();
        let task = crate::backend::bg(cx, move || backend_forge::forget_account(&services, &asked));
        let task = cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |model, cx| {
                match result {
                    Ok(()) => {
                        model.reload(false, cx);
                        cx.emit(AccountsEvent::Changed);
                    }
                    // The list itself is fine: say what went wrong without replacing it.
                    Err(failure) => crate::app_view::show_toast(
                        cx,
                        None,
                        ToastKind::Error,
                        format!("The account could not be removed: {}", failure.message),
                        None,
                    ),
                }
                cx.notify();
            })
            .ok();
        });
        self.tasks.push(task);
    }

    /// An account was added by a sign-in flow: reload the list and tell the controllers.
    pub fn added(&mut self, cx: &mut Context<Self>) {
        self.state = LoadState::NotLoaded;
        self.reload(false, cx);
        cx.emit(AccountsEvent::Changed);
    }

    fn set_validity(&mut self, id: &AccountId, validity: RowValidity) {
        if let Some(row) = self.rows.iter_mut().find(|row| &row.info.id == id) {
            row.validity = validity;
        }
    }
}

fn validity_of(result: Result<Validity, Failure>) -> RowValidity {
    match result {
        Ok(Validity::Valid) => RowValidity::Valid,
        Ok(Validity::Invalid) => RowValidity::Invalid,
        Ok(Validity::NoCredentials) => RowValidity::NoCredentials,
        Err(failure) => RowValidity::Failed(failure.message),
    }
}

/// Whether the settings dialog shows the Accounts page: when the services run (or accounts were injected).
pub fn available(cx: &App) -> bool {
    crate::app::services(cx).is_some()
        || installed(cx).is_some_and(|model| model.read(cx).is_seeded())
}

/// A connected account the HTTPS credential dialog can offer ("Use GitHub account alice").
#[derive(Clone)]
pub struct AccountOffer {
    /// "GitHub", "GitLab", "Bitbucket".
    pub forge: SharedString,
    /// The account's name.
    pub user: SharedString,
    /// The user name Git is answered with.
    username: String,
    id: AccountId,
    services: crate::backend::services::ServicesHandle,
}

impl std::fmt::Debug for AccountOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountOffer")
            .field("forge", &self.forge)
            .field("user", &self.user)
            .finish()
    }
}

impl PartialEq for AccountOffer {
    fn eq(&self, other: &Self) -> bool {
        self.forge == other.forge && self.user == other.user && self.id == other.id
    }
}

impl AccountOffer {
    /// The text of the checkbox.
    pub fn label(&self) -> String {
        format!("Use {} account {}", self.forge, self.user)
    }

    /// The answer to a user name prompt (no keychain access).
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Reads the token for a password prompt in the background (`None`: no token is stored).
    pub fn password(&self, cx: &App) -> Task<Result<Option<Sensitive>, Failure>> {
        let (services, id) = (self.services.clone(), self.id.clone());
        crate::backend::bg(cx, move || {
            backend_forge::https_credential(&services, &id)
                .map(|credential| credential.map(|(_, token)| token))
        })
    }
}

/// The host (with an explicit port) of an HTTPS URL, without credentials.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    (!host.is_empty()).then(|| host.to_owned())
}

/// The account to offer for an HTTPS username/password prompt: the connected account of the prompt's host, unless
/// the user turned the offer off or the URL names another user.
pub fn https_offer(req: &CredentialRequest, cx: &App) -> Option<AccountOffer> {
    if !crate::settings::get_or_default(cx).forge_https_answers {
        return None;
    }
    let (url, named) = match &req.kind {
        PromptKind::HttpsUsername { url } => (url, None),
        PromptKind::HttpsPassword { url, username } => (url, username.as_deref()),
        _ => return None,
    };
    let services = crate::app::services(cx)?;
    let account = backend_forge::account_for_host(&services, &host_of(url)?)?;
    let username = match account.forge {
        backend_forge::Forge::GitHub => account.name.clone(),
        backend_forge::Forge::GitLab => "oauth2".to_owned(),
        backend_forge::Forge::Bitbucket => "x-bitbucket-api-token-auth".to_owned(),
        backend_forge::Forge::Azure => return None,
    };
    // A user name in the URL that is not this account's belongs to another account of the same host.
    if let Some(named) = named
        && !named.eq_ignore_ascii_case(&username)
        && !named.eq_ignore_ascii_case(&account.name)
    {
        return None;
    }
    Some(AccountOffer {
        forge: account.forge.label().into(),
        user: account.name.into(),
        username,
        id: account.id,
        services,
    })
}

/// Tells the user that the repository's forge has no connected account, with an action that opens the Accounts
/// page.
pub fn explain_missing_account(window: &mut Window, cx: &mut App, tab: &TabCtx) {
    let _ = window;
    let what = super::controller_of(tab.id, cx)
        .and_then(|controller| {
            controller.read(cx).facts().and_then(|facts| {
                facts.forge.map(|forge| {
                    format!(
                        "Connect a {} account to use {}s",
                        forge.label(),
                        facts.unit_name
                    )
                })
            })
        })
        .unwrap_or_else(|| "Connect a forge account first".to_owned());
    crate::app_view::show_toast(
        cx,
        Some(tab.id),
        ToastKind::Warning,
        what,
        Some(ToastAction {
            label: "Open Settings".into(),
            run: std::rc::Rc::new(|window, cx| {
                crate::dialogs::settings::open(
                    window,
                    cx,
                    Some(crate::dialogs::settings::SettingsPage::Accounts),
                );
            }),
        }),
    );
}
