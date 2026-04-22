//! Phase 10 / T106 — reasoning-before-take invariant (FR-122 / SC-110).
//!
//! Pins the strictest post-T104 behavior: when `policy::classify_for_start`
//! returns a negative verdict (`Escalate(trigger)`), `open_session`
//! MUST NOT issue `TakeDispute` and MUST NOT commit a
//! `mediation_sessions` row. The handoff lives dispute-scoped in
//! `mediation_events` (`session_id = NULL`, payload carries the
//! dispute id) so Phase 4 can still consume it.
//!
//! This is the only FR-122 guarantee with an empirical integration-
//! level proof today. The happy-path ordering
//! (`start_attempt_started` < `reasoning_verdict` <
//! `take_dispute_issued{success}` < `session_opened` <
//! `classification_produced`) is exercised implicitly by
//! `phase3_session_open.rs` / `phase3_event_driven_start.rs`, and the
//! `reasoning_unavailable_skips_take` case is exercised by
//! `phase3_session_open_gating.rs`. The `take_fails_no_session_row`
//! sub-test is deferred — it needs a Mostro-side reject harness that
//! the current common helpers don't yet provide.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use serbero::db;
use serbero::mediation;
use serbero::mediation::auth_retry::AuthRetryHandle;
use serbero::mediation::session::OpenOutcome;
use serbero::models::dispute::InitiatorRole;
use serbero::models::mediation::{ClassificationLabel, EscalationTrigger, Flag};
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, EscalationReason, RationaleText, ReasoningError,
    SuggestedAction, SummaryRequest, SummaryResponse,
};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

/// Scripted provider that returns a `fraud_risk`-flagged
/// classification on `classify`, driving
/// `policy::classify_for_start` down the `Escalate(FraudIndicator)`
/// branch without any other infrastructure. `health_check` succeeds
/// so the T044 reasoning-health gate passes.
struct EscalatingProvider;

#[async_trait]
impl ReasoningProvider for EscalatingProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        Ok(ClassificationResponse {
            classification: ClassificationLabel::SuspectedFraud,
            confidence: 0.95,
            // The suggested action is intentionally `AskClarification`
            // — the FraudRisk flag dominates the rule table regardless,
            // so this pins that the flag-based short-circuit runs
            // (and not the low-confidence path).
            suggested_action: SuggestedAction::AskClarification {
                buyer_text: "ignored — FraudRisk flag dominates".into(),
                seller_text: "ignored — FraudRisk flag dominates".into(),
            },
            rationale: RationaleText("scripted fraud verdict for T106".into()),
            flags: vec![Flag::FraudRisk],
        })
    }
    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        Err(ReasoningError::Unreachable("summary unused in T106".into()))
    }
    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Ok(())
    }
}

/// Same as [`EscalatingProvider`] but asserts the suggested
/// `Escalate(reason)` branch (not the flag branch). Used for the
/// `reasoning_negative_verdict_skips_take` sub-test's second path so
/// both entry points to `PolicyDecision::Escalate` are covered.
struct ModelEscalatesProvider;

#[async_trait]
impl ReasoningProvider for ModelEscalatesProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        Ok(ClassificationResponse {
            classification: ClassificationLabel::NotSuitableForMediation,
            confidence: 0.9,
            suggested_action: SuggestedAction::Escalate(EscalationReason(
                "model says not suitable".into(),
            )),
            rationale: RationaleText("scripted model-escalate verdict".into()),
            flags: Vec::new(),
        })
    }
    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        Err(ReasoningError::Unreachable("summary unused in T106".into()))
    }
    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Ok(())
    }
}

fn fixture_bundle() -> Arc<PromptBundle> {
    let cfg = serbero::models::PromptsConfig {
        system_instructions_path: "./tests/fixtures/prompts/phase3-system.md".into(),
        classification_policy_path: "./tests/fixtures/prompts/phase3-classification.md".into(),
        escalation_policy_path: "./tests/fixtures/prompts/phase3-escalation-policy.md".into(),
        mediation_style_path: "./tests/fixtures/prompts/phase3-mediation-style.md".into(),
        message_templates_path: "./tests/fixtures/prompts/phase3-message-templates.md".into(),
    };
    Arc::new(prompts::load_bundle(&cfg).expect("fixture bundle must load"))
}

/// Bring up an in-memory DB with migrations + a single `disputes`
/// row for `dispute_id`. The test seeds the parent row directly
/// because no `dispute_detected` handler runs here — we invoke
/// `open_dispute_session` in isolation.
async fn seed_db(dispute_id: &str) -> Arc<AsyncMutex<rusqlite::Connection>> {
    let mut conn = db::open_in_memory().unwrap();
    db::migrations::run_migrations(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state
         ) VALUES (?1, 'evt-t106', 'mostro-t106', 'buyer',
                   'initiated', 0, 0, 'notified')",
        rusqlite::params![dispute_id],
    )
    .unwrap();
    Arc::new(AsyncMutex::new(conn))
}

async fn assert_no_session_row(conn: &Arc<AsyncMutex<rusqlite::Connection>>, dispute_id: &str) {
    let c = conn.lock().await;
    let count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM mediation_sessions WHERE dispute_id = ?1",
            rusqlite::params![dispute_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "FR-122: no mediation_sessions row must exist when the opening \
         verdict is Escalate; got {count}"
    );
}

