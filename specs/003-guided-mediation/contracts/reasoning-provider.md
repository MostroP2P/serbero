# Contract: Reasoning Provider Adapter

**Phase**: 3 (Guided Mediation)
**Status**: Contract definition. One adapter ships in Phase 3 (`openai`); additional adapters (`anthropic`, `ppqai`, `openclaw`) enter as `NotYetImplemented` to preserve the boundary without shipping four implementations.

## Purpose

Defines the boundary between Serbero's mediation engine and any
reasoning provider. The reasoning provider is **advisory only**.
Serbero's policy layer independently validates every output before
acting on it (spec FR-116).

## Trait Definition

```rust
#[async_trait]
pub trait ReasoningProvider: Send + Sync {
    /// Produce a classification over a mediation session transcript
    /// plus the configured classification policy.
    async fn classify(
        &self,
        request: ClassificationRequest,
    ) -> Result<ClassificationResponse, ReasoningError>;

    /// Produce a structured cooperative summary for the assigned
    /// solver. Used only on the coordination_failure_resolvable path.
    async fn summarize(
        &self,
        request: SummaryRequest,
    ) -> Result<SummaryResponse, ReasoningError>;

    /// Lightweight liveness probe called at startup and on config
    /// reload (spec Reasoning Provider Configuration: health checks).
    async fn health_check(&self) -> Result<(), ReasoningError>;
}
```

## Request / Response Types

### ClassificationRequest

```rust
pub struct ClassificationRequest {
    pub session_id: String,
    pub dispute_id: String,
    pub initiator_role: InitiatorRole,       // from Phase 2: Buyer | Seller
    pub prompt_bundle: PromptBundleView,     // system + classification policy bytes
    pub transcript: Vec<TranscriptEntry>,    // ordered, inner-event-timestamped
    pub context: ReasoningContext,           // round_count, last_classification, ...
}
```

### ClassificationResponse

```rust
pub struct ClassificationResponse {
    pub classification: ClassificationLabel, // enum from phase3-classification.md
    pub confidence: f64,                     // 0.0..=1.0
    pub suggested_action: SuggestedAction,   // AskClarification | Summarize | Escalate
    pub rationale: RationaleText,            // full text — stored in the audit table
    pub flags: Vec<Flag>,                    // FraudRisk, ConflictingClaims, LowInfo, ...
}
```

### SummaryRequest / SummaryResponse

```rust
pub struct SummaryRequest {
    pub session_id: String,
    pub dispute_id: String,
    pub prompt_bundle: PromptBundleView,
    pub transcript: Vec<TranscriptEntry>,
    pub classification: ClassificationLabel,
    pub confidence: f64,
}

pub struct SummaryResponse {
    pub summary_text: String,
    pub suggested_next_step: String,
    pub rationale: RationaleText,
}
```

### Supporting types

```rust
pub enum ClassificationLabel {
    CoordinationFailureResolvable,
    ConflictingClaims,
    SuspectedFraud,
    Unclear,                                 // never used as "just pick one"; always escalates
    NotSuitableForMediation,                 // e.g. protocol / authority issue
}

pub enum SuggestedAction {
    AskClarification(String),
    Summarize,
    Escalate(EscalationReason),
}

pub enum Flag {
    FraudRisk,
    ConflictingClaims,
    LowInfo,
    UnresponsiveParty,
    AuthorityBoundaryAttempt,                // model tried to suggest a fund action
}

pub struct TranscriptEntry {
    pub party: TranscriptParty,              // Buyer | Seller | Serbero
    pub inner_event_created_at: i64,         // authoritative timestamp
    pub content: String,
}

pub struct PromptBundleView<'a> {
    pub id: &'a str,
    pub policy_hash: &'a str,
    pub system: &'a str,
    pub classification_policy: &'a str,
    pub mediation_style: &'a str,
    pub message_templates: &'a str,
    pub escalation_policy: &'a str,
}

pub enum ReasoningError {
    Unreachable(String),
    Timeout,
    MalformedResponse(String),
    AuthorityBoundaryViolation(String),      // raised by the adapter when it detects the
                                             // model returning a fund-close style instruction
    Other(anyhow::Error),
}
```

## Policy-Layer Validation (executed in `mediation/policy.rs`)

Every `ClassificationResponse` and `SummaryResponse` MUST pass
through the policy validator **before** any side effect:

1. If `flags` contains `FraudRisk` or `ConflictingClaims` → escalate
   regardless of `suggested_action`.
2. If `confidence < configured_escalation_threshold` → escalate.
3. If `suggested_action` is `Escalate(_)` → escalate.
4. If `suggested_next_step` or `summary_text` would instruct a
   fund-moving or dispute-closing action → suppress and escalate with
   trigger `authority_boundary_attempt`, and record a `Flag::
   AuthorityBoundaryAttempt` event.
5. If the adapter returns `ReasoningError::AuthorityBoundaryViolation`
   → escalate immediately with the same trigger.
6. If the adapter returns any other `ReasoningError` and retries are
   exhausted → escalate with `reasoning_unavailable`.
7. The reasoning provider MUST NOT be called for any path whose only
   reasonable downstream action is `admin-settle` / `admin-cancel`.
   Phase 3 never takes such paths; Phase 4+ remains responsible for
   enforcing this when those paths exist.

## Adapters

### OpenAI adapter (Phase 3 default)

- Transport: HTTPS via `reqwest`.
- Auth: bearer token sourced from `[reasoning].api_key_env`.
- Base URL: `[reasoning].api_base` (defaults to
  `https://api.openai.com/v1`).
- Model: `[reasoning].model`.
- Request shape: chat-completions style with JSON mode for
  classification; plain-text for summary with adapter-side parsing.
- Timeout: `[reasoning].request_timeout_seconds`.
- Retry: up to `[reasoning].followup_retry_count` adapter-level
  retries on transient errors (408, 429, 5xx) before surfacing
  `ReasoningError::Unreachable`. Permanent client errors (401 / 403
  / 404 / etc.) fail fast without consuming retries. The adapter is
  the single owner of this retry budget — it is NOT split with the
  mediation engine.

### OpenAI-compatible endpoints

Covered by the same adapter via `api_base` / `api_key_env` / `model`.
Known compatible targets include self-hosted gateways, router proxies,
and some third-party providers. Phase 3 ships only this adapter.

### Anthropic, PPQ.ai, OpenClaw

Enter as `NotYetImplemented` in `src/reasoning/not_yet_implemented.rs`
that immediately returns `ReasoningError::Unreachable("<provider> not
yet implemented in Phase 3")`. This keeps the mediation call sites
generic and ensures configuration attempts fail loudly instead of
silently falling through to the OpenAI adapter.

## Non-goals (Phase 3)

- Multi-provider routing / fallback.
- A trait-crate extraction.
- Streaming responses.
- Function-calling beyond JSON mode for classification.

These are Phase 5 territory.
