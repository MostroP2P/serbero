//! Phase 10 / T111 — FR-124 final solver-facing resolution report
//! (SC-111).
//!
//! Pins the end-to-end delivery contract for
//! `handlers/dispute_resolved.rs` → `mediation::report::emit_final_report`
//! across the six shapes Phase 3 produces:
//!
//!   1. Full session — outbound + inbound messages, classification recorded.
//!   2. Outbound-only session — Serbero messaged parties but none replied.
//!   3. Escalation-recommended session — Phase 4 handoff already delivered;
//!      FR-124 DM still fires to close the loop for the solver.
//!   4. Reasoning-verdict-only (FR-122) — a dispute-scoped verdict exists
//!      but no session row was ever committed. FR-124 DM fires with
//!      `Session: <none — dispute-scoped handoff>` and the classification
//!      pulled from the verdict event's payload.
//!   5. No mediation context — Phase 1/2 handled the dispute; FR-124 MUST
//!      NOT fire.
//!   6. Idempotency — a replay of the same `DisputeStatus` event does NOT
//!      produce a second DM or a second audit row.
//!
//! These tests seed the DB directly (no daemon, no reasoning provider)
//! and invoke `dispute_resolved::handle` to drive the
//! `has_any_mediation_context → emit_final_report` path added in
//! T107/T108/T109. The T106 integration test covers the FR-122
//! reasoning-before-take ordering; this file covers the FR-124
//! downstream report shape.

mod common;

use std::sync::Arc;

use common::{publisher, solver_cfg, SolverListener, TestHarness, DISPUTE_EVENT_KIND};
use nostr_sdk::{Alphabet, Client, Event, EventBuilder, Keys, Kind, SingleLetterTag, Tag, TagKind};
use serbero::db;
use serbero::handlers::dispute_detected::HandlerContext;
use serbero::handlers::dispute_resolved;
use serbero::models::{SolverConfig, SolverPermission};
use tokio::sync::Mutex as AsyncMutex;

async fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
    let mut conn = db::open_in_memory().unwrap();
    db::migrations::run_migrations(&mut conn).unwrap();
    Arc::new(AsyncMutex::new(conn))
}

async fn seed_dispute(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    lifecycle_state: &str,
    assigned_solver: Option<&str>,
) {
    let guard = conn.lock().await;
    guard
        .execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state,
                assigned_solver, last_notified_at, last_state_change
             ) VALUES (?1, 'e1', 'm1', 'buyer', 'in-progress', 1, 2, ?2, ?3, NULL, NULL)",
            rusqlite::params![dispute_id, lifecycle_state, assigned_solver],
        )
        .unwrap();
}

async fn seed_session(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    dispute_id: &str,
    state: &str,
) {
    let guard = conn.lock().await;
    guard
        .execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                buyer_shared_pubkey, seller_shared_pubkey,
                started_at, last_transition_at
             ) VALUES (?1, ?2, ?3, 0,
                       'phase3-default', 'test-policy-hash',
                       'buyer-shared', 'seller-shared',
                       100, 100)",
            rusqlite::params![session_id, dispute_id, state],
        )
        .unwrap();
}

async fn seed_message(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    direction: &str,
    party: &str,
    inner_event_id: &str,
) {
    let guard = conn.lock().await;
    guard
        .execute(
            "INSERT INTO mediation_messages (
                session_id, direction, party, shared_pubkey,
                inner_event_id, inner_event_created_at, outer_event_id,
                content, prompt_bundle_id, policy_hash, persisted_at
             ) VALUES (?1, ?2, ?3, 'shared', ?4, 100, NULL,
                       'body', 'phase3-default', 'test-policy-hash', 100)",
            rusqlite::params![session_id, direction, party, inner_event_id],
        )
        .unwrap();
}

