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
use tracing::{debug, error, warn};

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
            // Retry budget is owned by the reasoning adapter
            // (FR-104 + plan degraded-mode table). Retries here are
            // additional attempts AFTER the initial request, so the
            // configured value maps 1:1: 0 = no retry, 1 = one retry
            // (two total attempts), etc. No standalone retry
            // framework; bounded by a plain for-loop in post_chat.
            retries: config.followup_retry_count,
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
        // The system message IS the versioned system prompt from the
        // bundle. Hardcoding a different system message here would
        // break the policy_hash invariant (SC-103).
        let system = request.prompt_bundle.system.clone();
        let prompt = build_classification_prompt(&request);
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: &system,
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
        let system = request.prompt_bundle.system.clone();
        let prompt = build_summary_prompt(&request);
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: &system,
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
                let err =
                    ReasoningError::Unreachable(format!("http {status}: {}", truncate(&body, 200)));
                // Retryable: request timeout (408), rate limited (429),
                // or any 5xx server error. Everything else is a
                // permanent client error — fail fast instead of
                // wasting attempts on 401/403/404/etc.
                let retryable =
                    status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
                if retryable {
                    last_err = Some(err);
                    warn!(attempt, %status, "openai returned retryable status");
                    continue;
                } else {
                    error!(%status, "openai returned non-retryable status; failing fast");
                    return Err(err);
                }
            }
            let text = resp
                .text()
                .await
                .map_err(|e| ReasoningError::MalformedResponse(e.to_string()))?;
            let parsed: ChatResponse = serde_json::from_str(&text).map_err(|e| {
                ReasoningError::MalformedResponse(format!("{e}: body={}", truncate(&text, 200)))
            })?;
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
    // Embed every policy section from the bundle so the model sees
    // the exact bytes the session's `policy_hash` pins. An auditor
    // can later grep the git-committed bundle for this hash and
    // recover the full prompt context.
    let transcript = r
        .transcript
        .iter()
        .map(|e| format!("[{}] {}: {}", e.inner_event_created_at, e.party, e.content))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Session metadata\n\
         session_id: {sid}\n\
         dispute_id: {did}\n\
         initiator: {init}\n\
         prompt_bundle_id: {bid}\n\
         policy_hash: {ph}\n\
         round_count: {rc}\n\n\
         ## Classification policy (from bundle)\n{cls}\n\n\
         ## Escalation policy (from bundle)\n{esc}\n\n\
         ## Mediation style (from bundle)\n{sty}\n\n\
         ## Message templates (from bundle)\n{tpl}\n\n\
         ## Transcript\n{tr}\n\n\
         ## Output contract\n\
         Return JSON with keys: classification (one of coordination_failure_resolvable, \
         conflicting_claims, suspected_fraud, unclear, not_suitable_for_mediation), \
         confidence (0..1), suggested_action (ask_clarification|summarize|escalate), \
         suggested_action_detail (string, optional), rationale (string), \
         flags (array of fraud_risk|conflicting_claims|low_info|unresponsive_party|\
         authority_boundary_attempt).",
        sid = r.session_id,
        did = r.dispute_id,
        init = r.initiator_role,
        bid = r.prompt_bundle.id,
        ph = r.prompt_bundle.policy_hash,
        rc = r.context.round_count,
        cls = r.prompt_bundle.classification,
        esc = r.prompt_bundle.escalation,
        sty = r.prompt_bundle.mediation_style,
        tpl = r.prompt_bundle.message_templates,
        tr = transcript,
    )
}

fn build_summary_prompt(r: &SummaryRequest) -> String {
    // As in the classification path, every relevant bundle section
    // flows into the user prompt so the policy_hash pin is honest.
    let transcript = r
        .transcript
        .iter()
        .map(|e| format!("[{}] {}: {}", e.inner_event_created_at, e.party, e.content))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Session metadata\n\
         session_id: {sid}\n\
         dispute_id: {did}\n\
         prompt_bundle_id: {bid}\n\
         policy_hash: {ph}\n\
         classification: {cls}\n\
         confidence: {cf}\n\n\
         ## Mediation style (from bundle)\n{sty}\n\n\
         ## Message templates (from bundle)\n{tpl}\n\n\
         ## Escalation policy (from bundle, for reference)\n{esc}\n\n\
         ## Transcript\n{tr}\n\n\
         ## Output contract\n\
         Produce a short summary for the assigned solver, followed by a single-line \
         SUGGESTED_NEXT_STEP: line. Do NOT suggest fund actions. Do NOT claim final \
         authority. End with a RATIONALE: line.",
        sid = r.session_id,
        did = r.dispute_id,
        bid = r.prompt_bundle.id,
        ph = r.prompt_bundle.policy_hash,
        cls = r.classification,
        cf = r.confidence,
        sty = r.prompt_bundle.mediation_style,
        tpl = r.prompt_bundle.message_templates,
        esc = r.prompt_bundle.escalation,
        tr = transcript,
    )
}

