mod anthropic;
mod chat;
mod chat_completions;
mod client;
mod config;
mod error;
mod http;
mod lmstudio;
mod ollama;
mod openai;
mod openai_utils;
mod openrouter;
mod runtime;
mod sse;
#[cfg(test)]
mod test_server;
pub mod tool;

use std::sync::Arc;

use but_secret::Sensitive;

pub use chat::{ChatMessage, StreamToolCallResult, ToolCall, ToolCallContent, ToolResponseContent};
pub use config::*;
pub use error::LlmError;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::client::LLMClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LLMProviderKind {
    OpenAi,
    Anthropic,
    Ollama,
    LMStudio,
    OpenRouter,
}

impl LLMProviderKind {
    pub fn from_config_value(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "openai" => Some(LLMProviderKind::OpenAi),
            "anthropic" => Some(LLMProviderKind::Anthropic),
            "ollama" => Some(LLMProviderKind::Ollama),
            "lmstudio" => Some(LLMProviderKind::LMStudio),
            "openrouter" => Some(LLMProviderKind::OpenRouter),
            _ => None,
        }
    }

    pub fn as_config_value(self) -> &'static str {
        match self {
            LLMProviderKind::OpenAi => "openai",
            LLMProviderKind::Anthropic => "anthropic",
            LLMProviderKind::Ollama => "ollama",
            LLMProviderKind::LMStudio => "lmstudio",
            LLMProviderKind::OpenRouter => "openrouter",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            LLMProviderKind::OpenAi => "OpenAI",
            LLMProviderKind::Anthropic => "Anthropic",
            LLMProviderKind::Ollama => "Ollama",
            LLMProviderKind::LMStudio => "LM Studio",
            LLMProviderKind::OpenRouter => "OpenRouter",
        }
    }
}

impl From<LLMProviderKind> for String {
    fn from(value: LLMProviderKind) -> Self {
        value.display_name().to_string()
    }
}

/// The API keys to use, for the providers that need one. They are handed over per provider because the
/// application, not this crate, knows where its secrets are kept.
#[derive(Debug, Clone, Default)]
pub struct ProviderKeys {
    pub openai: Option<Sensitive<String>>,
    pub anthropic: Option<Sensitive<String>>,
    pub openrouter: Option<Sensitive<String>>,
}

impl ProviderKeys {
    /// Which keys are present and not blank.
    pub fn availability(&self) -> KeyAvailability {
        let present =
            |key: &Option<Sensitive<String>>| key.as_ref().is_some_and(|k| !k.0.trim().is_empty());
        KeyAvailability {
            openai: present(&self.openai),
            anthropic: present(&self.anthropic),
            openrouter: present(&self.openrouter),
        }
    }
}

