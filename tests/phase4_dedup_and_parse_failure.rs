//! Phase 6 — T030 FR-214 parse-failed + queue-poisoning tests.
//!
//! Three sub-tests pin the two FR-214 sub-shapes and the scan-level
//! "mark consumed" guarantee that prevents a single malformed
//! handoff from poisoning the queue on every cycle:
//!
//!   * `malformed_payload_records_parse_failed_and_moves_on` —
//!     `handoff_prepared.payload_json` is not valid JSON. The
//!     dispatcher writes one `escalation_dispatch_parse_failed`
//!     audit row with `reason = 'deserialize_failed'` and `detail`
//!     carrying the parser error, and a second cycle over the same
//!     DB does NOT re-emit the audit row — proving the
//!     `NOT EXISTS` clause inside `list_pending_handoffs` filters
//!     the handoff out once the audit is in place.
//!
//!   * `orphan_dispute_reference_records_parse_failed` — the
//!     payload parses cleanly into a `HandoffPackage` but its
//!     `dispute_id` has no row in `disputes`. The dispatcher
//!     writes one `escalation_dispatch_parse_failed` audit row
//!     with `reason = 'orphan_dispute_reference'` and `detail =
//!     "dispute_id not found"`.
//!
//!   * `poisoning_is_prevented` — a malformed handoff and a valid
//!     handoff coexist in the queue. One cycle emits the
//!     parse_failed audit for the broken one and dispatches the
//!     valid one. On a second cycle, neither fires again: the
//!     parse_failed path is consumed by its audit row and the
//!     happy path is consumed by its `escalation_dispatches` row.
//!     This is the core queue-poisoning regression guard: a
//!     malformed event must NEVER re-trigger its handler on every
//!     cycle.

mod common;

use std::sync::Arc;

use common::{publisher, solver_cfg, SolverListener, TestHarness};
use rusqlite::params;
use serbero::db::migrations::run_migrations;
use serbero::db::open_in_memory;
use serbero::escalation;
use serbero::mediation::escalation::HandoffPackage;
use serbero::models::{EscalationConfig, SolverConfig, SolverPermission};
use tokio::sync::Mutex as AsyncMutex;

async fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
    let mut c = open_in_memory().unwrap();
    run_migrations(&mut c).unwrap();
    Arc::new(AsyncMutex::new(c))
}

fn sample_cfg() -> EscalationConfig {
    EscalationConfig {
        enabled: true,
        dispatch_interval_seconds: 1,
        fallback_to_all_solvers: false,
    }
}

fn sample_package(dispute_id: &str) -> HandoffPackage {
    HandoffPackage {
        dispute_id: dispute_id.to_string(),
        session_id: None,
        trigger: "conflicting_claims".to_string(),
        evidence_refs: Vec::new(),
        prompt_bundle_id: "phase3-default".to_string(),
        policy_hash: "hash-1".to_string(),
        rationale_refs: vec!["9f86d081884c".to_string()],
        assembled_at: 1_745_000_000,
    }
}

/// Seed a dispute row + a `handoff_prepared` row with
/// `payload_json = raw_payload`. The caller passes the payload
/// verbatim so parse-failed tests can inject deliberately invalid
/// JSON or orphaned dispute ids.
async fn seed_dispute_and_handoff_raw(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: Option<&str>,
    raw_payload: &str,
) -> i64 {
    let c = conn.lock().await;
    if let Some(d) = dispute_id {
        c.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state,
                assigned_solver
             ) VALUES (?1, ?2, 'mostro', 'buyer',
                       'initiated', 10, 11, 'notified', NULL)",
            params![d, format!("evt-{d}")],
        )
        .unwrap();
    }
    c.query_row(
        "INSERT INTO mediation_events (
            session_id, kind, payload_json,
            prompt_bundle_id, policy_hash, occurred_at
         ) VALUES (NULL, 'handoff_prepared', ?1,
                   'phase3-default', 'hash-1', 100)
         RETURNING id",
        params![raw_payload],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
}

async fn count(conn: &Arc<AsyncMutex<rusqlite::Connection>>, sql: &str) -> i64 {
    let c = conn.lock().await;
    c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap()
}

#[tokio::test]
async fn malformed_payload_records_parse_failed_and_moves_on() {
    let harness = TestHarness::new().await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    // Payload that can't deserialize into `HandoffPackage` but IS
    // valid JSON shape-wise — just missing fields / wrong types.
    // The `dispute_id` key is present so the best-effort extract
    // in `process_one` can pin it onto the audit row.
    let raw = r#"{"dispute_id":"d-malformed","this_is":"broken","missing_required_fields":true}"#;
    let handoff_id = seed_dispute_and_handoff_raw(&conn, None, raw).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();

    // The solver listener must NOT have received any DM — parse
    // failed BEFORE the send loop.
    match solver
        .assert_no_messages_within(std::time::Duration::from_millis(500))
        .await
    {
        Ok(()) => {}
        Err(n) => panic!("parse-failed path must not send a DM (got {n})"),
    }

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        0,
        "parse_failed must NOT write a dispatch row"
    );

    // Exactly one parse_failed audit row, reason = deserialize_failed,
    // detail populated, dispute_id extracted from the raw payload.
    let (hid, reason, detail, dispute_id): (i64, String, String, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.handoff_event_id'),
                    json_extract(payload_json, '$.reason'),
                    json_extract(payload_json, '$.detail'),
                    json_extract(payload_json, '$.dispute_id')
             FROM mediation_events WHERE kind = 'escalation_dispatch_parse_failed'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    assert_eq!(hid, handoff_id);
    assert_eq!(reason, "deserialize_failed");
    assert!(
        !detail.is_empty(),
        "detail must carry the parser error for operator inspection"
    );
    assert_eq!(
        dispute_id, "d-malformed",
        "best-effort dispute_id extraction should have recovered the key from the raw JSON"
    );

    // Second cycle — `list_pending_handoffs`'s `NOT EXISTS` clause
    // against `kind = 'escalation_dispatch_parse_failed'` must
    // filter the handoff out so no second audit row appears.
    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatch_parse_failed'",
        )
        .await,
        1,
        "cycle 2 must NOT re-emit the parse_failed audit — the scan filters the handoff out"
    );
}

