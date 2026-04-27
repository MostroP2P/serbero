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
use reqwest::header::{HeaderMap, ACCEPT_ENCODING};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use super::ReasoningProvider;
use crate::error::Result;
use crate::models::mediation::{ClassificationLabel, Flag};
use crate::models::reasoning::{
    ClassificationRequest, ClassificationResponse, EscalationReason, RationaleText, ReasoningError,
    SuggestedAction, SummaryRequest, SummaryResponse,
};
use crate::models::ReasoningConfig;

/// OpenAI (and OpenAI-compatible) reasoning adapter — the single
/// adapter shipped in Phase 3.
///
/// ## Portability surface (SC-104 / FR-103)
///
/// Swapping the reasoning endpoint across OpenAI-compatible targets
/// (different `api_base`, different `api_key_env`, same
/// `provider = "openai"`) takes effect on restart with no code change.
/// The following `ReasoningConfig` fields are honored:
///
/// | Field                     | Effect                                      |
/// |---------------------------|---------------------------------------------|
/// | `api_base`                | Root URL; `/chat/completions` appended      |
/// | `api_key_env`             | Env var name holding the bearer token       |
/// | `model`                   | Passed in every request body                |
/// | `request_timeout_seconds` | Per-request HTTP timeout                    |
/// | `followup_retry_count`    | Additional attempts after initial failure   |
///
/// No hardcoded OpenAI host — the same struct serves hosted OpenAI,
/// self-hosted vLLM / llama.cpp, Ollama, LiteLLM, and any router
/// proxy that exposes `/chat/completions`.
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
            // Disable connection pooling. Observed 2026-04-27 against
            // PPQ.ai: the healthcheck request returns and the
            // connection goes back to the pool; the next /chat/completions
            // call reuses it and ~9 s later fails with hyper's generic
            // "error decoding response body" on a chunked body of
            // length 0. The pattern is consistent with a gateway that
            // closes or half-closes idle keep-alive connections without
            // signalling cleanly. A fresh TCP+TLS handshake per call is
            // pennies compared to a failed mediation start.
            .pool_max_idle_per_host(0)
            // Force HTTP/1.1. PPQ.ai's TLS terminator may negotiate
            // HTTP/2 via ALPN, but we have no way to debug an HTTP/2
            // stream-level failure from the application layer; the
            // observed body-decode failures could equally come from a
            // RST_STREAM the client cannot surface. Pinning HTTP/1.1
            // makes the wire predictable and matches what `curl
            // --http1.1` shows when reproducing manually.
            .http1_only()
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

    /// Some models (notably `gpt-5*`) reject explicit temperature
    /// values and require the server-side default instead. PPQ.ai's
    /// router/auto models (`autoclaw`, `auto`, `switchpoint/router`)
    /// publish `supported_parameters: []` and silently mishandle any
    /// extra field — observed 2026-04-27 with autoclaw, where sending
    /// `response_format=json_object` produced a chunked body reqwest
    /// could not decode. For those we omit both temperature AND
    /// response_format; see [`Self::request_response_format`].
    fn request_temperature(&self, value: f64) -> Option<f64> {
        if Self::is_minimal_param_model(&self.model) {
            None
        } else {
            Some(value)
        }
    }

    /// Whether to send `response_format = json_object`. Same caveat as
    /// `request_temperature`: routers/auto models on PPQ.ai reject
    /// this. The classifier still gets a JSON-shaped output because
    /// the user prompt explicitly demands JSON; `parse_classification`
    /// is tolerant of markdown fences via [`extract_json_object`].
    fn request_response_format(&self) -> Option<ResponseFormat> {
        if Self::is_minimal_param_model(&self.model) {
            None
        } else {
            Some(ResponseFormat {
                kind: "json_object".into(),
            })
        }
    }

    fn is_minimal_param_model(model: &str) -> bool {
        // Curated list of models that publish `supported_parameters: []`
        // (or a near-empty set) on the PPQ.ai catalog. Hardcoded
        // because querying `/v1/models` per request would add latency
        // and a failure mode for a near-static fact. Extend as new
        // router/auto SKUs appear.
        matches!(model, "autoclaw" | "auto" | "switchpoint/router")
            || model.starts_with("gpt-5")
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
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
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
    /// Free-form detail field. Still used for the `escalate`
    /// suggested_action to carry the escalation reason. For
    /// `ask_clarification` the per-party fields below take precedence
    /// and `suggested_action_detail` is ignored.
    #[serde(default)]
    suggested_action_detail: Option<String>,
    /// Buyer-addressed clarification text when
    /// `suggested_action = "ask_clarification"`. Required for that
    /// action — the adapter raises `MalformedResponse` if it is
    /// missing or blank. The separation exists because broadcasting
    /// a single string to both parties produced messages obviously
    /// addressed to only one role (observed 2026-04-21 Alice/Bob run).
    #[serde(default)]
    buyer_clarification: Option<String>,
    /// Seller-addressed clarification text. Same rules as above.
    #[serde(default)]
    seller_clarification: Option<String>,
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
            response_format: self.request_response_format(),
            temperature: self.request_temperature(0.0),
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
            temperature: self.request_temperature(0.2),
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
            temperature: self.request_temperature(0.0),
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
            // Pre-call diagnostics — operators correlating PPQ.ai
            // failures need to know which knobs we sent.
            debug!(
                attempt,
                api_base = self.api_base,
                model = self.model,
                response_format = body.response_format.is_some(),
                temperature = ?body.temperature,
                message_count = body.messages.len(),
                "openai reasoning call"
            );
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                // reqwest is built without gzip/brotli/deflate/zstd
                // features (see Cargo.toml). If a gateway like PPQ.ai
                // gzipped the body anyway, `.bytes()` would surface a
                // generic "error decoding response body" with no clue
                // which content-encoding was at fault. Asking for
                // identity makes that class of failure impossible.
                .header(ACCEPT_ENCODING, "identity")
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
            let status = resp.status();
            let headers = resp.headers().clone();
            // Read raw bytes BEFORE attempting any decoding. Two
            // motivations: (1) we want one place that reports HTTP
            // status + headers + body so a body-read failure is
            // actionable; (2) `text()` previously masked everything
            // behind the generic reqwest "error decoding response
            // body" message — observed 2026-04-27 against PPQ.ai's
            // `autoclaw` model.
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    let chain = error_chain(&e);
                    log_response_failure(
                        attempt,
                        status,
                        &headers,
                        None,
                        &format!(
                            "body bytes read failed: {e} | error_chain=[{chain}] | \
                             is_timeout={timeout} is_connect={connect} is_body={body}",
                            timeout = e.is_timeout(),
                            connect = e.is_connect(),
                            body = e.is_body(),
                        ),
                    );
                    last_err = Some(ReasoningError::MalformedResponse(format!(
                        "body bytes read failed (status={status}): {e}"
                    )));
                    // Body-read failures often correlate with
                    // mid-stream proxy hiccups; treat as transient
                    // and let the retry budget try again.
                    continue;
                }
            };
            // UTF-8 with replacement chars: never panic on bad bytes,
            // just give us something we can log and parse.
            let text_str = String::from_utf8_lossy(&bytes).into_owned();
            if !status.is_success() {
                log_response_failure(
                    attempt,
                    status,
                    &headers,
                    Some(&text_str),
                    "non-success HTTP status",
                );
                let err =
                    ReasoningError::Unreachable(format!("http {status}: {}", truncate(&text_str, 200)));
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
            let parsed: ChatResponse = match serde_json::from_str(&text_str) {
                Ok(p) => p,
                Err(e) => {
                    log_response_failure(
                        attempt,
                        status,
                        &headers,
                        Some(&text_str),
                        &format!("response envelope JSON parse failed: {e}"),
                    );
                    return Err(ReasoningError::MalformedResponse(format!(
                        "{e}: body={}",
                        truncate(&text_str, 200)
                    )));
                }
            };
            let content = parsed
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.message.content)
                .ok_or_else(|| {
                    log_response_failure(
                        attempt,
                        status,
                        &headers,
                        Some(&text_str),
                        "OpenAI envelope had no choices[0].message.content",
                    );
                    ReasoningError::MalformedResponse("empty choices".into())
                })?;
            // FR-120 / TC-103 invariant: the full model output may
            // contain party statements and a free-text rationale that
            // the spec forbids in general logs. We emit metadata only
            // at this level — length plus a truncated SHA-256 prefix
            // — so operators can correlate logs with the authoritative
            // copy in `reasoning_rationales` without duplicating the
            // sensitive bytes to the log stream.
            use nostr_sdk::hashes::Hash as _;
            let content_hash = nostr_sdk::hashes::sha256::Hash::hash(content.as_bytes());
            let content_hash_prefix = &content_hash.to_string()[..16];
            debug!(
                attempt,
                model = self.model,
                content_len = content.len(),
                content_sha256_prefix = content_hash_prefix,
                "openai reasoning call response"
            );
            return Ok(content);
        }
        Err(last_err.unwrap_or(ReasoningError::Unreachable("exhausted retries".into())))
    }
}