/// Per-request options of the plain-text calls.
#[derive(Debug, Clone, Default)]
pub struct GenerateOptions {
    /// Upper bound of generated tokens. `None` leaves it to the provider (Anthropic: 4096, as it requires a
    /// limit on every request).
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum LLMClientType {
    OpenAi(Arc<openai::OpenAiProvider>),
    Anthropic(Arc<anthropic::AnthropicProvider>),
    Ollama(Arc<ollama::OllamaProvider>),
    LMStudio(Arc<lmstudio::LMStudioProvider>),
    OpenRouter(Arc<openrouter::OpenRouterProvider>),
}

#[derive(Debug, Clone)]
pub struct LLMProvider {
    client: LLMClientType,
}

/// The top-level LLM provider that wraps specific implementations.
///
/// This struct provides a unified interface to interact with different LLM providers
/// (currently OpenAI and Ollama). It abstracts away the differences between providers,
/// allowing callers to work with a consistent API regardless of which underlying
/// LLM service is being used. The provider handles authentication, request formatting,
/// response parsing, and tool calling orchestration for all supported backends.
impl LLMProvider {
    /// Create the provider that `configuration` selects, with the keys in `keys`.
    ///
    /// Fails with what is missing when the active provider is not usable: no provider selected, a blank model,
    /// an invalid endpoint, or no key where one is needed.
    pub fn from_configuration(
        configuration: &AiConfiguration,
        keys: &ProviderKeys,
    ) -> Result<Self, AiMissing> {
        if let Some(missing) = configuration.missing(keys.availability()) {
            return Err(missing);
        }
        let Some(provider) = configuration.provider else {
            return Err(AiMissing::Provider);
        };
        let key = |key: &Option<Sensitive<String>>| {
            key.as_ref()
                .map(|k| Sensitive(k.0.trim().to_owned()))
                .ok_or(AiMissing::Key)
        };
        let model = |model: &str| Some(model.trim().to_owned());
        let client = match provider {
            LLMProviderKind::OpenAi => {
                LLMClientType::OpenAi(Arc::new(openai::OpenAiProvider::with(
                    key(&keys.openai)?,
                    model(&configuration.openai.model),
                    configuration
                        .openai
                        .custom_endpoint
                        .as_deref()
                        .map(str::trim)
                        .filter(|endpoint| !endpoint.is_empty())
                        .map(str::to_owned),
                )))
            }
            LLMProviderKind::Anthropic => {
                LLMClientType::Anthropic(Arc::new(anthropic::AnthropicProvider::with(
                    key(&keys.anthropic)?,
                    model(&configuration.anthropic.model),
                )))
            }
            LLMProviderKind::Ollama => {
                LLMClientType::Ollama(Arc::new(ollama::OllamaProvider::new(
                    ollama::OllamaConfig {
                        host_config: Some(ollama::OllamaHostConfig::from(
                            configuration.ollama.endpoint.trim().to_owned(),
                        )),
                    },
                    model(&configuration.ollama.model),
                )))
            }
            LLMProviderKind::LMStudio => {
                LLMClientType::LMStudio(Arc::new(lmstudio::LMStudioProvider::with(
                    Some(lmstudio::LMStudioConfig {
                        api_base: configuration.lmstudio.endpoint.trim().to_owned(),
                    }),
                    model(&configuration.lmstudio.model),
                )))
            }
            LLMProviderKind::OpenRouter => {
                LLMClientType::OpenRouter(Arc::new(openrouter::OpenRouterProvider::with(
                    key(&keys.openrouter)?,
                    Some(openrouter::OpenRouterConfig {
                        api_base: configuration.openrouter.endpoint.trim().to_owned(),
                    }),
                    model(&configuration.openrouter.model),
                )))
            }
        };
        Ok(Self { client })
    }

    /// Which kind of provider this is.
    pub fn kind(&self) -> LLMProviderKind {
        match &self.client {
            LLMClientType::OpenAi(_) => LLMProviderKind::OpenAi,
            LLMClientType::Anthropic(_) => LLMProviderKind::Anthropic,
            LLMClientType::Ollama(_) => LLMProviderKind::Ollama,
            LLMClientType::LMStudio(_) => LLMProviderKind::LMStudio,
            LLMClientType::OpenRouter(_) => LLMProviderKind::OpenRouter,
        }
    }

    /// Stream a plain-text answer, calling `on_token` with each piece as it arrives. Returns the whole answer,
    /// or `None` if the model said nothing.
    ///
    /// The future does its work only while polled and holds the HTTP connection: dropping it (for instance
    /// when it loses a `select!` against a cancellation signal) closes the connection immediately. It runs on
    /// the caller's tokio runtime, which must have its time and IO drivers enabled.
    pub async fn stream_text(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        model: &str,
        options: &GenerateOptions,
        on_token: &mut (dyn FnMut(&str) + Send),
    ) -> Result<Option<String>, LlmError> {
        let (s, m, o) = (system_message, model, options);
        match &self.client {
            LLMClientType::OpenAi(c) => {
                chat_completions::stream(c.as_ref(), s, chat_messages, m, o, on_token).await
            }
            LLMClientType::Ollama(c) => {
                chat_completions::stream(c.as_ref(), s, chat_messages, m, o, on_token).await
            }
            LLMClientType::LMStudio(c) => {
                chat_completions::stream(c.as_ref(), s, chat_messages, m, o, on_token).await
            }
            LLMClientType::OpenRouter(c) => {
                chat_completions::stream(c.as_ref(), s, chat_messages, m, o, on_token).await
            }
            LLMClientType::Anthropic(c) => {
                let client = c
                    .client()
                    .map_err(|err| LlmError::Provider(format!("{err:#}")))?;
                anthropic::stream_text(&client, s, chat_messages, m, o, on_token).await
            }
        }
    }

