//! Phase 3 mediation models.
//!
//! Mirrors `specs/003-guided-mediation/data-model.md` §mediation_sessions
//! plus the enums referenced across the reasoning-provider contract.
//! The state machine here enforces the allowed transitions in the
//! data-model diagram; self-transitions are rejected, matching the
//! stricter discipline introduced in the Phase 2 review.

use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

/// Lifecycle state of a single mediation attempt for a dispute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediationSessionState {
    Opening,
    AwaitingResponse,
    Classified,
    FollowUpPending,
    SummaryPending,
    SummaryDelivered,
    EscalationRecommended,
    SupersededByHuman,
    Closed,
}

impl MediationSessionState {
    /// Enforce the allowed transitions from `data-model.md` §State
    /// Machine. Self-transitions are rejected (matching Phase 2's
    /// stricter discipline).
    pub fn can_transition_to(self, next: MediationSessionState) -> bool {
        use MediationSessionState::*;
        matches!(
            (self, next),
            // Initial progression
            (Opening, AwaitingResponse)
                | (AwaitingResponse, Classified)
                | (Classified, FollowUpPending)
                | (Classified, SummaryPending)
                | (FollowUpPending, AwaitingResponse)
                | (SummaryPending, SummaryDelivered)
                | (SummaryDelivered, Closed)
                // Escalation from any non-terminal state.
                | (Opening, EscalationRecommended)
                | (AwaitingResponse, EscalationRecommended)
                | (Classified, EscalationRecommended)
                | (FollowUpPending, EscalationRecommended)
                | (SummaryPending, EscalationRecommended)
                | (EscalationRecommended, Closed)
                // Superseded by human taking the dispute via Mostro.
                | (Opening, SupersededByHuman)
                | (AwaitingResponse, SupersededByHuman)
                | (Classified, SupersededByHuman)
                | (FollowUpPending, SupersededByHuman)
                | (SummaryPending, SupersededByHuman)
                | (SupersededByHuman, Closed)
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, MediationSessionState::Closed)
    }
}

impl fmt::Display for MediationSessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use MediationSessionState::*;
        let s = match self {
            Opening => "opening",
            AwaitingResponse => "awaiting_response",
            Classified => "classified",
            FollowUpPending => "follow_up_pending",
            SummaryPending => "summary_pending",
            SummaryDelivered => "summary_delivered",
            EscalationRecommended => "escalation_recommended",
            SupersededByHuman => "superseded_by_human",
            Closed => "closed",
        };
        f.write_str(s)
    }
}

impl FromStr for MediationSessionState {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        use MediationSessionState::*;
        match s {
            "opening" => Ok(Opening),
            "awaiting_response" => Ok(AwaitingResponse),
            "classified" => Ok(Classified),
            "follow_up_pending" => Ok(FollowUpPending),
            "summary_pending" => Ok(SummaryPending),
            "summary_delivered" => Ok(SummaryDelivered),
            "escalation_recommended" => Ok(EscalationRecommended),
            "superseded_by_human" => Ok(SupersededByHuman),
            "closed" => Ok(Closed),
            other => Err(Error::InvalidEvent(format!(
                "unknown mediation session state: {other}"
            ))),
        }
    }
}

/// Reasons a session may transition into `escalation_recommended`.
/// Aligned with `spec.md` FR-111 and the reasoning-provider contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationTrigger {
    ConflictingClaims,
    FraudIndicator,
    LowConfidence,
    PartyUnresponsive,
    RoundLimit,
    ReasoningUnavailable,
    AuthorizationLost,
    AuthorityBoundaryAttempt,
    MediationTimeout,
    PolicyBundleMissing,
    /// The reasoning provider returned a structurally inconsistent
    /// response (e.g. `SuggestedAction::Summarize` paired with a
    /// non-cooperative `ClassificationLabel`). Distinct from
    /// `ReasoningUnavailable` — the adapter round-trip succeeded,
    /// the model just produced something we refuse to act on. The
    /// operator alert shape is different (model-quality, not
    /// infra-health), so the trigger stays separate.
    InvalidModelOutput,
    /// The summary persisted but no solver DM could be delivered —
    /// either the configured recipient list resolved empty, or
    /// every send attempt failed at the relay. The session is
    /// escalated so a human operator can pick the summary up via
    /// the audit trail instead of leaving it stranded at
    /// `summary_pending`.
    NotificationFailed,
}

