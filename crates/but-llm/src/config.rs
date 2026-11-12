//! The AI settings of the application, independent of where they are stored.
//!
//! An [`AiConfiguration`] is plain data (serde) that the application keeps in its own settings file. It holds no
//! secrets: API keys live in the secret store, under the handles named by [`AiConfiguration::secret_handle`].

use anyhow::{Context as _, Result, bail};

use crate::LLMProviderKind;

pub const AI_OPENAI_SECRET_HANDLE: &str = "aiOpenAIKey";
pub const AI_ANTHROPIC_SECRET_HANDLE: &str = "aiAnthropicKey";
pub const AI_OPENROUTER_SECRET_HANDLE: &str = "aiOpenRouterKey";

pub const DEFAULT_OPENAI_MODEL: &str = "gpt-5.4-nano";
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-4-5";
pub const DEFAULT_OLLAMA_ENDPOINT: &str = "127.0.0.1:11434";
pub const DEFAULT_OLLAMA_MODEL: &str = "llama3";
pub const DEFAULT_LMSTUDIO_ENDPOINT: &str = "http://127.0.0.1:1234/v1";
pub const DEFAULT_LMSTUDIO_MODEL: &str = "default";
pub const DEFAULT_OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_OPENROUTER_MODEL: &str = "openai/gpt-4.1-mini";

/// What is missing for the active provider to be usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiMissing {
    /// No provider selected.
    Provider,
    /// No API key.
    Key,
    /// No model.
    Model,
    /// No endpoint, or one that is not valid for the provider.
    Endpoint,
}

/// Which API keys are available, as the secret store reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyAvailability {
    pub openai: bool,
    pub anthropic: bool,
    pub openrouter: bool,
}

/// OpenAI, or any OpenAI-compatible server behind `custom_endpoint`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OpenAiConfiguration {
    pub model: String,
    pub custom_endpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AnthropicConfiguration {
    pub model: String,
}

/// A local Ollama server, addressed as `host:port` (an IPv6 host in brackets).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OllamaConfiguration {
    pub endpoint: String,
    pub model: String,
}

/// A local LM Studio server, addressed by its base URL.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LmStudioConfiguration {
    pub endpoint: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OpenRouterConfiguration {
    pub endpoint: String,
    pub model: String,
}

/// The settings of every provider plus the choice among them. `provider: None` means AI is not set up.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AiConfiguration {
    /// A provider name this version does not know (written by a newer one) reads as `None`, so the rest of the
    /// settings survive.
    #[serde(deserialize_with = "known_provider_or_none")]
    pub provider: Option<LLMProviderKind>,
    pub openai: OpenAiConfiguration,
    pub anthropic: AnthropicConfiguration,
    pub ollama: OllamaConfiguration,
    pub lmstudio: LmStudioConfiguration,
    pub openrouter: OpenRouterConfiguration,
}

impl Default for OpenAiConfiguration {
    fn default() -> Self {
        Self {
            model: DEFAULT_OPENAI_MODEL.into(),
            custom_endpoint: None,
        }
    }
}

impl Default for AnthropicConfiguration {
    fn default() -> Self {
        Self {
            model: DEFAULT_ANTHROPIC_MODEL.into(),
        }
    }
}

impl Default for OllamaConfiguration {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_OLLAMA_ENDPOINT.into(),
            model: DEFAULT_OLLAMA_MODEL.into(),
        }
    }
}

impl Default for LmStudioConfiguration {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_LMSTUDIO_ENDPOINT.into(),
            model: DEFAULT_LMSTUDIO_MODEL.into(),
        }
    }
}

impl Default for OpenRouterConfiguration {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_OPENROUTER_ENDPOINT.into(),
            model: DEFAULT_OPENROUTER_MODEL.into(),
        }
    }
}

