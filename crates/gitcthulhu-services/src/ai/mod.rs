//! AI text generation (commit messages, pull-request text, branch names). 20 section 8.4.
//!
//! [`AiService`] drives `but-llm` on the shared runtime. Every call blocks the calling (background) thread,
//! streams partial text through a callback and can be cancelled through a [`CancelToken`]: cancelling drops the
//! request future, which closes the HTTP connection at once.

use std::sync::{Arc, PoisonError, RwLock};

use but_llm::{AiConfiguration, ChatMessage, GenerateOptions, LLMProvider, LlmError, ProviderKeys};
use but_secret::Sensitive;
use gitcthulhu_git::{CancelToken, PatchText};

use crate::Runtime;

pub mod format;
mod prompt;

pub use but_llm::{AiMissing, LLMProviderKind as AiProvider};

/// The AI configuration, stored in the application's settings file (serde).
///
/// Holds the selected provider (`None` = AI is off) and, per provider, its model and endpoint. API keys are not
/// part of it: they are resolved through a [`SecretSource`] under the handles of
/// [`AiSettings::secret_handle`](but_llm::AiConfiguration::secret_handle).
pub type AiSettings = AiConfiguration;

/// Resolves secret handles to secret values (implemented by the GUI over `but-secret`).
///
/// The service asks only for the key of the selected provider, so no keychain prompt appears for providers the
/// user does not use. An implementation may fall back to environment variables such as `OPENAI_API_KEY`.
pub trait SecretSource: Send + Sync {
    /// The secret stored under `handle`, if any.
    fn secret(&self, handle: &str) -> Option<String>;
}

/// Whether the service can generate text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiStatus {
    /// Something is missing.
    NotConfigured {
        /// What.
        missing: AiMissing,
    },
    /// Usable.
    Ready {
        /// The provider name, as shown to the user ("OpenAI", "LM Studio", ...).
        provider: String,
        /// The model.
        model: String,
    },
}

/// Commit message style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStyle {
    /// Conventional Commits (`feat:`, `fix:`), present tense.
    Conventional,
    /// Plain imperative subject without a prefix.
    PlainImperative,
}

/// Input of [`AiService::generate_commit_message`].
#[derive(Debug, Clone)]
pub struct CommitMessageRequest {
    /// The staged patch, from `gitcthulhu_git::diff::staged_patch_text`.
    pub patch: PatchText,
    /// Free-form user notes (what is already in the message box).
    pub notes: String,
    /// The requested style.
    pub style: CommitStyle,
}

/// Input of [`AiService::generate_pr_text`].
#[derive(Debug, Clone)]
pub struct PrRequest {
    /// The current title.
    pub title: String,
    /// The current body.
    pub body: String,
    /// Messages of the commits in the pull request, oldest first.
    pub commit_messages: Vec<String>,
}

/// Generated pull-request title and body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrText {
    /// The title.
    pub title: String,
    /// The body.
    pub body: String,
}

/// What a suggested branch name is derived from.
#[derive(Debug, Clone)]
pub enum BranchNameContext {
    /// The staged patch.
    Staged(PatchText),
    /// A commit message (the HEAD commit, or the commit being branched from).
    CommitMessage(String),
}

/// Errors of the AI service.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AiError {
    /// Something is missing in the configuration.
    #[error("AI is not configured: {0:?} is missing")]
    NotConfigured(AiMissing),
    /// The provider rejected the credentials.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The provider rate-limited the request.
    #[error("rate limited")]
    RateLimited,
    /// Transport failure.
    #[error("network error: {0}")]
    Network(String),
    /// The provider reported an error.
    #[error("provider error: {0}")]
    Provider(String),
    /// The operation was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// The model answered with nothing usable.
    #[error("the model returned an empty answer")]
    EmptyAnswer,
}

impl From<LlmError> for AiError {
    fn from(err: LlmError) -> Self {
        match err {
            LlmError::Auth(message) => AiError::Auth(message),
            LlmError::RateLimited(_) => AiError::RateLimited,
            LlmError::Network(message) => AiError::Network(message),
            LlmError::Provider(message) => AiError::Provider(message),
        }
    }
}

