//! Anthropic (Claude) reasoning adapter.
//!
//! Mirrors `OpenAiProvider` in retry/timeout semantics and prompt-
//! bundle handling, but speaks the Anthropic Messages API
//! (`POST /v1/messages`) instead of OpenAI chat completions. The
//! prompt-building and response-parsing helpers are shared with
//! `openai.rs` so the `policy_hash` invariant (SC-103) and the
//! classification JSON contract stay identical across providers.
//!
//! Wire-format differences captured here:
//!
//! - Auth header is `x-api-key`, not `Authorization: Bearer ...`.
//! - Mandatory `anthropic-version` header; pinned to `2023-06-01`
//!   (the stable release).
//! - `system` is a top-level string, not a role in `messages`.
//! - `max_tokens` is required.
//! - No native `response_format: json_object` — the prompt bundle's
//!   system instructions already tell the model to "Respond ONLY
//!   with a JSON object"; if the response isn't JSON, the shared
//!   `parse_classification` surfaces `MalformedResponse`.
//! - Response shape is `content: [{"type":"text","text":"..."}]`
//!   instead of `choices[].message.content`.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use super::openai::{
    build_classification_prompt, build_summary_prompt, parse_classification, parse_summary,
    truncate,
};
use super::ReasoningProvider;
use crate::error::Result;
use crate::models::reasoning::{
    ClassificationRequest, ClassificationResponse, ReasoningError, SummaryRequest, SummaryResponse,
};
use crate::models::ReasoningConfig;

/// Anthropic API version pinned at adapter construction time. The
/// value lives in the `anthropic-version` header on every request.
/// `2023-06-01` is the stable public release and matches the shape
/// documented in https://docs.anthropic.com/en/api/messages.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Safe upper bound for `max_tokens` on classification and summary
/// calls. The Messages API requires `max_tokens`; we pick a value
/// large enough to cover verbose rationales and transcripts but small
/// enough to bound cost if the model ever runs away. A classification
/// JSON with all optional fields populated fits comfortably inside
/// 1024 tokens; summaries are plain text and likewise fit.
const DEFAULT_MAX_TOKENS: u32 = 2048;

/// Anthropic (Claude) reasoning adapter.
///
/// Config surface mirrors `OpenAiProvider`: `api_base`,
/// `api_key_env`/`api_key`, `model`, `request_timeout_seconds`, and
/// `followup_retry_count` all behave identically. Example config:
///
/// ```toml
/// [reasoning]
/// provider = "anthropic"
/// api_base = "https://api.anthropic.com"
/// api_key_env = "ANTHROPIC_API_KEY"
/// model = "claude-3-5-sonnet-20241022"
/// ```
pub struct AnthropicProvider {
    http: Client,
    api_base: String,
    api_key: String,
    model: String,
    timeout: Duration,
    retries: u32,
}

impl AnthropicProvider {
    pub fn new(config: &ReasoningConfig) -> Result<Self> {
        let timeout = Duration::from_secs(config.request_timeout_seconds.max(1));
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| crate::error::Error::Config(format!("reqwest build failed: {e}")))?;
        Ok(Self {
            http,
            api_base: config.api_base.trim_end_matches('/').to_string(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            timeout,
            retries: config.followup_retry_count,
        })
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.api_base)
    }
}

// ---------------------------------------------------------------------------
// Wire formats
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<MessageInput<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Serialize)]
struct MessageInput<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ReasoningProvider for AnthropicProvider {
    async fn classify(
        &self,
        request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        let system = request.prompt_bundle.system.clone();
        let prompt = build_classification_prompt(&request);
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: DEFAULT_MAX_TOKENS,
            system: &system,
            messages: vec![MessageInput {
                role: "user",
                content: &prompt,
            }],
            temperature: Some(0.0),
        };
        let raw = self.post_messages(&body).await?;
        parse_classification(&raw)
    }

    async fn summarize(
        &self,
        request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        let system = request.prompt_bundle.system.clone();
        let prompt = build_summary_prompt(&request);
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: DEFAULT_MAX_TOKENS,
            system: &system,
            messages: vec![MessageInput {
                role: "user",
                content: &prompt,
            }],
            temperature: Some(0.2),
        };
        let raw = self.post_messages(&body).await?;
        parse_summary(&raw)
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        // Minimal-cost reachability probe. Anthropic rejects empty
        // `messages`, so we send a single one-byte user turn and cap
        // output at one token. A 401 from an invalid key surfaces as
        // `Unreachable` via `post_messages`'s status handling.
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: 1,
            system: "",
            messages: vec![MessageInput {
                role: "user",
                content: "ping",
            }],
            temperature: Some(0.0),
        };
        self.post_messages(&body).await.map(|_| ())
    }
}

