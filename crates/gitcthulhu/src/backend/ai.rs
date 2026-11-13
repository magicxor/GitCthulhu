//! AI backend calls (owned by X3 after G0b; spec 21 §2.10, §5.3, §5.4; 20 §8.4; 16 A7-A10).
//!
//! This file is the GUI's only door to the AI service and to the keychain entries of the API keys: the settings
//! file holds the provider choice (opaque JSON, read here into `AiSettings`), the keys live in the OS keychain
//! under the handles of `AiSettings::secret_handle`, and generation runs through `AiService`. Every function that
//! touches the service or the keychain blocks: call it inside `backend::bg`. Without the `services` feature only
//! the diff input exists.

use super::{CancelToken, GitError, PatchText, PatchTextOptions, Repo};

/// The staged changes as budgeted patch text, the input of commit-message generation.
pub fn staged_patch_text(repo: &Repo, cancel: &CancelToken) -> Result<PatchText, GitError> {
    gitcthulhu_git::diff::staged_patch_text(repo, &PatchTextOptions::default(), cancel)
}

/// Whether the AI service can generate text now (`AiService::status` is `Ready`).
#[cfg(feature = "services")]
pub fn is_ready(services: &super::services::ServicesHandle) -> bool {
    matches!(services.services().ai.status(), AiStatus::Ready { .. })
}

#[cfg(feature = "services")]
pub use service::*;

#[cfg(feature = "services")]
mod service {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use super::{CancelToken, GitError, Repo, staged_patch_text};
    use crate::backend::services::ServicesHandle;
    use crate::settings::AiCommitStyle;

    pub use gitcthulhu_services::ai::{
        AiError, AiMissing, AiProvider, AiSettings, AiStatus, BranchNameContext, CommitStyle,
        PrRequest, PrText,
    };
    use gitcthulhu_services::ai::{CommitMessageRequest, SecretSource};

    /// The providers of the settings page, in the order of its dropdown.
    pub const PROVIDERS: [AiProvider; 5] = [
        AiProvider::OpenAi,
        AiProvider::Anthropic,
        AiProvider::Ollama,
        AiProvider::LMStudio,
        AiProvider::OpenRouter,
    ];

    // ── settings ─────────────────────────────────────────────────────────────────────────────

