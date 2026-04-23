//! Phase 4 — T017/T018 US1 integration tests.
//!
//! Pins the full US1 dispatch path end-to-end against a real
//! `MockRelay` + `SolverListener` pipeline: a seeded
//! `handoff_prepared` audit event triggers one structured
//! `escalation_handoff/v1` DM, one `escalation_dispatches` row,
//! and one `escalation_dispatched` audit event. Every sub-test
//! exercises a distinct branch of the FR-202 router table or a
//! distinct failure/dedup invariant.
//!
//! We drive `escalation::run_once` directly rather than the
//! full `run_dispatcher` interval loop — the loop shape is
//! already covered by `tests/phase4_foundational.rs`. Running
//! single cycles here keeps the tests fast (no tokio::time waits)
//! and deterministic (one handoff per cycle).

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

/// Build an in-memory DB with Phase 4 schema applied. Tests that
/// want to spawn a full daemon already have `tests/phase4_foundational.rs`;
/// here we need tighter control over which handoff lands when.
async fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
    let mut c = open_in_memory().unwrap();
    run_migrations(&mut c).unwrap();
    Arc::new(AsyncMutex::new(c))
}

fn sample_cfg(fallback: bool) -> EscalationConfig {
    EscalationConfig {
        enabled: true,
        dispatch_interval_seconds: 1,
        fallback_to_all_solvers: fallback,
    }
}

/// Seed a dispute row + a `handoff_prepared` mediation_event row.
/// Returns the newly-minted handoff event id so the test can
/// assert dedup / dispatch pairing against it.
async fn seed_dispute_and_handoff(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    assigned_solver: Option<&str>,
    pkg: &HandoffPackage,
) -> i64 {
    let c = conn.lock().await;
    c.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state,
            assigned_solver
         ) VALUES (?1, ?2, 'mostro', 'buyer',
                   'initiated', 10, 11, 'notified', ?3)",
        params![dispute_id, format!("evt-{dispute_id}"), assigned_solver],
    )
    .unwrap();
    if let Some(sid) = pkg.session_id.as_deref() {
        // Seed a minimal session row so the FK on
        // escalation_dispatches.session_id resolves when the
        // tracker writes the dispatch row.
        c.execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                started_at, last_transition_at
             ) VALUES (?1, ?2, 'escalation_recommended', 0,
                       'phase3-default', 'hash-1', 100, 100)",
            params![sid, dispute_id],
        )
        .unwrap();
    }
    let payload = serde_json::to_string(pkg).unwrap();
    c.query_row(
        "INSERT INTO mediation_events (
            session_id, kind, payload_json,
            prompt_bundle_id, policy_hash, occurred_at
         ) VALUES (?1, 'handoff_prepared', ?2,
                   'phase3-default', 'hash-1', 100)
         RETURNING id",
        params![pkg.session_id, payload],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
}

fn sample_package(dispute_id: &str, session_id: Option<&str>) -> HandoffPackage {
    HandoffPackage {
        dispute_id: dispute_id.to_string(),
        session_id: session_id.map(|s| s.to_string()),
        trigger: "conflicting_claims".to_string(),
        evidence_refs: vec!["inner-event-1".to_string()],
        prompt_bundle_id: "phase3-default".to_string(),
        policy_hash: "hash-1".to_string(),
        rationale_refs: vec!["9f86d081884c".to_string()],
        assembled_at: 1_745_000_000,
    }
}

