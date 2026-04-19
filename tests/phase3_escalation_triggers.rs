//! US4 — per-trigger escalation integration tests (T063).
//!
//! One `#[tokio::test]` per escalation trigger the mediation engine
//! is expected to surface in Phase 3. Each sub-test uses the
//! direct-seed pattern: insert a `disputes` + `mediation_sessions`
//! row into an in-memory SQLite, call the function under test
//! directly (no live relay, no real Nostr), and assert the resulting
//! DB state.
//!
//! These tests deliberately avoid `open_session` so the trigger path
//! is exercised in isolation; the end-to-end `open → classify →
//! escalate` flow is covered separately by the session-open tests.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use serbero::db;
use serbero::mediation::auth_retry::{AuthRetryHandle, AuthState};
use serbero::mediation::escalation::{self, RecommendParams};
use serbero::mediation::policy::{self, PolicyDecision};
use serbero::mediation::session;
use serbero::models::dispute::InitiatorRole;
use serbero::models::mediation::{ClassificationLabel, EscalationTrigger, Flag};
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, RationaleText, ReasoningError, SuggestedAction,
    SummaryRequest, SummaryResponse,
};
use serbero::prompts::PromptBundle;
use serbero::reasoning::ReasoningProvider;
use tokio::sync::Mutex as AsyncMutex;

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

/// Fresh DB + one seeded dispute + one seeded session in
/// `awaiting_response` with a known `started_at`.
async fn seed_session(
    dispute_id: &str,
    session_id: &str,
    started_at: i64,
) -> Arc<AsyncMutex<rusqlite::Connection>> {
    let mut conn = db::open_in_memory().unwrap();
    db::migrations::run_migrations(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state
         ) VALUES (?1, 'e1', 'm1', 'buyer', 'initiated', 1, 2, 'notified')",
        rusqlite::params![dispute_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mediation_sessions (
            session_id, dispute_id, state, round_count,
            prompt_bundle_id, policy_hash,
            started_at, last_transition_at
         ) VALUES (?1, ?2, 'awaiting_response', 0,
                   'phase3-default', 'test-policy-hash',
                   ?3, ?3)",
        rusqlite::params![session_id, dispute_id, started_at],
    )
    .unwrap();
    Arc::new(AsyncMutex::new(conn))
}

async fn session_state(conn: &Arc<AsyncMutex<rusqlite::Connection>>, session_id: &str) -> String {
    let c = conn.lock().await;
    c.query_row(
        "SELECT state FROM mediation_sessions WHERE session_id = ?1",
        rusqlite::params![session_id],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
}

async fn count_event(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    kind: &str,
) -> i64 {
    let c = conn.lock().await;
    c.query_row(
        "SELECT COUNT(*) FROM mediation_events WHERE session_id = ?1 AND kind = ?2",
        rusqlite::params![session_id, kind],
        |r| r.get(0),
    )
    .unwrap()
}

async fn escalation_payload(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
) -> String {
    let c = conn.lock().await;
    c.query_row(
        "SELECT payload_json FROM mediation_events
         WHERE session_id = ?1 AND kind = 'escalation_recommended'
         ORDER BY id ASC LIMIT 1",
        rusqlite::params![session_id],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
}

#[tokio::test]
async fn conflicting_claims_triggers_escalation() {
    let conn = seed_session("dispute-cc", "sess-cc", 100).await;
    let bundle = test_bundle();
    let mut resp = base_response();
    resp.flags = vec![Flag::ConflictingClaims];

    let decision = policy::evaluate(&conn, "sess-cc", &bundle, "openai", "gpt-test", resp)
        .await
        .unwrap();
    assert_eq!(
        decision,
        PolicyDecision::Escalate(EscalationTrigger::ConflictingClaims)
    );

    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-cc",
        trigger: EscalationTrigger::ConflictingClaims,
        evidence_refs: Vec::new(),
        prompt_bundle_id: &bundle.id,
        policy_hash: &bundle.policy_hash,
    })
    .await
    .unwrap();

    assert_eq!(
        session_state(&conn, "sess-cc").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-cc").await;
    assert!(
        payload.contains("conflicting_claims"),
        "payload must carry the trigger string: {payload}"
    );
    assert_eq!(count_event(&conn, "sess-cc", "handoff_prepared").await, 1);
}

#[tokio::test]
async fn fraud_indicator_triggers_escalation() {
    let conn = seed_session("dispute-fr", "sess-fr", 100).await;
    let bundle = test_bundle();
    let mut resp = base_response();
    resp.flags = vec![Flag::FraudRisk];

    let decision = policy::evaluate(&conn, "sess-fr", &bundle, "openai", "gpt-test", resp)
        .await
        .unwrap();
    assert_eq!(
        decision,
        PolicyDecision::Escalate(EscalationTrigger::FraudIndicator)
    );

    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-fr",
        trigger: EscalationTrigger::FraudIndicator,
        evidence_refs: Vec::new(),
        prompt_bundle_id: &bundle.id,
        policy_hash: &bundle.policy_hash,
    })
    .await
    .unwrap();

    assert_eq!(
        session_state(&conn, "sess-fr").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-fr").await;
    assert!(payload.contains("fraud_indicator"), "{payload}");
    assert_eq!(count_event(&conn, "sess-fr", "handoff_prepared").await, 1);
}

