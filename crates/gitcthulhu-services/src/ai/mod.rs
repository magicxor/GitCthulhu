//! AI text generation (commit messages, pull-request text, branch names). 20 section 8.4.
//! Stub: the implementation is owned by WP-S2.

use gitcthulhu_git::{CancelToken, PatchText};

use crate::Runtime;

/// The AI configuration, stored in the GUI settings file (filled in by WP-S2).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AiSettings {
    /// The provider key (`openai`, `anthropic`, `ollama`, `lmstudio`, `openrouter`); `None` = not configured.
    pub provider: Option<String>,
}

/// Resolves secret handles to secret values (implemented by the GUI over `but-secret`).
pub trait SecretSource: Send + Sync {
    /// The secret stored under `handle`, if any.
    fn secret(&self, handle: &str) -> Option<String>;
}

/// What is missing for the AI service to be usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiMissing {
    /// No provider selected.
    Provider,
    /// No API key.
    Key,
    /// No model.
    Model,
    /// No endpoint.
    Endpoint,
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
        /// The provider name.
        provider: String,
        /// The model.
        model: String,
    },
}

/// Commit message style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStyle {
    /// Conventional Commits.
    Conventional,
    /// Plain imperative subject.
    PlainImperative,
}

/// Input of [`AiService::generate_commit_message`].
#[derive(Debug, Clone)]
pub struct CommitMessageRequest {
    /// The staged patch.
    pub patch: PatchText,
    /// Free-form user notes.
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
    /// Messages of the commits in the pull request.
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
    /// A commit message.
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
    /// The operation is not implemented in this skeleton (WP-S2 replaces it).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// The AI service.
pub struct AiService {
    #[allow(dead_code)]
    rt: Runtime,
}

impl AiService {
    /// Create an unconfigured service on the shared runtime.
    pub fn new(rt: Runtime) -> Self {
        AiService { rt }
    }

    /// Apply `settings`, resolving keys through `secrets`.
    pub fn configure(
        &self,
        _settings: &AiSettings,
        _secrets: &dyn SecretSource,
    ) -> Result<(), AiError> {
        Err(AiError::NotImplemented("ai::configure"))
    }

    /// The current status.
    pub fn status(&self) -> AiStatus {
        AiStatus::NotConfigured {
            missing: AiMissing::Provider,
        }
    }

    /// Generate a commit message (final text wrapped to 72 columns).
    pub fn generate_commit_message(
        &self,
        _req: &CommitMessageRequest,
        _cancel: &CancelToken,
        _on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<String, AiError> {
        Err(AiError::NotImplemented("ai::generate_commit_message"))
    }

    /// Generate pull-request title and body.
    pub fn generate_pr_text(
        &self,
        _req: &PrRequest,
        _cancel: &CancelToken,
        _on_text: &mut (dyn FnMut(&PrText) + Send),
    ) -> Result<PrText, AiError> {
        Err(AiError::NotImplemented("ai::generate_pr_text"))
    }

    /// Suggest a branch name.
    pub fn suggest_branch_name(
        &self,
        _ctx: &BranchNameContext,
        _cancel: &CancelToken,
    ) -> Result<String, AiError> {
        Err(AiError::NotImplemented("ai::suggest_branch_name"))
    }

    /// Check that the configured provider answers.
    pub fn test_connection(&self, _cancel: &CancelToken) -> Result<(), AiError> {
        Err(AiError::NotImplemented("ai::test_connection"))
    }
}
