//! Phase 4 — T021 US2 supersession integration tests.
//!
//! Pins the FR-208 supersession gate end-to-end. When a dispute's
//! `lifecycle_state` is already `resolved` at the moment the
//! dispatcher examines its handoff, the dispatcher MUST:
//!
//!   * not send any DM,
//!   * not write an `escalation_dispatches` row (FR-212),
//!   * write exactly one `escalation_superseded` audit row with
//!     `reason = "dispute_already_resolved"`,
//!   * leave the upstream `handoff_prepared` row unconsumed so a
//!     future policy change can re-process it (FR-213-adjacent).
//!
//! These tests drive `escalation::run_once` directly for
//! deterministic single-cycle execution (no tokio::time pauses).

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

/// Seed a dispute + a `handoff_prepared` mediation_event. The
/// `lifecycle_state` column is explicit here (unlike the US1 test
/// helpers) because supersession is all about that column.
async fn seed_dispute_and_handoff(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    lifecycle_state: &str,
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
                   'initiated', 10, 11, ?3, ?4)",
        params![
            dispute_id,
            format!("evt-{dispute_id}"),
            lifecycle_state,
            assigned_solver,
        ],
    )
    .unwrap();
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

async fn count(conn: &Arc<AsyncMutex<rusqlite::Connection>>, sql: &str) -> i64 {
    let c = conn.lock().await;
    c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap()
}

async fn set_lifecycle_state(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    new_state: &str,
) {
    let c = conn.lock().await;
    c.execute(
        "UPDATE disputes SET lifecycle_state = ?1 WHERE dispute_id = ?2",
        params![new_state, dispute_id],
    )
    .unwrap();
}

#[tokio::test]
async fn resolved_dispute_is_skipped_no_dm() {
    // Primary US2 happy path. Dispute is already at
    // `lifecycle_state = 'resolved'` when the dispatcher examines
    // its handoff → no DM, no dispatch row, one superseded audit row.
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    let pkg = sample_package("d-resolved");
    let handoff_id = seed_dispute_and_handoff(&conn, "d-resolved", "resolved", None, &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();

    // A short window to catch any spurious DM that the
    // dispatcher might have emitted before the supersession gate
    // fired. The gate runs BEFORE send_to_recipients, so under
    // correct behaviour this wait completes with zero messages.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        solver.count().await,
        0,
        "resolved dispute must not trigger any DM"
    );

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        0,
        "FR-212: supersession must NOT write an escalation_dispatches row"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatched'",
        )
        .await,
        0,
        "no paired dispatch audit row either"
    );

    let (reason, hid): (String, i64) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.reason'),
                    json_extract(payload_json, '$.handoff_event_id')
             FROM mediation_events WHERE kind = 'escalation_superseded'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(reason, "dispute_already_resolved");
    assert_eq!(hid, handoff_id, "audit row must reference the handoff id");
}

#[tokio::test]
async fn dispute_resolving_after_dispatch_does_not_recall() {
    // US2 acceptance scenario 2 — Phase 4 does NOT attempt to
    // recall an in-flight / landed DM. We can't easily test the
    // scan-to-send race at the second-level precision the spec
    // mentions (that would require a code-level hook between
    // dispute_metadata and send_to_recipients); instead we pin
    // the stronger observable property: once a dispatch has
    // landed, a later lifecycle flip to `resolved` leaves the
    // dispatch row and its audit event intact, and a subsequent
    // cycle does NOT emit a supersession (the handoff is already
    // consumed by the dispatch row).
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    let pkg = sample_package("d-race");
    seed_dispute_and_handoff(&conn, "d-race", "notified", None, &pkg).await;

    // Cycle 1: dispute is open → dispatch fires.
    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();
    assert!(solver.wait_for(1, 10).await, "cycle 1 should dispatch");
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        1,
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatched'",
        )
        .await,
        1,
    );

    // External resolution lands AFTER the dispatch completed.
    set_lifecycle_state(&conn, "d-race", "resolved").await;

    // Cycle 2: the handoff is now consumed by the dispatch row, so
    // the consumer scan MUST filter it out. No new DM, no new
    // rows, and — critically — no `escalation_superseded` event
    // (supersession applies only to unacted-on handoffs).
    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        solver.count().await,
        1,
        "post-dispatch lifecycle flip must not trigger a second DM"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        1,
        "dispatch row stays intact after lifecycle flip"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_superseded'",
        )
        .await,
        0,
        "Phase 4 does NOT attempt to recall an already-dispatched handoff",
    );
}

#[tokio::test]
async fn supersession_does_not_mark_handoff_consumed() {
    // FR-213: supersession leaves the `handoff_prepared` row
    // unconsumed so a future policy change can pick it up.
    //
    // We pin that property in two ways:
    //
    //  (a) Cycles 1 and 2 run against the still-resolved dispute.
    //      Cycle 1 writes one supersession audit row. Cycle 2 —
    //      gated by the idempotence check in
    //      `tracker::record_supersession` (FR-212 dedup) — MUST
    //      NOT emit a second row; left unchecked this would grow
    //      the audit by one row every `dispatch_interval_seconds`
    //      forever for any stuck-resolved dispute.
    //
    //  (b) The handoff is never written into `escalation_dispatches`,
    //      so when we flip `lifecycle_state` back to `notified`
    //      (simulating a policy-change reactivation) the scan
    //      re-surfaces the handoff and the dispatcher proceeds
    //      through the normal send path. This is the re-pickability
    //      guarantee — dedup of the audit row does NOT bleed into
    //      the scan's view of the handoff.
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Write)];

    let pkg = sample_package("d-idemp");
    seed_dispute_and_handoff(&conn, "d-idemp", "resolved", None, &pkg).await;

    // Cycle 1: supersession fires.
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
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_superseded'",
        )
        .await,
        1,
        "cycle 1: one supersession event"
    );

    // Cycle 2: lifecycle unchanged → the gate trips again, but
    // `record_supersession` observes the existing audit row and
    // skips the insert. Count stays at 1.
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
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_superseded'",
        )
        .await,
        1,
        "cycle 2: supersession is idempotent per handoff_event_id — \
         audit stays bounded on a stuck-resolved dispute"
    );

    // Confirm no DM was sent across the two supersession cycles
    // and the dispatch table stays empty.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(solver.count().await, 0);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        0,
        "no dispatch row in either supersession cycle"
    );

    // Policy-change simulation: the dispute reopens (lifecycle flips
    // back to `notified`). Because the handoff was never consumed in
    // the dispatch table, the scan still surfaces it. The gate now
    // sees a non-resolved lifecycle and proceeds through the normal
    // send path — FR-213's "stays unconsumed so a future policy
    // change can re-process it" contract in action.
    set_lifecycle_state(&conn, "d-idemp", "notified").await;
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
        "post-reopen cycle must dispatch the previously-superseded handoff"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        1,
        "dispatch row written on the reopen cycle"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatched'",
        )
        .await,
        1,
        "paired dispatch audit row on the reopen cycle"
    );
    // The supersession audit row for the earlier resolved window
    // stays intact — the replay-friendly audit trail documents both
    // the prior superseded state and the eventual dispatch.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_superseded'",
        )
        .await,
        1,
        "historical supersession row is preserved across the reopen"
    );
}
