//! Policy-layer validation of reasoning output.
//!
//! Owns the evaluator defined in
//! `contracts/reasoning-provider.md` §Policy-Layer Validation and
//! suppresses any suggestion that would cross the Phase 3 authority
//! boundary (fund actions, dispute closure). Those suppressed
//! outputs MUST escalate with trigger `AuthorityBoundaryAttempt`.
//!
//! This US1 slice ships [`initial_classification`], the entry point
//! the engine task calls right after [`crate::mediation::session::open_session`]
//! and before drafting the first clarifying message. It:
//!
//! 1. Builds a zero-transcript [`ClassificationRequest`] (no party
//!    replies exist yet on the opening call) and dispatches it to
//!    the configured [`ReasoningProvider`].
//! 2. Runs the five validation rules from the reasoning-provider
//!    contract in the documented order.
//! 3. Persists the rationale into the controlled audit store
//!    ([`crate::db::rationales`]) and emits a
//!    `classification_produced` event referencing the rationale by
//!    id only (FR-120: no raw text in general logs or event payloads).
//! 4. Returns a [`PolicyDecision`] the engine can dispatch on.
//!
//! On a provider-level [`ReasoningError`] the function does **not**
//! return `Err`: US1 treats every transport / timeout / malformed-
//! response failure as `Escalate(ReasoningUnavailable)` so the engine
//! loop can keep running. Only hard DB-side or rationale-store
//! failures surface as `Err` and terminate the current tick.

use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

use crate::db;
use crate::error::Result;
use crate::models::dispute::InitiatorRole;
use crate::models::mediation::{ClassificationLabel, EscalationTrigger, Flag};
use crate::models::reasoning::{
    ClassificationRequest, ClassificationResponse, ReasoningContext, SuggestedAction,
};
use crate::models::MediationConfig;
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

/// US1 hardcoded escalation threshold. Below this the session is
/// escalated with trigger [`EscalationTrigger::LowConfidence`].
/// Moved to config in US3+ per `tasks.md` §Foundational.
const LOW_CONFIDENCE_THRESHOLD: f64 = 0.5;

/// The three branches the engine dispatches on after policy
/// validation. Raw [`ClassificationResponse`] never leaves this
/// module — the engine only ever sees a validated decision.
#[derive(Debug, Clone, PartialEq)]
pub enum PolicyDecision {
    /// Ask each party a clarifying question. The two inner strings
    /// are the validated clarification texts the draft path will
    /// send to the buyer and seller respectively — one each, not
    /// broadcast. See [`SuggestedAction::AskClarification`] for the
    /// rationale behind per-party texts.
    AskClarification {
        buyer_text: String,
        seller_text: String,
    },
    /// Cooperative resolution path (US3). Carries the classification
    /// label and confidence so the engine can call the summarizer
    /// without having to re-read the classification_produced event.
    Summarize {
        classification: crate::models::mediation::ClassificationLabel,
        confidence: f64,
    },
    /// FR-001 — cooperative self-resolution invitation.
    ///
    /// Triggered when the model classifies a session as
    /// `coordination_failure_resolvable` with `suggested_action =
    /// summarize` AND the configured confidence floor is met AND the
    /// kill-switch is on AND no prior `self_resolution_offered` audit
    /// row exists for the session. The dispatch arm sends a
    /// per-party templated invitation in each detected language
    /// (with a human-assistance opt-in sentence) AND delivers the
    /// existing solver summary with `suggested_next_step =
    /// "self_resolution_offered_to_parties"`. When any of the
    /// pre-conditions fails, the legacy `Summarize` branch fires
    /// instead — preserving byte-for-byte legacy behaviour with the
    /// kill-switch off (SC-007).
    SuggestSelfResolutionWithSummary { confidence: f64 },
    /// Escalate to a human solver with the given trigger. The
    /// mediation engine translates this into a Phase 4 handoff.
    Escalate(EscalationTrigger),
}

/// Run the initial classification call for a just-opened session.
///
/// Persists the rationale and emits a `classification_produced`
/// audit event before returning. Validation order (critical signals
/// first so they are never shadowed by softer ones):
///
/// 1. `FraudRisk` / `ConflictingClaims` flags escalate regardless
///    of the suggested action.
/// 2. `AuthorityBoundaryAttempt` flag escalates with
///    `AuthorityBoundaryAttempt` — runs BEFORE low-confidence /
///    model-suggested-escalate so the trigger is preserved verbatim.
/// 3. Confidence below [`LOW_CONFIDENCE_THRESHOLD`] escalates with
///    `LowConfidence`.
/// 4. A provider-suggested `Escalate(_)` is escalated under
///    `ReasoningUnavailable` (no free-form escalation reasons for
///    the mediation engine).
/// 5. An `AskClarification(text)` whose text is empty / whitespace
///    is treated as a malformed response and escalated under
///    `ReasoningUnavailable` — blank text reaching the draft path
///    would produce a meaningless outbound message.
/// 6. Otherwise the classification's `suggested_action` is mapped
///    directly to a [`PolicyDecision`].
#[allow(clippy::too_many_arguments)]
pub async fn initial_classification(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    dispute_id: &str,
    initiator_role: InitiatorRole,
    prompt_bundle: &Arc<PromptBundle>,
    reasoning: &dyn ReasoningProvider,
    provider_name: &str,
    model_name: &str,
) -> Result<PolicyDecision> {
    let request = ClassificationRequest {
        session_id: session_id.to_string(),
        dispute_id: dispute_id.to_string(),
        initiator_role,
        prompt_bundle: Arc::clone(prompt_bundle),
        transcript: Vec::new(),
        context: ReasoningContext {
            round_count: 0,
            last_classification: None,
            last_confidence: None,
            session_has_self_resolution_offered: false,
        },
    };

    let classification = match reasoning.classify(request).await {
        Ok(response) => response,
        Err(e) => {
            // Every provider-level error on the opening call maps
            // to `reasoning_unavailable` escalation. No rationale
            // (there is no text to content-hash) and no
            // `classification_produced` event — but we DO emit a
            // `reasoning_call_failed` audit row (T070 / T074) so
            // the operator dashboard can distinguish an infra
            // failure from a silent "no classification event
            // emitted" gap.
            //
            // The audit payload stores a stable `error_category`
            // rather than the raw `e.to_string()` so operators have
            // a bounded tag space to alert on and so adapter-side
            // identifiers (URLs, IP addresses, internal host names)
            // never leak into `mediation_events`. The full message
            // still goes to `warn!` below for in-process logs.
            let now = current_ts_secs()?;
            let payload = serde_json::json!({
                "provider": provider_name,
                "model": model_name,
                "attempt_count": 1,
                "error_category": reasoning_error_category(&e),
            })
            .to_string();
            {
                let guard = conn.lock().await;
                if let Err(db_err) = db::mediation_events::record_event(
                    &guard,
                    db::mediation_events::MediationEventKind::ReasoningCallFailed,
                    Some(session_id),
                    &payload,
                    None,
                    Some(&prompt_bundle.id),
                    Some(&prompt_bundle.policy_hash),
                    now,
                ) {
                    // Best-effort: a failed audit write must not
                    // hide the escalation from the engine. Log
                    // and still return the escalation decision.
                    warn!(
                        session_id = %session_id,
                        error = %db_err,
                        "failed to record reasoning_call_failed event"
                    );
                }
            }
            warn!(
                session_id = %session_id,
                error = %e,
                "reasoning.classify failed on initial classification; escalating as reasoning_unavailable"
            );
            return Ok(PolicyDecision::Escalate(
                EscalationTrigger::ReasoningUnavailable,
            ));
        }
    };

    let decision = classify_to_decision(&classification, PolicyRound::Initial);

    // Persist rationale + emit audit event BEFORE returning: the
    // decision is only legitimate once the audit trail is durable.
    let rationale_id = persist_classification_audit(
        conn,
        session_id,
        prompt_bundle,
        provider_name,
        model_name,
        &classification,
    )
    .await?;

    debug!(
        session_id = %session_id,
        classification = %classification.classification,
        confidence = classification.confidence,
        rationale_id = %rationale_id,
        ?decision,
        "initial classification persisted"
    );

    Ok(decision)
}

