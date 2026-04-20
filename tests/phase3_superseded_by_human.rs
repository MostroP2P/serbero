//! US6 — externally resolved disputes supersede active mediation
//! sessions and notify solver(s) with an informational report.

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
    }
}

#[tokio::test]
async fn dispute_resolved_externally_closes_active_session() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-us6-1",
        "notified",
        Some(&solver.pubkey_hex()),
    )
    .await;
    seed_session(&conn, "sess-us6-1", "dispute-us6-1", "awaiting_response").await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-us6-1", "settled");

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
        "expected resolution report DM to assigned solver"
    );

    let (session_state, lifecycle_state, superseded_payload, session_closed_count, notif_count) = {
        let guard = conn.lock().await;
        let session_state = guard
            .query_row(
                "SELECT state FROM mediation_sessions WHERE session_id = 'sess-us6-1'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let lifecycle_state = guard
            .query_row(
                "SELECT lifecycle_state FROM disputes WHERE dispute_id = 'dispute-us6-1'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let superseded_payload = guard
            .query_row(
                "SELECT payload_json FROM mediation_events
                 WHERE session_id = 'sess-us6-1' AND kind = 'superseded_by_human'
                 ORDER BY id ASC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let session_closed_count = guard
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = 'sess-us6-1' AND kind = 'session_closed'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        let notif_count = guard
            .query_row(
                "SELECT COUNT(*) FROM notifications
                 WHERE dispute_id = 'dispute-us6-1'
                   AND notif_type = 'mediation_resolution_report'
                   AND status = 'sent'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        (
            session_state,
            lifecycle_state,
            superseded_payload,
            session_closed_count,
            notif_count,
        )
    };

    assert_eq!(session_state, "closed");
    assert_eq!(lifecycle_state, "resolved");
    assert!(
        superseded_payload.contains("\"resolution_status\":\"settled\""),
        "{superseded_payload}"
    );
    assert_eq!(session_closed_count, 1);
    assert_eq!(notif_count, 1);

    let messages = solver.messages().await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("dispute-us6-1"));
    assert!(messages[0].contains("sess-us6-1"));
    assert!(messages[0].contains("settled"));
}

#[tokio::test]
async fn dispute_resolved_without_active_session_is_noop_for_mediation() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-us6-2",
        "notified",
        Some(&solver.pubkey_hex()),
    )
    .await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-us6-2", "released");

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

    let (lifecycle_state, event_count, notif_count) = {
        let guard = conn.lock().await;
        let lifecycle_state = guard
            .query_row(
                "SELECT lifecycle_state FROM disputes WHERE dispute_id = 'dispute-us6-2'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let event_count = guard
            .query_row("SELECT COUNT(*) FROM mediation_events", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        let notif_count = guard
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        (lifecycle_state, event_count, notif_count)
    };

    assert_eq!(lifecycle_state, "resolved");
    assert_eq!(event_count, 0);
    assert_eq!(notif_count, 0);
    assert_eq!(solver.count().await, 0);
}

#[tokio::test]
async fn dispute_resolved_is_idempotent() {
    let harness = TestHarness::new().await;
    let conn = fresh_conn().await;
    seed_dispute(&conn, "dispute-us6-3", "resolved", None).await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-us6-3", "settled");

    dispute_resolved::handle(&ctx(conn.clone(), client, Vec::new()), &event)
        .await
        .unwrap();

    let (lifecycle_state, transition_count, event_count, notif_count) = {
        let guard = conn.lock().await;
        let lifecycle_state = guard
            .query_row(
                "SELECT lifecycle_state FROM disputes WHERE dispute_id = 'dispute-us6-3'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let transition_count = guard
            .query_row(
                "SELECT COUNT(*) FROM dispute_state_transitions WHERE dispute_id = 'dispute-us6-3'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        let event_count = guard
            .query_row("SELECT COUNT(*) FROM mediation_events", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        let notif_count = guard
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        (lifecycle_state, transition_count, event_count, notif_count)
    };

    assert_eq!(lifecycle_state, "resolved");
    assert_eq!(transition_count, 0);
    assert_eq!(event_count, 0);
    assert_eq!(notif_count, 0);
}

#[tokio::test]
async fn dispute_resolved_on_escalated_session_is_noop() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(
        &conn,
        "dispute-us6-4",
        "escalated",
        Some(&solver.pubkey_hex()),
    )
    .await;
    seed_session(
        &conn,
        "sess-us6-4",
        "dispute-us6-4",
        "escalation_recommended",
    )
    .await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-us6-4", "settled");

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

    let (session_state, lifecycle_state, event_count, notif_count) = {
        let guard = conn.lock().await;
        let session_state = guard
            .query_row(
                "SELECT state FROM mediation_sessions WHERE session_id = 'sess-us6-4'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let lifecycle_state = guard
            .query_row(
                "SELECT lifecycle_state FROM disputes WHERE dispute_id = 'dispute-us6-4'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let event_count = guard
            .query_row("SELECT COUNT(*) FROM mediation_events", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        let notif_count = guard
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        (session_state, lifecycle_state, event_count, notif_count)
    };

    assert_eq!(session_state, "escalation_recommended");
    assert_eq!(lifecycle_state, "resolved");
    assert_eq!(event_count, 0);
    assert_eq!(notif_count, 0);
    assert_eq!(solver.count().await, 0);
}

#[tokio::test]
async fn seller_release_triggers_superseded() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    seed_dispute(&conn, "dispute-us6-5", "taken", Some(&solver.pubkey_hex())).await;
    seed_session(&conn, "sess-us6-5", "dispute-us6-5", "awaiting_response").await;

    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let event = build_resolution_event(&harness.mostro_keys, "dispute-us6-5", "released");

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
        "expected resolution report DM on released dispute"
    );

    let (session_state, lifecycle_state, superseded_payload, session_closed_count) = {
        let guard = conn.lock().await;
        let session_state = guard
            .query_row(
                "SELECT state FROM mediation_sessions WHERE session_id = 'sess-us6-5'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let lifecycle_state = guard
            .query_row(
                "SELECT lifecycle_state FROM disputes WHERE dispute_id = 'dispute-us6-5'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let superseded_payload = guard
            .query_row(
                "SELECT payload_json FROM mediation_events
                 WHERE session_id = 'sess-us6-5' AND kind = 'superseded_by_human'
                 ORDER BY id ASC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let session_closed_count = guard
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = 'sess-us6-5' AND kind = 'session_closed'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        (
            session_state,
            lifecycle_state,
            superseded_payload,
            session_closed_count,
        )
    };

    assert_eq!(session_state, "closed");
    assert_eq!(lifecycle_state, "resolved");
    assert!(
        superseded_payload.contains("\"resolution_status\":\"released\""),
        "{superseded_payload}"
    );
    assert_eq!(session_closed_count, 1);
}
