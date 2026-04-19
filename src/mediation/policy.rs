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
use crate::models::mediation::{EscalationTrigger, Flag};
use crate::models::reasoning::{
    ClassificationRequest, ClassificationResponse, ReasoningContext, SuggestedAction,
};
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
    /// Ask both parties a clarifying question. The inner string is
    /// the validated clarification text the draft path will wrap in
    /// the per-party outbound messages.
    AskClarification(String),
    /// Cooperative resolution path (US3). Carries the classification
    /// label and confidence so the engine can call the summarizer
    /// without having to re-read the classification_produced event.
    Summarize {
        classification: crate::models::mediation::ClassificationLabel,
        confidence: f64,
    },
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
            let now = current_ts_secs();
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

    let decision = classify_to_decision(&classification);

    // Persist rationale + emit audit event BEFORE returning: the
    // decision is only legitimate once the audit trail is durable.
    // The same DB lock covers both writes so a crash leaves a
    // consistent view.
    let now = current_ts_secs();
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
    drop(guard);

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
#[allow(clippy::too_many_arguments)]
pub async fn evaluate(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    prompt_bundle: &Arc<PromptBundle>,
    provider_name: &str,
    model_name: &str,
    classification: ClassificationResponse,
) -> Result<PolicyDecision> {
    let decision = classify_to_decision(&classification);

    let now = current_ts_secs();
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
    drop(guard);

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

/// Pure validation: run the contract rules against a single
/// [`ClassificationResponse`] and return the resulting decision.
/// Extracted so unit tests can exercise the rule table without
/// setting up a DB / rationale store.
pub(crate) fn classify_to_decision(classification: &ClassificationResponse) -> PolicyDecision {
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

    // Low confidence. Strict `<` so a model that reports exactly
    // the threshold is still trusted to proceed (matches contract
    // wording: "below threshold").
    if classification.confidence < LOW_CONFIDENCE_THRESHOLD {
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
        SuggestedAction::AskClarification(text) => {
            // Reject empty / whitespace-only clarifications — the
            // session-open draft path cannot build a meaningful
            // gift-wrap from them, and letting the outbound message
            // go as literal "Buyer: " / "Seller: " would be worse
            // than escalating. Treated the same as a malformed
            // provider response (rule 6 in the contract).
            if text.trim().is_empty() {
                return PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable);
            }
            PolicyDecision::AskClarification(text.clone())
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

/// Fail loudly on a clock-before-UNIX-EPOCH error. A silent `0`
/// would corrupt rationale / event ordering; the audit store relies
/// on `generated_at` / `occurred_at` being a real Unix timestamp.
fn current_ts_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before UNIX_EPOCH; refusing to persist audit rows with ts = 0")
        .as_secs() as i64
}

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
        Arc::new(PromptBundle {
            id: "phase3-default".into(),
            policy_hash: "test-policy-hash".into(),
            system: "sys".into(),
            classification: "cls".into(),
            escalation: "esc".into(),
            mediation_style: "style".into(),
            message_templates: "tpl".into(),
        })
    }

    fn base_response() -> ClassificationResponse {
        ClassificationResponse {
            classification: ClassificationLabel::CoordinationFailureResolvable,
            confidence: 0.9,
            suggested_action: SuggestedAction::AskClarification("please confirm X".into()),
            rationale: RationaleText("rationale body".into()),
            flags: Vec::new(),
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
    async fn low_confidence_escalates_under_threshold() {
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.confidence = 0.3;
        let provider = ScriptedProvider::ok(resp);
        let decision = run_initial(&conn, &provider).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::Escalate(EscalationTrigger::LowConfidence)
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
            PolicyDecision::AskClarification("please confirm X".into())
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
    async fn empty_clarification_text_escalates_as_malformed() {
        let conn = fresh_conn();
        let mut resp = base_response();
        resp.suggested_action = SuggestedAction::AskClarification("   \n\t".into());
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

    async fn run_evaluate(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        classification: ClassificationResponse,
    ) -> Result<PolicyDecision> {
        let bundle = test_bundle();
        evaluate(
            conn,
            "sess-policy",
            &bundle,
            "openai",
            "gpt-test",
            classification,
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
    async fn evaluate_low_confidence_escalates() {
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
    async fn evaluate_happy_path_persists_audit() {
        let conn = fresh_conn();
        let decision = run_evaluate(&conn, base_response()).await.unwrap();
        assert_eq!(
            decision,
            PolicyDecision::AskClarification("please confirm X".into())
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
}