/// The ready provider (or what is missing) and the status that goes with it.
struct State {
    provider: Option<Arc<LLMProvider>>,
    status: AiStatus,
}

/// The AI service.
///
/// Created unconfigured by [`Services::new`](crate::Services::new); call [`configure`](Self::configure) with
/// the user's settings, again whenever they change.
pub struct AiService {
    rt: Runtime,
    state: RwLock<State>,
}

impl AiService {
    /// Create an unconfigured service on the shared runtime.
    pub fn new(rt: Runtime) -> Self {
        AiService {
            rt,
            state: RwLock::new(State {
                provider: None,
                status: AiStatus::NotConfigured {
                    missing: AiMissing::Provider,
                },
            }),
        }
    }

    /// Apply `settings`, resolving the key of the selected provider through `secrets`. Requests already running
    /// keep the provider they started with.
    ///
    /// The new state is recorded in every case ([`status`](Self::status) reports it). Fails with
    /// [`AiError::NotConfigured`] when a provider is selected but cannot be used (blank model, invalid endpoint,
    /// no key), so a settings page can show what to fix. Selecting no provider is not an error: AI is just off.
    pub fn configure(
        &self,
        settings: &AiSettings,
        secrets: &dyn SecretSource,
    ) -> Result<(), AiError> {
        let mut keys = ProviderKeys::default();
        if let Some(provider) = settings.provider
            && let Some(handle) = AiSettings::secret_handle(provider)
        {
            let key = secrets
                .secret(handle)
                .filter(|key| !key.trim().is_empty())
                .map(Sensitive);
            match provider {
                AiProvider::OpenAi => keys.openai = key,
                AiProvider::Anthropic => keys.anthropic = key,
                AiProvider::OpenRouter => keys.openrouter = key,
                AiProvider::Ollama | AiProvider::LMStudio => {}
            }
        }
        let (new_state, result) = match LLMProvider::from_configuration(settings, &keys) {
            Ok(provider) => {
                let status = AiStatus::Ready {
                    provider: provider.kind().display_name().to_owned(),
                    model: provider.model_or_default(),
                };
                (
                    State {
                        provider: Some(Arc::new(provider)),
                        status,
                    },
                    Ok(()),
                )
            }
            Err(missing) => (
                State {
                    provider: None,
                    status: AiStatus::NotConfigured { missing },
                },
                if settings.provider.is_some() {
                    Err(AiError::NotConfigured(missing))
                } else {
                    Ok(())
                },
            ),
        };
        *self.state.write().unwrap_or_else(PoisonError::into_inner) = new_state;
        result
    }

    /// The current status.
    pub fn status(&self) -> AiStatus {
        self.read().status.clone()
    }