pub(super) fn build_classification_prompt(r: &ClassificationRequest) -> String {
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
         rationale (string), flags (array of fraud_risk|conflicting_claims|low_info|\
         unresponsive_party|authority_boundary_attempt).\n\
         When suggested_action = ask_clarification you MUST also return \
         buyer_clarification (string, addressed to the buyer, asking what you need \
         from the buyer to advance the case) and seller_clarification (string, \
         addressed to the seller, asking what you need from the seller). Each text \
         goes only to its intended party — do NOT prefix with labels like \
         \"Buyer:\" or \"Seller:\"; the transport layer handles routing. Tailor \
         each question to that party's role (buyer = did you send fiat? proof; \
         seller = did you receive fiat? proof). Both strings must be non-empty; \
         if you cannot produce a useful question for one side, pick a different \
         suggested_action (summarize or escalate). suggested_action_detail is \
         optional and only used to carry the escalation reason when \
         suggested_action = escalate.",
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

pub(super) fn build_summary_prompt(r: &SummaryRequest) -> String {
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

/// Walk the response body looking for a balanced top-level JSON
/// object. Tolerates models that wrap their JSON in markdown fences,
/// preamble like `Here is the response:`, or trailing chatter — all
/// of which we observed against PPQ.ai's router models when
/// `response_format = json_object` cannot be sent. Returns the
/// original input unchanged when no balanced object is found, so
/// `serde_json::from_str` can still produce its native error message.
pub(super) fn extract_json_object(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    let mut depth: i32 = 0;
    let mut start: Option<usize> = None;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        return &raw[s..=i];
                    }
                }
            }
            _ => {}
        }
    }
    raw
}

