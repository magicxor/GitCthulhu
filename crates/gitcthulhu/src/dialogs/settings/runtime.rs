//! Rebuilds the Git runtime when a Git executable, proxy or security setting changes (spec 21 §7.2).
//!
//! The change is applied after a short quiet period (typing a host name must not start a `git version` per
//! keystroke). Repositories opened afterwards use the new runtime; a `Repo` keeps the runtime it was opened with
//! (20 §2.2), so open tabs get the Info toast "Reopen the repository to apply the new Git settings".

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{App, Global, Task};

use super::sm;
use crate::app::{self, AppState};
use crate::backend::proxy_secret::{self, Unavailable};
use crate::backend::{self, GitError, GitRuntime, GitStatus};
use crate::notifications::ToastKind;
use crate::settings::{self, ProxyType, Settings, SettingsGlobal};

/// The settings that shape the Git runtime. Two equal keys build the same runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeKey {
    git_path: Option<PathBuf>,
    use_proxy: bool,
    proxy_type: ProxyType,
    proxy_host: String,
    proxy_port: u16,
    proxy_auth: bool,
    proxy_user: String,
    verify_ssl: bool,
    ssh_ask_new_hosts: bool,
    /// Counts proxy password writes (the password is not a setting, but changes the runtime).
    password_epoch: u64,
}

impl RuntimeKey {
    /// The key of `settings` after `password_epoch` password writes.
    pub fn of(settings: &Settings, password_epoch: u64) -> Self {
        Self {
            git_path: settings.git_path.clone(),
            use_proxy: settings.use_proxy,
            proxy_type: settings.proxy_type,
            proxy_host: settings.proxy_host.clone(),
            proxy_port: settings.proxy_port,
            proxy_auth: settings.proxy_auth,
            proxy_user: settings.proxy_user.clone(),
            verify_ssl: settings.verify_ssl,
            ssh_ask_new_hosts: settings.ssh_ask_new_hosts,
            password_epoch,
        }
    }
}

/// What the runtime was last built from and what it should be built from next.
struct RuntimeWatch {
    applied: RuntimeKey,
    target: RuntimeKey,
    epoch: u64,
    task: Option<Task<()>>,
}

impl Global for RuntimeWatch {}

/// Starts watching the settings (called by `register`).
pub fn register_runtime_watch(cx: &mut App) {
    let key = RuntimeKey::of(&settings::get_or_default(cx), 0);
    cx.set_global(RuntimeWatch {
        applied: key.clone(),
        target: key,
        epoch: 0,
        task: None,
    });
    cx.observe_global::<SettingsGlobal>(check).detach();
}

/// Rebuilds the runtime after the proxy password changed.
pub fn rebuild_now(cx: &mut App) {
    if !cx.has_global::<RuntimeWatch>() {
        return;
    }
    cx.global_mut::<RuntimeWatch>().epoch += 1;
    check(cx);
}

fn check(cx: &mut App) {
    if !cx.has_global::<RuntimeWatch>() {
        return;
    }
    let settings = settings::get_or_default(cx);
    let watch = cx.global_mut::<RuntimeWatch>();
    let key = RuntimeKey::of(&settings, watch.epoch);
    if key == watch.target {
        return;
    }
    watch.target = key.clone();
    if key == watch.applied {
        // Changed back before the rebuild started: nothing to do.
        watch.task = None;
        return;
    }
    if !cx.has_global::<AppState>() {
        // No application state to put a runtime into (headless tests of other regions).
        let watch = cx.global_mut::<RuntimeWatch>();
        watch.applied = key;
        watch.task = None;
        return;
    }
    let task = cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(sm::RUNTIME_DEBOUNCE_MS))
            .await;
        let settings = cx.update(|cx| settings::get_or_default(cx));
        let result = cx
            .background_executor()
            .spawn(async move { backend::start_runtime(&settings) })
            .await;
        cx.update(|cx| finish(cx, key, result));
    });
    cx.global_mut::<RuntimeWatch>().task = Some(task);
}

fn finish(cx: &mut App, key: RuntimeKey, result: Result<Arc<GitRuntime>, GitError>) {
    let status = match result {
        Ok(runtime) => GitStatus::Ready(runtime),
        Err(err) => {
            tracing::warn!("the Git runtime cannot be rebuilt: {err}");
            GitStatus::Failed(Arc::new(err))
        }
    };
    if let Some(state) = app::state_mut(cx) {
        state.git = status;
    }
    if cx.has_global::<RuntimeWatch>() {
        let watch = cx.global_mut::<RuntimeWatch>();
        watch.applied = key;
        // The finishing task is this one: let it end instead of dropping it from inside.
        if let Some(task) = watch.task.take() {
            task.detach();
        }
    }
    cx.refresh_windows();
    if has_open_repository(cx) {
        crate::app_view::show_toast(
            cx,
            None,
            ToastKind::Positive,
            "Reopen the repository to apply the new Git settings",
            None,
        );
    }
}

fn has_open_repository(cx: &App) -> bool {
    let Some(view) = cx
        .try_global::<crate::app_view::AppViewGlobal>()
        .map(|g| g.0.clone())
    else {
        return false;
    };
    view.read_with(cx, |view, _| {
        view.tabs().iter().any(|slot| slot.repo().is_some())
    })
    .unwrap_or(false)
}

/// Where the proxy password lives: the system keychain, or a stand-in in tests.
#[derive(Clone)]
pub struct SecretStore {
    /// Whether a password is stored.
    pub exists: Arc<dyn Fn() -> Result<bool, Unavailable> + Send + Sync>,
    /// Stores a password; an empty one changes nothing.
    pub store: Arc<dyn Fn(&str) -> Result<(), Unavailable> + Send + Sync>,
    /// Deletes the stored password (the explicit Remove link).
    pub remove: Arc<dyn Fn() -> Result<(), Unavailable> + Send + Sync>,
}

impl SecretStore {
    /// The system keychain through `backend::proxy_secret`.
    pub fn keychain() -> Self {
        Self {
            exists: Arc::new(proxy_secret::exists),
            store: Arc::new(proxy_secret::store),
            remove: Arc::new(proxy_secret::remove),
        }
    }

    /// A store that keeps the password in memory; the returned cell shows what was stored.
    pub fn in_memory() -> (Self, Arc<Mutex<Option<String>>>) {
        let cell: Arc<Mutex<Option<String>>> = Arc::default();
        let (read, write, clear) = (cell.clone(), cell.clone(), cell.clone());
        let store = Self {
            exists: Arc::new(move || Ok(read.lock().map(|c| c.is_some()).unwrap_or_default())),
            store: Arc::new(move |password| {
                if let Ok(mut cell) = write.lock()
                    && !password.is_empty()
                {
                    *cell = Some(password.to_owned());
                }
                Ok(())
            }),
            remove: Arc::new(move || {
                if let Ok(mut cell) = clear.lock() {
                    *cell = None;
                }
                Ok(())
            }),
        };
        (store, cell)
    }
}

/// Replaces the password store (tests; the default is the keychain).
pub struct SecretStoreGlobal(pub SecretStore);

impl Global for SecretStoreGlobal {}

/// The password store in effect. Unit tests never touch the real keychain: without an installed store they get an
/// in-memory one.
pub(super) fn secret_store(cx: &App) -> SecretStore {
    if let Some(global) = cx.try_global::<SecretStoreGlobal>() {
        return global.0.clone();
    }
    if cfg!(test) {
        SecretStore::in_memory().0
    } else {
        SecretStore::keychain()
    }
}
