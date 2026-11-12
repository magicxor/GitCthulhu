use std::{collections::HashMap, ops::Deref, sync::Arc};

use crate::tool::Toolset;
use anyhow::{Context as _, Result};
use but_secret::Sensitive;
use futures::StreamExt as _;
use reqwest::{
    Response,
    header::{HeaderMap, HeaderValue},
};
use schemars::{JsonSchema, schema_for};
use serde::de::DeserializeOwned;
use tokio::time::timeout;

use crate::{
    GenerateOptions, StreamToolCallResult, ToolCall, ToolCallContent, ToolResponseContent,
    chat::ChatMessage,
    client::LLMClient,
    error::LlmError,
    http::{RESPONSE_START_TIMEOUT, STREAM_IDLE_TIMEOUT, TOTAL_TIMEOUT},
    sse::{SseEvent, SseParser},
};

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The output limit of requests that do not set one. Anthropic requires the field on every request.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Result of a tool calling loop with streaming
pub struct ConversationResult {
    pub final_response: String,
    pub message_history: Vec<ChatMessage>,
}

#[derive(Debug, Clone)]
pub struct AnthropicClient {
    api_base: String,
    headers: HeaderMap,
    max_tokens: u32,
}

impl AnthropicClient {
    fn new(api_base: &str, api_key: &Sensitive<String>, max_tokens: u32) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut key = HeaderValue::from_str(&api_key.0)
            .context("the Anthropic API key contains characters that cannot be sent in a header")?;
        key.set_sensitive(true);
        headers.insert("x-api-key", key);
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        Ok(Self {
            api_base: api_base.trim_end_matches('/').to_owned(),
            headers,
            max_tokens,
        })
    }

    /// Send a message request to the Anthropic Messages API
    async fn message(&self, request: &AnthropicRequest) -> Result<AnthropicResponse, LlmError> {
        let response = self
            .request(request)?
            .timeout(TOTAL_TIMEOUT)
            .send()
            .await
            .map_err(LlmError::from)?;
        let status = response.status();
        let body = response.text().await.map_err(LlmError::from)?;
        if !status.is_success() {
            return Err(LlmError::from_status(status, &body));
        }
        serde_json::from_str(&body)
            .map_err(|err| LlmError::Provider(format!("the answer could not be read: {err}")))
    }

    /// Send a streaming request; returns once the response head arrived and is a success.
    async fn message_raw(&self, request: &AnthropicRequest) -> Result<Response, LlmError> {
        let response = timeout(RESPONSE_START_TIMEOUT, self.request(request)?.send())
            .await
            .map_err(|_| LlmError::timed_out("the response", RESPONSE_START_TIMEOUT))??;
        let status = response.status();
        if !status.is_success() {
            let body = timeout(RESPONSE_START_TIMEOUT, response.text())
                .await
                .map_err(|_| {
                    LlmError::timed_out("the error description", RESPONSE_START_TIMEOUT)
                })??;
            return Err(LlmError::from_status(status, &body));
        }
        Ok(response)
    }

    fn request(&self, request: &AnthropicRequest) -> Result<reqwest::RequestBuilder, LlmError> {
        Ok(crate::http::client()?
            .post(format!("{}/messages", self.api_base))
            .headers(self.headers.clone())
            .json(request))
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    api_base: String,
    model: Option<String>,
    api_key: Sensitive<String>,
    max_tokens: u32,
}

impl AnthropicProvider {
    pub fn with(api_key: Sensitive<String>, model: Option<String>) -> Self {
        Self::with_api_base(api_key, model, ANTHROPIC_API_BASE)
    }