    /// Ask for a plain-text answer without streaming. `None` if the model said nothing. Cancellable by
    /// dropping the future, like [`Self::stream_text`].
    pub async fn text(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        model: &str,
        options: &GenerateOptions,
    ) -> Result<Option<String>, LlmError> {
        let (s, m, o) = (system_message, model, options);
        match &self.client {
            LLMClientType::OpenAi(c) => {
                chat_completions::complete(c.as_ref(), s, chat_messages, m, o).await
            }
            LLMClientType::Ollama(c) => {
                chat_completions::complete(c.as_ref(), s, chat_messages, m, o).await
            }
            LLMClientType::LMStudio(c) => {
                chat_completions::complete(c.as_ref(), s, chat_messages, m, o).await
            }
            LLMClientType::OpenRouter(c) => {
                chat_completions::complete(c.as_ref(), s, chat_messages, m, o).await
            }
            LLMClientType::Anthropic(c) => {
                let client = c
                    .client()
                    .map_err(|err| LlmError::Provider(format!("{err:#}")))?;
                anthropic::text(&client, s, chat_messages, m, o).await
            }
        }
    }

    /// Returns the model identifier configured for this LLM provider.
    ///
    /// This method retrieves the model name that was configured for the provider
    /// at the time of instantiation, from the application's [`AiConfiguration`].
    /// The model identifier (e.g., "gpt-4", "claude-3-opus", "llama2") determines
    /// which specific model variant will be used for LLM requests.
    ///
    /// The model value is read once during provider initialization and remains
    /// constant for the lifetime of the provider instance.
    ///
    /// # Returns
    ///
    /// Returns `Some(String)` containing the model identifier if one was configured,
    /// or `None` if no model was specified in the configuration.
    pub fn model(&self) -> Option<String> {
        match &self.client {
            LLMClientType::OpenAi(client) => client.model(),
            LLMClientType::Anthropic(client) => client.model(),
            LLMClientType::Ollama(client) => client.model(),
            LLMClientType::LMStudio(client) => client.model(),
            LLMClientType::OpenRouter(client) => client.model(),
        }
    }

    /// Returns the configured model, or a per-provider default when none is set.
    ///
    /// Callers that need a concrete model name (e.g. structured output) must not
    /// hardcode a provider-specific model, as it would 404 against other providers.
    /// This returns the configured model when available, otherwise a sensible
    /// default for the active provider.
    pub fn model_or_default(&self) -> String {
        if let Some(model) = self.model() {
            return model;
        }
        match &self.client {
            LLMClientType::OpenAi(_) => DEFAULT_OPENAI_MODEL,
            LLMClientType::Anthropic(_) => DEFAULT_ANTHROPIC_MODEL,
            LLMClientType::Ollama(_) => DEFAULT_OLLAMA_MODEL,
            LLMClientType::LMStudio(_) => DEFAULT_LMSTUDIO_MODEL,
            LLMClientType::OpenRouter(_) => DEFAULT_OPENROUTER_MODEL,
        }
        .to_string()
    }