fn parse_classification(raw: &str) -> std::result::Result<ClassificationResponse, ReasoningError> {
    let parsed: ClassificationJson = serde_json::from_str(raw).map_err(|e| {
        ReasoningError::MalformedResponse(format!("{e}: body={}", truncate(raw, 200)))
    })?;
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
    let flags: Vec<Flag> = parsed
        .flags
        .into_iter()
        .map(|f| match f.as_str() {
            "fraud_risk" => Ok(Flag::FraudRisk),
            "conflicting_claims" => Ok(Flag::ConflictingClaims),
            "low_info" => Ok(Flag::LowInfo),
            "unresponsive_party" => Ok(Flag::UnresponsiveParty),
            "authority_boundary_attempt" => Ok(Flag::AuthorityBoundaryAttempt),
            other => Err(ReasoningError::MalformedResponse(format!(
                "unknown flag: {other}"
            ))),
        })
        .collect::<std::result::Result<_, _>>()?;
    Ok(ClassificationResponse {
        classification,
        confidence: parsed.confidence.clamp(0.0, 1.0),
        suggested_action,
        rationale: RationaleText(parsed.rationale),
        flags,
    })
}

/// Parse a plain-text summary response of the shape:
///
/// ```text
/// <summary body>
/// SUGGESTED_NEXT_STEP: <one line>
/// RATIONALE: <free text>
/// ```
///
/// The previous implementation chained `split_once` and could
/// misattribute content if the markers arrived out of order (e.g.
/// RATIONALE before SUGGESTED_NEXT_STEP), leaving the next-step
/// embedded in the rationale string. This version locates both
/// markers explicitly, rejects the inverted order, and slices by
/// byte index so each section is derived from the canonical position
/// of its marker.
fn parse_summary(raw: &str) -> std::result::Result<SummaryResponse, ReasoningError> {
    const NEXT_MARKER: &str = "SUGGESTED_NEXT_STEP:";
    const RATIONALE_MARKER: &str = "RATIONALE:";

    let next_idx = raw.find(NEXT_MARKER);
    let rationale_idx = raw.find(RATIONALE_MARKER);

    if let (Some(n), Some(r)) = (next_idx, rationale_idx) {
        if r < n {
            return Err(ReasoningError::MalformedResponse(
                "summary markers out of order: RATIONALE: appeared before \
                 SUGGESTED_NEXT_STEP:"
                    .into(),
            ));
        }
    }

    let (summary_text, suggested_next_step, rationale_text) = match (next_idx, rationale_idx) {
        (Some(n), Some(r)) => {
            let summary = raw[..n].trim().to_string();
            let next = raw[n + NEXT_MARKER.len()..r].trim().to_string();
            let rationale = raw[r + RATIONALE_MARKER.len()..].trim().to_string();
            (summary, next, rationale)
        }
        (Some(n), None) => {
            let summary = raw[..n].trim().to_string();
            let next = raw[n + NEXT_MARKER.len()..].trim().to_string();
            (summary, next, String::new())
        }
        (None, Some(r)) => {
            let summary = raw[..r].trim().to_string();
            let rationale = raw[r + RATIONALE_MARKER.len()..].trim().to_string();
            (summary, String::new(), rationale)
        }
        (None, None) => (raw.trim().to_string(), String::new(), String::new()),
    };

    if summary_text.is_empty() {
        return Err(ReasoningError::MalformedResponse(
            "empty summary body".into(),
        ));
    }

    Ok(SummaryResponse {
        summary_text,
        suggested_next_step,
        rationale: RationaleText(rationale_text),
    })
}

