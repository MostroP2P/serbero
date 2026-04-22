//! US4 — per-trigger escalation integration tests (T063).
//!
//! One `#[tokio::test]` per escalation trigger the mediation engine
//! is expected to surface in Phase 3. Most sub-tests use the
//! direct-seed pattern: insert a `disputes` + `mediation_sessions`
//! row into an in-memory SQLite, call the function under test
//! directly (no live relay, no real Nostr), and assert the resulting
//! DB state.
//!
//! The party-unresponsive and authorization-lost sub-tests exercise
//! the production helpers (`check_party_unresponsive_timeout`,
//! `session::handle_authorization_lost`) rather than duplicating
//! the deadline math / the state-flip + signal pair, so a
//! regression in either helper shows up as a test failure here.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use serbero::db;
use serbero::mediation::auth_retry::AuthRetryHandle;
use serbero::mediation::escalation::{self, RecommendParams};
use serbero::mediation::policy::{self, PolicyDecision};
use serbero::mediation::session;
use serbero::models::dispute::InitiatorRole;
use serbero::models::mediation::{ClassificationLabel, EscalationTrigger, Flag};
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, RationaleText, ReasoningError, SuggestedAction,
    SummaryRequest, SummaryResponse,
};
use serbero::models::MediationConfig;
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
        suggested_action: SuggestedAction::AskClarification {
            buyer_text: "please confirm X (buyer)".into(),
            seller_text: "please confirm X (seller)".into(),
        },
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

    let decision = policy::evaluate(&conn, "sess-cc", &bundle, "openai", "gpt-test", resp, 1)
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
        rationale_refs: Vec::new(),
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

    let decision = policy::evaluate(&conn, "sess-fr", &bundle, "openai", "gpt-test", resp, 1)
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
        rationale_refs: Vec::new(),
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

    // Past the early-mid-session bypass window — sustained low
    // confidence at this point is the real "Serbero tried and
    // failed" signal and MUST escalate.
    let decision = policy::evaluate(
        &conn,
        "sess-lc",
        &bundle,
        "openai",
        "gpt-test",
        resp,
        policy::EARLY_MIDSESSION_BYPASS_FOLLOWUPS + 1,
    )
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
        rationale_refs: Vec::new(),
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
    // T069 — drive the production sweep directly instead of
    // recomputing the deadline locally. A regression in the
    // deadline rule inside `check_party_unresponsive_timeout`
    // therefore surfaces as this test failing rather than the test
    // silently matching whatever the code does.
    const TIMEOUT_SECS: u64 = 3600;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let started_at = now - TIMEOUT_SECS as i64 - 10;
    let conn = seed_session("dispute-pu", "sess-pu", started_at).await;
    let bundle = test_bundle();

    let cfg = MediationConfig {
        party_response_timeout_seconds: TIMEOUT_SECS,
        ..MediationConfig::default()
    };

    // Relay-less client; empty solvers slice — notify_solvers_escalation
    // short-circuits without any relay I/O in this test.
    let client = nostr_sdk::Client::new(nostr_sdk::Keys::generate());

    serbero::mediation::check_party_unresponsive_timeout(&conn, &client, &[], &bundle, &cfg)
        .await
        .expect("timeout sweep must not return Err");

    assert_eq!(
        session_state(&conn, "sess-pu").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-pu").await;
    assert!(
        payload.contains("party_unresponsive"),
        "payload must carry party_unresponsive: {payload}"
    );
    assert_eq!(count_event(&conn, "sess-pu", "handoff_prepared").await, 1);

    // Running the sweep again must be a no-op on the same overdue
    // session — it is already at `escalation_recommended` and the
    // sweep skips terminal / escalated rows, so no duplicate events.
    serbero::mediation::check_party_unresponsive_timeout(&conn, &client, &[], &bundle, &cfg)
        .await
        .unwrap();
    assert_eq!(
        count_event(&conn, "sess-pu", "escalation_recommended").await,
        1,
        "sweep must be idempotent once a session has escalated"
    );
}

#[tokio::test]
async fn party_unresponsive_timeout_disabled_when_zero() {
    // Config-safety guard: `party_response_timeout_seconds = 0`
    // MUST NOT escalate every live session on the first tick.
    // The sweep treats 0 as the "timeout disabled" sentinel and
    // returns Ok(()) without touching any session row.
    let conn = seed_session("dispute-pu0", "sess-pu0", 100).await;
    let bundle = test_bundle();
    let cfg = MediationConfig {
        party_response_timeout_seconds: 0,
        ..MediationConfig::default()
    };
    let client = nostr_sdk::Client::new(nostr_sdk::Keys::generate());

    serbero::mediation::check_party_unresponsive_timeout(&conn, &client, &[], &bundle, &cfg)
        .await
        .unwrap();

    // Session state unchanged, no escalation events.
    assert_eq!(session_state(&conn, "sess-pu0").await, "awaiting_response");
    assert_eq!(
        count_event(&conn, "sess-pu0", "escalation_recommended").await,
        0
    );
    assert_eq!(count_event(&conn, "sess-pu0", "handoff_prepared").await, 0);
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
        rationale_refs: Vec::new(),
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

    // The transport-error path in `initial_classification` records a
    // `reasoning_call_failed` audit row with a stable error category
    // (not the raw provider error string). Pin both facts: the row
    // exists AND the payload does NOT leak the raw error text.
    let payload: String = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT payload_json FROM mediation_events
             WHERE session_id = 'sess-ru' AND kind = 'reasoning_call_failed'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    };
    let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(v["error_category"], "unreachable");
    assert!(
        !payload.contains("network down"),
        "raw provider error text must not leak into the audit payload: {payload}"
    );

    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-ru",
        trigger: EscalationTrigger::ReasoningUnavailable,
        evidence_refs: Vec::new(),
        rationale_refs: Vec::new(),
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
}

#[tokio::test]
async fn authorization_lost_mid_session_triggers_escalation() {
    // Exercise the real session-level handler
    // `session::handle_authorization_lost`, which is the same code
    // path invoked from `open_session` when `draft_and_send_initial_message`
    // returns `Error::AuthorizationLost`. A regression where the
    // handler forgets any of signal_auth_lost / escalation::recommend /
    // notify_solvers_escalation surfaces here as one failure.
    use nostr_sdk::{Client, Keys};
    let conn = seed_session("dispute-al", "sess-al", 100).await;
    let bundle = test_bundle();
    let handle = AuthRetryHandle::new_authorized();
    assert!(handle.is_authorized(), "precondition: handle authorized");

    // Minimal relay-less client — `notify_solvers_escalation` runs
    // with an empty `solvers` slice so it short-circuits without
    // attempting any relay I/O.
    let client = Client::new(Keys::generate());

    session::handle_authorization_lost(
        &conn,
        &client,
        &[],
        "dispute-al",
        "sess-al",
        &handle,
        &bundle,
        "mostro revoked us",
    )
    .await;

    // (1) Escalation events landed with the matching trigger.
    assert_eq!(
        session_state(&conn, "sess-al").await,
        "escalation_recommended"
    );
    let payload = escalation_payload(&conn, "sess-al").await;
    assert!(payload.contains("authorization_lost"), "{payload}");
    assert_eq!(count_event(&conn, "sess-al", "handoff_prepared").await, 1);

    // (2) The auth-retry handle flipped out of `Authorized`.
    assert!(
        !handle.is_authorized(),
        "handle_authorization_lost must flip the auth handle"
    );
}