async fn assert_dispute_scoped_event_count(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    kind: &str,
    expected: i64,
) {
    let c = conn.lock().await;
    let count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM mediation_events \
             WHERE kind = ?1 AND session_id IS NULL",
            rusqlite::params![kind],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, expected,
        "expected {expected} dispute-scoped `{kind}` events; got {count}"
    );
}

async fn run_open(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    reasoning: &dyn ReasoningProvider,
    dispute_id: &str,
) -> OpenOutcome {
    // A relay-less Client is sufficient for the FR-122 escalate
    // path: the branch short-circuits before any chat-transport
    // call, so the client is never reached. We still need a
    // well-formed `Client` because `open_dispute_session` takes a
    // reference.
    let serbero_keys = Keys::generate();
    let mostro_keys = Keys::generate();
    let client = Client::new(serbero_keys.clone());
    let bundle = fixture_bundle();
    let dispute_uuid = Uuid::new_v4();
    let auth_handle = AuthRetryHandle::new_authorized();

    mediation::open_dispute_session(
        conn,
        &client,
        &serbero_keys,
        &mostro_keys.public_key(),
        reasoning,
        &bundle,
        dispute_id,
        InitiatorRole::Buyer,
        dispute_uuid,
        "mock-provider",
        "mock-model",
        &auth_handle,
    )
    .await
    .expect("open_dispute_session must not surface Err on the FR-122 escalate branch")
}

#[tokio::test]
async fn reasoning_negative_verdict_flag_path_skips_take() {
    // Entry point 1 to PolicyDecision::Escalate: a `FraudRisk`
    // flag on the classification. The rule table fires
    // FraudIndicator regardless of suggested_action.
    let dispute_id = "dispute-t106-flag";
    let conn = seed_db(dispute_id).await;
    let provider = EscalatingProvider;

    let outcome = run_open(&conn, &provider, dispute_id).await;

    match outcome {
        OpenOutcome::EscalatedBeforeTake {
            dispute_id: did,
            trigger,
        } => {
            assert_eq!(did, dispute_id);
            assert_eq!(trigger, EscalationTrigger::FraudIndicator);
        }
        other => panic!("expected EscalatedBeforeTake, got {other:?}"),
    }

    // (i) No session row.
    assert_no_session_row(&conn, dispute_id).await;

    // (ii) Dispute-scoped reasoning_verdict event — exactly one.
    assert_dispute_scoped_event_count(&conn, "reasoning_verdict", 1).await;

    // (iii) Dispute-scoped start_attempt_stopped{reason:policy_escalate}.
    let c = conn.lock().await;
    let (sas_count, payload_has_reason): (i64, i64) = c
        .query_row(
            "SELECT COUNT(*), SUM(CASE WHEN payload_json LIKE '%policy_escalate%' THEN 1 ELSE 0 END) \
             FROM mediation_events \
             WHERE kind = 'start_attempt_stopped' AND session_id IS NULL",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(sas_count, 1, "expected exactly one start_attempt_stopped");
    assert_eq!(payload_has_reason, 1, "reason must be policy_escalate");
    drop(c);

    // (iv) Dispute-scoped handoff pair (escalation_recommended +
    //     handoff_prepared) with session_id IS NULL.
    assert_dispute_scoped_event_count(&conn, "escalation_recommended", 1).await;
    assert_dispute_scoped_event_count(&conn, "handoff_prepared", 1).await;

    // And the payload MUST carry the dispute id so Phase 4 can
    // navigate back.
    let c = conn.lock().await;
    let handoff_payload: String = c
        .query_row(
            "SELECT payload_json FROM mediation_events \
             WHERE kind = 'handoff_prepared' AND session_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        handoff_payload.contains(dispute_id),
        "handoff_prepared payload must reference the dispute id; got {handoff_payload}"
    );
}

#[tokio::test]
async fn reasoning_negative_verdict_model_escalate_path_skips_take() {
    // Entry point 2 to PolicyDecision::Escalate: the model
    // returned `SuggestedAction::Escalate`. The policy layer
    // funnels this to `ReasoningUnavailable` (the Phase 3
    // contract-bounded trigger set has no free-form model reason).
    let dispute_id = "dispute-t106-model-escalate";
    let conn = seed_db(dispute_id).await;
    let provider = ModelEscalatesProvider;

    let outcome = run_open(&conn, &provider, dispute_id).await;

    match outcome {
        OpenOutcome::EscalatedBeforeTake {
            dispute_id: did,
            trigger,
        } => {
            assert_eq!(did, dispute_id);
            assert_eq!(trigger, EscalationTrigger::ReasoningUnavailable);
        }
        other => panic!("expected EscalatedBeforeTake, got {other:?}"),
    }

    assert_no_session_row(&conn, dispute_id).await;
    assert_dispute_scoped_event_count(&conn, "reasoning_verdict", 1).await;
    assert_dispute_scoped_event_count(&conn, "escalation_recommended", 1).await;
    assert_dispute_scoped_event_count(&conn, "handoff_prepared", 1).await;
}