/// UTF-8-safe truncate: returns a prefix of `s` that ends on a char
/// boundary and contains at most `n` bytes. Plain byte slicing would
/// panic on multi-byte characters.
fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    // Walk char boundaries until we exceed n bytes, then cut at the
    // last boundary that fits.
    let mut end = 0;
    for (idx, ch) in s.char_indices() {
        let next = idx + ch.len_utf8();
        if next > n {
            break;
        }
        end = next;
    }
    &s[..end]
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

    #[test]
    fn parse_summary_rejects_inverted_markers() {
        // RATIONALE before SUGGESTED_NEXT_STEP must be rejected — the
        // old split_once-based parser would silently absorb the next
        // step into the rationale text.
        let raw = "the summary body.\n\
                   RATIONALE: some rationale.\n\
                   SUGGESTED_NEXT_STEP: too late.";
        let err = parse_summary(raw).unwrap_err();
        match err {
            ReasoningError::MalformedResponse(msg) => {
                assert!(
                    msg.to_lowercase().contains("out of order"),
                    "expected an out-of-order error: {msg}"
                );
            }
            other => panic!("expected MalformedResponse, got {other:?}"),
        }
    }

    #[test]
    fn parse_summary_handles_missing_rationale() {
        let raw = "just a summary.\nSUGGESTED_NEXT_STEP: do the thing.";
        let parsed = parse_summary(raw).unwrap();
        assert_eq!(parsed.summary_text, "just a summary.");
        assert_eq!(parsed.suggested_next_step, "do the thing.");
        assert_eq!(parsed.rationale.0, "");
    }

    #[test]
    fn parse_summary_handles_missing_next_step() {
        let raw = "just a summary.\nRATIONALE: because reasons.";
        let parsed = parse_summary(raw).unwrap();
        assert_eq!(parsed.summary_text, "just a summary.");
        assert_eq!(parsed.suggested_next_step, "");
        assert_eq!(parsed.rationale.0, "because reasons.");
    }

    #[test]
    fn parse_classification_rejects_unknown_flag() {
        let raw = r#"{
            "classification":"coordination_failure_resolvable",
            "confidence":0.8,
            "suggested_action":"summarize",
            "rationale":"",
            "flags":["fraud_risk","totally_made_up"]
        }"#;
        let err = parse_classification(raw).unwrap_err();
        assert!(matches!(err, ReasoningError::MalformedResponse(_)));
    }

    #[test]
    fn truncate_respects_utf8_boundaries() {
        // "héllo" is 6 bytes: h(1) é(2) l(1) l(1) o(1).
        let s = "héllo";
        // Requesting 2 bytes must NOT split the `é` (2 bytes starting
        // at index 1) — the safe cut is after `h` (1 byte).
        let got = truncate(s, 2);
        assert_eq!(got, "h");
        assert_eq!(truncate(s, 3), "hé");
        assert_eq!(truncate(s, 100), "héllo");
    }

    #[test]
    fn provider_honors_configured_retry_count() {
        // The previous implementation hardcoded retries = 1 regardless
        // of the configured value. This test pins the new ownership:
        // the adapter's retry budget comes from
        // [reasoning].followup_retry_count (FR-104 + plan degraded-
        // mode table).
        for configured in [0u32, 1, 3, 7] {
            let cfg = ReasoningConfig {
                provider: "openai".into(),
                followup_retry_count: configured,
                ..ReasoningConfig::default()
            };
            let provider = OpenAiProvider::new(&cfg).unwrap();
            assert_eq!(
                provider.retries, configured,
                "adapter must reflect the configured followup_retry_count"
            );
        }
    }

    // ---- policy_hash invariant regression tests ---------------------
    //
    // The old code hardcoded a system message and used only
    // `prompt_bundle_id` / `policy_hash` as metadata in the user
    // message. That breaks SC-103: the hash would reference bundle
    // bytes the model never saw. These tests pin the fix.

    use std::sync::Arc;

    use crate::models::dispute::InitiatorRole;
    use crate::models::reasoning::{ClassificationRequest, ReasoningContext, SummaryRequest};
    use crate::prompts::PromptBundle;

    fn fixture_bundle() -> Arc<PromptBundle> {
        Arc::new(PromptBundle {
            id: "phase3-test".to_string(),
            policy_hash: "abc123".to_string(),
            system: "SYSTEM_MARKER: you are serbero".to_string(),
            classification: "CLASSIFICATION_MARKER: policy text".to_string(),
            escalation: "ESCALATION_MARKER: escalation rules".to_string(),
            mediation_style: "STYLE_MARKER: neutral tone".to_string(),
            message_templates: "TEMPLATE_MARKER: templates here".to_string(),
        })
    }

    #[test]
    fn classify_prompt_includes_every_bundle_section() {
        let req = ClassificationRequest {
            session_id: "s1".into(),
            dispute_id: "d1".into(),
            initiator_role: InitiatorRole::Buyer,
            prompt_bundle: fixture_bundle(),
            transcript: vec![],
            context: ReasoningContext {
                round_count: 0,
                last_classification: None,
                last_confidence: None,
            },
        };
        let user = build_classification_prompt(&req);
        // The user-facing prompt must include every section whose
        // bytes contribute to policy_hash — NOT just the id+hash.
        for marker in [
            "CLASSIFICATION_MARKER",
            "ESCALATION_MARKER",
            "STYLE_MARKER",
            "TEMPLATE_MARKER",
        ] {
            assert!(
                user.contains(marker),
                "classification user prompt missing `{marker}`:\n{user}"
            );
        }
        // The system prompt (verified in classify() itself) is the
        // bundle's `system` field. The hash MUST also appear so the
        // model's own output can reference it.
        assert!(user.contains("policy_hash: abc123"));
        assert!(user.contains("prompt_bundle_id: phase3-test"));
    }

    #[test]
    fn summary_prompt_includes_every_relevant_bundle_section() {
        let req = SummaryRequest {
            session_id: "s1".into(),
            dispute_id: "d1".into(),
            prompt_bundle: fixture_bundle(),
            transcript: vec![],
            classification: ClassificationLabel::CoordinationFailureResolvable,
            confidence: 0.9,
        };
        let user = build_summary_prompt(&req);
        // The summary path embeds style + templates + escalation.
        // (It does NOT re-embed the classification policy — the
        // classification is already a decided label at this point.)
        for marker in ["STYLE_MARKER", "TEMPLATE_MARKER", "ESCALATION_MARKER"] {
            assert!(
                user.contains(marker),
                "summary user prompt missing `{marker}`:\n{user}"
            );
        }
        assert!(user.contains("policy_hash: abc123"));
        assert!(user.contains("prompt_bundle_id: phase3-test"));
    }
}
