//! OpenAI (and OpenAI-compatible) reasoning adapter — the single
//! adapter shipped in Phase 3. `api_base` parameterises everything,
//! so the same code covers hosted OpenAI, self-hosted
//! OpenAI-compatible gateways, and router proxies (SC-104 / FR-103).
//!
//! Scope-control (plan): a plain `for _ in 0..retries { ... }` loop,
//! no `tokio-retry` crate. JSON-mode classification, plain-text
//! summary. The `policy_hash` travels with every request so audit
//! records are reproducible.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::ReasoningProvider;
use crate::error::Result;
use crate::models::mediation::{ClassificationLabel, Flag};
use crate::models::reasoning::{
    ClassificationRequest, ClassificationResponse, EscalationReason, RationaleText, ReasoningError,
    SuggestedAction, SummaryRequest, SummaryResponse,
};
use crate::models::ReasoningConfig;

pub struct OpenAiProvider {
    http: Client,
    api_base: String,
    api_key: String,
    model: String,
    timeout: Duration,
    retries: u32,
}

impl OpenAiProvider {
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
            // Inherit the mediation-level followup retry count here so
            // transient HTTP failures are bounded the same way the
            // mediation engine bounds its own retries. This keeps the
            // scope-control promise (no standalone retry framework).
            retries: 1,
        })
    }

    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.api_base)
    }
}

// ---------------------------------------------------------------------------
// Wire formats
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    temperature: f64,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

/// Structured classification JSON returned by the model when we pass
/// `response_format = json_object`. The adapter maps any unexpected
/// values to `ReasoningError::MalformedResponse`.
#[derive(Deserialize)]
struct ClassificationJson {
    classification: String,
    confidence: f64,
    #[serde(default)]
    suggested_action: String,
    #[serde(default)]
    suggested_action_detail: Option<String>,
    #[serde(default)]
    rationale: String,
    #[serde(default)]
    flags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ReasoningProvider for OpenAiProvider {
    async fn classify(
        &self,
        request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        let prompt = build_classification_prompt(&request);
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: "You are Serbero's Phase 3 classification subsystem. \
                              Output ONLY valid JSON.",
                },
                ChatMessage {
                    role: "user",
                    content: &prompt,
                },
            ],
            response_format: Some(ResponseFormat {
                kind: "json_object".into(),
            }),
            temperature: 0.0,
        };
        let raw = self.post_chat(&body).await?;
        parse_classification(&raw)
    }

    async fn summarize(
        &self,
        request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        let prompt = build_summary_prompt(&request);
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: "You are Serbero's Phase 3 summarization subsystem. \
                              Produce a short cooperative-resolution summary for the \
                              assigned solver. You are an assistance system, not the \
                              final authority.",
                },
                ChatMessage {
                    role: "user",
                    content: &prompt,
                },
            ],
            response_format: None,
            temperature: 0.2,
        };
        let raw = self.post_chat(&body).await?;
        parse_summary(&raw)
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        // Minimal-cost reachability probe: a two-token completion.
        let body = ChatRequest {
            model: &self.model,
            messages: vec![ChatMessage {
                role: "user",
                content: "ping",
            }],
            response_format: None,
            temperature: 0.0,
        };
        self.post_chat(&body).await.map(|_| ())
    }
}

impl OpenAiProvider {
    async fn post_chat(
        &self,
        body: &ChatRequest<'_>,
    ) -> std::result::Result<String, ReasoningError> {
        let url = self.chat_completions_url();
        let mut last_err: Option<ReasoningError> = None;
        let total_attempts = self.retries.saturating_add(1);
        for attempt in 0..total_attempts {
            debug!(
                attempt,
                api_base = self.api_base,
                model = self.model,
                "openai reasoning call"
            );
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(body)
                .timeout(self.timeout)
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) if e.is_timeout() => {
                    last_err = Some(ReasoningError::Timeout);
                    warn!(attempt, "openai request timed out");
                    continue;
                }
                Err(e) => {
                    last_err = Some(ReasoningError::Unreachable(e.to_string()));
                    warn!(attempt, error = %e, "openai request failed");
                    continue;
                }
            };
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                last_err = Some(ReasoningError::Unreachable(format!(
                    "http {status}: {}",
                    truncate(&body, 200)
                )));
                warn!(attempt, %status, "openai returned non-success status");
                continue;
            }
            let text = resp
                .text()
                .await
                .map_err(|e| ReasoningError::MalformedResponse(e.to_string()))?;
            let parsed: ChatResponse = serde_json::from_str(&text)
                .map_err(|e| ReasoningError::MalformedResponse(format!("{e}: body={text}")))?;
            let content = parsed
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.message.content)
                .ok_or_else(|| ReasoningError::MalformedResponse("empty choices".into()))?;
            return Ok(content);
        }
        Err(last_err.unwrap_or(ReasoningError::Unreachable("exhausted retries".into())))
    }
}

fn build_classification_prompt(r: &ClassificationRequest) -> String {
    let transcript = r
        .transcript
        .iter()
        .map(|e| format!("[{}] {}: {}", e.inner_event_created_at, e.party, e.content))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "session_id: {sid}\ndispute_id: {did}\ninitiator: {init}\npolicy_hash: {ph}\n\
         round_count: {rc}\n\nTranscript:\n{tr}\n\n\
         Return JSON with keys: classification (one of coordination_failure_resolvable, \
         conflicting_claims, suspected_fraud, unclear, not_suitable_for_mediation), \
         confidence (0..1), suggested_action (ask_clarification|summarize|escalate), \
         suggested_action_detail (string, optional), rationale (string), \
         flags (array of fraud_risk|conflicting_claims|low_info|unresponsive_party|\
         authority_boundary_attempt).",
        sid = r.session_id,
        did = r.dispute_id,
        init = r.initiator_role,
        ph = r.policy_hash,
        rc = r.context.round_count,
        tr = transcript,
    )
}