#[tokio::test]
async fn malformed_payload_with_unrecoverable_dispute_id_uses_unknown_sentinel() {
    // Defensive edge case: when the payload is so corrupted that
    // even `serde_json::Value` cannot extract `$.dispute_id`, the
    // parse_failed audit row uses the sentinel string "unknown"
    // so the required payload field is still populated. Operators
    // cross-reference by `handoff_event_id` in this case.
    let harness = TestHarness::new().await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = Vec::new();

    // Not JSON at all.
    let raw = "this is not json at all {{{";
    let _handoff_id = seed_dispute_and_handoff_raw(&conn, None, raw).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();

    let dispute_id: String = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.dispute_id')
             FROM mediation_events WHERE kind = 'escalation_dispatch_parse_failed'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        dispute_id, "unknown",
        "unrecoverable payload must fall back to the 'unknown' sentinel rather than drop the audit"
    );
}

#[tokio::test]
async fn orphan_dispute_reference_records_parse_failed() {
    let harness = TestHarness::new().await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    // Payload that parses cleanly, but the referenced `dispute_id`
    // has no row in `disputes` (note: seed_dispute_and_handoff_raw
    // is called with `dispute_id = None` so the FK is absent).
    let pkg = sample_package("d-orphan");
    let raw = serde_json::to_string(&pkg).unwrap();
    let handoff_id = seed_dispute_and_handoff_raw(&conn, None, &raw).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();

    match solver
        .assert_no_messages_within(std::time::Duration::from_millis(500))
        .await
    {
        Ok(()) => {}
        Err(n) => panic!("orphan-dispute path must not send a DM (got {n})"),
    }
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        0,
        "orphan_dispute_reference must NOT write a dispatch row"
    );

    let (hid, reason, detail, dispute_id): (i64, String, String, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.handoff_event_id'),
                    json_extract(payload_json, '$.reason'),
                    json_extract(payload_json, '$.detail'),
                    json_extract(payload_json, '$.dispute_id')
             FROM mediation_events WHERE kind = 'escalation_dispatch_parse_failed'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    assert_eq!(hid, handoff_id);
    assert_eq!(reason, "orphan_dispute_reference");
    assert_eq!(
        detail, "dispute_id not found",
        "detail for orphan_dispute_reference is the fixed string pinned in contracts/audit-events.md"
    );
    assert_eq!(
        dispute_id, "d-orphan",
        "dispute_id field carries the resolved-but-not-found value verbatim"
    );
}

#[tokio::test]
async fn poisoning_is_prevented() {
    // Queue-poisoning regression guard. A single malformed handoff
    // must not prevent the dispatcher from making progress on a
    // sibling valid handoff in the same cycle, and — critically —
    // must not re-trigger its handler on every subsequent cycle.
    //
    // Cycle 1: both handoffs are visible to the scan; the dispatcher
    // writes one parse_failed audit row for the broken one and one
    // dispatch row (plus DM) for the valid one.
    //
    // Cycle 2: `list_pending_handoffs` filters both out — the
    // parse_failed path via the `NOT EXISTS` clause against
    // `escalation_dispatch_parse_failed`, the valid path via the
    // `LEFT JOIN ... d.dispatch_id IS NULL` clause against
    // `escalation_dispatches`. Nothing new should fire.
    let harness = TestHarness::new().await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    // Malformed payload (no dispute row, no valid package).
    let malformed_id = seed_dispute_and_handoff_raw(&conn, None, "not valid json").await;

    // Valid payload with its own dispute row.
    let pkg = sample_package("d-valid");
    let valid_payload = serde_json::to_string(&pkg).unwrap();
    let valid_id = seed_dispute_and_handoff_raw(&conn, Some("d-valid"), &valid_payload).await;

    // Cycle 1: both handoffs processed; malformed → parse_failed,
    // valid → dispatched.
    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();

    assert!(
        solver.wait_for(1, 10).await,
        "the valid handoff must dispatch to the configured Write solver"
    );
    let dispatched_count = count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await;
    assert_eq!(
        dispatched_count, 1,
        "exactly one dispatch row from the valid handoff; parse_failed branch writes none"
    );
    let dispatched_handoff: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT handoff_event_id FROM escalation_dispatches",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        dispatched_handoff, valid_id,
        "the dispatch row must key on the VALID handoff, not the malformed one"
    );
    let pf_handoff: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.handoff_event_id')
             FROM mediation_events WHERE kind = 'escalation_dispatch_parse_failed'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        pf_handoff, malformed_id,
        "the parse_failed audit must key on the MALFORMED handoff, not the valid one"
    );

    // Cycle 2: BOTH handoffs must stay consumed. No new parse_failed
    // row, no new dispatch row, and no second DM to the solver.
    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        solver.count().await,
        1,
        "cycle 2 must NOT re-dispatch the valid handoff (at-least-once, consumer dedup)"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        1,
        "cycle 2 must NOT add another dispatch row"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatch_parse_failed'",
        )
        .await,
        1,
        "cycle 2 must NOT re-emit the parse_failed audit — the scan's NOT EXISTS filters it"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatched'",
        )
        .await,
        1,
        "cycle 2 must NOT add another dispatched audit row"
    );
}