    /// Like [`Self::with`], but sending requests to `api_base` (the part before `/messages`).
    pub(crate) fn with_api_base(
        api_key: Sensitive<String>,
        model: Option<String>,
        api_base: &str,
    ) -> Self {
        Self {
            api_base: api_base.to_owned(),
            model,
            api_key,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    /// The output limit of requests that do not set their own.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn client(&self) -> Result<AnthropicClient> {
        AnthropicClient::new(&self.api_base, &self.api_key, self.max_tokens)
    }
}

impl LLMClient for AnthropicProvider {
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

// Anthropic API types
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AnthropicMessage {
    role: String,
    content: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AnthropicResponse {
    id: String,
    #[serde(rename = "type")]
    response_type: String,
    role: String,
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    usage: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_block: Option<AnthropicContentBlock>,
}

impl From<ChatMessage> for AnthropicMessage {
    fn from(msg: ChatMessage) -> Self {
        match msg {
            ChatMessage::User(content) => AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!([{"type": "text", "text": content}]),
            },
            ChatMessage::Assistant(content) => AnthropicMessage {
                role: "assistant".to_string(),
                content: serde_json::json!([{"type": "text", "text": content}]),
            },
            ChatMessage::ToolCall(content) => AnthropicMessage {
                role: "assistant".to_string(),
                content: serde_json::json!([{
                    "type": "tool_use",
                    "id": content.id,
                    "name": content.name,
                    "input": serde_json::from_str::<serde_json::Value>(&content.arguments).unwrap_or(serde_json::json!({}))
                }]),
            },
            ChatMessage::ToolResponse(content) => AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": content.id,
                    "content": content.result
                }]),
            },
        }
    }
}