/// Re-run the policy evaluator against a classification that was
/// produced mid-session (after at least one inbound round).
///
/// Identical rule table to [`initial_classification`] — the only
/// difference is the entry point: `evaluate` does NOT call the
/// reasoning provider. The caller supplies an already-produced
/// [`ClassificationResponse`] (from a scripted provider in tests, or
/// from a mid-session reasoning call the engine owns). This keeps the
/// "where does the call happen" decision outside the policy layer
/// while the "is this response acceptable" decision stays inside.
///
/// Audit writes are identical: one rationale row + one
/// `classification_produced` event, both inside a single transaction
/// so the decision is only surfaced once the audit trail is durable.
///
/// Like [`initial_classification`], `evaluate` does NOT send any
/// outbound chat on its own — the caller dispatches on the returned
/// [`PolicyDecision`].
///
/// `followup_number` is 1-based and identifies which mid-session
/// evaluation this is for the session. It governs the low-confidence
/// bypass window — see [`EARLY_MIDSESSION_BYPASS_FOLLOWUPS`].
#[allow(clippy::too_many_arguments)]
pub async fn evaluate(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    prompt_bundle: &Arc<PromptBundle>,
    provider_name: &str,
    model_name: &str,
    classification: ClassificationResponse,
    followup_number: u32,
    mediation_cfg: &MediationConfig,
) -> Result<PolicyDecision> {
    // Persist the audit trail FIRST so any returned decision is
    // already durable. Identical to the legacy shape — the new
    // branches below only choose a different variant; they do not
    // skip the rationale row.
    let rationale_id = persist_classification_audit(
        conn,
        session_id,
        prompt_bundle,
        provider_name,
        model_name,
        &classification,
    )
    .await?;

    // Predicate guard for both Feature 005 branches.
    //
    // No TOCTOU window here in single-process operation, even
    // though the predicate read and the eventual write in
    // `follow_up::draft_and_send_self_resolution_invitation` happen
    // in two different `conn.lock().await` regions: the mediation
    // engine spawns exactly one tokio task that runs ticks
    // sequentially in a loop, `run_ingest_tick` processes sessions
    // serially via `while let Some(res) = fetchers.join_next().await
    // { ... advance_session_round(...).await }`, and a single call
    // to `advance_session_round` always completes (predicate +
    // dispatch write) before the next call to it for any session
    // can begin. A per-session UNIQUE partial index would be the
    // belt-and-braces defence for an HA / multi-process deploy, but
    // (a) HA is out of scope per `plan.md`, and (b) such an index
    // would require a new SQL migration which `plan.md` also
    // forbids as a feature goal — see
    // `specs/005-cooperative-self-resolution/plan.md` §"strictly
    // additive: no DB migration".
    let prior_offered = {
        let guard = conn.lock().await;
        db::mediation_events::session_has_self_resolution_offered(&guard, session_id)?
    };

    // Branch 1 (FR-008, T023): explicit human-assistance request
    // after the cooperative invitation. Short-circuits the
    // classification-label dispatch so a `conflicting_claims` round
    // that ALSO carries `human_requested = true` still escalates as
    // `PartyRequestedHuman` — the party's explicit ask wins. The
    // predicate guard scopes the short-circuit to sessions that
    // actually received the invitation, so an adversarial party
    // cannot use the field to skip mediation on round 0 and a buggy
    // provider that emits the field where the prompt did not request
    // it cannot accidentally trigger the path.
    if classification.human_requested && prior_offered {
        debug!(
            session_id = %session_id,
            rationale_id = %rationale_id,
            "evaluate: human_requested + prior self_resolution_offered → escalate(party_requested_human)"
        );
        return Ok(PolicyDecision::Escalate(
            EscalationTrigger::PartyRequestedHuman,
        ));
    }

    let base_decision =
        classify_to_decision(&classification, PolicyRound::MidSession { followup_number });

    // Branch 2 (FR-001 / FR-006 / FR-010 / FR-011, T016): cooperative
    // self-resolution rewrite. Triggered when the legacy
    // `classify_to_decision` would have returned a cooperative
    // `Summarize` AND every feature pre-condition holds. When any
    // pre-condition fails the legacy decision passes through
    // unchanged — preserving byte-for-byte legacy behaviour with the
    // kill-switch off (SC-007).
    // Bundle-availability gate. A daemon that hasn't shipped
    // `prompts/phase3-self-resolution.md` yet loads with empty
    // templates and `render_for` would otherwise emit the operator
    // placeholder to end users. Falling through to the legacy
    // Summarize keeps the user-facing surface clean — the cooperative
    // branch becomes inert until the file lands.
    let templates_present = !prompt_bundle.self_resolution.by_language.is_empty();

    let decision = match base_decision {
        PolicyDecision::Summarize {
            classification: ClassificationLabel::CoordinationFailureResolvable,
            confidence,
        } if mediation_cfg.self_resolution_enabled
            && templates_present
            // Compare in f64 to avoid losing precision on the
            // higher-precision classifier confidence — the threshold
            // is f32 by config-contract but the comparison stays
            // honest.
            && confidence >= f64::from(mediation_cfg.self_resolution_threshold)
            && !prior_offered =>
        {
            debug!(
                session_id = %session_id,
                rationale_id = %rationale_id,
                confidence,
                threshold = mediation_cfg.self_resolution_threshold,
                "evaluate: cooperative self-resolution branch eligible → SuggestSelfResolutionWithSummary"
            );
            PolicyDecision::SuggestSelfResolutionWithSummary { confidence }
        }
        other => other,
    };

    debug!(
        session_id = %session_id,
        classification = %classification.classification,
        confidence = classification.confidence,
        rationale_id = %rationale_id,
        ?decision,
        "evaluate: mid-session classification persisted"
    );

    Ok(decision)
}

