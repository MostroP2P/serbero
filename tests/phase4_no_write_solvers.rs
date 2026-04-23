//! Phase 5 — T025 US3 integration tests.
//!
//! Pins the FR-202 rule 3/4 outcomes end-to-end:
//!
//!   * Rule 4 (zero write solvers + fallback off) writes a single
//!     `escalation_dispatch_unroutable` audit row, emits no DM,
//!     writes no `escalation_dispatches` row, and leaves the
//!     `handoff_prepared` event unconsumed so a later config
//!     change that adds a write-permission solver can pick it up
//!     on the next cycle (FR-213).
//!
//!   * Rule 3 (zero write solvers + fallback on + at least one
//!     configured solver) broadcasts the DM to every configured
//!     solver, writes one `escalation_dispatches` row with
//!     `fallback_broadcast = 1` and a comma-joined
//!     `target_solver`, and emits one `escalation_dispatched`
//!     audit row whose payload carries `fallback_broadcast: true`.
//!
//! The third sub-test drives the re-pickability contract: after
//! the unroutable cycle, the operator adds a write-permission
//! solver and a subsequent cycle dispatches the same handoff
//! normally.

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

fn sample_cfg(fallback: bool) -> EscalationConfig {
    EscalationConfig {
        enabled: true,
        dispatch_interval_seconds: 1,
        fallback_to_all_solvers: fallback,
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

/// Seed a dispute row at `lifecycle_state = 'notified'` (not
/// resolved, so the US2 supersession gate does not fire and we
/// reach the router) plus a `handoff_prepared` mediation_event
/// row. Returns the newly-minted handoff event id so the test can
/// assert audit / dispatch pairing against it.
async fn seed_dispute_and_handoff(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    pkg: &HandoffPackage,
) -> i64 {
    let c = conn.lock().await;
    c.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state,
            assigned_solver
         ) VALUES (?1, ?2, 'mostro', 'buyer',
                   'initiated', 10, 11, 'notified', NULL)",
        params![dispute_id, format!("evt-{dispute_id}")],
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

async fn count(conn: &Arc<AsyncMutex<rusqlite::Connection>>, sql: &str) -> i64 {
    let c = conn.lock().await;
    c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap()
}

#[tokio::test]
async fn zero_write_solvers_fallback_off_records_unroutable() {
    // FR-202 rule 4: config carries only Read-permission solvers
    // and `fallback_to_all_solvers = false`. The dispatcher must:
    //   - send zero DMs,
    //   - write zero rows to `escalation_dispatches`,
    //   - write exactly one `escalation_dispatch_unroutable` audit
    //     row carrying configured_solver_count + fallback flag,
    //   - leave the `handoff_prepared` row unconsumed so a later
    //     config change can re-process it.
    let harness = TestHarness::new().await;
    let read_solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> =
        vec![solver_cfg(read_solver.pubkey_hex(), SolverPermission::Read)];

    let pkg = sample_package("d-unr-off");
    let handoff_id = seed_dispute_and_handoff(&conn, "d-unr-off", &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(false),
    )
    .await
    .unwrap();

    // The Unroutable arm runs BEFORE send_to_recipients; the short
    // sleep catches any spurious DM that a misbehaving future
    // refactor might slip in.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        read_solver.count().await,
        0,
        "unroutable arm must not send a DM to a read-permission solver"
    );

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        0,
        "FR-213: unroutable must NOT write an escalation_dispatches row"
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

    let (hid, csc, fallback): (i64, i64, bool) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.handoff_event_id'),
                    json_extract(payload_json, '$.configured_solver_count'),
                    json_extract(payload_json, '$.fallback_to_all_solvers')
             FROM mediation_events WHERE kind = 'escalation_dispatch_unroutable'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(hid, handoff_id, "audit row must reference the handoff id");
    assert_eq!(
        csc, 1,
        "configured_solver_count counts every configured solver (any permission) — \
         one Read solver configured"
    );
    assert!(
        !fallback,
        "fallback_to_all_solvers payload field must mirror the config flag (false here)"
    );

    // FR-213: the handoff stays unconsumed, so a future cycle that
    // sees the same broken config re-enters the gate. Dedup in
    // `tracker::record_unroutable` keeps the audit table bounded
    // so the second cycle emits no new row.
    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(false),
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatch_unroutable'",
        )
        .await,
        1,
        "cycle 2 with still-broken config must not grow the audit table — \
         the writer-side dedup bounds the row count per handoff"
    );
}

#[tokio::test]
async fn zero_write_solvers_fallback_on_broadcasts_to_everyone() {
    // FR-202 rule 3: config carries only Read-permission solvers
    // but `fallback_to_all_solvers = true`. The dispatcher must
    // broadcast to every configured solver, write one
    // `escalation_dispatches` row with `fallback_broadcast = 1`
    // and `target_solver` = comma-joined list of every configured
    // pubkey, and emit one `escalation_dispatched` audit row whose
    // payload key `fallback_broadcast` is `true`.
    let harness = TestHarness::new().await;
    let r1 = SolverListener::start(&harness.relay_url).await;
    let r2 = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let solvers: Vec<SolverConfig> = vec![
        solver_cfg(r1.pubkey_hex(), SolverPermission::Read),
        solver_cfg(r2.pubkey_hex(), SolverPermission::Read),
    ];

    let pkg = sample_package("d-unr-on");
    let handoff_id = seed_dispute_and_handoff(&conn, "d-unr-on", &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(true),
    )
    .await
    .unwrap();

    assert!(
        r1.wait_for(1, 10).await,
        "fallback broadcast must reach Read solver #1"
    );
    assert!(
        r2.wait_for(1, 10).await,
        "fallback broadcast must reach Read solver #2"
    );

    let (target_solver, fb_col): (String, i64) = {
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
    assert_eq!(
        parts.len(),
        2,
        "target_solver must list every recipient the send loop attempted"
    );
    assert!(
        parts.contains(&r1.pubkey_hex().as_str()),
        "target_solver missing Read solver #1"
    );
    assert!(
        parts.contains(&r2.pubkey_hex().as_str()),
        "target_solver missing Read solver #2"
    );
    assert_eq!(
        fb_col, 1,
        "fallback_broadcast column must be 1 on the rule-3 fallback path"
    );

    // Paired audit row carries the same flag.
    let fb_audit: bool = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.fallback_broadcast')
             FROM mediation_events WHERE kind = 'escalation_dispatched'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(
        fb_audit,
        "escalation_dispatched audit payload must carry fallback_broadcast: true"
    );

    // Rule 3 fires — rule 4's unroutable audit must NOT.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatch_unroutable'",
        )
        .await,
        0,
        "rule 3 (fallback broadcast) must not emit an unroutable audit row"
    );
}

