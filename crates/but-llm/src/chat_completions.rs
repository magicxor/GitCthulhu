//! Plain-text requests over the OpenAI chat-completions wire (OpenAI, Ollama, LM Studio, OpenRouter).
//!
//! These talk HTTP directly instead of going through `async-openai`'s streaming: dropping the future returned
//! here drops the response body and so closes the connection at once, which is what cancellation relies on.
//! (`async-openai` reads the stream in a spawned task that only notices a dropped receiver on the next event.)

use async_openai::{
    config::Config as _,
    types::chat::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        CreateChatCompletionRequestArgs,
    },
};
use futures::StreamExt as _;
use tokio::time::timeout;

use crate::{
    ChatMessage, GenerateOptions,
    error::{LlmError, error_message},
    http::{RESPONSE_START_TIMEOUT, STREAM_IDLE_TIMEOUT, TOTAL_TIMEOUT},
    openai_utils::OpenAIClientProvider,
    sse::SseParser,
};

/// Ask for the whole answer at once. `Ok(None)` if the model said nothing.
pub(crate) async fn complete(
    provider: &impl OpenAIClientProvider,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
    options: &GenerateOptions,
) -> Result<Option<String>, LlmError> {
    let request = request(
        provider,
        system_message,
        chat_messages,
        model,
        options,
        false,
    )?;
    // A local model's longer start (loading, prompt evaluation) comes on top of the generation time.
    let total = TOTAL_TIMEOUT
        + provider
            .response_start_timeout()
            .saturating_sub(RESPONSE_START_TIMEOUT);
    let response = request.timeout(total).send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(LlmError::from_status(status, &body));
    }
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|err| {
        LlmError::Provider(format!(
            "the answer is not JSON ({err}): {}",
            error_message(&body)
        ))
    })?;
    if let Some(err) = error_in(&value) {
        return Err(err);
    }
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(|content| content.as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    Ok(text)
}

/// Ask for the answer as a stream, calling `on_token` with each piece as it arrives. Returns the whole answer
/// (`None` if the model said nothing). Dropping the returned future closes the connection.
///
/// A stream that ends without `[DONE]` and without a finish reason is an error, not a short answer.
pub(crate) async fn stream(
    provider: &impl OpenAIClientProvider,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
    options: &GenerateOptions,
    on_token: &mut (dyn FnMut(&str) + Send),
) -> Result<Option<String>, LlmError> {
    let request = request(
        provider,
        system_message,
        chat_messages,
        model,
        options,
        true,
    )?;
    let start_timeout = provider.response_start_timeout();
    let response = timeout(start_timeout, request.send())
        .await
        .map_err(|_| LlmError::timed_out("the response", start_timeout))??;
    let status = response.status();
    if !status.is_success() {
        let body = timeout(start_timeout, response.text())
            .await
            .map_err(|_| LlmError::timed_out("the error description", start_timeout))??;
        return Err(LlmError::from_status(status, &body));
    }

    let mut chunks = response.bytes_stream();
    let mut parser = SseParser::default();
    let mut answer: Option<String> = None;
    // Whether the server said the answer is complete by naming a finish reason (some omit the `[DONE]` line).
    let mut finished = false;
    loop {
        let (events, last) = match timeout(STREAM_IDLE_TIMEOUT, chunks.next()).await {
            Err(_) => return Err(LlmError::timed_out("the next token", STREAM_IDLE_TIMEOUT)),
            Ok(None) => (parser.finish().into_iter().collect::<Vec<_>>(), true),
            Ok(Some(chunk)) => (parser.push(&chunk?), false),
        };
        for event in events {
            if event.data == "[DONE]" {
                return Ok(answer);
            }
            let value: serde_json::Value = match serde_json::from_str(&event.data) {
                Ok(value) => value,
                // Servers send keep-alives and vendor extensions that are not chunks; skip what we cannot read.
                Err(_) => continue,
            };
            if let Some(err) = error_in(&value) {
                return Err(err);
            }
            let token = value
                .pointer("/choices/0/delta/content")
                .and_then(|content| content.as_str())
                .filter(|token| !token.is_empty());
            if let Some(token) = token {
                answer.get_or_insert_with(String::new).push_str(token);
                on_token(token);
            }
            if value
                .pointer("/choices/0/finish_reason")
                .is_some_and(|reason| !reason.is_null())
            {
                finished = true;
            }
        }
        if last {
            // A connection that dropped mid-answer must not look like a finished (truncated) message.
            return if finished {
                Ok(answer)
            } else {
                Err(LlmError::incomplete())
            };
        }
    }
}

/// An error object some servers send as a chunk or as a 200 response body (`{"error": {...}}`).
fn error_in(value: &serde_json::Value) -> Option<LlmError> {
    let error = value.get("error").filter(|error| !error.is_null())?;
    let message = error
        .get("message")
        .and_then(|m| m.as_str())
        .or_else(|| error.as_str())
        .unwrap_or("the provider reported an error")
        .to_owned();
    let kind = error
        .get("type")
        .and_then(|t| t.as_str())
        .or_else(|| error.get("code").and_then(|c| c.as_str()))
        .unwrap_or_default();
    Some(LlmError::from_event(kind, message))
}