#[tokio::test]
async fn low_confidence_triggers_escalation() {
    let conn = seed_session("dispute-lc", "sess-lc", 100).await;
    let bundle = test_bundle();
    let mut resp = base_response();
    resp.confidence = 0.2;

    let decision = policy::evaluate(&conn, "sess-lc", &bundle, "openai", "gpt-test", resp)
        .await
        .unwrap();
    assert_eq!(
        decision,
        PolicyDecision::Escalate(EscalationTrigger::LowConfidence)
    );

    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-lc",
        trigger: EscalationTrigger::LowConfidence,
        evidence_refs: Vec::new(),
        prompt_bundle_id: &bundle.id,
        policy_hash: &bundle.policy_hash,
    })
    .await
    .unwrap();

    assert_eq!(
        session_state(&conn, "sess-lc").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-lc").await;
    assert!(payload.contains("low_confidence"), "{payload}");
    assert_eq!(count_event(&conn, "sess-lc", "handoff_prepared").await, 1);
}

#[tokio::test]
async fn party_unresponsive_timeout_triggers_escalation() {
    // T069 — the timeout check is a pure deadline computation.
    // Replicate it here against a session whose started_at is well
    // behind the configured timeout, call escalation::recommend with
    // PartyUnresponsive, and assert the session transitions.
    const TIMEOUT_SECS: i64 = 3600;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let started_at = now - TIMEOUT_SECS - 10;
    let conn = seed_session("dispute-pu", "sess-pu", started_at).await;
    let bundle = test_bundle();

    // Deadline computation (verbatim from run_ingest_tick):
    // reference = max(started_at, buyer_last, seller_last)
    // deadline  = reference + timeout
    let reference = started_at; // neither party has replied yet
    let deadline = reference + TIMEOUT_SECS;
    assert!(now > deadline, "precondition: session must be overdue");

    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-pu",
        trigger: EscalationTrigger::PartyUnresponsive,
        evidence_refs: Vec::new(),
        prompt_bundle_id: &bundle.id,
        policy_hash: &bundle.policy_hash,
    })
    .await
    .unwrap();

    assert_eq!(
        session_state(&conn, "sess-pu").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-pu").await;
    assert!(payload.contains("party_unresponsive"), "{payload}");
}