fn build_summary_prompt(r: &SummaryRequest) -> String {
    let transcript = r
        .transcript
        .iter()
        .map(|e| format!("[{}] {}: {}", e.inner_event_created_at, e.party, e.content))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "session_id: {sid}\ndispute_id: {did}\npolicy_hash: {ph}\n\
         classification: {cls}\nconfidence: {cf}\n\nTranscript:\n{tr}\n\n\
         Produce a short summary for the assigned solver, followed by a single-line \
         SUGGESTED_NEXT_STEP: line. Do NOT suggest fund actions. Do NOT claim final \
         authority. End with a RATIONALE: line.",
        sid = r.session_id,
        did = r.dispute_id,
        ph = r.policy_hash,
        cls = r.classification,
        cf = r.confidence,
        tr = transcript,
    )
}

fn parse_classification(raw: &str) -> std::result::Result<ClassificationResponse, ReasoningError> {
    let parsed: ClassificationJson = serde_json::from_str(raw)
        .map_err(|e| ReasoningError::MalformedResponse(format!("{e}: body={raw}")))?;
    let classification = match parsed.classification.as_str() {
        "coordination_failure_resolvable" => ClassificationLabel::CoordinationFailureResolvable,
        "conflicting_claims" => ClassificationLabel::ConflictingClaims,
        "suspected_fraud" => ClassificationLabel::SuspectedFraud,
        "unclear" => ClassificationLabel::Unclear,
        "not_suitable_for_mediation" => ClassificationLabel::NotSuitableForMediation,
        other => {
            return Err(ReasoningError::MalformedResponse(format!(
                "unknown classification label: {other}"
            )))
        }
    };
    let suggested_action = match parsed.suggested_action.as_str() {
        "ask_clarification" => SuggestedAction::AskClarification(
            parsed.suggested_action_detail.clone().unwrap_or_default(),
        ),
        "summarize" => SuggestedAction::Summarize,
        "escalate" => SuggestedAction::Escalate(EscalationReason(
            parsed.suggested_action_detail.clone().unwrap_or_default(),
        )),
        other => {
            return Err(ReasoningError::MalformedResponse(format!(
                "unknown suggested_action: {other}"
            )))
        }
    };
    let flags = parsed
        .flags
        .into_iter()
        .filter_map(|f| match f.as_str() {
            "fraud_risk" => Some(Flag::FraudRisk),
            "conflicting_claims" => Some(Flag::ConflictingClaims),
            "low_info" => Some(Flag::LowInfo),
            "unresponsive_party" => Some(Flag::UnresponsiveParty),
            "authority_boundary_attempt" => Some(Flag::AuthorityBoundaryAttempt),
            _ => None,
        })
        .collect();
    Ok(ClassificationResponse {
        classification,
        confidence: parsed.confidence.clamp(0.0, 1.0),
        suggested_action,
        rationale: RationaleText(parsed.rationale),
        flags,
    })
}

fn parse_summary(raw: &str) -> std::result::Result<SummaryResponse, ReasoningError> {
    // Plain text: free-form summary, then SUGGESTED_NEXT_STEP:, then RATIONALE:.
    let (body, rationale) = raw.split_once("RATIONALE:").unwrap_or((raw, ""));
    let (summary, next_step) = body
        .split_once("SUGGESTED_NEXT_STEP:")
        .unwrap_or((body, ""));
    let summary_text = summary.trim().to_string();
    let suggested_next_step = next_step.trim().to_string();
    if summary_text.is_empty() {
        return Err(ReasoningError::MalformedResponse(
            "empty summary body".into(),
        ));
    }
    Ok(SummaryResponse {
        summary_text,
        suggested_next_step,
        rationale: RationaleText(rationale.trim().to_string()),
    })
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        &s[..n]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_classification_happy_path() {
        let raw = r#"{
            "classification":"coordination_failure_resolvable",
            "confidence":0.91,
            "suggested_action":"summarize",
            "rationale":"parties agreed on payment timing",
            "flags":["low_info"]
        }"#;
        let parsed = parse_classification(raw).unwrap();
        assert_eq!(
            parsed.classification,
            ClassificationLabel::CoordinationFailureResolvable
        );
        assert!((parsed.confidence - 0.91).abs() < f64::EPSILON);
        assert_eq!(parsed.suggested_action, SuggestedAction::Summarize);
        assert_eq!(parsed.flags, vec![Flag::LowInfo]);
    }

    #[test]
    fn parse_classification_rejects_unknown_label() {
        let raw = r#"{
            "classification":"totally_made_up",
            "confidence":0.5,
            "suggested_action":"summarize",
            "rationale":""
        }"#;
        let err = parse_classification(raw).unwrap_err();
        assert!(matches!(err, ReasoningError::MalformedResponse(_)));
    }

    #[test]
    fn parse_summary_happy_path() {
        let raw = "Buyer confirmed receipt, seller confirmed funds released.\n\
                   SUGGESTED_NEXT_STEP: close the dispute in favor of buyer.\n\
                   RATIONALE: both parties aligned on the timeline.";
        let parsed = parse_summary(raw).unwrap();
        assert!(parsed.summary_text.starts_with("Buyer"));
        assert!(parsed.suggested_next_step.contains("close"));
        assert!(parsed.rationale.0.contains("aligned"));
    }

    #[test]
    fn parse_summary_rejects_empty() {
        let err = parse_summary("").unwrap_err();
        assert!(matches!(err, ReasoningError::MalformedResponse(_)));
    }
}