    /// Executes an interactive tool-calling loop with streaming output.
    ///
    /// This method orchestrates a conversation with the LLM where the model can call
    /// tools (functions) to gather information or perform actions. The LLM's text output
    /// is streamed token-by-token via the provided callback, enabling real-time display
    /// of the assistant's responses. The loop continues until the LLM produces a final
    /// response without tool calls.
    ///
    /// The tool-calling loop works as follows:
    /// 1. Send messages to the LLM with available tools
    /// 2. If the LLM requests tool calls, execute them and add results to the conversation
    /// 3. Continue until the LLM provides a final text response
    /// 4. Stream all text output through the `on_token` callback as it's generated
    ///
    /// # Arguments
    ///
    /// * `system_message` - The system prompt that defines the assistant's behavior and context
    /// * `chat_messages` - The conversation history including user and assistant messages
    /// * `tool_set` - A mutable reference to the toolset containing available functions the LLM can call
    /// * `model` - The model identifier (e.g., "gpt-4", "llama2") to use for generation
    /// * `on_token` - A callback function that receives each token as it's generated, enabling streaming display
    ///
    /// # Returns
    ///
    /// Returns `Ok((final_response, updated_messages))` where:
    /// - `final_response` is the complete text response from the LLM
    /// - `updated_messages` is the full conversation history including all tool calls and responses
    ///
    /// Returns `Err` if the LLM request fails or tool execution encounters an error.
    pub fn tool_calling_loop_stream(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        tool_set: &mut impl crate::tool::Toolset,
        model: &str,
        on_token: impl Fn(&str) + Send + Sync + 'static,
    ) -> anyhow::Result<(String, Vec<ChatMessage>)> {
        match &self.client {
            LLMClientType::OpenAi(client) => client.tool_calling_loop_stream(
                system_message,
                chat_messages,
                tool_set,
                model,
                on_token,
            ),
            LLMClientType::Anthropic(client) => client.tool_calling_loop_stream(
                system_message,
                chat_messages,
                tool_set,
                model,
                on_token,
            ),
            LLMClientType::Ollama(client) => client.tool_calling_loop_stream(
                system_message,
                chat_messages,
                tool_set,
                model,
                on_token,
            ),
            LLMClientType::LMStudio(client) => client.tool_calling_loop_stream(
                system_message,
                chat_messages,
                tool_set,
                model,
                on_token,
            ),
            LLMClientType::OpenRouter(client) => client.tool_calling_loop_stream(
                system_message,
                chat_messages,
                tool_set,
                model,
                on_token,
            ),
        }
    }

    /// Executes an interactive tool-calling loop without streaming.
    ///
    /// Similar to `tool_calling_loop_stream`, but returns the complete final response
    /// only after the entire conversation is finished, without streaming intermediate tokens.
    /// This is useful when you don't need real-time output and prefer to wait for the
    /// complete response before processing it.
    ///
    /// The tool-calling loop works as follows:
    /// 1. Send messages to the LLM with available tools
    /// 2. If the LLM requests tool calls, execute them and add results to the conversation
    /// 3. Continue until the LLM provides a final text response
    /// 4. Return only the final complete response
    ///
    /// # Arguments
    ///
    /// * `system_message` - The system prompt that defines the assistant's behavior and context
    /// * `chat_messages` - The conversation history including user and assistant messages
    /// * `tool_set` - A mutable reference to the toolset containing available functions the LLM can call
    /// * `model` - The model identifier (e.g., "gpt-4", "llama2") to use for generation
    ///
    /// # Returns
    ///
    /// Returns `Ok(final_response)` containing the complete text response from the LLM
    /// after all tool calls have been resolved, or `Err` if the request or tool execution fails.
    pub fn tool_calling_loop(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        tool_set: &mut impl crate::tool::Toolset,
        model: &str,
    ) -> anyhow::Result<String> {
        match &self.client {
            LLMClientType::OpenAi(client) => {
                client.tool_calling_loop(system_message, chat_messages, tool_set, model)
            }
            LLMClientType::Anthropic(client) => {
                client.tool_calling_loop(system_message, chat_messages, tool_set, model)
            }
            LLMClientType::Ollama(client) => {
                client.tool_calling_loop(system_message, chat_messages, tool_set, model)
            }
            LLMClientType::LMStudio(client) => {
                client.tool_calling_loop(system_message, chat_messages, tool_set, model)
            }
            LLMClientType::OpenRouter(client) => {
                client.tool_calling_loop(system_message, chat_messages, tool_set, model)
            }
        }
    }

