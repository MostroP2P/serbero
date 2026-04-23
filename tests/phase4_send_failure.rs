//! Phase 6 — T027 send-failure integration test.
//!
//! Pins the FR-211 "every recipient failed" branch end-to-end.
//! When `send_to_recipients` returns `DispatchOutcome::AllFailed`,
//! the tracker writes:
//!
//!   * one `escalation_dispatches` row with
//!     `status = 'send_failed'`,
//!   * one paired `escalation_dispatched` audit row (the audit
//!     kind does NOT change with send outcome — only the payload's
//!     `status` field flips),
//!   * one `notifications` row per attempted recipient with
//!     `status = 'failed'`.
//!
//! Also pins SC-208: operator queries can pick up failed
//! dispatches without a JOIN against `notifications`. Running
//!
//!   SELECT * FROM escalation_dispatches WHERE status = 'send_failed'
//!
//! is sufficient to enumerate every dispatch that reached zero
//! recipients.
//!
//! The all-recipient failure is forced by configuring the Write
//! solver with a malformed pubkey. Inside
//! `dispatcher::send_to_recipients`, `PublicKey::parse` is called
//! on the configured hex string; it returns an error, the send
//! loop records a failed `notifications` row + pushes onto
//! `failed`, and because every recipient followed the same path
//! the loop returns `DispatchOutcome::AllFailed`. The alternative
//! (live relay that rejects the publish) would require a custom
//! relay impl; the malformed-pubkey route covers the exact same
//! code path with zero infrastructure.

mod common;

use std::sync::Arc;

use common::{publisher, solver_cfg, TestHarness};
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
async fn all_recipient_failure_records_send_failed_status() {
    // One Write solver with a malformed hex pubkey. `PublicKey::parse`
    // inside `send_to_recipients` returns Err, the loop records a
    // failed notifications row + pushes to `failed`, and because
    // there's only one recipient (and it failed), the outcome is
    // `DispatchOutcome::AllFailed`. The tracker then writes the
    // dispatch row with `status = 'send_failed'` per FR-211.
    let harness = TestHarness::new().await;
    let conn = fresh_conn().await;
    let client = publisher(&harness.relay_url, harness.serbero_keys.clone()).await;
    let malformed = "not-a-real-hex-pubkey-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
    let solvers: Vec<SolverConfig> =
        vec![solver_cfg(malformed.to_string(), SolverPermission::Write)];

    let pkg = sample_package("d-sf");
    let handoff_id = seed_dispute_and_handoff(&conn, "d-sf", &pkg).await;

    escalation::run_once(
        &conn,
        &client,
        &harness.serbero_keys,
        &solvers,
        &sample_cfg(),
    )
    .await
    .unwrap();

    // Exactly one escalation_dispatches row for the handoff,
    // `status = 'send_failed'`, `target_solver` reflects the
    // attempted recipient (the malformed pubkey — not silently
    // filtered out).
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
    assert_eq!(status, "send_failed");
    assert_eq!(
        target_solver, malformed,
        "target_solver records the attempted recipient in send-loop order, \
         even when the attempt failed at `PublicKey::parse`"
    );
    assert_eq!(
        fb, 0,
        "single-recipient write-set route is NOT the fallback_broadcast path"
    );

    // Exactly one paired escalation_dispatched audit row — the
    // audit kind does not change with send outcome (per
    // contracts/audit-events.md). The payload's `status` field
    // mirrors the dispatch-row column.
    let (audit_status, audit_dispatch_id): (String, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT json_extract(payload_json, '$.status'),
                    json_extract(payload_json, '$.dispatch_id')
             FROM mediation_events WHERE kind = 'escalation_dispatched'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(audit_status, "send_failed");
    assert_eq!(
        audit_dispatch_id, dispatch_id,
        "audit payload's dispatch_id must match the dispatch-row uuid"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatched'",
        )
        .await,
        1,
        "exactly one paired audit row expected"
    );

    // One notifications row per attempted recipient, with
    // `status = 'failed'` and a populated `error_message` so the
    // operator can see WHY it failed (malformed pubkey here).
    let notif_count = count(
        &conn,
        "SELECT COUNT(*) FROM notifications WHERE status = 'failed'",
    )
    .await;
    assert_eq!(
        notif_count, 1,
        "one notifications row per attempted recipient"
    );

    let (n_status, n_solver_pubkey, n_error): (String, String, Option<String>) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT status, solver_pubkey, error_message
             FROM notifications WHERE status = 'failed'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(n_status, "failed");
    assert_eq!(n_solver_pubkey, malformed);
    assert!(
        n_error.as_deref().map(|m| !m.is_empty()).unwrap_or(false),
        "failed notifications row must carry a non-empty error_message"
    );

    // SC-208: `SELECT * FROM escalation_dispatches WHERE status = 'send_failed'`
    // returns the row WITHOUT a JOIN against `notifications`. This
    // is the invariant operators rely on for a cheap "which
    // dispatches reached zero recipients?" query.
    let sf_rows: Vec<(String, String)> = {
        let c = conn.lock().await;
        let mut stmt = c
            .prepare(
                "SELECT dispatch_id, status
                 FROM escalation_dispatches WHERE status = 'send_failed'",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        sf_rows.len(),
        1,
        "SC-208: send_failed dispatches must be findable without a JOIN"
    );
    assert_eq!(sf_rows[0].0, dispatch_id);
    assert_eq!(sf_rows[0].1, "send_failed");

    // And — critically — the handoff DID get marked consumed:
    // `list_pending_handoffs` excludes rows with an
    // `escalation_dispatches` row regardless of status. A future
    // cycle must NOT re-attempt the failed dispatch (at-least-once
    // semantics + operator ownership of the retry decision).
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
        count(&conn, "SELECT COUNT(*) FROM escalation_dispatches").await,
        1,
        "cycle 2 must NOT add another dispatch row for the same handoff — \
         the send_failed row already consumes the handoff from the scan's perspective"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM mediation_events WHERE kind = 'escalation_dispatched'",
        )
        .await,
        1,
        "cycle 2 must NOT add another dispatched audit row for the same handoff"
    );
}