#[tokio::test]
async fn targeted_write_solver_receives_dm() {
    // US1 scenario 3: `assigned_solver` matches a configured
    // Write solver → DM targets exactly that solver, `target_solver`
    // column = that pubkey, `fallback_broadcast = 0`, status =
    // 'dispatched', one paired audit row.
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    let pkg = sample_package("d-target", Some("sess-target"));
    let handoff_id =
        seed_dispute_and_handoff(&conn, "d-target", Some(&solver.pubkey_hex()), &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(false),
    )
    .await
    .unwrap();

    assert!(
        solver.wait_for(1, 10).await,
        "targeted write solver did not receive the handoff DM"
    );
    let msg = solver.messages().await.remove(0);
    assert!(msg.starts_with("escalation_handoff/v1"));
    assert!(msg.contains("d-target"));
    assert!(msg.contains("sess-target"));
    assert!(msg.contains("conflicting_claims"));

    let (dispatch_id, target_solver, status, fb): (String, String, String, i64) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT dispatch_id, target_solver, status, fallback_broadcast
             FROM escalation_dispatches WHERE handoff_event_id = ?1",
            params![handoff_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    assert_eq!(target_solver, solver.pubkey_hex());
    assert_eq!(status, "dispatched");
    assert_eq!(fb, 0, "targeted path must NOT set fallback_broadcast");

    // Paired audit event.
    let audit_count: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_events
             WHERE kind = 'escalation_dispatched'
               AND json_extract(payload_json, '$.dispatch_id') = ?1",
            params![dispatch_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(audit_count, 1, "exactly one paired audit row expected");
}

#[tokio::test]
async fn broadcast_to_all_write_solvers_when_assigned_unknown() {
    // US1 scenario 2: assigned_solver is NULL (or matches an
    // unconfigured solver) and two write solvers exist →
    // broadcast to both. target_solver = comma-joined list in
    // send-loop order.
    let harness = TestHarness::new().await;
    let solver_a = SolverListener::start(&harness.relay_url).await;
    let solver_b = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![
        solver_cfg(solver_a.pubkey_hex(), SolverPermission::Write),
        solver_cfg(solver_b.pubkey_hex(), SolverPermission::Write),
    ];

    let pkg = sample_package("d-broadcast", None);
    let handoff_id = seed_dispute_and_handoff(&conn, "d-broadcast", None, &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(false),
    )
    .await
    .unwrap();

    assert!(solver_a.wait_for(1, 10).await, "solver A missed the DM");
    assert!(solver_b.wait_for(1, 10).await, "solver B missed the DM");

    let (target_solver, fb): (String, i64) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT target_solver, fallback_broadcast
             FROM escalation_dispatches WHERE handoff_event_id = ?1",
            params![handoff_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    let parts: Vec<&str> = target_solver.split(',').collect();
    assert_eq!(parts.len(), 2, "target_solver must list both recipients");
    assert!(parts.contains(&solver_a.pubkey_hex().as_str()));
    assert!(parts.contains(&solver_b.pubkey_hex().as_str()));
    assert_eq!(fb, 0, "write-set broadcast is NOT the fallback path");
}

#[tokio::test]
async fn read_permission_assignment_falls_back_to_broadcast() {
    // US1 scenario 4: assigned_solver points at a Read-permission
    // solver → that solver is IGNORED; broadcast to the Write set.
    let harness = TestHarness::new().await;
    let read_solver = SolverListener::start(&harness.relay_url).await;
    let write_solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![
        solver_cfg(read_solver.pubkey_hex(), SolverPermission::Read),
        solver_cfg(write_solver.pubkey_hex(), SolverPermission::Write),
    ];

    let pkg = sample_package("d-readassign", None);
    seed_dispute_and_handoff(&conn, "d-readassign", Some(&read_solver.pubkey_hex()), &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(false),
    )
    .await
    .unwrap();

    assert!(
        write_solver.wait_for(1, 10).await,
        "write solver must receive the DM"
    );
    // Read solver must NOT receive anything — a single-second
    // window is enough since the send loop is synchronous and the
    // write-solver delivery already landed above.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        read_solver.count().await,
        0,
        "read-permission solver MUST NOT receive the handoff DM"
    );
}

#[tokio::test]
async fn dispute_scoped_handoff_emits_none_session_marker() {
    // US1 scenario 2 (FR-122 shape): session_id = None in the
    // HandoffPackage → DM body carries the literal marker string
    // and the JSON payload line omits the session_id key entirely.
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    // session_id deliberately None — matches FR-122 dispute-scoped
    // handoffs where no mediation session was ever opened.
    let pkg = sample_package("d-disp-scoped", None);
    seed_dispute_and_handoff(&conn, "d-disp-scoped", None, &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(false),
    )
    .await
    .unwrap();

    assert!(solver.wait_for(1, 10).await);
    let msg = solver.messages().await.remove(0);
    assert!(
        msg.contains("Session: <none — dispute-scoped handoff>"),
        "dispute-scoped handoff must render the <none> marker; got: {msg}"
    );
    let json_line = msg
        .lines()
        .skip_while(|l| !l.starts_with("Handoff payload (JSON)"))
        .nth(1)
        .unwrap();
    assert!(
        !json_line.contains("session_id"),
        "JSON payload must OMIT the session_id key entirely when None; got: {json_line}"
    );
}

#[tokio::test]
async fn dispatch_audit_row_paired_with_tracking_row() {
    // SC-203 invariant: every escalation_dispatches row has
    // exactly one matching escalation_dispatched audit row,
    // keyed by dispatch_id. An audit reconciliation query returns
    // zero mismatches.
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    let pkg = sample_package("d-pair", Some("sess-pair"));
    seed_dispute_and_handoff(&conn, "d-pair", Some(&solver.pubkey_hex()), &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(false),
    )
    .await
    .unwrap();
    assert!(solver.wait_for(1, 10).await);

    let (orphaned_dispatches, orphaned_audits): (i64, i64) = {
        let c = conn.lock().await;
        let od: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM escalation_dispatches d
                 LEFT JOIN mediation_events e
                   ON e.kind = 'escalation_dispatched'
                  AND json_extract(e.payload_json, '$.dispatch_id') = d.dispatch_id
                 WHERE e.id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let oa: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events e
                 LEFT JOIN escalation_dispatches d
                   ON json_extract(e.payload_json, '$.dispatch_id') = d.dispatch_id
                 WHERE e.kind = 'escalation_dispatched'
                   AND d.dispatch_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        (od, oa)
    };
    assert_eq!(
        orphaned_dispatches, 0,
        "every dispatch row must have a matching audit event"
    );
    assert_eq!(
        orphaned_audits, 0,
        "every audit event must have a matching dispatch row"
    );
}

#[tokio::test]
async fn duplicate_handoff_deduplicated() {
    // SC-205 / FR-203: two cycles over the same handoff_event_id
    // produce exactly one dispatch row and one audit event. The
    // second cycle is a silent no-op because `scan_pending`'s
    // LEFT JOIN filters out rows that already have a dispatch row.
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];
    let cfg = sample_cfg(false);

    let pkg = sample_package("d-dedup", None);
    let handoff_id =
        seed_dispute_and_handoff(&conn, "d-dedup", Some(&solver.pubkey_hex()), &pkg).await;

    // Cycle 1: dispatch runs normally.
    escalation::run_once(&conn, &client, &harness.serbero_keys, &solvers, &cfg)
        .await
        .unwrap();
    assert!(solver.wait_for(1, 10).await);

    // Cycle 2: consumer scan must exclude the already-dispatched
    // handoff. No new DM, no new rows.
    escalation::run_once(&conn, &client, &harness.serbero_keys, &solvers, &cfg)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        solver.count().await,
        1,
        "dedup must prevent a second DM for the same handoff_event_id"
    );

    let (dispatches, audits): (i64, i64) = {
        let c = conn.lock().await;
        let d: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM escalation_dispatches
                 WHERE handoff_event_id = ?1",
                params![handoff_id],
                |r| r.get(0),
            )
            .unwrap();
        let a: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE kind = 'escalation_dispatched'
                   AND json_extract(payload_json, '$.handoff_event_id') = ?1",
                params![handoff_id],
                |r| r.get(0),
            )
            .unwrap();
        (d, a)
    };
    assert_eq!(dispatches, 1, "exactly one dispatch row for this handoff");
    assert_eq!(audits, 1, "exactly one audit row for this handoff");
}