fn from_anthropic_messages(messages: Vec<AnthropicMessage>) -> Vec<ChatMessage> {
    let mut chat_messages = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "user" => {
                if let Ok(blocks) =
                    serde_json::from_value::<Vec<AnthropicContentBlock>>(msg.content)
                {
                    for block in blocks {
                        match block.block_type.as_str() {
                            "text" => {
                                if let Some(text) = block.text {
                                    chat_messages.push(ChatMessage::User(text));
                                }
                            }
                            "tool_result" => {
                                if let (Some(id), Some(input)) = (block.id, block.input) {
                                    let result = input.as_str().unwrap_or("").to_string();
                                    chat_messages.push(ChatMessage::ToolResponse(
                                        ToolResponseContent { id, result },
                                    ));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "assistant" => {
                if let Ok(blocks) =
                    serde_json::from_value::<Vec<AnthropicContentBlock>>(msg.content)
                {
                    for block in blocks {
                        match block.block_type.as_str() {
                            "text" => {
                                if let Some(text) = block.text {
                                    chat_messages.push(ChatMessage::Assistant(text));
                                }
                            }
                            "tool_use" => {
                                if let (Some(id), Some(name), Some(input)) =
                                    (block.id, block.name, block.input)
                                {
                                    let arguments =
                                        serde_json::to_string(&input).unwrap_or_default();
                                    chat_messages.push(ChatMessage::ToolCall(ToolCallContent {
                                        id,
                                        name,
                                        arguments,
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    chat_messages
}

pub fn structured_output_blocking<
    T: serde::Serialize + DeserializeOwned + JsonSchema + std::marker::Send + 'static,
>(
    anthropic: &AnthropicProvider,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
) -> anyhow::Result<Option<T>> {
    let client = anthropic.client()?;
    let messages: Vec<AnthropicMessage> = chat_messages
        .into_iter()
        .map(AnthropicMessage::from)
        .collect();
    let model = model.to_string();
    let system_message = system_message.to_string();

    crate::runtime::block_on(async move {
        structured_output::<T>(&client, messages, model, system_message).await
    })
}

async fn structured_output<T: serde::Serialize + DeserializeOwned + JsonSchema>(
    client: &AnthropicClient,
    messages: Vec<AnthropicMessage>,
    model: String,
    system_message: String,
) -> anyhow::Result<Option<T>> {
    let schema = schema_for!(T);

    // For now, we'll use a system prompt to request JSON output
    // Anthropic doesn't have native structured output like OpenAI, but we can guide it
    let enhanced_system = format!(
        "{}\n\nPlease respond with valid JSON matching this schema: {}",
        system_message,
        serde_json::to_string_pretty(&schema)?
    );

    let request = AnthropicRequest {
        model,
        max_tokens: client.max_tokens,
        messages,
        system: Some(enhanced_system),
        tools: None,
        stream: None,
    };

    let anthropic_response = client.message(&request).await?;

    for block in anthropic_response.content {
        if block.block_type == "text"
            && let Some(text) = block.text
        {
            // Try to extract JSON from the text
            let json_text = if text.trim().starts_with("```json") {
                text.trim()
                    .strip_prefix("```json")
                    .and_then(|s| s.strip_suffix("```"))
                    .unwrap_or(&text)
                    .trim()
            } else if text.trim().starts_with("```") {
                text.trim()
                    .strip_prefix("```")
                    .and_then(|s| s.strip_suffix("```"))
                    .unwrap_or(&text)
                    .trim()
            } else {
                text.trim()
            };

            return Ok(Some(serde_json::from_str::<T>(json_text)?));
        }
    }

    Ok(None)
}

fn response_blocking(
    client: &AnthropicProvider,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
) -> anyhow::Result<Option<String>> {
    let client = client.client()?;
    crate::runtime::block_on(async {
        Ok(text(
            &client,
            system_message,
            chat_messages,
            model,
            &GenerateOptions::default(),
        )
        .await?)
    })
}

/// Ask for the whole answer at once. `Ok(None)` if the model said nothing.
pub(crate) async fn text(
    client: &AnthropicClient,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
    options: &GenerateOptions,
) -> Result<Option<String>, LlmError> {
    let request = text_request(client, system_message, chat_messages, model, options, false);
    let response = client.message(&request).await?;
    let text = response
        .content
        .into_iter()
        .find(|block| block.block_type == "text")
        .and_then(|block| block.text)
        .filter(|text| !text.is_empty());
    Ok(text)
}

fn text_request(
    client: &AnthropicClient,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
    options: &GenerateOptions,
    stream: bool,
) -> AnthropicRequest {
    AnthropicRequest {
        model: model.to_owned(),
        max_tokens: options.max_tokens.unwrap_or(client.max_tokens),
        messages: chat_messages
            .into_iter()
            .map(AnthropicMessage::from)
            .collect(),
        system: Some(system_message.to_owned()),
        tools: None,
        stream: stream.then_some(true),
    }
}

pub fn stream_response_blocking(
    client: &AnthropicProvider,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
    on_token: impl Fn(&str) + Send + Sync + 'static,
) -> anyhow::Result<Option<String>> {
    let client = client.client()?;
    crate::runtime::block_on(async {
        Ok(stream_text(
            &client,
            system_message,
            chat_messages,
            model,
            &GenerateOptions::default(),
            &mut |token| on_token(token),
        )
        .await?)
    })
}

/// Ask for the answer as a stream, calling `on_token` with each piece as it arrives. Dropping the returned
/// future closes the connection.
///
/// An `error` event in the stream (Anthropic reports overload and similar failures that way after the `200`)
/// ends the request with that error, and so does a stream that ends before `message_stop`.
pub(crate) async fn stream_text(
    client: &AnthropicClient,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
    options: &GenerateOptions,
    on_token: &mut (dyn FnMut(&str) + Send),
) -> Result<Option<String>, LlmError> {
    let request = text_request(client, system_message, chat_messages, model, options, true);
    let response = client.message_raw(&request).await?;
    let mut answer = String::new();
    for_each_event(response, |event| {
        match event.event_type.as_str() {
            "content_block_delta" => {
                if let Some(token) = event
                    .delta
                    .as_ref()
                    .and_then(|delta| delta.get("text"))
                    .and_then(|text| text.as_str())
                {
                    answer.push_str(token);
                    on_token(token);
                }
            }
            "message_stop" => return std::ops::ControlFlow::Break(()),
            _ => {}
        }
        std::ops::ControlFlow::Continue(())
    })
    .await?;
    Ok((!answer.is_empty()).then_some(answer))
}

/// What a streamed `error` event says, if `sse` is one.
fn error_event(sse: &SseEvent, value: &serde_json::Value) -> Option<LlmError> {
    let is_error = value.get("type").and_then(|t| t.as_str()) == Some("error")
        || sse.event.as_deref() == Some("error");
    if !is_error {
        return None;
    }
    let kind = value
        .pointer("/error/type")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    let message = value
        .pointer("/error/message")
        .and_then(|m| m.as_str())
        .unwrap_or("the provider reported an error");
    Some(LlmError::from_event(kind, message.to_owned()))
}

/// Read `response` as a stream of events and hand each to `handle` until it breaks.
///
/// Fails with the provider's error if it sends an `error` event, and with a network error if the stream ends
/// (or goes silent) before `handle` broke.
async fn for_each_event(
    response: Response,
    mut handle: impl FnMut(AnthropicStreamEvent) -> std::ops::ControlFlow<()>,
) -> Result<(), LlmError> {
    let mut chunks = response.bytes_stream();
    let mut parser = SseParser::default();
    loop {
        let (events, last) = match timeout(STREAM_IDLE_TIMEOUT, chunks.next()).await {
            Err(_) => return Err(LlmError::timed_out("the next token", STREAM_IDLE_TIMEOUT)),
            Ok(None) => (parser.finish().into_iter().collect::<Vec<_>>(), true),
            Ok(Some(chunk)) => (parser.push(&chunk?), false),
        };
        for sse in events {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&sse.data) else {
                continue;
            };
            if let Some(err) = error_event(&sse, &value) {
                return Err(err);
            }
            let Ok(event) = serde_json::from_value::<AnthropicStreamEvent>(value) else {
                continue;
            };
            if handle(event).is_break() {
                return Ok(());
            }
        }
        if last {
            return Err(LlmError::Network(
                "the stream ended before the answer was complete".to_owned(),
            ));
        }
    }
}

fn tool_calling_blocking(
    client: &AnthropicProvider,
    messages: Vec<AnthropicMessage>,
    tools: Vec<serde_json::Value>,
    model: &str,
    system_message: &str,
) -> anyhow::Result<AnthropicResponse> {
    let client = client.client()?;
    let model = model.to_string();
    let system_message = system_message.to_string();

    crate::runtime::block_on(async move {
        tool_calling(&client, messages, tools, model, system_message).await
    })
}

async fn tool_calling(
    client: &AnthropicClient,
    messages: Vec<AnthropicMessage>,
    tools: Vec<serde_json::Value>,
    model: String,
    system_message: String,
) -> anyhow::Result<AnthropicResponse> {
    let request = AnthropicRequest {
        model,
        max_tokens: client.max_tokens,
        messages,
        system: Some(system_message),
        tools: Some(tools),
        stream: None,
    };

    Ok(client.message(&request).await?)
}

fn tool_calling_stream_blocking(
    client: &AnthropicProvider,
    messages: Vec<AnthropicMessage>,
    tools: Vec<serde_json::Value>,
    model: &str,
    system_message: &str,
    on_token: impl Fn(&str) + Send + Sync + 'static,
) -> anyhow::Result<StreamToolCallResult> {
    let client = client.client()?;
    let model = model.to_string();
    let system_message = system_message.to_string();

    crate::runtime::block_on(async move {
        tool_calling_stream(&client, messages, tools, model, system_message, on_token).await
    })
}

async fn tool_calling_stream(
    client: &AnthropicClient,
    messages: Vec<AnthropicMessage>,
    tools: Vec<serde_json::Value>,
    model: String,
    system_message: String,
    on_token: impl Fn(&str) + Send + Sync + 'static,
) -> anyhow::Result<StreamToolCallResult> {
    let request = AnthropicRequest {
        model,
        max_tokens: client.max_tokens,
        messages,
        system: Some(system_message),
        tools: Some(tools),
        stream: Some(true),
    };

    let response = client.message_raw(&request).await?;

    let mut response_text = String::new();
    let mut tool_calls: HashMap<String, ToolCall> = HashMap::new();
    let mut current_tool_id: Option<String> = None;

    for_each_event(response, |event| {
        match event.event_type.as_str() {
            "content_block_start" => {
                if let Some(block) = event.content_block
                    && block.block_type == "tool_use"
                    && let (Some(id), Some(name)) = (block.id, block.name)
                {
                    current_tool_id = Some(id.clone());
                    tool_calls.insert(
                        id.clone(),
                        ToolCall {
                            id,
                            name,
                            arguments: String::new(),
                        },
                    );
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.delta {
                    // Handle text delta
                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                        response_text.push_str(text);
                        on_token(text);
                    }

                    // Handle tool input delta
                    if let Some(partial_json) = delta.get("partial_json").and_then(|t| t.as_str())
                        && let Some(tool_id) = &current_tool_id
                        && let Some(tool_call) = tool_calls.get_mut(tool_id)
                    {
                        tool_call.arguments.push_str(partial_json);
                    }
                }
            }
            "message_stop" => return std::ops::ControlFlow::Break(()),
            _ => {}
        }
        std::ops::ControlFlow::Continue(())
    })
    .await?;

    let text = (!response_text.is_empty()).then_some(response_text);
    if tool_calls.is_empty() {
        Ok((None, text))
    } else {
        Ok((Some(tool_calls.into_values().collect()), text))
    }
}

/// Helper function to handle text response from streaming
fn handle_text_response(
    text_response: String,
    text_response_buffer: &mut Vec<String>,
    messages: &mut Vec<AnthropicMessage>,
) {
    text_response_buffer.push(text_response.clone());
    messages.push(AnthropicMessage {
        role: "assistant".to_string(),
        content: serde_json::json!([{"type": "text", "text": text_response}]),
    });
}

fn convert_tool_to_anthropic_format(tool: &dyn crate::tool::Tool) -> serde_json::Value {
    serde_json::json!({
        "name": tool.name(),
        "description": tool.description(),
        "input_schema": tool.parameters()
    })
}

pub fn tool_calling_loop(
    provider: &AnthropicProvider,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    tool_set: &mut impl Toolset,
    model: &str,
) -> Result<String> {
    let mut messages: Vec<AnthropicMessage> = chat_messages
        .into_iter()
        .map(AnthropicMessage::from)
        .collect();

    let anthropic_tools: Vec<serde_json::Value> = tool_set
        .list()
        .iter()
        .map(|t| convert_tool_to_anthropic_format(t.deref()))
        .collect();

    let mut response = tool_calling_blocking(
        provider,
        messages.clone(),
        anthropic_tools.clone(),
        model,
        system_message,
    )?;

    let mut text_response_buffer = vec![];

    // Check for text content
    for block in &response.content {
        if block.block_type == "text"
            && let Some(text) = &block.text
        {
            handle_text_response(text.clone(), &mut text_response_buffer, &mut messages);
        }
    }

    // Loop while we have tool_use blocks
    while response.content.iter().any(|b| b.block_type == "tool_use") {
        let mut tool_call_blocks = vec![];
        let mut tool_response_messages = vec![];

        for block in &response.content {
            if block.block_type == "tool_use"
                && let (Some(id), Some(name), Some(input)) = (&block.id, &block.name, &block.input)
            {
                let arguments =
                    serde_json::to_string(input).context("Failed to serialize tool input")?;

                let tool_response = tool_set.call_tool(name, &arguments);
                let tool_response_str = serde_json::to_string(&tool_response)
                    .context("Failed to serialize tool response")?;

                tool_call_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input
                }));

                tool_response_messages.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": tool_response_str
                }));
            }
        }

        // Add assistant message with tool calls
        if !tool_call_blocks.is_empty() {
            messages.push(AnthropicMessage {
                role: "assistant".to_string(),
                content: serde_json::Value::Array(tool_call_blocks),
            });

            // Add user message with tool results
            messages.push(AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::Value::Array(tool_response_messages),
            });
        }

        response = tool_calling_blocking(
            provider,
            messages.clone(),
            anthropic_tools.clone(),
            model,
            system_message,
        )?;

        // Check for text content
        for block in &response.content {
            if block.block_type == "text"
                && let Some(text) = &block.text
            {
                handle_text_response(text.clone(), &mut text_response_buffer, &mut messages);
            }
        }
    }

    let response = text_response_buffer
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<String>>()
        .join("\n\n");

    Ok(response)
}

pub fn tool_calling_loop_stream(
    provider: &AnthropicProvider,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    tool_set: &mut impl Toolset,
    model: &str,
    on_token: impl Fn(&str) + Send + Sync + 'static,
) -> anyhow::Result<ConversationResult> {
    let mut messages: Vec<AnthropicMessage> = chat_messages
        .into_iter()
        .map(AnthropicMessage::from)
        .collect();

    let anthropic_tools: Vec<serde_json::Value> = tool_set
        .list()
        .iter()
        .map(|t| convert_tool_to_anthropic_format(t.deref()))
        .collect();

    let on_token = Arc::new(on_token);

    let mut response = tool_calling_stream_blocking(
        provider,
        messages.clone(),
        anthropic_tools.clone(),
        model,
        system_message,
        {
            let on_token = Arc::clone(&on_token);
            move |s: &str| on_token(s)
        },
    )?;

    let mut text_response_buffer = vec![];
    if let Some(text_response) = response.1 {
        handle_text_response(text_response, &mut text_response_buffer, &mut messages);
    }

    while let Some(tool_calls) = response.0 {
        let mut tool_call_blocks = vec![];
        let mut tool_response_messages = vec![];

        for call in tool_calls {
            let ToolCall {
                id,
                name: function_name,
                arguments: function_args,
            } = call;

            let tool_response = tool_set.call_tool(&function_name, &function_args);
            let tool_response_str = serde_json::to_string(&tool_response)
                .context("Failed to serialize tool response")?;

            let input: serde_json::Value =
                serde_json::from_str(&function_args).unwrap_or(serde_json::json!({}));

            tool_call_blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": function_name,
                "input": input
            }));

            tool_response_messages.push(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": tool_response_str
            }));
        }

        // Add assistant message with tool calls
        messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::Value::Array(tool_call_blocks),
        });

        // Add user message with tool results
        messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: serde_json::Value::Array(tool_response_messages),
        });

        response = tool_calling_stream_blocking(
            provider,
            messages.clone(),
            anthropic_tools.clone(),
            model,
            system_message,
            {
                let on_token = Arc::clone(&on_token);
                move |s: &str| on_token(s)
            },
        )?;

        if let Some(text_response) = response.1 {
            handle_text_response(text_response, &mut text_response_buffer, &mut messages);
        }
    }

    let chat_messages = from_anthropic_messages(messages);
    let final_response = text_response_buffer
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok(ConversationResult {
        final_response,
        message_history: chat_messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{respond, send, serve_once, start_event_stream};

    fn provider(addr: std::net::SocketAddr) -> AnthropicProvider {
        AnthropicProvider::with_api_base(
            Sensitive("secret-key".into()),
            None,
            &format!("http://{addr}/v1"),
        )
    }

    fn user(text: &str) -> Vec<ChatMessage> {
        vec![ChatMessage::User(text.to_owned())]
    }

    const HELLO: &str = "event: content_block_delta\n\
        data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n";

    const EMPTY_MESSAGE: &str = r#"{"id":"1","type":"message","role":"assistant","content":[],"stop_reason":null,"usage":{}}"#;

    #[tokio::test]
    async fn the_own_key_goes_in_x_api_key_and_the_limit_defaults_to_4096() {
        let (addr, server) = serve_once(|request, mut stream| async move {
            respond(
                &mut stream,
                "200 OK",
                "application/json",
                r#"{"id":"1","type":"message","role":"assistant","content":[{"type":"text","text":"pong"}],"stop_reason":"end_turn","usage":{}}"#,
            )
            .await;
            request
        })
        .await;
        let client = provider(addr).client().expect("client");
        let answer = text(
            &client,
            "sys",
            user("ping"),
            "m",
            &GenerateOptions::default(),
        )
        .await
        .expect("the request succeeds");
        assert_eq!(answer.as_deref(), Some("pong"));

        let request = server.await.expect("server task");
        let head = request.head.to_ascii_lowercase();
        assert!(
            request.head.starts_with("POST /v1/messages "),
            "messages path: {}",
            request.head
        );
        assert!(
            head.contains("x-api-key: secret-key"),
            "Anthropic takes the key as x-api-key: {head}"
        );
        assert!(head.contains("anthropic-version: 2023-06-01"));
        let body: serde_json::Value = serde_json::from_str(&request.body).expect("JSON body");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS, "the default limit");
        assert_eq!(body["system"], "sys");
    }

    #[tokio::test]
    async fn a_request_can_set_its_own_limit() {
        let (addr, server) = serve_once(|request, mut stream| async move {
            respond(&mut stream, "200 OK", "application/json", EMPTY_MESSAGE).await;
            request
        })
        .await;
        let client = provider(addr)
            .with_max_tokens(100)
            .client()
            .expect("client");
        let answer = text(
            &client,
            "s",
            user("u"),
            "m",
            &GenerateOptions {
                max_tokens: Some(16),
            },
        )
        .await
        .expect("success");
        assert_eq!(answer, None, "no text block is no answer");
        let body: serde_json::Value =
            serde_json::from_str(&server.await.expect("server task").body).expect("JSON body");
        assert_eq!(
            body["max_tokens"], 16,
            "the request's limit wins over the provider's"
        );
    }

    #[tokio::test]
    async fn an_error_event_in_the_stream_fails_the_request() {
        let (addr, _server) = serve_once(|_request, mut stream| async move {
            start_event_stream(&mut stream).await;
            send(&mut stream, HELLO).await;
            send(
                &mut stream,
                "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
            )
            .await;
        })
        .await;
        let client = provider(addr).client().expect("client");
        let mut seen = String::new();
        let err = stream_text(
            &client,
            "s",
            user("u"),
            "m",
            &GenerateOptions::default(),
            &mut |t| seen.push_str(t),
        )
        .await
        .expect_err("a mid-stream overload must not look like a short answer");
        assert_eq!(
            err.to_string(),
            "overloaded_error: Overloaded",
            "the provider's error is reported"
        );
        assert_eq!(seen, "Hel", "text before the error was delivered");
    }

    #[tokio::test]
    async fn a_stream_that_ends_before_message_stop_is_an_error() {
        let (addr, _server) = serve_once(|_request, mut stream| async move {
            start_event_stream(&mut stream).await;
            send(&mut stream, HELLO).await;
        })
        .await;
        let client = provider(addr).client().expect("client");
        let err = stream_text(
            &client,
            "s",
            user("u"),
            "m",
            &GenerateOptions::default(),
            &mut |_| {},
        )
        .await
        .expect_err("a cut-off answer is not a complete one");
        assert!(matches!(err, LlmError::Network(_)), "{err}");
    }

    #[tokio::test]
    async fn a_complete_stream_returns_the_text() {
        let (addr, _server) = serve_once(|_request, mut stream| async move {
            start_event_stream(&mut stream).await;
            send(&mut stream, HELLO).await;
            send(
                &mut stream,
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            )
            .await;
        })
        .await;
        let client = provider(addr).client().expect("client");
        let answer = stream_text(
            &client,
            "s",
            user("u"),
            "m",
            &GenerateOptions::default(),
            &mut |_| {},
        )
        .await
        .expect("a complete stream succeeds");
        assert_eq!(answer.as_deref(), Some("Hello"));
    }
}