/// Where in the session lifecycle a classification was produced.
/// Drives whether low-confidence escalates on an otherwise-benign
/// `AskClarification` suggestion.
///
/// Two escalation-bypass windows exist:
///
/// - `Initial`: opening call, transcript is empty by construction,
///   the model has nothing to be confident about. A clarifying
///   question is the expected next step.
/// - `MidSession { followup_number }` with
///   `followup_number <= EARLY_MIDSESSION_BYPASS_FOLLOWUPS`: the first
///   few mid-session evaluations frequently come back low-confidence
///   because we only have one party's partial info. Escalating there
///   hands off to a solver before Serbero has made a real attempt.
///   After the bypass window, persistent low confidence is a genuine
///   "we tried and can't resolve" signal — escalation is correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyRound {
    /// Very first classification for a freshly-taken dispute. No
    /// inbound messages from either party yet.
    Initial,
    /// Later round. `followup_number` is 1-based: 1 = first
    /// mid-session evaluation, 2 = second, etc.
    MidSession { followup_number: u32 },
}

/// Number of mid-session follow-ups that bypass the low-confidence
/// escalation rule for `AskClarification` suggestions. Follow-ups
/// 1..=N pass through; follow-up N+1 onward escalate as before.
///
/// Rationale:
/// - 2026-04-21 Alice/Bob run: gpt-5 returned confidence 0.31 after
///   the first single-party reply, which escalated the session to a
///   solver before Serbero ever asked a second clarifying question.
///   The original window of 3 was the first-cut sweet spot.
/// - 2026-04-29 Bob run (buyer claimed "ya envié el dinero" without
///   proof, seller had not corroborated receipt): the model honestly
///   returned `AskClarification` with confidence 0.20 → 0.24 → 0.24
///   → 0.28 across four mid-session rounds, and the 4th tripped the
///   bypass and escalated as `LowConfidence` even though the right
///   move was to keep asking the buyer for the receipt. The model's
///   confidence trajectory was rising, but never crossed 0.5; with a
///   window of 3 the policy never gave it room to extract the proof.
///
/// 6 is the new empirical sweet spot: doubles the budget so the model
/// can keep pulling on a "buyer claims paid, no proof yet" or similar
/// evidence-gathering loop, while still surfacing a session truly
/// stuck in ambiguity to a human within a bounded number of rounds.
pub const EARLY_MIDSESSION_BYPASS_FOLLOWUPS: u32 = 6;

/// Pure validation: run the contract rules against a single
/// [`ClassificationResponse`] and return the resulting decision.
/// Extracted so unit tests can exercise the rule table without
/// setting up a DB / rationale store.
///
/// The `round` argument differentiates two otherwise-identical rule
/// paths. See [`PolicyRound`] for the rationale.
pub(crate) fn classify_to_decision(
    classification: &ClassificationResponse,
    round: PolicyRound,
) -> PolicyDecision {
    // Rule 1: fraud / conflicting-claims flags dominate every other
    // signal. Both are explicit "this dispute does not belong in
    // guided mediation" indicators from the model.
    if classification.flags.contains(&Flag::FraudRisk) {
        return PolicyDecision::Escalate(EscalationTrigger::FraudIndicator);
    }
    if classification.flags.contains(&Flag::ConflictingClaims) {
        return PolicyDecision::Escalate(EscalationTrigger::ConflictingClaims);
    }

    // Authority-boundary suppression runs BEFORE the low-confidence
    // and model-suggested-escalate checks. Losing the
    // `AuthorityBoundaryAttempt` trigger to `LowConfidence` would
    // weaken the audit story: the operator needs to see *why* the
    // response was suppressed, not a generic "confidence too low"
    // tag. An adapter that detects an authority-boundary attempt
    // may surface it via either the flags vector or (for future
    // adapter-specific shapes) the `suggested_action` string.
    if classification
        .flags
        .contains(&Flag::AuthorityBoundaryAttempt)
    {
        return PolicyDecision::Escalate(EscalationTrigger::AuthorityBoundaryAttempt);
    }

    // Low-confidence gate. Two windows bypass this gate for
    // `AskClarification` suggestions — see [`PolicyRound`]:
    //
    // - Opening round: transcript is empty, the model has nothing to
    //   anchor its confidence on. Escalating there prevents Serbero
    //   from ever talking to the parties (observed 2026-04-21 with
    //   `gpt-5` returning ~0.3 on empty transcripts).
    // - First `EARLY_MIDSESSION_BYPASS_FOLLOWUPS` mid-session
    //   evaluations: the model usually only has one party's partial
    //   reply by then; escalating to a solver after a single follow-up
    //   hand-offs before Serbero has actually tried to mediate
    //   (observed 2026-04-21 Alice/Bob run, confidence 0.31 after
    //   buyer's first reply triggered LowConfidence).
    //
    // Past the bypass window, sustained low confidence means
    // "Serbero tried and cannot get traction" — escalation is the
    // right outcome. For non-`AskClarification` actions the gate
    // always applies: an unconfident `Summarize` is a terminal
    // commitment Serbero must not make, even in the bypass window.
    let in_bypass_window = match round {
        PolicyRound::Initial => true,
        PolicyRound::MidSession { followup_number } => {
            followup_number <= EARLY_MIDSESSION_BYPASS_FOLLOWUPS
        }
    };
    let passthrough_low_confidence_ask_clarification = in_bypass_window
        && matches!(
            classification.suggested_action,
            SuggestedAction::AskClarification { .. }
        );
    if classification.confidence < LOW_CONFIDENCE_THRESHOLD
        && !passthrough_low_confidence_ask_clarification
    {
        return PolicyDecision::Escalate(EscalationTrigger::LowConfidence);
    }

    // Model-suggested escalation funnels into `ReasoningUnavailable`.
    // The mediation engine does not propagate adapter-free-form
    // escalation reasons; US4 owns the finer-grained triggers.
    if let SuggestedAction::Escalate(_) = &classification.suggested_action {
        return PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable);
    }

    // Pass-through: map the suggested action to the decision.
    match &classification.suggested_action {
        SuggestedAction::AskClarification {
            buyer_text,
            seller_text,
        } => {
            // Reject empty / whitespace-only clarifications — the
            // session-open draft path cannot build a meaningful
            // gift-wrap from them, and letting the outbound message
            // go as literal "Buyer: " / "Seller: " would be worse
            // than escalating. Treated the same as a malformed
            // provider response (rule 6 in the contract). Either
            // side being blank is enough to escalate: we cannot
            // silently drop one party for the round because the
            // session bookkeeping assumes both outbound rows land.
            if buyer_text.trim().is_empty() || seller_text.trim().is_empty() {
                return PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable);
            }
            PolicyDecision::AskClarification {
                buyer_text: buyer_text.clone(),
                seller_text: seller_text.clone(),
            }
        }
        SuggestedAction::Summarize => {
            // Cross-check the classification label before trusting
            // the model's `Summarize` suggestion. The only label
            // that maps to a cooperative summary is
            // `CoordinationFailureResolvable`; any other label
            // combined with `Summarize` is an inconsistent
            // response (e.g. `SuspectedFraud` + "summarize this")
            // — a structural bug in the model output, not an
            // infrastructure failure. `ReasoningUnavailable` is
            // reserved for adapter / transport issues (provider
            // down), so mapping inconsistent output there would
            // drown model-quality alerts in infra-health noise.
            // `InvalidModelOutput` is the dedicated trigger.
            use crate::models::mediation::ClassificationLabel;
            match classification.classification {
                ClassificationLabel::CoordinationFailureResolvable => PolicyDecision::Summarize {
                    classification: classification.classification,
                    confidence: classification.confidence,
                },
                _ => PolicyDecision::Escalate(EscalationTrigger::InvalidModelOutput),
            }
        }
        // Unreachable because the rule above already handled this
        // case; kept defensive so an accidental enum widening does
        // not silently bypass the escalation path.
        SuggestedAction::Escalate(_) => {
            PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable)
        }
    }
}

