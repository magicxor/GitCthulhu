use crate::tool::Toolset;
use anyhow::Result;
use async_openai::{Client, config::OpenAIConfig};
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OpenRouterConfig {
    pub api_base: String,
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            api_base: crate::DEFAULT_OPENROUTER_ENDPOINT.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    model: Option<String>,
    config: OpenRouterConfig,
    api_key: Sensitive<String>,
}

impl OpenRouterProvider {
    pub fn with(
        api_key: Sensitive<String>,
        config: Option<OpenRouterConfig>,
        model: Option<String>,
    ) -> Self {
        Self {
            config: config.unwrap_or_default(),
            model,
            api_key,
        }
    }
}

impl OpenAIClientProvider for OpenRouterProvider {
    fn client(&self) -> Result<Client<OpenAIConfig>> {
        let open_ai_config = OpenAIConfig::new()
            .with_api_base(self.config.api_base.clone())
            .with_api_key(self.api_key.0.clone());

        Ok(Client::with_config(open_ai_config))
    }
}

impl LLMClient for OpenRouterProvider {
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