async fn seed_classification_event(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    classification: &str,
    confidence: f64,
) {
    let guard = conn.lock().await;
    let payload = format!(r#"{{"classification":"{classification}","confidence":{confidence}}}"#,);
    db::mediation_events::record_event(
        &guard,
        db::mediation_events::MediationEventKind::ClassificationProduced,
        Some(session_id),
        &payload,
        None,
        Some("phase3-default"),
        Some("test-policy-hash"),
        100,
    )
    .unwrap();
}

async fn seed_dispute_scoped_reasoning_verdict(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    classification: &str,
    confidence: f64,
    decision: &str,
) {
    let guard = conn.lock().await;
    let payload = format!(
        r#"{{"dispute_id":"{dispute_id}","classification":"{classification}","confidence":{confidence},"decision":"{decision}"}}"#,
    );
    db::mediation_events::record_event(
        &guard,
        db::mediation_events::MediationEventKind::ReasoningVerdict,
        None,
        &payload,
        None,
        Some("phase3-default"),
        Some("test-policy-hash"),
        100,
    )
    .unwrap();
}

fn build_resolution_event(keys: &Keys, dispute_id: &str, status: &str) -> Event {
    let tags = vec![
        Tag::identifier(dispute_id),
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::S)),
            [status],
        ),
    ];
    EventBuilder::new(Kind::Custom(DISPUTE_EVENT_KIND), "")
        .tags(tags)
        .sign_with_keys(keys)
        .unwrap()
}

fn ctx(
    conn: Arc<AsyncMutex<rusqlite::Connection>>,
    client: Client,
    solvers: Vec<SolverConfig>,
) -> HandlerContext {
    HandlerContext {
        conn,
        client,
        solvers,
        phase3: None,
    }
}

async fn final_report_event_count(conn: &Arc<AsyncMutex<rusqlite::Connection>>) -> i64 {
    let guard = conn.lock().await;
    guard
        .query_row(
            "SELECT COUNT(*) FROM mediation_events \
             WHERE kind = 'resolved_externally_reported'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
}

async fn notif_count(conn: &Arc<AsyncMutex<rusqlite::Connection>>, dispute_id: &str) -> i64 {
    let guard = conn.lock().await;
    guard
        .query_row(
            "SELECT COUNT(*) FROM notifications \
             WHERE dispute_id = ?1 \
               AND notif_type = 'mediation_resolution_report' \
               AND status = 'sent'",
            rusqlite::params![dispute_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
}

#[tokio::test]
async fn full_session_resolved_externally() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-fr124-full",
        "notified",
        Some(&solver.pubkey_hex()),
    )
    .await;
    seed_session(
        &conn,
        "sess-fr124-full",
        "dispute-fr124-full",
        "awaiting_response",
    )
    .await;

    // Two outbound (buyer + seller) and two inbound messages.
    seed_message(&conn, "sess-fr124-full", "outbound", "buyer", "out-b-1").await;
    seed_message(&conn, "sess-fr124-full", "outbound", "seller", "out-s-1").await;
    seed_message(&conn, "sess-fr124-full", "inbound", "buyer", "in-b-1").await;
    seed_message(&conn, "sess-fr124-full", "inbound", "seller", "in-s-1").await;

    // Classification recorded on the session.
    seed_classification_event(
        &conn,
        "sess-fr124-full",
        "coordination_failure_resolvable",
        0.88,
    )
    .await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-fr124-full", "settled");

    dispute_resolved::handle(
        &ctx(
            conn.clone(),
            client,
            vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        ),
        &event,
    )
    .await
    .unwrap();

    assert!(
        solver.wait_for(1, 10).await,
        "expected FR-124 DM for full session"
    );

    assert_eq!(final_report_event_count(&conn).await, 1);
    assert_eq!(notif_count(&conn, "dispute-fr124-full").await, 1);

    let messages = solver.messages().await;
    assert_eq!(messages.len(), 1);
    let body = &messages[0];
    assert!(
        body.starts_with("mediation_resolution_report/v1"),
        "body must carry versioned prefix; got: {body}"
    );
    assert!(body.contains("dispute-fr124-full"));
    assert!(body.contains("sess-fr124-full"));
    assert!(body.contains("coordination_failure_resolvable"));
    assert!(body.contains("0.88"));
    assert!(
        body.contains("Outbound party messages: 2"),
        "counter must be 2 for both parties messaged; got: {body}"
    );
    assert!(body.contains("settled"));
}

#[tokio::test]
async fn outbound_only_session_resolved_externally() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-fr124-out",
        "notified",
        Some(&solver.pubkey_hex()),
    )
    .await;
    seed_session(
        &conn,
        "sess-fr124-out",
        "dispute-fr124-out",
        "awaiting_response",
    )
    .await;

    // Only the buyer was messaged; no replies.
    seed_message(&conn, "sess-fr124-out", "outbound", "buyer", "out-b-1").await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-fr124-out", "settled");

    dispute_resolved::handle(
        &ctx(
            conn.clone(),
            client,
            vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        ),
        &event,
    )
    .await
    .unwrap();

    assert!(solver.wait_for(1, 10).await);

    assert_eq!(final_report_event_count(&conn).await, 1);
    assert_eq!(notif_count(&conn, "dispute-fr124-out").await, 1);

    let body = &solver.messages().await[0];
    assert!(body.starts_with("mediation_resolution_report/v1"));
    assert!(body.contains("dispute-fr124-out"));
    assert!(
        body.contains("Outbound party messages: 1"),
        "counter must be 1 (only buyer messaged); got: {body}"
    );
    // No classification was recorded.
    assert!(
        body.contains("Classification: <none recorded>"),
        "body should note missing classification; got: {body}"
    );
}

