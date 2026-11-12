use crate::tool::Toolset;
use anyhow::Result;
use async_openai::{
    Client,
    config::{OPENAI_API_BASE, OpenAIConfig},
};
use but_secret::Sensitive;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::{
    chat::ChatMessage,
    client::LLMClient,
    openai_utils::{
        OpenAIClientProvider, response_blocking, stream_response_blocking,
        structured_output_blocking, tool_calling_loop, tool_calling_loop_stream,
    },
};

/// OpenAI, or an OpenAI-compatible server (Azure, vLLM, LiteLLM, ...) behind a custom endpoint, with the
/// user's own API key.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    /// The API base URL to use instead of OpenAI's.
    custom_endpoint: Option<String>,
    /// The model to use.
    model: Option<String>,
    api_key: Sensitive<String>,
}

impl OpenAiProvider {
    pub fn with(
        api_key: Sensitive<String>,
        model: Option<String>,
        custom_endpoint: Option<String>,
    ) -> Self {
        Self {
            custom_endpoint,
            model,
            api_key,
        }
    }
}

impl OpenAIClientProvider for OpenAiProvider {
    fn client(&self) -> Result<Client<OpenAIConfig>> {
        // The base is always explicit: `OpenAIConfig::new()` alone would honor `OPENAI_BASE_URL` and send the
        // user's key wherever the environment points.
        let api_base = self.custom_endpoint.as_deref().unwrap_or(OPENAI_API_BASE);
        let config = OpenAIConfig::new()
            .with_api_base(api_base)
            .with_api_key(self.api_key.0.clone());
        Ok(Client::with_config(config))
    }

    fn uses_max_completion_tokens(&self) -> bool {
        // OpenAI's newer models reject `max_tokens`; compatible servers may not know the new name.
        self.custom_endpoint.is_none()
    }
}

impl LLMClient for OpenAiProvider {
    fn model(&self) -> Option<String> {
        self.model.clone()
    }

    fn tool_calling_loop_stream(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        tool_set: &mut impl Toolset,
        model: &str,
        on_token: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<(String, Vec<ChatMessage>)> {
        let result = tool_calling_loop_stream(
            self,
            system_message,
            chat_messages,
            tool_set,
            model,
            on_token,
        )?;
        Ok((result.final_response, result.message_history))
    }

    fn tool_calling_loop(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        tool_set: &mut impl Toolset,
        model: &str,
    ) -> Result<String> {
        tool_calling_loop(self, system_message, chat_messages, tool_set, model)
    }

    fn stream_response(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        model: &str,
        on_token: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<Option<String>> {
        stream_response_blocking(self, system_message, chat_messages, model, on_token)
    }

    fn structured_output<
        T: serde::Serialize + DeserializeOwned + JsonSchema + std::marker::Send + 'static,
    >(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        model: &str,
    ) -> Result<Option<T>> {
        structured_output_blocking::<T>(self, system_message, chat_messages, model)
    }

    fn response(
        &self,
        system_message: &str,
        chat_messages: Vec<ChatMessage>,
        model: &str,
    ) -> Result<Option<String>> {
        response_blocking(self, system_message, chat_messages, model)
    }
}
