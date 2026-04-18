//! Reasoning provider request/response types (Phase 3).
//!
//! Mirrors `specs/003-guided-mediation/contracts/reasoning-provider.md`.
//! The adapter trait lives in `crate::reasoning`; this file only owns
//! the transport-agnostic data shape so the mediation call sites stay
//! provider-neutral.

use std::fmt;

use crate::models::mediation::{ClassificationLabel, Flag, TranscriptParty};

/// View over the loaded prompt bundle passed into reasoning calls.
/// Borrowed to avoid cloning large text payloads on every request.
#[derive(Debug, Clone, Copy)]
pub struct PromptBundleView<'a> {
    pub id: &'a str,
    pub policy_hash: &'a str,
    pub system: &'a str,
    pub classification_policy: &'a str,
    pub mediation_style: &'a str,
    pub message_templates: &'a str,
    pub escalation_policy: &'a str,
}

/// One transcript entry. Ordered by inner-event `created_at` per the
/// mediation transport contract.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub party: TranscriptParty,
    pub inner_event_created_at: i64,
    pub content: String,
}

/// Context shared across reasoning calls in the same session.
#[derive(Debug, Clone)]
pub struct ReasoningContext {
    pub round_count: u32,
    pub last_classification: Option<ClassificationLabel>,
    pub last_confidence: Option<f64>,
}

/// Initiator role on a dispute — mirrors Phase 1/2's enum but kept
/// local to the reasoning module to avoid a circular dependency
/// between models and reasoning adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitiatorRole {
    Buyer,
    Seller,
}

impl fmt::Display for InitiatorRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitiatorRole::Buyer => f.write_str("buyer"),
            InitiatorRole::Seller => f.write_str("seller"),
        }
    }
}

/// Classification request.
#[derive(Debug, Clone)]
pub struct ClassificationRequest {
    pub session_id: String,
    pub dispute_id: String,
    pub initiator_role: InitiatorRole,
    pub prompt_bundle_id: String,
    pub policy_hash: String,
    pub transcript: Vec<TranscriptEntry>,
    pub context: ReasoningContext,
}

/// Actions the reasoning provider may suggest. Always validated by
/// the policy layer before any side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestedAction {
    AskClarification(String),
    Summarize,
    Escalate(EscalationReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationReason(pub String);

/// Rationale text kept opaque so general logs never accidentally
/// embed it. Full contents go to the controlled audit store only
/// (FR-120).
#[derive(Debug, Clone)]
pub struct RationaleText(pub String);

/// Classification response.
#[derive(Debug, Clone)]
pub struct ClassificationResponse {
    pub classification: ClassificationLabel,
    pub confidence: f64,
    pub suggested_action: SuggestedAction,
    pub rationale: RationaleText,
    pub flags: Vec<Flag>,
}

/// Summary request.
#[derive(Debug, Clone)]
pub struct SummaryRequest {
    pub session_id: String,
    pub dispute_id: String,
    pub prompt_bundle_id: String,
    pub policy_hash: String,
    pub transcript: Vec<TranscriptEntry>,
    pub classification: ClassificationLabel,
    pub confidence: f64,
}

/// Summary response.
#[derive(Debug, Clone)]
pub struct SummaryResponse {
    pub summary_text: String,
    pub suggested_next_step: String,
    pub rationale: RationaleText,
}

/// Errors surfaced by the reasoning adapter. These are transport-
/// or response-shape errors; the policy layer still gets the final
/// say via the validation rules in the reasoning-provider contract.
#[derive(Debug, thiserror::Error)]
pub enum ReasoningError {
    #[error("reasoning provider unreachable: {0}")]
    Unreachable(String),

    #[error("reasoning provider timed out")]
    Timeout,

    #[error("reasoning provider returned malformed output: {0}")]
    MalformedResponse(String),

    /// The adapter detected the model suggesting an action that would
    /// cross the Phase 3 authority boundary (e.g. a settlement). The
    /// session MUST escalate with trigger `AuthorityBoundaryAttempt`.
    #[error("reasoning output would cross authority boundary: {0}")]
    AuthorityBoundaryViolation(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