fn request(
    provider: &impl OpenAIClientProvider,
    system_message: &str,
    chat_messages: Vec<ChatMessage>,
    model: &str,
    options: &GenerateOptions,
    stream: bool,
) -> Result<reqwest::RequestBuilder, LlmError> {
    let invalid =
        |err: &dyn std::fmt::Display| LlmError::Provider(format!("invalid request: {err}"));
    let mut messages: Vec<ChatCompletionRequestMessage> =
        vec![ChatCompletionRequestSystemMessage::from(system_message).into()];
    messages.extend(
        chat_messages
            .into_iter()
            .map(ChatCompletionRequestMessage::from),
    );

    let mut args = CreateChatCompletionRequestArgs::default();
    args.model(model).messages(messages);
    if stream {
        args.stream(true);
    }
    if let Some(limit) = options.max_tokens {
        if provider.uses_max_completion_tokens() {
            args.max_completion_tokens(limit);
        } else {
            #[allow(deprecated)]
            // Every OpenAI-compatible server understands it; OpenAI itself gets the new name.
            args.max_tokens(limit);
        }
    }
    let body = args.build().map_err(|err| invalid(&err))?;

    let client = provider
        .client()
        .map_err(|err| LlmError::Provider(format!("cannot set up the provider: {err:#}")))?;
    let config = client.config();
    let url = format!(
        "{}/chat/completions",
        config.api_base().trim_end_matches('/')
    );
    Ok(crate::http::client()?
        .post(url)
        .headers(config.headers())
        .json(&body))
}

#[cfg(test)]
mod tests {
    use async_openai::config::OPENAI_API_BASE;
    use but_secret::Sensitive;

    use super::*;
    use crate::{
        lmstudio::{LMStudioConfig, LMStudioProvider},
        openai::OpenAiProvider,
        test_server::{respond, send, serve_once, start_event_stream},
    };

    fn lmstudio(addr: std::net::SocketAddr) -> LMStudioProvider {
        LMStudioProvider::with(
            Some(LMStudioConfig {
                api_base: format!("http://{addr}/v1"),
            }),
            None,
        )
    }

    fn user(text: &str) -> Vec<ChatMessage> {
        vec![ChatMessage::User(text.to_owned())]
    }

    #[tokio::test]
    async fn streamed_tokens_arrive_in_order_and_the_request_names_model_and_limit() {
        let (addr, server) = serve_once(|request, mut stream| async move {
            start_event_stream(&mut stream).await;
            send(
                &mut stream,
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                 data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
                 : keep-alive\n\n\
                 data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
                 data: [DONE]\n\n",
            )
            .await;
            request
        })
        .await;

        let mut tokens = Vec::new();
        let answer = stream(
            &lmstudio(addr),
            "be brief",
            user("hi"),
            "m1",
            &GenerateOptions {
                max_tokens: Some(16),
            },
            &mut |t| tokens.push(t.to_owned()),
        )
        .await
        .expect("the stream succeeds");

        assert_eq!(
            answer.as_deref(),
            Some("Hello"),
            "the answer is the concatenated tokens"
        );
        assert_eq!(
            tokens,
            ["Hel", "lo"],
            "each delta is reported once, in order"
        );
        let request = server.await.expect("server task");
        assert!(
            request.head.starts_with("POST /v1/chat/completions "),
            "the request goes to the chat-completions path: {}",
            request.head
        );
        let body: serde_json::Value = serde_json::from_str(&request.body).expect("a JSON body");
        assert_eq!(body["model"], "m1");
        assert_eq!(body["stream"], true, "streaming is requested");
        assert_eq!(body["max_tokens"], 16, "compatible servers get max_tokens");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be brief");
        assert_eq!(body["messages"][1]["content"], "hi");
    }

    #[test]
    fn openai_itself_gets_max_completion_tokens_and_its_own_base() {
        let custom = OpenAiProvider::with(
            Sensitive("k".into()),
            None,
            Some("http://127.0.0.1:1".into()),
        );
        assert!(
            !custom.uses_max_completion_tokens(),
            "a custom endpoint keeps the universal max_tokens"
        );
        let own = OpenAiProvider::with(Sensitive("k".into()), None, None);
        assert!(
            own.uses_max_completion_tokens(),
            "OpenAI's models want the new name"
        );
        let client = own.client().expect("client");
        assert_eq!(
            async_openai::config::Config::api_base(client.config()),
            OPENAI_API_BASE,
            "the base is OpenAI's regardless of OPENAI_BASE_URL"
        );
    }