    /// The AI settings of the settings file's opaque `ai` value. `null`, and anything that does not read, are the
    /// defaults (no provider).
    pub fn parse_settings(value: &serde_json::Value) -> AiSettings {
        if value.is_null() {
            return AiSettings::default();
        }
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    /// `settings` as the opaque value stored in the settings file.
    pub fn settings_value(settings: &AiSettings) -> serde_json::Value {
        serde_json::to_value(settings).unwrap_or(serde_json::Value::Null)
    }

    /// The service's commit style for the setting.
    pub fn commit_style(style: AiCommitStyle) -> CommitStyle {
        match style {
            AiCommitStyle::Conventional => CommitStyle::Conventional,
            AiCommitStyle::PlainImperative => CommitStyle::PlainImperative,
        }
    }

    /// The model of `provider` as configured.
    pub fn model_of(settings: &AiSettings, provider: AiProvider) -> &str {
        match provider {
            AiProvider::OpenAi => &settings.openai.model,
            AiProvider::Anthropic => &settings.anthropic.model,
            AiProvider::Ollama => &settings.ollama.model,
            AiProvider::LMStudio => &settings.lmstudio.model,
            AiProvider::OpenRouter => &settings.openrouter.model,
        }
    }

    /// Sets the model of `provider`.
    pub fn set_model(settings: &mut AiSettings, provider: AiProvider, model: &str) {
        let model = model.to_owned();
        match provider {
            AiProvider::OpenAi => settings.openai.model = model,
            AiProvider::Anthropic => settings.anthropic.model = model,
            AiProvider::Ollama => settings.ollama.model = model,
            AiProvider::LMStudio => settings.lmstudio.model = model,
            AiProvider::OpenRouter => settings.openrouter.model = model,
        }
    }

    /// The default model of `provider` (the hint of the model field).
    pub fn default_model(provider: AiProvider) -> String {
        let defaults = AiSettings::default();
        model_of(&defaults, provider).to_owned()
    }

    /// Whether `provider` has an endpoint the user can change (Anthropic's is fixed).
    pub fn has_endpoint(provider: AiProvider) -> bool {
        provider != AiProvider::Anthropic
    }

    /// The endpoint of `provider` as configured (blank = the provider's own, OpenAI only).
    pub fn endpoint_of(settings: &AiSettings, provider: AiProvider) -> String {
        match provider {
            AiProvider::OpenAi => settings.openai.custom_endpoint.clone().unwrap_or_default(),
            AiProvider::Anthropic => String::new(),
            AiProvider::Ollama => settings.ollama.endpoint.clone(),
            AiProvider::LMStudio => settings.lmstudio.endpoint.clone(),
            AiProvider::OpenRouter => settings.openrouter.endpoint.clone(),
        }
    }

    /// Sets the endpoint of `provider` (a blank OpenAI endpoint means OpenAI's own).
    pub fn set_endpoint(settings: &mut AiSettings, provider: AiProvider, endpoint: &str) {
        let endpoint = endpoint.trim().to_owned();
        match provider {
            AiProvider::OpenAi => {
                settings.openai.custom_endpoint = (!endpoint.is_empty()).then_some(endpoint);
            }
            AiProvider::Anthropic => {}
            AiProvider::Ollama => settings.ollama.endpoint = endpoint,
            AiProvider::LMStudio => settings.lmstudio.endpoint = endpoint,
            AiProvider::OpenRouter => settings.openrouter.endpoint = endpoint,
        }
    }

    /// The hint of the endpoint field of `provider`: what a blank field means or a typical value.
    pub fn endpoint_hint(provider: AiProvider) -> String {
        let defaults = AiSettings::default();
        match provider {
            AiProvider::OpenAi => "https://api.openai.com/v1 (default)".to_owned(),
            AiProvider::Anthropic => String::new(),
            other => endpoint_of(&defaults, other),
        }
    }

    /// Why `model` cannot be the model of `provider`, or `Ok`.
    pub fn validate_model(provider: AiProvider, model: &str) -> Result<(), String> {
        if model.trim().is_empty() {
            Err(format!("The {} model is required", provider.display_name()))
        } else {
            Ok(())
        }
    }

    /// Why `endpoint` cannot be the endpoint of `provider`, or `Ok` (a blank OpenAI endpoint means OpenAI's own).
    pub fn validate_endpoint(provider: AiProvider, endpoint: &str) -> Result<(), String> {
        let mut probe = AiSettings {
            provider: Some(provider),
            ..AiSettings::default()
        };
        set_endpoint(&mut probe, provider, endpoint);
        probe.validate_active().map_err(|err| err.to_string())
    }

    /// What the active provider lacks, in words for the status row.
    pub fn missing_text(missing: AiMissing) -> &'static str {
        match missing {
            AiMissing::Provider => "no provider selected",
            AiMissing::Key => "no API key",
            AiMissing::Model => "no model",
            AiMissing::Endpoint => "no valid endpoint",
        }
    }

    /// The `{provider}@{host}` key of [`hosted_target`], what the privacy confirmation is remembered for.
    pub fn consent_key(settings: &AiSettings) -> Option<String> {
        hosted_target(settings).map(|(provider, host)| format!("{provider}@{host}"))
    }

    /// Where the active provider sends the text, for the privacy confirmation: `Some((provider name, host))` when
    /// the provider is not on this machine (the hosted APIs, and local-server providers pointed at another host).
    pub fn hosted_target(settings: &AiSettings) -> Option<(String, String)> {
        let provider = settings.provider?;
        let host = match provider {
            AiProvider::OpenAi => host_of(settings.openai.custom_endpoint.as_deref())
                .unwrap_or_else(|| "api.openai.com".to_owned()),
            AiProvider::Anthropic => "api.anthropic.com".to_owned(),
            AiProvider::OpenRouter => host_of(Some(&settings.openrouter.endpoint))
                .unwrap_or_else(|| "openrouter.ai".to_owned()),
            AiProvider::Ollama => {
                let endpoint = &settings.ollama.endpoint;
                let host = endpoint
                    .rsplit_once(':')
                    .map_or(endpoint.as_str(), |(h, _)| h);
                host.trim_matches(['[', ']']).to_owned()
            }
            AiProvider::LMStudio => host_of(Some(&settings.lmstudio.endpoint)).unwrap_or_default(),
        };
        (!is_local_host(&host)).then(|| (provider.display_name().to_owned(), host))
    }

