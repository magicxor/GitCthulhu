//! Classified failures of text requests.

use std::time::Duration;

/// Why a text request failed, in the classes callers act on differently.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The provider rejected the credentials (HTTP 401/403 or an authentication error event).
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The provider rate-limited the request (HTTP 429 or a rate-limit error event).
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// The request did not reach the provider, the connection broke, or the provider stopped answering.
    #[error("network error: {0}")]
    Network(String),
    /// The provider answered with an error, or with something that is not an answer.
    #[error("{0}")]
    Provider(String),
}

impl LlmError {
    /// Classify an HTTP error response by status, taking the message from its JSON body when it has one.
    pub(crate) fn from_status(status: reqwest::StatusCode, body: &str) -> Self {
        let message = error_message(body);
        match status.as_u16() {
            401 | 403 => LlmError::Auth(message),
            429 => LlmError::RateLimited(message),
            code => LlmError::Provider(format!("HTTP {code}: {message}")),
        }
    }

    /// Classify an error event the provider sent in the middle of an otherwise successful stream.
    ///
    /// `kind` is the provider's error type (Anthropic: `overloaded_error`, `rate_limit_error`, ...).
    pub(crate) fn from_event(kind: &str, message: String) -> Self {
        let kind = kind.to_ascii_lowercase();
        if kind.contains("authentication") || kind.contains("permission") {
            LlmError::Auth(message)
        } else if kind.contains("rate_limit") {
            LlmError::RateLimited(message)
        } else if kind.is_empty() {
            LlmError::Provider(message)
        } else {
            LlmError::Provider(format!("{kind}: {message}"))
        }
    }

    /// A stream that ended without the provider saying that the answer was complete (a dropped connection).
    pub(crate) fn incomplete() -> Self {
        LlmError::Network("the stream ended before the answer was complete".to_owned())
    }

    pub(crate) fn timed_out(what: &str, after: Duration) -> Self {
        LlmError::Network(format!(
            "timed out after {} s waiting for {what}",
            after.as_secs()
        ))
    }
}

impl From<reqwest::Error> for LlmError {
    fn from(err: reqwest::Error) -> Self {
        // Walk to the root cause: reqwest's own text is just "error sending request for url (...)".
        let mut text = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            text.push_str(": ");
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        LlmError::Network(text)
    }
}

/// The human-readable reason in an error response body: OpenAI's `{"error": {"message": ..}}`, Anthropic's
/// `{"type": "error", "error": {"message": ..}}`, a proxy's `{"error": ".."}`, or the (clipped) text itself.
pub(crate) fn error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let found = value
            .pointer("/error/message")
            .or_else(|| value.get("error"))
            .or_else(|| value.get("message"))
            .or_else(|| value.get("detail"))
            .and_then(|v| v.as_str());
        if let Some(message) = found {
            return message.to_owned();
        }
    }
    let body = body.trim();
    if body.is_empty() {
        return "the provider sent no explanation".to_owned();
    }
    match body.char_indices().nth(300) {
        Some((at, _)) => format!("{}...", &body[..at]),
        None => body.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::*;

    #[test]
    fn statuses_map_to_classes_and_messages_come_from_the_json_body() {
        let openai =
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#;
        assert!(
            matches!(
                LlmError::from_status(StatusCode::UNAUTHORIZED, openai),
                LlmError::Auth(m) if m == "Incorrect API key provided"
            ),
            "401 is an authentication failure carrying OpenAI's message"
        );
        assert!(
            matches!(
                LlmError::from_status(StatusCode::TOO_MANY_REQUESTS, "{}"),
                LlmError::RateLimited(_)
            ),
            "429 is rate limiting"
        );
        let anthropic =
            r#"{"type":"error","error":{"type":"not_found_error","message":"model: nope"}}"#;
        assert_eq!(
            LlmError::from_status(StatusCode::NOT_FOUND, anthropic).to_string(),
            "HTTP 404: model: nope",
            "other statuses are provider errors with the status in the text"
        );
        assert_eq!(
            error_message(r#"{"error":"401 Unauthorized"}"#),
            "401 Unauthorized",
            "a proxy's flat error string is readable too"
        );
        assert_eq!(
            error_message("<html>bad gateway</html>"),
            "<html>bad gateway</html>",
            "a non-JSON body is shown as is"
        );
    }

    #[test]
    fn error_events_are_classified_by_their_type() {
        assert!(matches!(
            LlmError::from_event("rate_limit_error", "slow down".into()),
            LlmError::RateLimited(_)
        ));
        assert!(matches!(
            LlmError::from_event("authentication_error", "bad key".into()),
            LlmError::Auth(_)
        ));
        assert_eq!(
            LlmError::from_event("overloaded_error", "Overloaded".into()).to_string(),
            "overloaded_error: Overloaded",
            "an overload is a provider error that names its type"
        );
    }
}