    #[tokio::test]
    async fn http_statuses_are_classified() {
        for (status, expect) in [
            ("401 Unauthorized", "auth"),
            ("429 Too Many Requests", "rate"),
            ("500 Internal Server Error", "provider"),
        ] {
            let (addr, _server) = serve_once(move |_request, mut stream| async move {
                respond(
                    &mut stream,
                    status,
                    "application/json",
                    r#"{"error":{"message":"nope","type":"x"}}"#,
                )
                .await;
            })
            .await;
            let err = complete(
                &lmstudio(addr),
                "s",
                user("u"),
                "m",
                &GenerateOptions::default(),
            )
            .await
            .expect_err("an error status fails the request");
            let class = match err {
                LlmError::Auth(_) => "auth",
                LlmError::RateLimited(_) => "rate",
                LlmError::Provider(_) => "provider",
                LlmError::Network(_) => "network",
            };
            assert_eq!(class, expect, "{status} is classified as {expect}");
        }
    }

    #[tokio::test]
    async fn an_error_chunk_inside_the_stream_fails_the_request() {
        let (addr, _server) = serve_once(|_request, mut stream| async move {
            start_event_stream(&mut stream).await;
            send(
                &mut stream,
                "data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n\
                 data: {\"error\":{\"message\":\"upstream overloaded\",\"code\":\"overloaded\"}}\n\n",
            )
            .await;
        })
        .await;
        let mut seen = String::new();
        let err = stream(
            &lmstudio(addr),
            "s",
            user("u"),
            "m",
            &GenerateOptions::default(),
            &mut |t| seen.push_str(t),
        )
        .await
        .expect_err("the error chunk ends the request");
        assert!(
            err.to_string().contains("upstream overloaded"),
            "the provider's reason is kept: {err}"
        );
        assert_eq!(seen, "par", "text before the error was delivered");
    }

    #[tokio::test]
    async fn a_stream_cut_off_before_done_or_a_finish_reason_is_an_error() {
        let (addr, _server) = serve_once(|_request, mut stream| async move {
            start_event_stream(&mut stream).await;
            send(
                &mut stream,
                "data: {\"choices\":[{\"delta\":{\"content\":\"Fix the par\"}}]}\n\n",
            )
            .await;
        })
        .await;
        let mut seen = String::new();
        let err = stream(
            &lmstudio(addr),
            "s",
            user("u"),
            "m",
            &GenerateOptions::default(),
            &mut |t| seen.push_str(t),
        )
        .await
        .expect_err("a dropped connection is not a finished message");
        assert!(matches!(err, LlmError::Network(_)), "{err}");
        assert_eq!(seen, "Fix the par", "the text that arrived was delivered");
    }

    #[tokio::test]
    async fn a_stream_that_names_a_finish_reason_is_complete_without_done() {
        let (addr, _server) = serve_once(|_request, mut stream| async move {
            start_event_stream(&mut stream).await;
            send(
                &mut stream,
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n\
                 data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            )
            .await;
        })
        .await;
        let answer = stream(
            &lmstudio(addr),
            "s",
            user("u"),
            "m",
            &GenerateOptions::default(),
            &mut |_| {},
        )
        .await
        .expect("a finish reason completes the answer");
        assert_eq!(answer.as_deref(), Some("ok"));
    }

    #[test]
    fn local_models_get_a_longer_response_start_timeout_than_hosted_providers() {
        use crate::ollama::{OllamaConfig, OllamaProvider};
        let hosted = OpenAiProvider::with(Sensitive("k".into()), None, None);
        assert_eq!(hosted.response_start_timeout().as_secs(), 60);
        let lm = lmstudio("127.0.0.1:1".parse().expect("an address"));
        assert_eq!(lm.response_start_timeout().as_secs(), 180);
        let ollama = OllamaProvider::new(OllamaConfig::default(), None);
        assert_eq!(ollama.response_start_timeout().as_secs(), 180);
    }

    #[tokio::test]
    async fn a_complete_answer_and_an_empty_one() {
        let (addr, _server) = serve_once(|_request, mut stream| async move {
            respond(
                &mut stream,
                "200 OK",
                "application/json",
                r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}]}"#,
            )
            .await;
        })
        .await;
        let answer = complete(
            &lmstudio(addr),
            "s",
            user("ping"),
            "m",
            &GenerateOptions::default(),
        )
        .await
        .expect("the request succeeds");
        assert_eq!(answer.as_deref(), Some("pong"));

        let (addr, _server) = serve_once(|_request, mut stream| async move {
            respond(
                &mut stream,
                "200 OK",
                "application/json",
                r#"{"choices":[{"message":{"role":"assistant","content":""},"finish_reason":"length"}]}"#,
            )
            .await;
        })
        .await;
        let answer = complete(
            &lmstudio(addr),
            "s",
            user("ping"),
            "m",
            &GenerateOptions::default(),
        )
        .await
        .expect("an empty answer is still a successful request");
        assert_eq!(answer, None, "empty content is reported as no answer");
    }
}