    /// The host of an `http(s)://` endpoint (without user info, port and IPv6 brackets).
    fn host_of(endpoint: Option<&str>) -> Option<String> {
        let (_, rest) = endpoint?.trim().split_once("://")?;
        let authority = rest.split(['/', '?', '#']).next()?;
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = if let Some(inner) = authority.strip_prefix('[') {
            inner.split(']').next()?
        } else {
            authority.split(':').next()?
        };
        (!host.is_empty()).then(|| host.to_owned())
    }

    fn is_local_host(host: &str) -> bool {
        let host = host.trim().to_ascii_lowercase();
        host == "localhost"
            || host.ends_with(".localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }

    // ── API keys ─────────────────────────────────────────────────────────────────────────────

    /// What the key store knows about the key of one provider.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum KeyState {
        /// No key anywhere.
        Missing,
        /// A key in the keychain.
        Stored,
        /// No keychain entry, but the environment variable of that name holds a key.
        Environment(&'static str),
        /// The keychain cannot be used (reason) and no environment variable helps.
        Unavailable(String),
    }

    impl KeyState {
        /// Whether a key is available for requests.
        pub fn usable(&self) -> bool {
            matches!(self, KeyState::Stored | KeyState::Environment(_))
        }
    }

    /// The environment variable that stands in for the key under `handle`.
    pub fn environment_variable(handle: &str) -> Option<&'static str> {
        [
            (AiProvider::OpenAi, "OPENAI_API_KEY"),
            (AiProvider::Anthropic, "ANTHROPIC_API_KEY"),
            (AiProvider::OpenRouter, "OPENROUTER_API_KEY"),
        ]
        .into_iter()
        .find(|(provider, _)| AiSettings::secret_handle(*provider) == Some(handle))
        .map(|(_, name)| name)
    }

    type StateFn = dyn Fn(&str) -> KeyState + Send + Sync;
    type ReadFn = dyn Fn(&str) -> Option<String> + Send + Sync;
    type StoreFn = dyn Fn(&str, &str) -> Result<(), String> + Send + Sync;
    type DeleteFn = dyn Fn(&str) -> Result<(), String> + Send + Sync;

    /// Where the API keys live: the system keychain, or a stand-in in tests and QA runs. Keys are write-only for
    /// the UI: it can store, delete and ask whether one exists, but only the service reads the value.
    #[derive(Clone)]
    pub struct KeyStore {
        state: Arc<StateFn>,
        read: Arc<ReadFn>,
        store: Arc<StoreFn>,
        delete: Arc<DeleteFn>,
    }

    /// What an in-memory [`KeyStore`] holds, by handle (tests look at it).
    pub type MemoryKeys = Arc<Mutex<HashMap<String, String>>>;

    impl KeyStore {
        /// The system keychain (`but_secret`, entries of the app's namespace), with the environment variables
        /// `OPENAI_API_KEY`, `ANTHROPIC_API_KEY` and `OPENROUTER_API_KEY` as a fallback for requests.
        pub fn keychain() -> Self {
            use but_secret::secret::{self, Namespace};
            Self {
                state: Arc::new(|handle| match secret::retrieve(handle, Namespace::Global) {
                    Ok(Some(key)) if !key.0.trim().is_empty() => KeyState::Stored,
                    Ok(_) => environment_state(handle).unwrap_or(KeyState::Missing),
                    Err(err) => environment_state(handle)
                        .unwrap_or_else(|| KeyState::Unavailable(err.to_string())),
                }),
                read: Arc::new(|handle| {
                    match secret::retrieve(handle, Namespace::Global) {
                        Ok(Some(key)) if !key.0.trim().is_empty() => return Some(key.0),
                        Ok(_) => {}
                        Err(err) => tracing::warn!("cannot read the secret {handle}: {err}"),
                    }
                    environment_key(handle)
                }),
                store: Arc::new(|handle, key| {
                    secret::persist(
                        handle,
                        &but_secret::Sensitive(key.to_owned()),
                        Namespace::Global,
                    )
                    .map_err(|err| err.to_string())
                }),
                delete: Arc::new(|handle| {
                    secret::delete(handle, Namespace::Global).map_err(|err| err.to_string())
                }),
            }
        }

        /// A store that keeps the keys in memory (the returned map shows what was stored).
        pub fn in_memory() -> (Self, MemoryKeys) {
            let keys: MemoryKeys = Arc::default();
            fn lock(keys: &MemoryKeys) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
                keys.lock().unwrap_or_else(PoisonError::into_inner)
            }
            let (a, b, c, d) = (keys.clone(), keys.clone(), keys.clone(), keys.clone());
            let store = Self {
                state: Arc::new(move |handle| {
                    if lock(&a).contains_key(handle) {
                        KeyState::Stored
                    } else {
                        KeyState::Missing
                    }
                }),
                read: Arc::new(move |handle| lock(&b).get(handle).cloned()),
                store: Arc::new(move |handle, key| {
                    lock(&c).insert(handle.to_owned(), key.to_owned());
                    Ok(())
                }),
                delete: Arc::new(move |handle| {
                    lock(&d).remove(handle);
                    Ok(())
                }),
            };
            (store, keys)
        }

        /// Whether a key is available under `handle`, and where from. Blocks.
        pub fn state(&self, handle: &str) -> KeyState {
            (self.state)(handle)
        }

        /// Stores `key` under `handle`. Blocks. The key is never logged.
        pub fn store(&self, handle: &str, key: &str) -> Result<(), String> {
            (self.store)(handle, key)
        }

        /// Removes the key under `handle` (no error when there is none). Blocks.
        pub fn delete(&self, handle: &str) -> Result<(), String> {
            (self.delete)(handle)
        }
    }