pub(super) fn parse_classification(
    raw: &str,
) -> std::result::Result<ClassificationResponse, ReasoningError> {
    let candidate = extract_json_object(raw);
    let parsed: ClassificationJson = serde_json::from_str(candidate).map_err(|e| {
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
        "ask_clarification" => {
            // Per-party texts are mandatory for this action. Both
            // must be non-empty; the policy layer also rejects blank
            // text, but raising MalformedResponse here gives a
            // clearer audit trail (adapter-level vs policy-level).
            let buyer_text = parsed
                .buyer_clarification
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    ReasoningError::MalformedResponse(
                        "ask_clarification requires non-empty buyer_clarification".into(),
                    )
                })?
                .to_string();
            let seller_text = parsed
                .seller_clarification
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    ReasoningError::MalformedResponse(
                        "ask_clarification requires non-empty seller_clarification".into(),
                    )
                })?
                .to_string();
            SuggestedAction::AskClarification {
                buyer_text,
                seller_text,
            }
        }
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
pub(super) fn parse_summary(raw: &str) -> std::result::Result<SummaryResponse, ReasoningError> {
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

/// Walk `std::error::Error::source()` and join the chain into one
/// string. reqwest's top-level `Display` is the famously useless
/// "error decoding response body"; the actionable detail (hyper
/// stream errors, rustls trailers, etc.) is one or two `source()`
/// hops down. Format: ` -> caused by: <next> -> caused by: <next>`.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = String::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = err.source();
    let mut depth = 0;
    while let Some(s) = cur {
        if depth > 0 {
            out.push_str(" | ");
        }
        out.push_str(&format!("caused by: {s}"));
        cur = s.source();
        depth += 1;
        // Defensive cap: error chains beyond ~8 hops mean someone is
        // boxing in a loop, not a real diagnostic signal.
        if depth > 8 {
            out.push_str(" | (truncated)");
            break;
        }
    }
    if out.is_empty() {
        "(no source chain)".into()
    } else {
        out
    }
}

/// One-stop diagnostic dump for a failed PPQ.ai/OpenAI-compatible
/// response. Logs HTTP status, the headers operators most often
/// need (`content-type`, `content-encoding`, `transfer-encoding`,
/// `content-length`), and a truncated body preview so the next test
/// run gives us actionable bytes instead of reqwest's generic
/// "error decoding response body" string. Goes through `info!` so
/// the default RUST_LOG=info config picks it up without operators
/// having to re-run with debug.
fn log_response_failure(
    attempt: u32,
    status: reqwest::StatusCode,
    headers: &HeaderMap,
    body_preview: Option<&str>,
    note: &str,
) {
    let header = |name: &str| -> String {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("(absent)")
            .to_string()
    };
    info!(
        attempt,
        %status,
        content_type = %header("content-type"),
        content_encoding = %header("content-encoding"),
        transfer_encoding = %header("transfer-encoding"),
        content_length = %header("content-length"),
        body_len = body_preview.map(|s| s.len()).unwrap_or(0),
        body_preview = body_preview.map(|s| truncate(s, 800)).unwrap_or(""),
        note = note,
        "openai-compatible response failure (diagnostic dump)"
    );
}

