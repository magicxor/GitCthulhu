//! The HTTP client and the timeouts shared by all text requests.

use std::{sync::OnceLock, time::Duration};

use crate::error::LlmError;

/// How long connecting and waiting for the response head may take.
pub(crate) const RESPONSE_START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a stream may stay silent between two chunks (a local model may load before its first token).
pub(crate) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a whole non-streaming request may take.
pub(crate) const TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();

/// The process-wide HTTP client. Connections are not kept alive between requests: a request is rare and a pooled
/// connection would belong to whichever runtime opened it.
pub(crate) fn client() -> Result<reqwest::Client, LlmError> {
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(RESPONSE_START_TIMEOUT)
                .pool_max_idle_per_host(0)
                .build()
                .map_err(|err| err.to_string())
        })
        .clone()
        .map_err(|err| LlmError::Provider(format!("cannot create the HTTP client: {err}")))
}