    /// Generate a commit message of the staged patch.
    ///
    /// `on_text` receives the answer so far, cleaned (reasoning and a surrounding code fence removed), every time
    /// it changes. Returns the final message wrapped to 72 columns.
    ///
    /// Errors: [`AiError::NotConfigured`], [`AiError::Cancelled`] (the connection is closed), [`AiError::EmptyAnswer`]
    /// and the provider/transport errors.
    pub fn generate_commit_message(
        &self,
        req: &CommitMessageRequest,
        cancel: &CancelToken,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<String, AiError> {
        let mut shown = String::new();
        let raw = self.stream(
            prompt::COMMIT_SYSTEM,
            prompt::commit_prompt(req),
            cancel,
            &mut |answer| {
                let text = format::strip_fence(format::without_thinking(answer)).trim_start();
                if text != shown {
                    shown.clear();
                    shown.push_str(text);
                    on_text(text);
                }
            },
        )?;
        let message = format::strip_fence(format::without_thinking(&raw)).trim();
        if message.is_empty() {
            return Err(AiError::EmptyAnswer);
        }
        Ok(format::format_commit_message(message))
    }

    /// Generate pull-request title and body.
    ///
    /// `on_text` receives the title and body so far every time they change. Returns the final text.
    /// Errors as for [`generate_commit_message`](Self::generate_commit_message).
    pub fn generate_pr_text(
        &self,
        req: &PrRequest,
        cancel: &CancelToken,
        on_text: &mut (dyn FnMut(&PrText) + Send),
    ) -> Result<PrText, AiError> {
        let mut shown = PrText::default();
        let raw = self.stream(
            prompt::PR_SYSTEM,
            prompt::pr_prompt(req),
            cancel,
            &mut |answer| {
                let (title, body) = format::split_title_body(format::without_thinking(answer));
                if title != shown.title || body != shown.body {
                    shown = PrText { title, body };
                    on_text(&shown);
                }
            },
        )?;
        let (title, body) = format::split_title_body(format::without_thinking(&raw));
        if title.is_empty() && body.is_empty() {
            return Err(AiError::EmptyAnswer);
        }
        Ok(PrText { title, body })
    }

    /// Suggest a branch name: lower-case, words joined by `-`, valid by Git's rules. It is not checked against
    /// existing branches; the caller prefills a dialog with it and never creates the branch itself.
    ///
    /// Fails with [`AiError::EmptyAnswer`] when the answer holds no valid name. Other errors as for
    /// [`generate_commit_message`](Self::generate_commit_message).
    pub fn suggest_branch_name(
        &self,
        ctx: &BranchNameContext,
        cancel: &CancelToken,
    ) -> Result<String, AiError> {
        let answer = self.ask(
            prompt::BRANCH_SYSTEM,
            prompt::branch_prompt(ctx),
            &GenerateOptions::default(),
            cancel,
        )?;
        answer
            .as_deref()
            .and_then(format::branch_name_from_answer)
            .ok_or(AiError::EmptyAnswer)
    }

    /// Check that the configured provider answers: one tiny request. Any answer, even an empty one, counts as
    /// success; the interesting results are the errors (wrong key, unreachable server, unknown model).
    pub fn test_connection(&self, cancel: &CancelToken) -> Result<(), AiError> {
        self.ask(
            "You answer with a single word.",
            "ping".to_owned(),
            &GenerateOptions {
                max_tokens: Some(16),
            },
            cancel,
        )
        .map(|_| ())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The provider, or what is missing.
    fn provider(&self) -> Result<Arc<LLMProvider>, AiError> {
        let state = self.read();
        match (&state.provider, &state.status) {
            (Some(provider), _) => Ok(provider.clone()),
            (None, AiStatus::NotConfigured { missing }) => Err(AiError::NotConfigured(*missing)),
            (None, AiStatus::Ready { .. }) => Err(AiError::NotConfigured(AiMissing::Provider)),
        }
    }

    /// Stream the answer to `prompt`, calling `on_answer` with the whole raw answer so far after each piece.
    /// Returns the whole raw answer.
    fn stream(
        &self,
        system: &str,
        prompt: String,
        cancel: &CancelToken,
        on_answer: &mut (dyn FnMut(&str) + Send),
    ) -> Result<String, AiError> {
        let provider = self.provider()?;
        let model = provider.model_or_default();
        let mut answer = String::new();
        let mut on_token = |token: &str| {
            answer.push_str(token);
            on_answer(&answer);
        };
        let result = self
            .rt
            .block_on_cancellable(
                cancel,
                provider.stream_text(
                    system,
                    vec![ChatMessage::User(prompt)],
                    &model,
                    &GenerateOptions::default(),
                    &mut on_token,
                ),
            )
            .map_err(|_| AiError::Cancelled)??;
        result.ok_or(AiError::EmptyAnswer)
    }

    /// Ask without streaming. `Ok(None)` if the model said nothing.
    fn ask(
        &self,
        system: &str,
        prompt: String,
        options: &GenerateOptions,
        cancel: &CancelToken,
    ) -> Result<Option<String>, AiError> {
        let provider = self.provider()?;
        let model = provider.model_or_default();
        Ok(self
            .rt
            .block_on_cancellable(
                cancel,
                provider.text(system, vec![ChatMessage::User(prompt)], &model, options),
            )
            .map_err(|_| AiError::Cancelled)??)
    }
}