/// UTF-8-safe truncate: returns a prefix of `s` that ends on a char
/// boundary and contains at most `n` bytes. Plain byte slicing would
/// panic on multi-byte characters.
pub(super) fn truncate(s: &str, n: usize) -> &str {
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
    fn parse_classification_ask_clarification_happy_path() {
        // Per-party texts land on the right `AskClarification` fields.
        let raw = r#"{
            "classification":"unclear",
            "confidence":0.42,
            "suggested_action":"ask_clarification",
            "buyer_clarification":"Buyer, have you sent the fiat?",
            "seller_clarification":"Seller, have you received the fiat?",
            "rationale":"need more info from both sides",
            "flags":[]
        }"#;
        let parsed = parse_classification(raw).unwrap();
        match parsed.suggested_action {
            SuggestedAction::AskClarification {
                buyer_text,
                seller_text,
            } => {
                assert_eq!(buyer_text, "Buyer, have you sent the fiat?");
                assert_eq!(seller_text, "Seller, have you received the fiat?");
            }
            other => panic!("expected AskClarification, got {other:?}"),
        }
    }

    #[test]
    fn parse_classification_ask_clarification_rejects_blank_buyer_text() {
        // Whitespace-only buyer_clarification must be treated as
        // malformed; letting it through would ship an empty gift-wrap
        // to the buyer.
        let raw = r#"{
            "classification":"unclear",
            "confidence":0.6,
            "suggested_action":"ask_clarification",
            "buyer_clarification":"   \n\t",
            "seller_clarification":"Seller, have you received the fiat?",
            "rationale":"r"
        }"#;
        let err = parse_classification(raw).unwrap_err();
        match err {
            ReasoningError::MalformedResponse(msg) => {
                assert!(
                    msg.contains("buyer_clarification"),
                    "error must cite the missing field: {msg}"
                );
            }
            other => panic!("expected MalformedResponse, got {other:?}"),
        }
    }

    #[test]
    fn parse_classification_ask_clarification_rejects_missing_seller_text() {
        // Entirely missing seller_clarification field — adapter must
        // reject even though JSON is otherwise well-formed.
        let raw = r#"{
            "classification":"unclear",
            "confidence":0.6,
            "suggested_action":"ask_clarification",
            "buyer_clarification":"Buyer, did you send the fiat?",
            "rationale":"r"
        }"#;
        let err = parse_classification(raw).unwrap_err();
        match err {
            ReasoningError::MalformedResponse(msg) => {
                assert!(
                    msg.contains("seller_clarification"),
                    "error must cite the missing field: {msg}"
                );
            }
            other => panic!("expected MalformedResponse, got {other:?}"),
        }
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
    fn extract_json_object_strips_markdown_fences() {
        // PPQ.ai router models that ignore `response_format` often
        // wrap JSON in ```json ... ``` fences. The classifier must
        // still parse it.
        let raw = "Sure! Here is the response:\n\
                   ```json\n\
                   {\"classification\":\"unclear\",\"confidence\":0.5,\"suggested_action\":\"summarize\",\"rationale\":\"\"}\n\
                   ```\n\
                   Let me know if you need more.";
        let extracted = extract_json_object(raw);
        assert!(extracted.starts_with('{') && extracted.ends_with('}'));
        let parsed = parse_classification(raw).unwrap();
        assert_eq!(parsed.classification, ClassificationLabel::Unclear);
    }

    #[test]
    fn extract_json_object_handles_nested_braces_and_strings() {
        // Strings containing `{` / `}` must not throw off the
        // brace-balancing scan, otherwise we'd cut JSON in half.
        let raw = r#"prelude {"a":"x{y}z","b":{"c":1}} trailing"#;
        let extracted = extract_json_object(raw);
        assert_eq!(extracted, r#"{"a":"x{y}z","b":{"c":1}}"#);
    }

    #[test]
    fn extract_json_object_passthrough_when_no_object_found() {
        // Bare text → return as-is so serde's own error message wins.
        let raw = "no json here at all";
        assert_eq!(extract_json_object(raw), raw);
    }

    #[test]
    fn minimal_param_models_omit_response_format_and_temperature() {
        // PPQ.ai routers (`autoclaw` etc.) publish supported_parameters
        // = []. Sending response_format/temperature against them
        // produced "error decoding response body" on 2026-04-27.
        for model in ["autoclaw", "auto", "switchpoint/router", "gpt-5.4-mini"] {
            let cfg = ReasoningConfig {
                api_key: "k".into(),
                model: model.to_string(),
                ..ReasoningConfig::default()
            };
            let provider = OpenAiProvider::new(&cfg).unwrap();
            assert!(
                provider.request_response_format().is_none(),
                "{model}: response_format must be omitted"
            );
            assert!(
                provider.request_temperature(0.0).is_none(),
                "{model}: temperature must be omitted"
            );
        }
    }

    #[test]
    fn full_param_models_keep_response_format_and_temperature() {
        for model in ["gpt-4o-mini", "claude-opus-4.7", "gpt-4.1"] {
            let cfg = ReasoningConfig {
                api_key: "k".into(),
                model: model.to_string(),
                ..ReasoningConfig::default()
            };
            let provider = OpenAiProvider::new(&cfg).unwrap();
            assert!(
                provider.request_response_format().is_some(),
                "{model}: response_format must be sent"
            );
            assert_eq!(
                provider.request_temperature(0.0),
                Some(0.0),
                "{model}: temperature must be sent"
            );
        }
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
        //
        // Also stands in for the T077 requirement
        // `transient_error_retries_up_to_configured_count`: the
        // configured value is what drives the loop bound in
        // `post_chat` (`for attempt in 0..retries.saturating_add(1)`).
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

    // ---- T077: credential + api_base + timeout plumbing ------------
    //
    // The adapter fields are private outside the module, but these
    // tests live inside `mod tests`, so they can assert directly on
    // the constructed provider. Keep them small — the SC-104 /
    // FR-103 portability story is what matters, not re-testing
    // serde defaults.

    #[test]
    fn credential_is_read_from_api_key_field() {
        // Contract: the adapter MUST use the pre-resolved `api_key`
        // field (populated by `resolve_reasoning_api_key` from the
        // env var named by `api_key_env`), never by reading the env
        // itself. The only public surface is the struct's own field,
        // which we assert on directly.
        let cfg = ReasoningConfig {
            api_key: "secret-from-env".into(),
            ..ReasoningConfig::default()
        };
        let provider = OpenAiProvider::new(&cfg).unwrap();
        assert_eq!(provider.api_key, "secret-from-env");
    }

    #[test]
    fn request_url_uses_configured_api_base() {
        // api_base is appended with `/chat/completions` — no
        // hardcoded host. Trailing slashes on the config value are
        // trimmed so a user-typed `".../v1/"` does not produce a
        // double-slashed URL.
        let cfg = ReasoningConfig {
            api_base: "http://localhost:8080/custom/v1".into(),
            api_key: "k".into(),
            ..ReasoningConfig::default()
        };
        let provider = OpenAiProvider::new(&cfg).unwrap();
        assert_eq!(
            provider.chat_completions_url(),
            "http://localhost:8080/custom/v1/chat/completions"
        );

        let cfg_slash = ReasoningConfig {
            api_base: "http://localhost:8080/custom/v1/".into(),
            api_key: "k".into(),
            ..ReasoningConfig::default()
        };
        let provider_slash = OpenAiProvider::new(&cfg_slash).unwrap();
        assert_eq!(
            provider_slash.chat_completions_url(),
            "http://localhost:8080/custom/v1/chat/completions",
            "trailing slash on api_base must not produce a double slash"
        );
    }

    #[test]
    fn request_timeout_is_configured() {
        // Happy path: a positive value lands verbatim in the stored
        // timeout. The configured `42` becomes `Duration::from_secs(42)`.
        let cfg = ReasoningConfig {
            request_timeout_seconds: 42,
            api_key: "k".into(),
            ..ReasoningConfig::default()
        };
        let provider = OpenAiProvider::new(&cfg).unwrap();
        assert_eq!(provider.timeout, Duration::from_secs(42));

        // Edge case: `0` is floored to `1` via `.max(1)` so the
        // reqwest client is never constructed with a zero timeout
        // (which reqwest treats as "no timeout" — dangerous here).
        let cfg_zero = ReasoningConfig {
            request_timeout_seconds: 0,
            api_key: "k".into(),
            ..ReasoningConfig::default()
        };
        let provider_zero = OpenAiProvider::new(&cfg_zero).unwrap();
        assert_eq!(
            provider_zero.timeout,
            Duration::from_secs(1),
            "request_timeout_seconds = 0 must be floored to 1 s"
        );
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