    fn environment_key(handle: &str) -> Option<String> {
        let name = environment_variable(handle)?;
        std::env::var(name)
            .ok()
            .filter(|key| !key.trim().is_empty())
    }

    fn environment_state(handle: &str) -> Option<KeyState> {
        let name = environment_variable(handle)?;
        environment_key(handle).map(|_| KeyState::Environment(name))
    }

    /// `SecretSource` of the service over a [`KeyStore`].
    struct Secrets<'a>(&'a KeyStore);

    impl SecretSource for Secrets<'_> {
        fn secret(&self, handle: &str) -> Option<String> {
            (self.0.read)(handle)
        }
    }

    // ── the service ──────────────────────────────────────────────────────────────────────────

    /// Why a generation or a connection test failed.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum AiFailure {
        /// Cancelled by the user (Stop, Escape, tab switch, closing).
        Cancelled,
        /// The settings are incomplete (what is missing).
        NotConfigured(String),
        /// Nothing is staged.
        NothingStaged,
        /// Anything else, in words for a toast.
        Failed(String),
    }

    impl AiFailure {
        /// The failure as one sentence.
        pub fn message(&self) -> String {
            match self {
                AiFailure::Cancelled => "Cancelled".to_owned(),
                AiFailure::NotConfigured(_) => {
                    "AI isn't set up. Choose a provider in Settings > AI.".to_owned()
                }
                AiFailure::NothingStaged => "Stage changes to generate a commit message".to_owned(),
                AiFailure::Failed(message) => message.clone(),
            }
        }
    }

    impl From<AiError> for AiFailure {
        fn from(err: AiError) -> Self {
            match err {
                AiError::Cancelled => AiFailure::Cancelled,
                AiError::NotConfigured(missing) => {
                    AiFailure::NotConfigured(missing_text(missing).to_owned())
                }
                AiError::Auth(message) => AiFailure::Failed(format!(
                    "The AI provider rejected the credentials: {message}"
                )),
                AiError::RateLimited => AiFailure::Failed(
                    "The AI provider is limiting requests. Try again in a moment.".to_owned(),
                ),
                AiError::Network(message) => {
                    AiFailure::Failed(format!("Could not reach the AI provider: {message}"))
                }
                AiError::Provider(message) => {
                    AiFailure::Failed(format!("The AI provider reported an error: {message}"))
                }
                AiError::EmptyAnswer => {
                    AiFailure::Failed("The AI provider answered with nothing".to_owned())
                }
                other => AiFailure::Failed(other.to_string()),
            }
        }
    }

    fn git_failure(err: GitError) -> AiFailure {
        if matches!(err, GitError::Cancelled) {
            AiFailure::Cancelled
        } else {
            AiFailure::Failed(format!("Could not read the staged changes: {err}"))
        }
    }

    /// Applies `settings` to the service, resolving the key of the selected provider through `keys`. Blocks (the
    /// keychain). Returns the resulting status.
    pub fn configure(
        services: &ServicesHandle,
        settings: &AiSettings,
        keys: &KeyStore,
    ) -> AiStatus {
        let ai = &services.services().ai;
        if let Err(err) = ai.configure(settings, &Secrets(keys)) {
            tracing::debug!("the AI settings are not usable yet: {err}");
        }
        ai.status()
    }

    /// The service's current status.
    pub fn status(services: &ServicesHandle) -> AiStatus {
        services.services().ai.status()
    }

    /// Generates a commit message for the staged changes of `repo`, streaming the cleaned answer so far into
    /// `on_text`. `notes` is what the message box holds. The cancel token also aborts building the patch. Blocks.
    pub fn generate_commit_message(
        services: &ServicesHandle,
        repo: &Repo,
        notes: String,
        style: CommitStyle,
        cancel: &CancelToken,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<String, AiFailure> {
        let patch = staged_patch_text(repo, cancel).map_err(git_failure)?;
        if patch.files.is_empty() && patch.text.trim().is_empty() {
            return Err(AiFailure::NothingStaged);
        }
        let request = CommitMessageRequest {
            patch,
            notes,
            style,
        };
        services
            .services()
            .ai
            .generate_commit_message(&request, cancel, on_text)
            .map_err(AiFailure::from)
    }

    /// Generates a pull-request title and description (hook for the pull-request dialog). Blocks.
    pub fn generate_pr_text(
        services: &ServicesHandle,
        request: &PrRequest,
        cancel: &CancelToken,
        on_text: &mut (dyn FnMut(&PrText) + Send),
    ) -> Result<PrText, AiFailure> {
        services
            .services()
            .ai
            .generate_pr_text(request, cancel, on_text)
            .map_err(AiFailure::from)
    }

    /// Suggests a branch name for the staged changes of `repo` (hook for the new-branch dialog). Blocks.
    pub fn suggest_branch_name_for_staged(
        services: &ServicesHandle,
        repo: &Repo,
        cancel: &CancelToken,
    ) -> Result<String, AiFailure> {
        let patch = staged_patch_text(repo, cancel).map_err(git_failure)?;
        if patch.files.is_empty() && patch.text.trim().is_empty() {
            return Err(AiFailure::NothingStaged);
        }
        suggest_branch_name(services, &BranchNameContext::Staged(patch), cancel)
    }

    /// Suggests a branch name from a commit message or a staged patch. Blocks.
    pub fn suggest_branch_name(
        services: &ServicesHandle,
        context: &BranchNameContext,
        cancel: &CancelToken,
    ) -> Result<String, AiFailure> {
        services
            .services()
            .ai
            .suggest_branch_name(context, cancel)
            .map_err(AiFailure::from)
    }

    /// Applies `settings` and sends one tiny request to the configured provider. Blocks.
    pub fn test_connection(
        services: &ServicesHandle,
        settings: &AiSettings,
        keys: &KeyStore,
        cancel: &CancelToken,
    ) -> Result<(), AiFailure> {
        let ai = &services.services().ai;
        ai.configure(settings, &Secrets(keys))
            .map_err(AiFailure::from)?;
        ai.test_connection(cancel).map_err(AiFailure::from)
    }
}