    /// Generates a streaming text response without tool calling capability.
    ///
    /// This method sends a conversation to the LLM and streams the response token-by-token
    /// through the provided callback. Unlike the tool-calling methods, this is a simple
    /// request-response interaction without the ability for the model to call functions.
    /// Use this when you want a straightforward chat completion with streaming output.
    ///
    /// # Arguments
    ///
    /// * `system_message` - The system prompt that defines the assistant's behavior and context
    /// * `chat_messages` - The conversation history including user and assistant messages
    /// * `model` - The model identifier (e.g., "gpt-4", "llama2") to use for generation
    /// * `on_token` - A callback function that receives each token as it's generated, enabling streaming display
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(response))` containing the complete text response if successful,
    /// `Ok(None)` if the model produced no output, or `Err` if the request failed.
    pub fn stream_response(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        model: &str,
        on_token: impl Fn(&str) + Send + Sync + 'static,
    ) -> anyhow::Result<Option<String>> {
        match &self.client {
            LLMClientType::OpenAi(client) => {
                client.stream_response(system_message, chat_messages, model, on_token)
            }
            LLMClientType::Anthropic(client) => {
                client.stream_response(system_message, chat_messages, model, on_token)
            }
            LLMClientType::Ollama(client) => {
                client.stream_response(system_message, chat_messages, model, on_token)
            }
            LLMClientType::LMStudio(client) => {
                client.stream_response(system_message, chat_messages, model, on_token)
            }
            LLMClientType::OpenRouter(client) => {
                client.stream_response(system_message, chat_messages, model, on_token)
            }
        }
    }

    /// Generates a structured response conforming to a specific schema.
    ///
    /// This method requests the LLM to produce output in a specific structured format
    /// defined by a Rust type. The response is automatically validated against the
    /// JSON schema derived from the type and deserialized into the requested structure.
    /// This is useful for extracting structured data (like configuration, JSON objects,
    /// or specific data models) from LLM responses with type safety.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The type that defines the expected structure. Must implement
    ///   `Serialize`, `Deserialize`, and `JsonSchema` to enable schema generation
    ///   and validation.
    ///
    /// # Arguments
    ///
    /// * `system_message` - The system prompt that defines the assistant's behavior and context
    /// * `chat_messages` - The conversation history including user and assistant messages
    /// * `model` - The model identifier (e.g., "gpt-4", "llama2") to use for generation
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(value))` containing the parsed structured response if successful,
    /// `Ok(None)` if the model produced no output, or `Err` if the request failed or
    /// the response didn't conform to the expected schema.
    pub fn structured_output<
        T: serde::Serialize + DeserializeOwned + JsonSchema + std::marker::Send + 'static,
    >(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        model: &str,
    ) -> anyhow::Result<Option<T>> {
        match &self.client {
            LLMClientType::OpenAi(client) => {
                client.structured_output::<T>(system_message, chat_messages, model)
            }
            LLMClientType::Anthropic(client) => {
                client.structured_output::<T>(system_message, chat_messages, model)
            }
            LLMClientType::Ollama(client) => {
                client.structured_output::<T>(system_message, chat_messages, model)
            }
            LLMClientType::LMStudio(client) => {
                client.structured_output::<T>(system_message, chat_messages, model)
            }
            LLMClientType::OpenRouter(client) => {
                client.structured_output::<T>(system_message, chat_messages, model)
            }
        }
    }

    /// Generates a simple text response without streaming or tool calling.
    ///
    /// This is the most basic interaction method that sends a conversation to the LLM
    /// and waits for the complete response. The entire response is returned at once
    /// without streaming. Use this for simple request-response scenarios where you
    /// don't need real-time output or function calling capabilities.
    ///
    /// # Arguments
    ///
    /// * `system_message` - The system prompt that defines the assistant's behavior and context
    /// * `chat_messages` - The conversation history including user and assistant messages
    /// * `model` - The model identifier (e.g., "gpt-4", "llama2") to use for generation
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(response))` containing the complete text response if successful,
    /// `Ok(None)` if the model produced no output, or `Err` if the request failed.
    pub fn response(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        model: &str,
    ) -> anyhow::Result<Option<String>> {
        match &self.client {
            LLMClientType::OpenAi(client) => client.response(system_message, chat_messages, model),
            LLMClientType::Anthropic(client) => {
                client.response(system_message, chat_messages, model)
            }
            LLMClientType::Ollama(client) => client.response(system_message, chat_messages, model),
            LLMClientType::LMStudio(client) => {
                client.response(system_message, chat_messages, model)
            }
            LLMClientType::OpenRouter(client) => {
                client.response(system_message, chat_messages, model)
            }
        }
    }
}