impl fmt::Display for EscalationTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use EscalationTrigger::*;
        let s = match self {
            ConflictingClaims => "conflicting_claims",
            FraudIndicator => "fraud_indicator",
            LowConfidence => "low_confidence",
            PartyUnresponsive => "party_unresponsive",
            RoundLimit => "round_limit",
            ReasoningUnavailable => "reasoning_unavailable",
            AuthorizationLost => "authorization_lost",
            AuthorityBoundaryAttempt => "authority_boundary_attempt",
            MediationTimeout => "mediation_timeout",
            PolicyBundleMissing => "policy_bundle_missing",
            InvalidModelOutput => "invalid_model_output",
            NotificationFailed => "notification_failed",
        };
        f.write_str(s)
    }
}

/// Roles that can author a transcript entry (or own a mediation_messages
/// row). Serbero itself is also a participant: it authors outbound
/// drafts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptParty {
    Buyer,
    Seller,
    Serbero,
}

impl fmt::Display for TranscriptParty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TranscriptParty::*;
        let s = match self {
            Buyer => "buyer",
            Seller => "seller",
            Serbero => "serbero",
        };
        f.write_str(s)
    }
}

/// Classification label emitted by the reasoning provider. `Unclear`
/// never means "just pick one" — it always escalates (see the
/// reasoning-provider contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationLabel {
    CoordinationFailureResolvable,
    ConflictingClaims,
    SuspectedFraud,
    Unclear,
    NotSuitableForMediation,
}

impl fmt::Display for ClassificationLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ClassificationLabel::*;
        let s = match self {
            CoordinationFailureResolvable => "coordination_failure_resolvable",
            ConflictingClaims => "conflicting_claims",
            SuspectedFraud => "suspected_fraud",
            Unclear => "unclear",
            NotSuitableForMediation => "not_suitable_for_mediation",
        };
        f.write_str(s)
    }
}

/// Flags surfaced alongside a classification. Every flag carries a
/// policy meaning — see `contracts/reasoning-provider.md`
/// §Policy-Layer Validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    FraudRisk,
    ConflictingClaims,
    LowInfo,
    UnresponsiveParty,
    AuthorityBoundaryAttempt,
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Flag::*;
        let s = match self {
            FraudRisk => "fraud_risk",
            ConflictingClaims => "conflicting_claims",
            LowInfo => "low_info",
            UnresponsiveParty => "unresponsive_party",
            AuthorityBoundaryAttempt => "authority_boundary_attempt",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::MediationSessionState::*;
    use super::*;

    #[test]
    fn allowed_transitions_pass() {
        assert!(Opening.can_transition_to(AwaitingResponse));
        assert!(AwaitingResponse.can_transition_to(Classified));
        assert!(Classified.can_transition_to(FollowUpPending));
        assert!(Classified.can_transition_to(SummaryPending));
        assert!(FollowUpPending.can_transition_to(AwaitingResponse));
        assert!(SummaryPending.can_transition_to(SummaryDelivered));
        assert!(SummaryDelivered.can_transition_to(Closed));
        assert!(Opening.can_transition_to(EscalationRecommended));
        assert!(EscalationRecommended.can_transition_to(Closed));
        assert!(AwaitingResponse.can_transition_to(SupersededByHuman));
        assert!(SupersededByHuman.can_transition_to(Closed));
    }

    #[test]
    fn disallowed_transitions_reject() {
        assert!(!Closed.can_transition_to(Opening));
        assert!(!SummaryDelivered.can_transition_to(AwaitingResponse));
        assert!(!Opening.can_transition_to(Closed));
        assert!(!AwaitingResponse.can_transition_to(AwaitingResponse)); // self
        assert!(!Classified.can_transition_to(Opening));
        assert!(!EscalationRecommended.can_transition_to(AwaitingResponse));
    }

    #[test]
    fn all_self_transitions_rejected() {
        for s in [
            Opening,
            AwaitingResponse,
            Classified,
            FollowUpPending,
            SummaryPending,
            SummaryDelivered,
            EscalationRecommended,
            SupersededByHuman,
            Closed,
        ] {
            assert!(
                !s.can_transition_to(s),
                "self-transition should be rejected for {s}"
            );
        }
    }

    #[test]
    fn parse_and_display_roundtrip_for_every_state() {
        for s in [
            "opening",
            "awaiting_response",
            "classified",
            "follow_up_pending",
            "summary_pending",
            "summary_delivered",
            "escalation_recommended",
            "superseded_by_human",
            "closed",
        ] {
            let parsed: MediationSessionState = s.parse().unwrap();
            assert_eq!(parsed.to_string(), s);
        }
    }
}