#[tokio::test]
async fn fallback_on_with_zero_solvers_writes_contract_compliant_unroutable() {
    // Edge case the router collapses onto Unroutable even though
    // `fallback_to_all_solvers = true`: there are zero solvers
    // configured at all, so rule 3 has nothing to broadcast to. The
    // audit payload's `fallback_to_all_solvers` field encodes
    // "rule 3 fired?" (per contracts/audit-events.md), not the raw
    // config flag — so the field MUST land as `false` even with
    // fallback on, otherwise operator dashboards that filter
    // `WHERE fallback_to_all_solvers = false` to pull every
    // unroutable event would miss this one. Without the derived-
    // value fix, the writer used to leak the raw config flag into
    // the payload and write `true` in this shape, violating the
    // contract.
    let harness = TestHarness::new().await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    // The defining knob: zero configured solvers at all.
    let solvers: Vec<SolverConfig> = Vec::new();

    let pkg = sample_package("d-unr-empty-fb-on");
    let handoff_id = seed_dispute_and_handoff(&conn, "d-unr-empty-fb-on", &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(true), // fallback ON
    )
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        0,
        "no dispatch row when there is nobody to dispatch to, fallback flag notwithstanding"
    );

    let (hid, csc, fallback): (i64, i64, bool) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.handoff_event_id'),
                    json_extract(payload_json, '$.configured_solver_count'),
                    json_extract(payload_json, '$.fallback_to_all_solvers')
             FROM mediation_events WHERE kind = 'escalation_dispatch_unroutable'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(hid, handoff_id);
    assert_eq!(
        csc, 0,
        "configured_solver_count mirrors solvers.len() — zero configured here"
    );
    assert!(
        !fallback,
        "contracts/audit-events.md pins fallback_to_all_solvers to false on every unroutable \
         row — rule 3 did not fire (there was nothing to fall back to), so the semantic value \
         is `false` regardless of the raw [escalation].fallback_to_all_solvers config flag"
    );
}

#[tokio::test]
async fn unroutable_handoff_picked_up_after_config_change() {
    // FR-213 re-pickability end-to-end. Cycle 1 runs with only a
    // Read solver + fallback off → unroutable audit row, zero
    // dispatches. The operator then reconfigures the solver set to
    // include a Write solver; cycle 2 on the SAME DB must
    // dispatch the previously-unroutable handoff through the
    // normal path (one `escalation_dispatched` audit row, one
    // `escalation_dispatches` row with `fallback_broadcast = 0`,
    // one DM landing on the new Write solver). The earlier
    // unroutable audit row is preserved so the history documents
    // the broken-config interval.
    let harness = TestHarness::new().await;
    let read_solver = SolverListener::start(&harness.relay_url).await;
    let write_solver = SolverListener::start(&harness.relay_url).await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;

    let pkg = sample_package("d-unr-repick");
    let handoff_id = seed_dispute_and_handoff(&conn, "d-unr-repick", &pkg).await;

    // Cycle 1 — only a Read solver, fallback off.
    let solvers_before: Vec<SolverConfig> =
        vec![solver_cfg(read_solver.pubkey_hex(), SolverPermission::Read)];
    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers_before,
        &sample_cfg(false),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(read_solver.count().await, 0);
    assert_eq!(write_solver.count().await, 0);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatch_unroutable'",
        )
        .await,
        1,
        "cycle 1 must emit exactly one unroutable audit row"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        0,
        "cycle 1 must not write any dispatch row"
    );

    // Cycle 2 — operator adds a Write solver. Config is passed per
    // call, so swapping the `solvers` slice is all it takes.
    let solvers_after: Vec<SolverConfig> = vec![
        solver_cfg(read_solver.pubkey_hex(), SolverPermission::Read),
        solver_cfg(write_solver.pubkey_hex(), SolverPermission::Write),
    ];
    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers_after,
        &sample_cfg(false),
    )
    .await
    .unwrap();
    assert!(
        write_solver.wait_for(1, 10).await,
        "the previously-unroutable handoff must dispatch to the newly-added Write solver"
    );
    assert_eq!(
        read_solver.count().await,
        0,
        "Read solver still must not receive the DM — fallback is off"
    );

    // One dispatch row, not a fallback broadcast, carrying the
    // Write solver's pubkey.
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
    assert_eq!(target_solver, write_solver.pubkey_hex());
    assert_eq!(
        fb, 0,
        "post-config-change dispatch is a normal write-set broadcast, not the fallback path"
    );

    // The earlier unroutable audit row is preserved; the history
    // documents the broken-config interval.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatch_unroutable'",
        )
        .await,
        1,
        "historical unroutable row is preserved across the config change"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatched'",
        )
        .await,
        1,
        "exactly one dispatched audit row from the cycle-2 dispatch"
    );
}