impl AiConfiguration {
    /// The secret-store handle of the API key of `provider`, for the providers that need one.
    pub fn secret_handle(provider: LLMProviderKind) -> Option<&'static str> {
        match provider {
            LLMProviderKind::OpenAi => Some(AI_OPENAI_SECRET_HANDLE),
            LLMProviderKind::Anthropic => Some(AI_ANTHROPIC_SECRET_HANDLE),
            LLMProviderKind::OpenRouter => Some(AI_OPENROUTER_SECRET_HANDLE),
            LLMProviderKind::Ollama | LLMProviderKind::LMStudio => None,
        }
    }

    /// The model of the active provider, as configured (possibly blank).
    pub fn active_model(&self) -> Option<&str> {
        Some(match self.provider? {
            LLMProviderKind::OpenAi => &self.openai.model,
            LLMProviderKind::Anthropic => &self.anthropic.model,
            LLMProviderKind::Ollama => &self.ollama.model,
            LLMProviderKind::LMStudio => &self.lmstudio.model,
            LLMProviderKind::OpenRouter => &self.openrouter.model,
        })
    }

    /// Check every provider's settings, as when the settings page saves them.
    pub fn validate(&self) -> Result<()> {
        required(&self.openai.model, "OpenAI model")?;
        required(&self.anthropic.model, "Anthropic model")?;
        required(&self.ollama.model, "Ollama model")?;
        required(&self.lmstudio.model, "LM Studio model")?;
        required(&self.openrouter.model, "OpenRouter model")?;
        validate_ollama_endpoint(&self.ollama.endpoint)?;
        validate_url(&self.lmstudio.endpoint, "LM Studio endpoint")?;
        validate_url(&self.openrouter.endpoint, "OpenRouter endpoint")?;
        if let Some(endpoint) = nonempty(self.openai.custom_endpoint.as_deref()) {
            validate_url(endpoint, "OpenAI custom endpoint")?;
        }
        Ok(())
    }

    /// Check the settings of the active provider; fails with the reason if there is none.
    pub fn validate_active(&self) -> Result<()> {
        let Some(provider) = self.provider else {
            bail!("No AI provider is selected")
        };
        match provider {
            LLMProviderKind::OpenAi => {
                required(&self.openai.model, "OpenAI model")?;
                if let Some(endpoint) = nonempty(self.openai.custom_endpoint.as_deref()) {
                    validate_url(endpoint, "OpenAI custom endpoint")?;
                }
            }
            LLMProviderKind::Anthropic => {
                required(&self.anthropic.model, "Anthropic model")?;
            }
            LLMProviderKind::Ollama => {
                required(&self.ollama.model, "Ollama model")?;
                validate_ollama_endpoint(&self.ollama.endpoint)?;
            }
            LLMProviderKind::LMStudio => {
                required(&self.lmstudio.model, "LM Studio model")?;
                validate_url(&self.lmstudio.endpoint, "LM Studio endpoint")?;
            }
            LLMProviderKind::OpenRouter => {
                required(&self.openrouter.model, "OpenRouter model")?;
                validate_url(&self.openrouter.endpoint, "OpenRouter endpoint")?;
            }
        }
        Ok(())
    }

    /// What the active provider still lacks, or `None` if it is ready to be used.
    pub fn missing(&self, keys: KeyAvailability) -> Option<AiMissing> {
        let Some(provider) = self.provider else {
            return Some(AiMissing::Provider);
        };
        let (model, endpoint_ok, key_ok) = match provider {
            LLMProviderKind::OpenAi => (
                &self.openai.model,
                nonempty(self.openai.custom_endpoint.as_deref())
                    .is_none_or(|e| validate_url(e, "").is_ok()),
                keys.openai,
            ),
            LLMProviderKind::Anthropic => (&self.anthropic.model, true, keys.anthropic),
            LLMProviderKind::Ollama => (
                &self.ollama.model,
                validate_ollama_endpoint(&self.ollama.endpoint).is_ok(),
                true,
            ),
            LLMProviderKind::LMStudio => (
                &self.lmstudio.model,
                validate_url(&self.lmstudio.endpoint, "").is_ok(),
                true,
            ),
            LLMProviderKind::OpenRouter => (
                &self.openrouter.model,
                validate_url(&self.openrouter.endpoint, "").is_ok(),
                keys.openrouter,
            ),
        };
        if model.trim().is_empty() {
            Some(AiMissing::Model)
        } else if !endpoint_ok {
            Some(AiMissing::Endpoint)
        } else if !key_ok {
            Some(AiMissing::Key)
        } else {
            None
        }
    }

    /// Whether the active provider has valid settings and its required key.
    pub fn is_configured(&self, keys: KeyAvailability) -> bool {
        self.missing(keys).is_none()
    }
}

fn known_provider_or_none<'de, D>(deserializer: D) -> Result<Option<LLMProviderKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let name: Option<String> = serde::Deserialize::deserialize(deserializer)?;
    Ok(name.as_deref().and_then(LLMProviderKind::from_config_value))
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn required<'a>(value: &'a str, field: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} is required")
    }
    Ok(value)
}