/// Rationale audit handle returned by [`classify_for_start`] so the
/// caller can re-bind the rationale row to the session after the
/// take-dispute step succeeds, without having to re-supply the
/// classification label / confidence to the audit writer.
#[derive(Debug, Clone)]
pub struct RationaleAudit {
    /// SHA-256 content hash of the rationale text, matching the
    /// `rationale_id` primary key in `reasoning_rationales`.
    pub rationale_id: String,
    /// Persisted alongside `rationale_id` so
    /// [`record_classification_for_session`] can emit the
    /// `classification_produced` event without the caller holding
    /// onto the full [`ClassificationResponse`].
    pub classification: crate::models::mediation::ClassificationLabel,
    pub confidence: f64,
}

/// Outcome of the opening-call reasoning step (FR-122 step 2).
///
/// `decision` drives the next step of `open_session`:
/// - `AskClarification` / `Summarize` → proceed to `TakeDispute`
///   (the take is strictly gated on this positive verdict).
/// - `Escalate(trigger)` → no take, no session row; the caller
///   writes the dispute-scoped handoff via
///   [`escalation::recommend`](super::escalation::recommend) with
///   `session_id = None`.
///
/// `rationale_audit` is `None` only on the transport-error branch
/// (provider unreachable / timeout / malformed response). In that
/// case the decision is always `Escalate(ReasoningUnavailable)` and
/// there is no rationale text to content-hash.
#[derive(Debug, Clone)]
pub struct ClassifyForStartOutcome {
    pub decision: PolicyDecision,
    pub rationale_audit: Option<RationaleAudit>,
}

/// FR-122 step 2: run the opening classification BEFORE any chat-
/// transport or take step. Persists the rationale dispute-scoped
/// (`reasoning_rationales.session_id = NULL`) and returns both the
/// policy decision and a [`RationaleAudit`] handle the caller can
/// pass to [`record_classification_for_session`] after the session
/// row is committed. On a reasoning-provider error, records
/// `reasoning_call_failed` dispute-scoped and returns
/// `Escalate(ReasoningUnavailable)` with no audit handle.
///
/// Does NOT write a `classification_produced` event — that row is
/// session-scoped by design (the kind's schema expects a non-null
/// `session_id`) and is emitted by
/// [`record_classification_for_session`] once the session row
/// exists. The rationale is content-hashed so a retried call does
/// not duplicate rows.
#[allow(clippy::too_many_arguments)]
pub async fn classify_for_start(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    initiator_role: InitiatorRole,
    prompt_bundle: &Arc<PromptBundle>,
    reasoning: &dyn ReasoningProvider,
    provider_name: &str,
    model_name: &str,
) -> Result<ClassifyForStartOutcome> {
    let request = ClassificationRequest {
        // `ClassificationRequest` predates the split — its
        // `session_id` field is informational and the downstream
        // adapter only puts it in prompt metadata. Pass the
        // dispute_id so logs are useful even before a session row
        // exists.
        session_id: dispute_id.to_string(),
        dispute_id: dispute_id.to_string(),
        initiator_role,
        prompt_bundle: Arc::clone(prompt_bundle),
        transcript: Vec::new(),
        context: ReasoningContext {
            round_count: 0,
            last_classification: None,
            last_confidence: None,
            session_has_self_resolution_offered: false,
        },
    };

    let classification = match reasoning.classify(request).await {
        Ok(response) => response,
        Err(e) => {
            // Same dispute-scoped `reasoning_call_failed` shape as
            // the session-scoped path in `initial_classification`,
            // but with `session_id = None` because the session row
            // does not exist yet on the opening reasoning-first
            // path (FR-122).
            let now = current_ts_secs()?;
            let payload = serde_json::json!({
                "dispute_id": dispute_id,
                "provider": provider_name,
                "model": model_name,
                "attempt_count": 1,
                "error_category": reasoning_error_category(&e),
            })
            .to_string();
            {
                let guard = conn.lock().await;
                if let Err(db_err) = db::mediation_events::record_event(
                    &guard,
                    db::mediation_events::MediationEventKind::ReasoningCallFailed,
                    None,
                    &payload,
                    None,
                    Some(&prompt_bundle.id),
                    Some(&prompt_bundle.policy_hash),
                    now,
                ) {
                    warn!(
                        dispute_id = %dispute_id,
                        error = %db_err,
                        "failed to record dispute-scoped reasoning_call_failed event"
                    );
                }
            }
            warn!(
                dispute_id = %dispute_id,
                error = %e,
                "classify_for_start: reasoning.classify failed; escalating as reasoning_unavailable"
            );
            return Ok(ClassifyForStartOutcome {
                decision: PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable),
                rationale_audit: None,
            });
        }
    };

    let decision = classify_to_decision(&classification, PolicyRound::Initial);

    // Persist rationale dispute-scoped (`session_id = NULL`). The
    // content-hash dedup on `reasoning_rationales` keeps retries
    // idempotent. No `classification_produced` event here — it
    // belongs to the session scope and `record_classification_for_session`
    // writes it after the session row exists.
    let now = current_ts_secs()?;
    let rationale_id = {
        let guard = conn.lock().await;
        db::rationales::insert_rationale(
            &guard,
            None,
            provider_name,
            model_name,
            &prompt_bundle.id,
            &prompt_bundle.policy_hash,
            &classification.rationale.0,
            now,
        )?
    };

    debug!(
        dispute_id = %dispute_id,
        classification = %classification.classification,
        confidence = classification.confidence,
        rationale_id = %rationale_id,
        ?decision,
        "classify_for_start: rationale persisted dispute-scoped"
    );

    Ok(ClassifyForStartOutcome {
        decision,
        rationale_audit: Some(RationaleAudit {
            rationale_id,
            classification: classification.classification,
            confidence: classification.confidence,
        }),
    })
}

