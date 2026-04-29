//! Reasoning provider request/response types (Phase 3).
//!
//! Mirrors `specs/003-guided-mediation/contracts/reasoning-provider.md`.
//! The adapter trait lives in `crate::reasoning`; this file only owns
//! the transport-agnostic data shape so the mediation call sites stay
//! provider-neutral.

use std::fmt;
use std::sync::Arc;

use crate::models::dispute::InitiatorRole;
use crate::models::mediation::{ClassificationLabel, Flag, TranscriptParty};
use crate::prompts::PromptBundle;

/// One transcript entry. Ordered by inner-event `created_at` per the
/// mediation transport contract.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub party: TranscriptParty,
    pub inner_event_created_at: i64,
    pub content: String,
}

/// Context shared across reasoning calls in the same session.
#[derive(Debug, Clone, Default)]
pub struct ReasoningContext {
    pub round_count: u32,
    pub last_classification: Option<ClassificationLabel>,
    pub last_confidence: Option<f64>,
    /// FR-008. `true` iff a `self_resolution_offered` audit row
    /// already exists for this session. Drives the conditional
    /// `human_requested` instruction block in the classifier prompt:
    /// when true, the prompt asks the model to flag explicit
    /// human-assistance requests so `policy::evaluate` can
    /// short-circuit to `Escalate(PartyRequestedHuman)`.
    pub session_has_self_resolution_offered: bool,
}

/// Classification request.
///
/// Carries the full `PromptBundle` (via `Arc` for cheap sharing
/// across sessions) rather than just its id+hash. The adapter MUST
/// feed the bundle bytes to the model: hardcoded system prompts
/// would break the `policy_hash` invariant (SC-103), because the
/// hash would reference bundle bytes the model never saw. `Arc`
/// avoids cloning the bundle text on every call.
#[derive(Debug, Clone)]
pub struct ClassificationRequest {
    pub session_id: String,
    pub dispute_id: String,
    pub initiator_role: InitiatorRole,
    pub prompt_bundle: Arc<PromptBundle>,
    pub transcript: Vec<TranscriptEntry>,
    pub context: ReasoningContext,
}

/// Actions the reasoning provider may suggest. Always validated by
/// the policy layer before any side effect.
///
/// `AskClarification` carries two distinct texts — one tailored for
/// the buyer, one for the seller. A single shared text was the
/// original shape but it broke in the wild (2026-04-21 Alice/Bob
/// run): the model sometimes addressed one role explicitly (e.g.
/// "Buyer: have you sent the fiat payment?") and that question went
/// to both parties, confusing the one it wasn't aimed at. Asking the
/// model for two messages and delivering each to its intended
/// recipient keeps both parties in the loop with questions that
/// make sense for their role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestedAction {
    AskClarification {
        buyer_text: String,
        seller_text: String,
    },
    Summarize,
    Escalate(EscalationReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationReason(pub String);

/// Rationale text kept opaque so general logs never accidentally
/// embed it. Full contents go to the controlled audit store only
/// (FR-120). The `Debug` impl redacts the inner contents — use
/// `.0` (or a dedicated audit-store API) to access the raw text,
/// and even then only when writing to the audit store, not to
/// general logs.
#[derive(Clone)]
pub struct RationaleText(pub String);

impl fmt::Debug for RationaleText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RationaleText(<{} bytes redacted>)", self.0.len())
    }
}

/// Classification response.
///
/// The three optional language / opt-in fields below are additive
/// extensions for Feature 005 (cooperative self-resolution). Adapters
/// that haven't been updated to emit them simply leave the defaults
/// (`false` / `None`); the policy short-circuit and the dispatch
/// arm both handle missing values gracefully.
#[derive(Debug, Clone)]
pub struct ClassificationResponse {
    pub classification: ClassificationLabel,
    pub confidence: f64,
    pub suggested_action: SuggestedAction,
    pub rationale: RationaleText,
    pub flags: Vec<Flag>,
    /// FR-008. Set by the classifier when a party reply explicitly
    /// asks for human assistance. Honoured by `policy::evaluate` only
    /// after a `self_resolution_offered` audit row exists for the
    /// session — that predicate guard prevents an adversarial party
    /// from skipping mediation by emitting human-assistance phrasing
    /// on round 0 and prevents a buggy provider from triggering
    /// escalation on rounds the prompt did not request the field.
    pub human_requested: bool,
    /// FR-002. ISO-639-1 code (`"en"`, `"es"`, `"pt"`, …) emitted by
    /// the classifier alongside `buyer_clarification`. `None` when
    /// the latest reply has no buyer content or is too short to
    /// disambiguate. Consumed by the cooperative-self-resolution
    /// dispatch arm to pick the right `[xx]` template section; falls
    /// back to `bundle.self_resolution.fallback_language` when
    /// absent.
    pub buyer_language: Option<String>,
    /// FR-002. Same shape as `buyer_language`, for the seller.
    pub seller_language: Option<String>,
    /// Structured cooperative-resolution guard signal emitted by the
    /// classifier after reading the transcript. `Some(true)` means the
    /// seller clearly confirmed receipt of the fiat payment;
    /// `Some(false)` means the seller clearly denied receipt; `None`
    /// means the transcript does not support a confident seller-side
    /// receipt determination yet.
    pub seller_confirmed_fiat_receipt: Option<bool>,
}

/// Summary request.
///
/// Same invariant as `ClassificationRequest`: carries the full
/// `PromptBundle` so the adapter actually sends the bytes the
/// `policy_hash` points at (SC-103).
#[derive(Debug, Clone)]
pub struct SummaryRequest {
    pub session_id: String,
    pub dispute_id: String,
    pub prompt_bundle: Arc<PromptBundle>,
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