fn validate_url(value: &str, field: &str) -> Result<()> {
    let value = required(value, field)?;
    let url = url::Url::parse(value).with_context(|| format!("{field} must be a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("{field} must be an HTTP or HTTPS URL")
    }
    Ok(())
}

fn validate_ollama_endpoint(value: &str) -> Result<()> {
    let value = required(value, "Ollama endpoint")?;
    if value.contains("://") {
        bail!("Ollama endpoint must use host:port format")
    }
    let (host, port) = value
        .rsplit_once(':')
        .context("Ollama endpoint must use host:port format")?;
    let host = host.trim();
    // An IPv6 host needs brackets, or the URL built from it (`http://{host}:{port}`) is invalid.
    let ipv6 = host.starts_with('[') || host.contains(':');
    let bracketed = host.len() > 2 && host.starts_with('[') && host.ends_with(']');
    if host.is_empty() || (ipv6 && !bracketed) || port.parse::<u16>().is_err() {
        bail!("Ollama endpoint must use host:port format (an IPv6 host in brackets)")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_json_and_missing_fields_take_defaults() {
        let mut configuration = AiConfiguration {
            provider: Some(LLMProviderKind::OpenRouter),
            ..AiConfiguration::default()
        };
        configuration.openrouter.model = "anthropic/claude-sonnet".into();
        configuration.openai.custom_endpoint = Some("http://localhost:8000/v1".into());

        let json = serde_json::to_string(&configuration).expect("plain data serializes");
        assert_eq!(
            serde_json::from_str::<AiConfiguration>(&json).expect("what was written reads back"),
            configuration,
            "the settings file keeps every field"
        );
        assert_eq!(
            serde_json::from_str::<AiConfiguration>(r#"{"provider":"lmstudio"}"#)
                .expect("a partial file reads")
                .lmstudio,
            LmStudioConfiguration::default(),
            "fields absent from an older settings file take their defaults"
        );
        assert_eq!(
            serde_json::from_str::<AiConfiguration>("{}")
                .expect("an empty object reads")
                .provider,
            None,
            "a fresh install has no provider"
        );
    }

    #[test]
    fn an_unknown_provider_reads_as_none_and_keeps_the_other_settings() {
        let read: AiConfiguration = serde_json::from_str(
            r#"{"provider":"gemini","ollama":{"endpoint":"[::1]:11434","model":"qwen3"}}"#,
        )
        .expect("a provider written by a newer version does not break the whole file");
        assert_eq!(read.provider, None, "the unknown provider is not selected");
        assert_eq!(
            read.ollama.model, "qwen3",
            "the settings of known providers survive"
        );
        assert_eq!(
            serde_json::from_str::<AiConfiguration>(r#"{"provider":null}"#)
                .expect("null reads")
                .provider,
            None
        );
        for kind in [
            LLMProviderKind::OpenAi,
            LLMProviderKind::Anthropic,
            LLMProviderKind::Ollama,
            LLMProviderKind::LMStudio,
            LLMProviderKind::OpenRouter,
        ] {
            let configuration = AiConfiguration {
                provider: Some(kind),
                ..AiConfiguration::default()
            };
            let json = serde_json::to_string(&configuration).expect("serializes");
            assert_eq!(
                serde_json::from_str::<AiConfiguration>(&json)
                    .expect("reads back")
                    .provider,
                Some(kind),
                "{kind:?} round-trips through {json}"
            );
        }
    }

    #[test]
    fn custom_openai_endpoint_must_be_a_url_and_blank_means_openai() {
        let mut configuration = AiConfiguration::default();
        configuration.openai.custom_endpoint = Some("not a URL".into());
        assert!(
            configuration.validate().is_err(),
            "an endpoint that is no URL is rejected"
        );
        configuration.openai.custom_endpoint = Some("  ".into());
        assert!(
            configuration.validate().is_ok(),
            "a blank endpoint means OpenAI's own"
        );
    }

    #[test]
    fn configured_requires_the_active_providers_key_and_settings() {
        let none = KeyAvailability::default();
        let mut config = AiConfiguration::default();
        assert_eq!(
            config.missing(none),
            Some(AiMissing::Provider),
            "nothing is selected"
        );

        config.provider = Some(LLMProviderKind::OpenAi);
        assert_eq!(
            config.missing(none),
            Some(AiMissing::Key),
            "OpenAI needs its key"
        );
        assert!(
            config.is_configured(KeyAvailability {
                openai: true,
                ..none
            }),
            "OpenAI needs only its own key"
        );

        config.provider = Some(LLMProviderKind::Anthropic);
        assert!(
            config.is_configured(KeyAvailability {
                anthropic: true,
                ..none
            }),
            "Anthropic needs its own key"
        );

        config.provider = Some(LLMProviderKind::OpenRouter);
        assert!(
            !config.is_configured(none),
            "OpenRouter without a key is not configured"
        );
        assert!(
            config.is_configured(KeyAvailability {
                openrouter: true,
                ..none
            }),
            "an OpenRouter key configures OpenRouter"
        );

        config.provider = Some(LLMProviderKind::Ollama);
        assert!(
            config.is_configured(none),
            "a valid local provider needs no key"
        );
        config.ollama.endpoint.clear();
        assert_eq!(
            config.missing(none),
            Some(AiMissing::Endpoint),
            "an invalid local endpoint is reported as such"
        );
        config.ollama.endpoint = DEFAULT_OLLAMA_ENDPOINT.into();
        config.ollama.model = " ".into();
        assert_eq!(
            config.missing(none),
            Some(AiMissing::Model),
            "a blank model"
        );
    }

    #[test]
    fn ollama_endpoints_are_host_and_port() {
        for good in ["127.0.0.1:11434", "localhost:1", "[::1]:11434"] {
            assert!(validate_ollama_endpoint(good).is_ok(), "{good} is valid");
        }
        for bad in [
            "",
            "localhost",
            "http://localhost:11434",
            ":11434",
            "h:port",
            "::1:11434",
            "[]:11434",
        ] {
            assert!(validate_ollama_endpoint(bad).is_err(), "{bad:?} is invalid");
        }
    }
}