#[tokio::test]
async fn round_limit_triggers_escalation() {
    const MAX_ROUNDS: u32 = 3;
    assert!(session::check_round_limit(MAX_ROUNDS, MAX_ROUNDS));
    assert!(!session::check_round_limit(MAX_ROUNDS - 1, MAX_ROUNDS));

    let conn = seed_session("dispute-rl", "sess-rl", 100).await;
    // Pin the round counter at the cap.
    {
        let c = conn.lock().await;
        c.execute(
            "UPDATE mediation_sessions SET round_count = ?1 WHERE session_id = 'sess-rl'",
            rusqlite::params![MAX_ROUNDS],
        )
        .unwrap();
    }
    let bundle = test_bundle();

    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-rl",
        trigger: EscalationTrigger::RoundLimit,
        evidence_refs: Vec::new(),
        prompt_bundle_id: &bundle.id,
        policy_hash: &bundle.policy_hash,
    })
    .await
    .unwrap();

    assert_eq!(
        session_state(&conn, "sess-rl").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-rl").await;
    assert!(payload.contains("round_limit"), "{payload}");
    assert_eq!(count_event(&conn, "sess-rl", "handoff_prepared").await, 1);
}

/// Scripted unreachable provider for the reasoning-unavailable path.
struct UnreachableProvider;

#[async_trait]
impl ReasoningProvider for UnreachableProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        Err(ReasoningError::Unreachable("network down".into()))
    }
    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        Err(ReasoningError::Unreachable("network down".into()))
    }
    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Err(ReasoningError::Unreachable("network down".into()))
    }
}

#[tokio::test]
async fn reasoning_unavailable_triggers_escalation() {
    let conn = seed_session("dispute-ru", "sess-ru", 100).await;
    let bundle = test_bundle();
    let provider = UnreachableProvider;

    let decision = policy::initial_classification(
        &conn,
        "sess-ru",
        "dispute-ru",
        InitiatorRole::Buyer,
        &bundle,
        &provider,
        "openai",
        "gpt-test",
    )
    .await
    .unwrap();
    assert_eq!(
        decision,
        PolicyDecision::Escalate(EscalationTrigger::ReasoningUnavailable)
    );

    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-ru",
        trigger: EscalationTrigger::ReasoningUnavailable,
        evidence_refs: Vec::new(),
        prompt_bundle_id: &bundle.id,
        policy_hash: &bundle.policy_hash,
    })
    .await
    .unwrap();

    assert_eq!(
        session_state(&conn, "sess-ru").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-ru").await;
    assert!(payload.contains("reasoning_unavailable"), "{payload}");
    assert_eq!(count_event(&conn, "sess-ru", "handoff_prepared").await, 1);

    // The transport-error path in `initial_classification` also
    // writes a `reasoning_call_failed` audit row (T070 / T074).
    assert!(
        count_event(&conn, "sess-ru", "reasoning_call_failed").await >= 1,
        "initial_classification must record reasoning_call_failed on provider error"
    );
}

#[tokio::test]
async fn authorization_lost_mid_session_triggers_escalation() {
    let conn = seed_session("dispute-al", "sess-al", 100).await;
    let bundle = test_bundle();

    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-al",
        trigger: EscalationTrigger::AuthorizationLost,
        evidence_refs: Vec::new(),
        prompt_bundle_id: &bundle.id,
        policy_hash: &bundle.policy_hash,
    })
    .await
    .unwrap();

    assert_eq!(
        session_state(&conn, "sess-al").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-al").await;
    assert!(payload.contains("authorization_lost"), "{payload}");
    assert_eq!(count_event(&conn, "sess-al", "handoff_prepared").await, 1);

    // `signal_auth_lost` flips an `Authorized` handle to
    // `Unauthorized`, and `is_authorized` returns false afterwards.
    let handle = AuthRetryHandle::new_authorized();
    assert!(handle.is_authorized());
    handle.signal_auth_lost();
    assert!(!handle.is_authorized());

    // Re-signalling while already Unauthorized is a no-op.
    handle.signal_auth_lost();
    assert!(!handle.is_authorized());

    // A `Terminated` handle is also left alone (documented no-op for
    // the other two non-Authorized states).
    let handle = AuthRetryHandle::new_authorized();
    // Flip to Unauthorized then Terminated is not directly
    // exposable outside `#[cfg(test)]`, but the Authorized/Unauth
    // transition above already covers the public invariant.
    let _ = AuthState::Terminated; // imported to silence unused warnings in no-op branch
    drop(handle);
}