/// Bind a rationale previously persisted by [`classify_for_start`]
/// to a freshly-committed `mediation_sessions` row, and emit the
/// session-scoped `classification_produced` audit event.
///
/// One transaction: (1) UPDATE `reasoning_rationales` setting
/// `session_id = ?1` WHERE the row still has `session_id IS NULL`
/// (so a replay after a race is a no-op), (2) insert the
/// `classification_produced` event with the same session_id. A
/// crash between the two is impossible; if the tx rolls back the
/// session ends up without a classification_produced row and the
/// next tick will either re-classify (T121 retry path) or escalate
/// via FR-130 after three failures.
///
/// The caller supplies the `RationaleAudit` it received from
/// `classify_for_start` — the rationale id is content-addressed, so
/// a re-bind is safe even under retry.
pub async fn record_classification_for_session(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    audit: &RationaleAudit,
    prompt_bundle: &Arc<PromptBundle>,
) -> Result<()> {
    let now = current_ts_secs()?;
    let mut guard = conn.lock().await;
    let tx = guard.transaction()?;

    // Re-bind the rationale. The `AND session_id IS NULL` clause
    // makes the UPDATE idempotent: a retry after the row has
    // already been bound is a no-op (zero rows affected, no
    // error). We do NOT assert the exact row count here because a
    // legitimate retry returns 0.
    tx.execute(
        "UPDATE reasoning_rationales
         SET session_id = ?1
         WHERE rationale_id = ?2 AND session_id IS NULL",
        rusqlite::params![session_id, &audit.rationale_id],
    )?;

    db::mediation_events::record_classification_produced(
        &tx,
        session_id,
        &audit.rationale_id,
        &audit.classification.to_string(),
        audit.confidence,
        Some(&prompt_bundle.id),
        Some(&prompt_bundle.policy_hash),
        now,
    )?;

    tx.commit()?;

    debug!(
        session_id = %session_id,
        rationale_id = %audit.rationale_id,
        classification = %audit.classification,
        confidence = audit.confidence,
        "record_classification_for_session: rationale rebound + classification_produced emitted"
    );

    Ok(())
}

/// Persist the rationale + emit the `classification_produced`
/// audit event for one classification response inside a single
/// transaction. Returns the content-addressed rationale id.
///
/// Single source of truth for the audit-write path shared by
/// [`initial_classification`] and [`evaluate`]. A crash between the
/// two inserts is impossible because the transaction either lands
/// both or neither, and the content-hash dedup on
/// `reasoning_rationales` makes a retried call idempotent.
async fn persist_classification_audit(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    prompt_bundle: &Arc<PromptBundle>,
    provider_name: &str,
    model_name: &str,
    classification: &ClassificationResponse,
) -> Result<String> {
    let now = current_ts_secs()?;
    let mut guard = conn.lock().await;
    let tx = guard.transaction()?;
    let rationale_id = db::rationales::insert_rationale(
        &tx,
        Some(session_id),
        provider_name,
        model_name,
        &prompt_bundle.id,
        &prompt_bundle.policy_hash,
        &classification.rationale.0,
        now,
    )?;
    db::mediation_events::record_classification_produced(
        &tx,
        session_id,
        &rationale_id,
        &classification.classification.to_string(),
        classification.confidence,
        Some(&prompt_bundle.id),
        Some(&prompt_bundle.policy_hash),
        now,
    )?;
    tx.commit()?;
    Ok(rationale_id)
}

/// Map a [`ReasoningError`] to a short, stable tag persisted in the
/// `reasoning_call_failed` audit payload. Keeping the tag space
/// bounded lets operator dashboards alert on categories without
/// parsing free-form provider error strings, and keeps adapter
/// internals (URLs, IPs, keys) out of the audit table.
fn reasoning_error_category(err: &crate::models::reasoning::ReasoningError) -> &'static str {
    use crate::models::reasoning::ReasoningError::*;
    match err {
        Unreachable(_) => "unreachable",
        Timeout => "timeout",
        MalformedResponse(_) => "malformed_response",
        AuthorityBoundaryViolation(_) => "authority_boundary_violation",
        Other(_) => "unknown",
    }
}