impl AnthropicProvider {
    async fn post_messages(
        &self,
        body: &MessagesRequest<'_>,
    ) -> std::result::Result<String, ReasoningError> {
        let url = self.messages_url();
        let mut last_err: Option<ReasoningError> = None;
        let total_attempts = self.retries.saturating_add(1);
        for attempt in 0..total_attempts {
            debug!(
                attempt,
                api_base = self.api_base,
                model = self.model,
                "anthropic reasoning call"
            );
            let resp = self
                .http
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(body)
                .timeout(self.timeout)
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) if e.is_timeout() => {
                    last_err = Some(ReasoningError::Timeout);
                    warn!(attempt, "anthropic request timed out");
                    continue;
                }
                Err(e) => {
                    last_err = Some(ReasoningError::Unreachable(format!("anthropic: {e}")));
                    warn!(attempt, error = %e, "anthropic request failed");
                    continue;
                }
            };
            if !resp.status().is_success() {
                let status = resp.status();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string());
                let resp_body = resp.text().await.unwrap_or_default();
                let mut msg = format!("anthropic http {status}: {}", truncate(&resp_body, 200));
                if let Some(ra) = &retry_after {
                    msg.push_str(&format!(" (retry-after: {ra})"));
                }
                let err = ReasoningError::Unreachable(msg);
                let retryable =
                    status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
                if retryable {
                    last_err = Some(err);
                    warn!(attempt, %status, "anthropic returned retryable status");
                    continue;
                } else {
                    error!(%status, "anthropic returned non-retryable status; failing fast");
                    return Err(err);
                }
            }
            let text = resp
                .text()
                .await
                .map_err(|e| ReasoningError::MalformedResponse(e.to_string()))?;
            let parsed: MessagesResponse = serde_json::from_str(&text).map_err(|e| {
                ReasoningError::MalformedResponse(format!("{e}: body={}", truncate(&text, 200)))
            })?;
            let content = parsed
                .content
                .into_iter()
                .find(|b| b.kind == "text")
                .and_then(|b| b.text)
                .ok_or_else(|| {
                    ReasoningError::MalformedResponse("no text block in anthropic response".into())
                })?;
            use nostr_sdk::hashes::Hash as _;
            let content_hash = nostr_sdk::hashes::sha256::Hash::hash(content.as_bytes());
            let content_hash_prefix = &content_hash.to_string()[..16];
            debug!(
                attempt,
                model = self.model,
                content_len = content.len(),
                content_sha256_prefix = content_hash_prefix,
                "anthropic reasoning call response"
            );
            return Ok(content);
        }
        Err(last_err.unwrap_or_else(|| {
            ReasoningError::Unreachable("anthropic: exhausted retries".into())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_url_appends_v1_messages() {
        let cfg = ReasoningConfig {
            provider: "anthropic".into(),
            api_base: "https://api.anthropic.com".into(),
            api_key: "k".into(),
            ..ReasoningConfig::default()
        };
        let provider = AnthropicProvider::new(&cfg).unwrap();
        assert_eq!(
            provider.messages_url(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn messages_url_trims_trailing_slash() {
        let cfg = ReasoningConfig {
            provider: "anthropic".into(),
            api_base: "https://api.anthropic.com/".into(),
            api_key: "k".into(),
            ..ReasoningConfig::default()
        };
        let provider = AnthropicProvider::new(&cfg).unwrap();
        assert_eq!(
            provider.messages_url(),
            "https://api.anthropic.com/v1/messages",
            "trailing slash on api_base must not produce a double slash"
        );
    }

    #[test]
    fn credential_is_read_from_api_key_field() {
        let cfg = ReasoningConfig {
            provider: "anthropic".into(),
            api_key: "secret-from-env".into(),
            ..ReasoningConfig::default()
        };
        let provider = AnthropicProvider::new(&cfg).unwrap();
        assert_eq!(provider.api_key, "secret-from-env");
    }

    #[test]
    fn request_timeout_is_configured() {
        let cfg = ReasoningConfig {
            provider: "anthropic".into(),
            request_timeout_seconds: 42,
            api_key: "k".into(),
            ..ReasoningConfig::default()
        };
        let provider = AnthropicProvider::new(&cfg).unwrap();
        assert_eq!(provider.timeout, Duration::from_secs(42));

        // Zero is floored to one second so reqwest never receives the
        // "no timeout" sentinel.
        let cfg_zero = ReasoningConfig {
            provider: "anthropic".into(),
            request_timeout_seconds: 0,
            api_key: "k".into(),
            ..ReasoningConfig::default()
        };
        let provider_zero = AnthropicProvider::new(&cfg_zero).unwrap();
        assert_eq!(provider_zero.timeout, Duration::from_secs(1));
    }

    #[test]
    fn provider_honors_configured_retry_count() {
        for configured in [0u32, 1, 3, 7] {
            let cfg = ReasoningConfig {
                provider: "anthropic".into(),
                api_key: "k".into(),
                followup_retry_count: configured,
                ..ReasoningConfig::default()
            };
            let provider = AnthropicProvider::new(&cfg).unwrap();
            assert_eq!(provider.retries, configured);
        }
    }
}