#[tokio::test]
async fn escalation_recommended_resolved_externally() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-fr124-esc",
        "escalated",
        Some(&solver.pubkey_hex()),
    )
    .await;
    seed_session(
        &conn,
        "sess-fr124-esc",
        "dispute-fr124-esc",
        "escalation_recommended",
    )
    .await;

    // Seed a classification so the body proves it was populated from
    // the session-scoped event even when the session is escalated.
    seed_classification_event(&conn, "sess-fr124-esc", "conflicting_claims", 0.72).await;

    // Simulate the Phase 4 handoff row that was already delivered
    // when the session escalated. FR-124 must NOT mutate this row.
    {
        let guard = conn.lock().await;
        db::mediation_events::record_event(
            &guard,
            db::mediation_events::MediationEventKind::HandoffPrepared,
            Some("sess-fr124-esc"),
            r#"{"reason":"fraud_indicator"}"#,
            None,
            Some("phase3-default"),
            Some("test-policy-hash"),
            100,
        )
        .unwrap();
    }

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-fr124-esc", "settled");

    dispute_resolved::handle(
        &ctx(
            conn.clone(),
            client,
            vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        ),
        &event,
    )
    .await
    .unwrap();

    assert!(solver.wait_for(1, 10).await);

    // FR-124 DM fires.
    assert_eq!(final_report_event_count(&conn).await, 1);
    assert_eq!(notif_count(&conn, "dispute-fr124-esc").await, 1);

    // Session state is NOT flipped (EscalationRecommended →
    // SupersededByHuman is not a legal transition).
    let session_state: String = {
        let guard = conn.lock().await;
        guard
            .query_row(
                "SELECT state FROM mediation_sessions WHERE session_id = 'sess-fr124-esc'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(session_state, "escalation_recommended");

    // Phase 4 handoff row remains intact.
    let handoff_count: i64 = {
        let guard = conn.lock().await;
        guard
            .query_row(
                "SELECT COUNT(*) FROM mediation_events \
                 WHERE kind = 'handoff_prepared' AND session_id = 'sess-fr124-esc'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(handoff_count, 1, "Phase 4 handoff row must not be mutated");

    let body = &solver.messages().await[0];
    assert!(body.contains("dispute-fr124-esc"));
    assert!(body.contains("sess-fr124-esc"));
    assert!(
        body.contains("escalation_recommended"),
        "narrative must surface the escalation state; got: {body}"
    );
    assert!(body.contains("conflicting_claims"));
}

#[tokio::test]
async fn reasoning_verdict_no_session_resolved_externally() {
    // FR-122 shape: reasoning ran and recorded a dispute-scoped
    // verdict, but no `mediation_sessions` row exists (e.g.
    // `TakeDispute` failed or the verdict was `Escalate`). FR-124
    // must still fire, with `session_id = None` and classification
    // pulled from the verdict event's payload.
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-fr124-verdict",
        "notified",
        Some(&solver.pubkey_hex()),
    )
    .await;
    seed_dispute_scoped_reasoning_verdict(
        &conn,
        "dispute-fr124-verdict",
        "suspected_fraud",
        0.95,
        "escalate",
    )
    .await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-fr124-verdict", "settled");

    dispute_resolved::handle(
        &ctx(
            conn.clone(),
            client,
            vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        ),
        &event,
    )
    .await
    .unwrap();

    assert!(solver.wait_for(1, 10).await);

    assert_eq!(final_report_event_count(&conn).await, 1);
    assert_eq!(notif_count(&conn, "dispute-fr124-verdict").await, 1);

    let body = &solver.messages().await[0];
    assert!(body.starts_with("mediation_resolution_report/v1"));
    assert!(body.contains("dispute-fr124-verdict"));
    assert!(
        body.contains("Session: <none"),
        "body must mark the session-less path; got: {body}"
    );
    assert!(body.contains("suspected_fraud"));
    assert!(body.contains("0.95"));
    assert!(
        body.contains("Outbound party messages: 0"),
        "counter must be 0 for session-less path; got: {body}"
    );
}

#[tokio::test]
async fn no_mediation_context_no_report() {
    // Phase 1/2-only dispute: Serbero notified solvers but never
    // opened a session nor ran reasoning. External resolution
    // therefore MUST NOT produce a FR-124 DM — `has_any_mediation_context`
    // returns false and the handler returns early.
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-fr124-p12only",
        "notified",
        Some(&solver.pubkey_hex()),
    )
    .await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-fr124-p12only", "settled");

    dispute_resolved::handle(
        &ctx(
            conn.clone(),
            client,
            vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        ),
        &event,
    )
    .await
    .unwrap();

    // Give the relay a moment to deliver a (hypothetical) DM.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert_eq!(solver.count().await, 0, "no FR-124 DM for Phase 1/2-only");
    assert_eq!(final_report_event_count(&conn).await, 0);
    assert_eq!(notif_count(&conn, "dispute-fr124-p12only").await, 0);

    // Lifecycle still flips to resolved — FR-124 is orthogonal to
    // the lifecycle update.
    let lifecycle: String = {
        let guard = conn.lock().await;
        guard
            .query_row(
                "SELECT lifecycle_state FROM disputes WHERE dispute_id = 'dispute-fr124-p12only'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(lifecycle, "resolved");
}

#[tokio::test]
async fn idempotency_no_double_send() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-fr124-idem",
        "notified",
        Some(&solver.pubkey_hex()),
    )
    .await;
    seed_session(
        &conn,
        "sess-fr124-idem",
        "dispute-fr124-idem",
        "awaiting_response",
    )
    .await;
    seed_message(&conn, "sess-fr124-idem", "outbound", "buyer", "out-b-1").await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-fr124-idem", "settled");
    let handler_ctx = ctx(
        conn.clone(),
        client,
        vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
    );

    // First delivery — DM fires, event row written.
    dispute_resolved::handle(&handler_ctx, &event)
        .await
        .unwrap();
    assert!(solver.wait_for(1, 10).await);
    assert_eq!(final_report_event_count(&conn).await, 1);
    assert_eq!(notif_count(&conn, "dispute-fr124-idem").await, 1);

    // Replay: same event body, same dispute id, already at
    // `LifecycleState::Resolved` from the first call. The outer
    // handler short-circuits before the report path runs.
    dispute_resolved::handle(&handler_ctx, &event)
        .await
        .unwrap();

    // Give the relay a brief window to surface any accidental DM.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert_eq!(
        solver.count().await,
        1,
        "replay must not produce a second FR-124 DM"
    );
    assert_eq!(
        final_report_event_count(&conn).await,
        1,
        "replay must not produce a second resolved_externally_reported event"
    );
    assert_eq!(
        notif_count(&conn, "dispute-fr124-idem").await,
        1,
        "replay must not write a second notifications row"
    );
}
