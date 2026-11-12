//! The app-wide AI state: whether the service is configured and ready, and the key store in effect.
//!
//! The hub keeps the service in line with the settings: it re-applies them (on a background thread, because the
//! keychain is read) whenever the `ai` setting or a stored key changes and when the services have just started.
//! Views read [`Readiness`] and never block.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Context, Entity, Global, Subscription, Task};

use crate::app;
use crate::backend::ai::{self as backend_ai, AiMissing, AiSettings, AiStatus, KeyStore};
use crate::settings::{self, SettingsGlobal};

/// A change of the AI settings reaches the service after this quiet period.
const SETTINGS_DEBOUNCE_MS: u64 = 250;

/// Whether AI generation can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// The settings have not been applied to the service yet.
    Pending,
    /// No provider is selected: AI is off.
    Off,
    /// A provider is selected but unusable.
    NotConfigured(AiMissing),
    /// Usable.
    Ready {
        /// The provider's display name ("OpenAI", "LM Studio", ...).
        provider: String,
        /// The model.
        model: String,
    },
}

impl Readiness {
    /// Whether generation can start.
    pub fn is_ready(&self) -> bool {
        matches!(self, Readiness::Ready { .. })
    }

    fn of(status: AiStatus) -> Self {
        match status {
            AiStatus::Ready { provider, model } => Readiness::Ready { provider, model },
            AiStatus::NotConfigured {
                missing: AiMissing::Provider,
            } => Readiness::Off,
            AiStatus::NotConfigured { missing } => Readiness::NotConfigured(missing),
            // `AiStatus` is not `#[non_exhaustive]`, but a future state is "not usable" for the GUI.
            #[allow(unreachable_patterns)]
            _ => Readiness::Pending,
        }
    }
}

/// The AI hub entity.
pub struct AiHub {
    readiness: Readiness,
    /// The settings the service was last given.
    applied: AiSettings,
    /// Identity of the services instance the settings were given to.
    services_id: Option<usize>,
    generation: u64,
    /// The newest generation, readable from the background threads: an older configuration that has not started
    /// yet is skipped, so the service always ends with the newest settings.
    latest: Arc<AtomicU64>,
    /// Serialises the background configure calls (two of them must not interleave or finish out of order).
    gate: Arc<Mutex<()>>,
    /// The pending re-apply after a settings change (typing in a field must not read the keychain per keystroke).
    debounce: Option<Task<()>>,
    _settings: Subscription,
}

impl AiHub {
    fn new(cx: &mut Context<Self>) -> Self {
        let settings = cx.observe_global::<SettingsGlobal>(|this, cx| {
            let wanted = backend_ai::parse_settings(&settings::get_or_default(cx).ai);
            if wanted != this.applied {
                this.debounce = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(SETTINGS_DEBOUNCE_MS))
                        .await;
                    this.update(cx, |hub, cx| {
                        hub.debounce = None;
                        hub.reconfigure(cx);
                    })
                    .ok();
                }));
            }
        });
        Self {
            readiness: Readiness::Pending,
            applied: AiSettings::default(),
            services_id: None,
            generation: 0,
            latest: Arc::new(AtomicU64::new(0)),
            gate: Arc::new(Mutex::new(())),
            debounce: None,
            _settings: settings,
        }
    }

    /// The current readiness.
    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// Applies the current settings and keys to the service in the background. Without running services the
    /// hub goes back to [`Readiness::Pending`].
    pub fn reconfigure(&mut self, cx: &mut Context<Self>) {
        let Some(services) = app::services(cx) else {
            self.services_id = None;
            self.set_readiness(Readiness::Pending, cx);
            return;
        };
        self.services_id = Some(services_id(&services));
        let settings = backend_ai::parse_settings(&settings::get_or_default(cx).ai);
        self.applied = settings.clone();
        self.generation += 1;
        let generation = self.generation;
        self.latest.store(generation, Ordering::SeqCst);
        let latest = self.latest.clone();
        let gate = self.gate.clone();
        let keys = key_store(cx);
        let task = crate::backend::bg(cx, move || {
            let _turn = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            // A newer configuration is queued behind this one: it carries the settings that count.
            (latest.load(Ordering::SeqCst) == generation)
                .then(|| backend_ai::configure(&services, &settings, &keys))
        });
        cx.spawn(async move |this, cx| {
            let status = task.await;
            this.update(cx, |hub, cx| {
                // A newer configuration is on its way: this result is stale.
                if hub.generation == generation
                    && let Some(status) = status
                {
                    hub.set_readiness(Readiness::of(status), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_readiness(&mut self, readiness: Readiness, cx: &mut Context<Self>) {
        if self.readiness != readiness {
            self.readiness = readiness;
            cx.notify();
        }
    }
}

fn services_id(services: &crate::backend::services::ServicesHandle) -> usize {
    Arc::as_ptr(services.services()) as usize
}

/// The hub's global handle.
pub struct HubGlobal(pub Entity<AiHub>);

impl Global for HubGlobal {}

/// The hub, once [`install`] ran.
pub fn hub(cx: &App) -> Option<Entity<AiHub>> {
    cx.try_global::<HubGlobal>().map(|g| g.0.clone())
}

/// Creates the hub (called by `ai::register`).
pub fn install(cx: &mut App) {
    let hub = cx.new(AiHub::new);
    cx.set_global(HubGlobal(hub));
    // Unit tests never touch the real keychain.
    if cfg!(test) && !cx.has_global::<KeyStoreGlobal>() {
        cx.set_global(KeyStoreGlobal(KeyStore::in_memory().0));
    }
}

/// The current readiness; [`Readiness::Pending`] without a hub.
pub fn readiness(cx: &App) -> Readiness {
    hub(cx).map_or(Readiness::Pending, |h| h.read(cx).readiness.clone())
}

/// Makes sure the settings were given to the running services (they start in the background some time after the
/// window opens). Cheap; call it from anything that is about to show AI state.
pub fn ensure(cx: &mut App) {
    let (Some(hub), Some(services)) = (hub(cx), app::services(cx)) else {
        return;
    };
    if hub.read(cx).services_id == Some(services_id(&services)) {
        return;
    }
    cx.defer(move |cx| hub.update(cx, |hub, cx| hub.reconfigure(cx)));
}

/// Re-applies the settings after a key was stored or removed.
pub fn keys_changed(cx: &mut App) {
    if let Some(hub) = hub(cx) {
        hub.update(cx, |hub, cx| hub.reconfigure(cx));
    }
}

/// Replaces the key store (tests and QA runs; the default is the keychain).
pub struct KeyStoreGlobal(pub KeyStore);

impl Global for KeyStoreGlobal {}

/// The key store in effect: the installed one (tests and QA runs install an in-memory store), else the keychain.
pub fn key_store(cx: &App) -> KeyStore {
    // Unit tests never touch the real keychain, even one that did not run `install`.
    let fallback = if cfg!(test) {
        || KeyStore::in_memory().0
    } else {
        KeyStore::keychain
    };
    cx.try_global::<KeyStoreGlobal>()
        .map_or_else(fallback, |global| global.0.clone())
}