// Clock access is delegated to `super::current_ts_secs()` so this
// module, `session.rs`, `summarizer.rs`, and the engine in `mod.rs`
// share one implementation of the "system clock is before UNIX_EPOCH"
// guard. Failure surfaces as `Error::ChatTransport` so the engine
// loop keeps running — a pre-epoch clock is operator-facing, not a
// reason to panic the mediation tick.
use super::current_ts_secs;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex as SyncMutex;

    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::models::mediation::{ClassificationLabel, Flag};
    use crate::models::reasoning::{
        ClassificationResponse, EscalationReason, RationaleText, ReasoningError, SummaryRequest,
        SummaryResponse,
    };
    use crate::prompts::PromptBundle;

    fn test_bundle() -> Arc<PromptBundle> {
        // Populate one language entry so the cooperative-branch
        // tests below see a non-empty `by_language` map. Tests that
        // exercise the empty-bundle inert behavior construct their
        // own bundle inline.
        let mut by_language = std::collections::HashMap::new();
        by_language.insert(
            "en".to_string(),
            crate::mediation::self_resolution::SelfResolutionLanguageEntry {
                template: "test invitation".into(),
                human_assistance_optin: "test optin".into(),
            },
        );
        Arc::new(PromptBundle {
            id: "phase3-default".into(),
            policy_hash: "test-policy-hash".into(),
            system: "sys".into(),
            classification: "cls".into(),
            escalation: "esc".into(),
            mediation_style: "style".into(),
            message_templates: "tpl".into(),
            self_resolution: crate::mediation::self_resolution::SelfResolutionTemplates {
                by_language,
                fallback_language: "en".into(),
            },
        })
    }

    fn base_response() -> ClassificationResponse {
        ClassificationResponse {
            classification: ClassificationLabel::CoordinationFailureResolvable,
            confidence: 0.9,
            suggested_action: SuggestedAction::AskClarification {
                buyer_text: "please confirm X (buyer)".into(),
                seller_text: "please confirm X (seller)".into(),
            },
            rationale: RationaleText("rationale body".into()),
            flags: Vec::new(),
            human_requested: false,
            buyer_language: None,
            seller_language: None,
            seller_confirmed_fiat_receipt: None,
        }
    }

    /// Scripted provider — one queued response / error consumed per
    /// `classify` call.
    struct ScriptedProvider {
        next: SyncMutex<Option<std::result::Result<ClassificationResponse, ReasoningError>>>,
    }

    impl ScriptedProvider {
        fn ok(response: ClassificationResponse) -> Self {
            Self {
                next: SyncMutex::new(Some(Ok(response))),
            }
        }
        fn err(err: ReasoningError) -> Self {
            Self {
                next: SyncMutex::new(Some(Err(err))),
            }
        }
    }

    #[async_trait]
    impl ReasoningProvider for ScriptedProvider {
        async fn classify(
            &self,
            _request: ClassificationRequest,
        ) -> std::result::Result<ClassificationResponse, ReasoningError> {
            self.next
                .lock()
                .unwrap()
                .take()
                .expect("classify called twice; scripted provider only has one entry")
        }
        async fn summarize(
            &self,
            _request: SummaryRequest,
        ) -> std::result::Result<SummaryResponse, ReasoningError> {
            panic!("summarize not expected in policy tests")
        }
        async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
            Ok(())
        }
    }

    fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        // FK: session row needs a parent dispute + session.
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d1', 'e1', 'm1', 'buyer', 'initiated', 1, 2, 'notified')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                started_at, last_transition_at
             ) VALUES ('sess-policy', 'd1', 'awaiting_response', 0,
                       'phase3-default', 'test-policy-hash', 100, 100)",
            [],
        )
        .unwrap();
        Arc::new(AsyncMutex::new(conn))
    }

    async fn run_initial(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        provider: &dyn ReasoningProvider,
    ) -> Result<PolicyDecision> {
        let bundle = test_bundle();
        initial_classification(
            conn,
            "sess-policy",
            "d1",
            InitiatorRole::Buyer,
            &bundle,
            provider,
            "openai",
            "gpt-test",
        )
        .await
    }

    #[tokio::test]
    async fn fraud_risk_flag_escalates_regardless_of_action() {
        let conn = fresh_conn();
        let mut resp = base_response();
        // Suggested action is still AskClarification — the fraud flag
        // MUST dominate.
        resp.flags = vec![Flag::FraudRisk];
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::FraudIndicator)
        );
    }

    #[tokio::test]
    async fn low_confidence_with_summarize_escalates_on_initial() {
        // The low-confidence gate still fires on the initial round
        // for consumative suggested actions (Summarize). Only
        // AskClarification is allowed through on low confidence,
        // because an extra clarifying round is cheap while an
        // unconfident summary is a terminal commitment.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.3;
        resp.suggested_action = SuggestedAction::Summarize;
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::LowConfidence)
        );
    }

    #[tokio::test]
    async fn initial_round_passes_low_confidence_ask_clarification() {
        // Production case observed 2026-04-21: `gpt-5` returned
        // `AskClarification(...)` with confidence 0.3 on an empty
        // transcript (nothing else to be confident about). The old
        // policy escalated on LowConfidence and the session never
        // reached the parties. The new rule lets the clarifying
        // round proceed; if the next round is still confused, the
        // mid-session gate applies as usual.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.3; // below LOW_CONFIDENCE_THRESHOLD
                               // base_response already sets AskClarification("please confirm X").
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::AskClarification {
                buyer_text: "please confirm X (buyer)".into(),
                seller_text: "please confirm X (seller)".into(),
            }
        );
    }

    #[tokio::test]
    async fn model_suggested_escalate_maps_to_reasoning_unavailable() {
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.suggested_action = SuggestedAction::Escalate(EscalationReason("model says so".into()));
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable)
        );
    }

    #[tokio::test]
    async fn provider_unreachable_error_escalates_reasoning_unavailable() {
        let conn = fresh_conn();
        let provider = ScriptedProvider::err(ReasoningError::Unreachable("network".into()));
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable)
        );
        // No rationale / no classification_produced event on the
        // transport-error path.
        let count: i64 = {
            let guard = conn.lock().await;
            guard
                .query_row("SELECT COUNT(*) FROM reasoning_rationales", [], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn happy_path_returns_ask_clarification_and_persists_audit() {
        let conn = fresh_conn();
        let provider = ScriptedProvider::ok(base_response());
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::AskClarification {
                buyer_text: "please confirm X (buyer)".into(),
                seller_text: "please confirm X (seller)".into(),
            }
        );
        let (rat_count, evt_count): (i64, i64) = {
            let guard = conn.lock().await;
            let rat = guard
                .query_row("SELECT COUNT(*) FROM reasoning_rationales", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let evt = guard
                .query_row(
                    "SELECT COUNT(*) FROM mediation_events
                     WHERE session_id='sess-policy' AND kind='classification_produced'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (rat, evt)
        };
        assert_eq!(rat_count, 1, "rationale audit row expected");
        assert_eq!(evt_count, 1, "classification_produced event expected");
    }

    #[tokio::test]
    async fn authority_boundary_flag_suppresses_and_escalates() {
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.flags = vec![Flag::AuthorityBoundaryAttempt];
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::AuthorityBoundaryAttempt)
        );
    }

    #[tokio::test]
    async fn authority_boundary_flag_dominates_low_confidence() {
        // Pin the ordering fix: when a response is *both* below the
        // confidence threshold AND carries an authority-boundary
        // flag, the policy must surface the authority-boundary
        // trigger — losing that signal to LowConfidence would hide a
        // critical class of escalation from the audit log.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.2;
        resp.flags = vec![Flag::AuthorityBoundaryAttempt];
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::AuthorityBoundaryAttempt)
        );
    }

    #[tokio::test]
    async fn empty_buyer_clarification_text_escalates_as_malformed() {
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.suggested_action = SuggestedAction::AskClarification {
            buyer_text: "   \n\t".into(),
            seller_text: "please confirm X (seller)".into(),
        };
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable)
        );
    }

    #[tokio::test]
    async fn empty_seller_clarification_text_escalates_as_malformed() {
        // Either side being blank is sufficient to reject the whole
        // clarification — the drafter commits two rows atomically and
        // silently dropping one party would break that invariant.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.suggested_action = SuggestedAction::AskClarification {
            buyer_text: "please confirm X (buyer)".into(),
            seller_text: "".into(),
        };
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable)
        );
    }

    // ------------------------------------------------------------------
    // `evaluate` — mid-session entry point (US4 / T066).
    //
    // Same rule table as `initial_classification` but the caller
    // supplies the `ClassificationResponse` directly. These tests
    // drive the audit-write path (rationale + classification_produced
    // event) without going through a reasoning-provider stub.
    // ------------------------------------------------------------------

    /// Disabled cooperative-self-resolution branch so legacy tests
    /// preserve their previous semantics. Cooperative-branch
    /// behavior is exercised in dedicated tests further down.
    fn legacy_mediation_cfg() -> MediationConfig {
        MediationConfig {
            self_resolution_enabled: false,
            ..MediationConfig::default()
        }
    }

    async fn run_evaluate(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        classification: ClassificationResponse,
    ) -> Result<PolicyDecision> {
        // Default to a followup_number past the bypass window so tests
        // that don't care about the bypass still exercise the
        // escalation path by default. Tests that DO care about the
        // bypass use `run_evaluate_at` directly.
        run_evaluate_at(conn, classification, EARLY_MIDSESSION_BYPASS_FOLLOWUPS + 1).await
    }

    async fn run_evaluate_at(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        classification: ClassificationResponse,
        followup_number: u32,
    ) -> Result<PolicyDecision> {
        let bundle = test_bundle();
        evaluate(
            conn,
            "sess-policy",
            &bundle,
            "openai",
            "gpt-test",
            classification,
            followup_number,
            &legacy_mediation_cfg(),
        )
        .await
    }

    async fn run_evaluate_with_cfg(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        classification: ClassificationResponse,
        followup_number: u32,
        cfg: &MediationConfig,
    ) -> Result<PolicyDecision> {
        let bundle = test_bundle();
        evaluate(
            conn,
            "sess-policy",
            &bundle,
            "openai",
            "gpt-test",
            classification,
            followup_number,
            cfg,
        )
        .await
    }

    #[tokio::test]
    async fn evaluate_fraud_flag_escalates() {
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.flags = vec![Flag::FraudRisk];
        let decision = run_evaluate(&conn, resp).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::FraudIndicator)
        );
    }

    #[tokio::test]
    async fn evaluate_authority_boundary_escalates() {
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.flags = vec![Flag::AuthorityBoundaryAttempt];
        let decision = run_evaluate(&conn, resp).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::AuthorityBoundaryAttempt)
        );
    }

    #[tokio::test]
    async fn evaluate_passes_buyer_and_seller_texts_through_distinctly() {
        // Regression for the 2026-04-21 Alice/Bob run where a single
        // broadcast string leaked a buyer-directed question to the
        // seller. The policy layer MUST preserve both texts without
        // mixing or dropping either side.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.suggested_action = SuggestedAction::AskClarification {
            buyer_text: "BUYER-ONLY-QUESTION".into(),
            seller_text: "SELLER-ONLY-QUESTION".into(),
        };
        let decision = run_evaluate(&conn, resp).await.unwrap();
        match decision {
            PolicyDecision::AskClarification {
                buyer_text,
                seller_text,
            } => {
                assert_eq!(buyer_text, "BUYER-ONLY-QUESTION");
                assert_eq!(seller_text, "SELLER-ONLY-QUESTION");
                assert_ne!(
                    buyer_text, seller_text,
                    "per-party texts must survive as separate strings"
                );
            }
            other => panic!("expected AskClarification, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn evaluate_low_confidence_escalates() {
        // Default `run_evaluate` runs past the bypass window (follow-up
        // N+1), so a low-confidence response still escalates as before.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.3;
        let decision = run_evaluate(&conn, resp).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::LowConfidence)
        );
    }

    #[tokio::test]
    async fn evaluate_low_confidence_ask_clarification_bypasses_in_early_mid_session() {
        // Production case 2026-04-21 Alice/Bob: gpt-5 returned
        // confidence 0.31 after the first single-party reply. Old
        // policy escalated with LowConfidence and the parties never
        // saw a second clarifying question. The bypass window
        // (follow-ups 1..=EARLY_MIDSESSION_BYPASS_FOLLOWUPS) lets
        // Serbero ask again before giving up.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.3; // below LOW_CONFIDENCE_THRESHOLD
                               // base_response's suggested_action is AskClarification.
        let decision = run_evaluate_at(&conn, resp, 1).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::AskClarification {
                buyer_text: "please confirm X (buyer)".into(),
                seller_text: "please confirm X (seller)".into(),
            }
        );
    }

    #[tokio::test]
    async fn evaluate_low_confidence_ask_clarification_bypasses_at_boundary() {
        // The bypass window is inclusive. Follow-up exactly equal to
        // EARLY_MIDSESSION_BYPASS_FOLLOWUPS must still pass through.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.3;
        let decision = run_evaluate_at(&conn, resp, EARLY_MIDSESSION_BYPASS_FOLLOWUPS)
            .await
            .unwrap();
        assert_eq!(
            decision,
            PolicyDecision::AskClarification {
                buyer_text: "please confirm X (buyer)".into(),
                seller_text: "please confirm X (seller)".into(),
            }
        );
    }

    #[tokio::test]
    async fn evaluate_low_confidence_ask_clarification_past_bypass_escalates() {
        // Immediately past the bypass window, sustained low confidence
        // is a real "tried and failed" signal — escalate as before.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.3;
        let decision = run_evaluate_at(&conn, resp, EARLY_MIDSESSION_BYPASS_FOLLOWUPS + 1)
            .await
            .unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::LowConfidence)
        );
    }

    #[tokio::test]
    async fn evaluate_low_confidence_summarize_escalates_inside_bypass_window() {
        // The bypass only covers AskClarification. An unconfident
        // Summarize is a terminal commitment Serbero must NOT make
        // even in the early mid-session window — Summarize triggers
        // notification-to-solvers and a state walk all the way to
        // `closed`; getting that wrong is expensive to recover from.
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.3;
        resp.suggested_action = SuggestedAction::Summarize;
        let decision = run_evaluate_at(&conn, resp, 1).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::LowConfidence)
        );
    }

    #[tokio::test]
    async fn evaluate_happy_path_persists_audit() {
        let conn = fresh_conn();
        let decision = run_evaluate(&conn, base_response()).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::AskClarification {
                buyer_text: "please confirm X (buyer)".into(),
                seller_text: "please confirm X (seller)".into(),
            }
        );
        let (rat_count, evt_count): (i64, i64) = {
            let guard = conn.lock().await;
            let rat = guard
                .query_row("SELECT COUNT(*) FROM reasoning_rationales", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let evt = guard
                .query_row(
                    "SELECT COUNT(*) FROM mediation_events
                     WHERE session_id='sess-policy' AND kind='classification_produced'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (rat, evt)
        };
        assert_eq!(rat_count, 1, "rationale audit row expected");
        assert_eq!(evt_count, 1, "classification_produced event expected");
    }

    // ------------------------------------------------------------------
    // Feature 005 — cooperative self-resolution branch (T016) +
    // human-assistance opt-in short-circuit (T023). Tests pin the
    // pre-condition logic so a future refactor can't accidentally
    // weaken the kill-switch / threshold / one-shot guards.
    // ------------------------------------------------------------------

    fn enabled_cooperative_cfg(threshold: f32) -> MediationConfig {
        MediationConfig {
            self_resolution_enabled: true,
            self_resolution_threshold: threshold,
            ..MediationConfig::default()
        }
    }

    fn cooperative_summary_response(confidence: f64) -> ClassificationResponse {
        let mut resp = base_response();
        resp.classification = ClassificationLabel::CoordinationFailureResolvable;
        resp.suggested_action = SuggestedAction::Summarize;
        resp.confidence = confidence;
        resp
    }

    async fn seed_self_resolution_offered_row(conn: &Arc<AsyncMutex<rusqlite::Connection>>) {
        let guard = conn.lock().await;
        guard
            .execute(
                "INSERT INTO mediation_events (
                    session_id, kind, payload_json, occurred_at
                 ) VALUES ('sess-policy', 'self_resolution_offered', '{}', 50)",
                [],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn evaluate_cooperative_branch_fires_when_all_preconditions_hold() {
        let conn = fresh_conn();
        let cfg = enabled_cooperative_cfg(0.75);
        let resp = cooperative_summary_response(0.85);
        let decision = run_evaluate_with_cfg(&conn, resp, 4, &cfg).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::SuggestSelfResolutionWithSummary { confidence: 0.85 },
        );
    }

    #[tokio::test]
    async fn evaluate_cooperative_branch_inclusive_at_threshold() {
        // FR-010 / R-007: the threshold check is `>=` so a confidence
        // exactly equal to the configured floor still fires.
        let conn = fresh_conn();
        let cfg = enabled_cooperative_cfg(0.75);
        let resp = cooperative_summary_response(0.75);
        let decision = run_evaluate_with_cfg(&conn, resp, 4, &cfg).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::SuggestSelfResolutionWithSummary { confidence: 0.75 },
        );
    }

    #[tokio::test]
    async fn evaluate_cooperative_branch_falls_through_below_threshold() {
        let conn = fresh_conn();
        let cfg = enabled_cooperative_cfg(0.90);
        let resp = cooperative_summary_response(0.80);
        let decision = run_evaluate_with_cfg(&conn, resp, 4, &cfg).await.unwrap();
        // Falls through to the legacy cooperative-summary path.
        assert_eq!(
            decision,
            PolicyDecision::Summarize {
                classification: ClassificationLabel::CoordinationFailureResolvable,
                confidence: 0.80,
            },
        );
    }

    #[tokio::test]
    async fn evaluate_cooperative_branch_skipped_when_kill_switch_off() {
        // SC-007: with `self_resolution_enabled = false`, the legacy
        // cooperative-summary path runs unchanged.
        let conn = fresh_conn();
        let cfg = MediationConfig {
            self_resolution_enabled: false,
            self_resolution_threshold: 0.75,
            ..MediationConfig::default()
        };
        let resp = cooperative_summary_response(0.99);
        let decision = run_evaluate_with_cfg(&conn, resp, 4, &cfg).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Summarize {
                classification: ClassificationLabel::CoordinationFailureResolvable,
                confidence: 0.99,
            },
        );
    }

    #[tokio::test]
    async fn evaluate_cooperative_branch_one_shot_guard() {
        // FR-006 / SC-006: a session that already has a
        // `self_resolution_offered` audit row MUST NOT receive a
        // second invitation; the legacy summarize path takes over.
        let conn = fresh_conn();
        seed_self_resolution_offered_row(&conn).await;
        let cfg = enabled_cooperative_cfg(0.75);
        let resp = cooperative_summary_response(0.95);
        let decision = run_evaluate_with_cfg(&conn, resp, 4, &cfg).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Summarize {
                classification: ClassificationLabel::CoordinationFailureResolvable,
                confidence: 0.95,
            },
        );
    }

    #[tokio::test]
    async fn evaluate_human_requested_short_circuit_after_invitation() {
        // FR-008 / T023: an explicit human-assistance request after
        // the cooperative invitation escalates regardless of the
        // round's classification label.
        let conn = fresh_conn();
        seed_self_resolution_offered_row(&conn).await;
        let cfg = enabled_cooperative_cfg(0.75);
        let mut resp = base_response();
        resp.human_requested = true;
        resp.classification = ClassificationLabel::ConflictingClaims; // would normally escalate, but trigger differs
        let decision = run_evaluate_with_cfg(&conn, resp, 4, &cfg).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::PartyRequestedHuman),
        );
    }

    #[tokio::test]
    async fn evaluate_human_requested_ignored_without_prior_invitation() {
        // Defence in depth: even if a buggy provider sets
        // `human_requested = true` on a round where the prompt did
        // not request it, the policy must NOT escalate as
        // `PartyRequestedHuman` unless a prior `self_resolution_offered`
        // row exists for the session.
        let conn = fresh_conn();
        let cfg = enabled_cooperative_cfg(0.75);
        let mut resp = base_response();
        resp.human_requested = true; // no seeded self_resolution_offered row
        let decision = run_evaluate_with_cfg(&conn, resp, 4, &cfg).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::AskClarification {
                buyer_text: "please confirm X (buyer)".into(),
                seller_text: "please confirm X (seller)".into(),
            },
        );
    }

    #[tokio::test]
    async fn evaluate_cooperative_branch_inert_when_templates_empty() {
        // Backstop for an upgrade path: the daemon loads with empty
        // `self_resolution` templates (file not yet shipped) and the
        // operator left `self_resolution_enabled = true` by default.
        // Without this gate, the branch would fire and `render_for`
        // would emit the placeholder operator-message string to end
        // users. The gate falls through to the legacy summarize so
        // the user-facing chat stays clean.
        let conn = fresh_conn();
        let cfg = enabled_cooperative_cfg(0.75);
        // Bundle with empty by_language map.
        let empty_bundle = Arc::new(PromptBundle {
            id: "phase3-default".into(),
            policy_hash: "test-policy-hash".into(),
            system: "sys".into(),
            classification: "cls".into(),
            escalation: "esc".into(),
            mediation_style: "style".into(),
            message_templates: "tpl".into(),
            self_resolution: crate::mediation::self_resolution::SelfResolutionTemplates::default(),
        });
        let resp = cooperative_summary_response(0.95);
        let decision = evaluate(
            &conn,
            "sess-policy",
            &empty_bundle,
            "openai",
            "gpt-test",
            resp,
            4,
            &cfg,
        )
        .await
        .unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Summarize {
                classification: ClassificationLabel::CoordinationFailureResolvable,
                confidence: 0.95,
            },
        );
    }

    #[tokio::test]
    async fn evaluate_no_lock_in_after_invitation_for_non_cooperative_label() {
        // US3: a non-cooperative classification on a round following
        // the cooperative invitation MUST escalate under its own
        // standard trigger, NOT under PartyRequestedHuman.
        let conn = fresh_conn();
        seed_self_resolution_offered_row(&conn).await;
        let cfg = enabled_cooperative_cfg(0.75);
        let mut resp = base_response();
        resp.classification = ClassificationLabel::ConflictingClaims;
        resp.flags = vec![Flag::ConflictingClaims];
        let decision = run_evaluate_with_cfg(&conn, resp, 4, &cfg).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::ConflictingClaims),
        );
    }
}
