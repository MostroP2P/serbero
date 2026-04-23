//! Phase 4 tracker — writes the `escalation_dispatches` row and
//! the matching `escalation_dispatched` audit event in a single
//! transaction (FR-211 atomicity invariant).
//!
//! Supersession (T020), unroutable (T023), and parse-failed (T029)
//! paths add their own helpers here; this file currently carries
//! the happy-path writer for US1.

use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::db::escalation_dispatches::{
    insert_dispatch, DispatchStatus, EscalationDispatch, PendingHandoff,
};
use crate::db::mediation_events::record_escalation_dispatched;
use crate::error::Result;

use super::dispatcher::DispatchOutcome;

/// Persist one successful-send-step dispatch attempt.
///
/// Executes both writes — the `escalation_dispatches` row and the
/// paired `escalation_dispatched` audit event — inside a single
/// transaction so FR-211's atomicity invariant holds regardless of
/// a crash-between-writes scenario. Returns the recorded
/// [`DispatchStatus`] so the caller can emit a well-formed
/// operator log line.
///
/// Status derivation:
/// - `AllSucceeded` / `PartialSuccess` → `DispatchStatus::Dispatched`.
///   Partial success counts as success at the Phase 4 layer (FR-211);
///   the per-recipient gaps stay visible in `notifications`.
/// - `AllFailed` → `DispatchStatus::SendFailed`. SC-208's
///   "no-JOIN" operator query reads this directly.
///
/// Target-solver encoding: single pubkey on the targeted path; a
/// comma-joined list on the broadcast path. The ordering matches
/// the send-loop order so operators who split on the comma can
/// line up with `notifications` rows by timestamp.
pub async fn record_successful_dispatch(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    handoff: &PendingHandoff,
    dispute_id: &str,
    outcome: &DispatchOutcome,
    fallback_broadcast: bool,
    now_ts: i64,
) -> Result<DispatchStatus> {
    let dispatch_id = Uuid::new_v4().to_string();
    let status = match outcome {
        DispatchOutcome::AllSucceeded { .. } | DispatchOutcome::PartialSuccess { .. } => {
            DispatchStatus::Dispatched
        }
        DispatchOutcome::AllFailed { .. } => DispatchStatus::SendFailed,
    };
    let target_solver = outcome.attempted_recipients().join(",");

    let row = EscalationDispatch {
        dispatch_id: dispatch_id.clone(),
        dispute_id: dispute_id.to_string(),
        session_id: handoff.session_id.clone(),
        handoff_event_id: handoff.handoff_event_id,
        target_solver: target_solver.clone(),
        dispatched_at: now_ts,
        created_at: now_ts,
        status,
        fallback_broadcast,
    };

    let mut guard = conn.lock().await;
    let tx = guard.transaction()?;

    // (1) Dispatch-tracking row.
    insert_dispatch(&tx, &row)?;

    // (2) Paired audit event. `record_escalation_dispatched`
    // takes `&Transaction<'_>` so the whole write set commits
    // atomically — if either statement fails, neither lands
    // (FR-211 invariant).
    record_escalation_dispatched(
        &tx,
        handoff.session_id.as_deref(),
        &dispatch_id,
        dispute_id,
        handoff.handoff_event_id,
        &target_solver,
        status.to_string().as_str(),
        fallback_broadcast,
        handoff.prompt_bundle_id.as_deref(),
        handoff.policy_hash.as_deref(),
        now_ts,
    )?;

    tx.commit()?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::escalation_dispatches::find_dispatch_by_handoff_event_id;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use rusqlite::params;

    async fn fresh_with_dispute_and_handoff(
    ) -> (Arc<AsyncMutex<rusqlite::Connection>>, PendingHandoff) {
        let mut c = open_in_memory().unwrap();
        run_migrations(&mut c).unwrap();
        c.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-trk', 'evt-trk', 'mostro', 'buyer',
                       'initiated', 10, 11, 'notified')",
            [],
        )
        .unwrap();
        let event_id: i64 = c
            .query_row(
                "INSERT INTO mediation_events (
                    session_id, kind, payload_json,
                    prompt_bundle_id, policy_hash, occurred_at
                 ) VALUES (NULL, 'handoff_prepared', '{}',
                           'phase3-default', 'hash-1', 100)
                 RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let handoff = PendingHandoff {
            handoff_event_id: event_id,
            session_id: None,
            payload_json: "{}".into(),
            prompt_bundle_id: Some("phase3-default".into()),
            policy_hash: Some("hash-1".into()),
            occurred_at: 100,
        };
        let arc = Arc::new(AsyncMutex::new(c));
        (arc, handoff)
    }

    #[tokio::test]
    async fn all_succeeded_outcome_records_dispatched_row_and_audit_event() {
        let (conn, handoff) = fresh_with_dispute_and_handoff().await;
        let outcome = DispatchOutcome::AllSucceeded {
            recipients: vec!["pk-1".into()],
        };

        let status = record_successful_dispatch(&conn, &handoff, "d-trk", &outcome, false, 200)
            .await
            .unwrap();
        assert_eq!(status, DispatchStatus::Dispatched);

        let row = {
            let c = conn.lock().await;
            find_dispatch_by_handoff_event_id(&c, handoff.handoff_event_id).unwrap()
        }
        .expect("dispatch row must exist");
        assert_eq!(row.status, DispatchStatus::Dispatched);
        assert_eq!(row.target_solver, "pk-1");
        assert!(!row.fallback_broadcast);

        let (audit_kind, audit_payload): (String, String) = {
            let c = conn.lock().await;
            c.query_row(
                "SELECT kind, payload_json FROM mediation_events
                 WHERE kind = 'escalation_dispatched'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(audit_kind, "escalation_dispatched");
        let v: serde_json::Value = serde_json::from_str(&audit_payload).unwrap();
        assert_eq!(v["dispatch_id"].as_str().unwrap(), row.dispatch_id);
        assert_eq!(v["status"], "dispatched");
        assert_eq!(v["fallback_broadcast"], false);
    }

    #[tokio::test]
    async fn partial_success_still_maps_to_dispatched() {
        let (conn, handoff) = fresh_with_dispute_and_handoff().await;
        // Send-loop order: ok-1 succeeded, bad-1 failed. The
        // `target_solver` column must reflect that original order.
        let outcome = DispatchOutcome::PartialSuccess {
            attempted: vec!["ok-1".into(), "bad-1".into()],
            succeeded: vec!["ok-1".into()],
            failed: vec!["bad-1".into()],
        };
        let status = record_successful_dispatch(&conn, &handoff, "d-trk", &outcome, false, 200)
            .await
            .unwrap();
        assert_eq!(status, DispatchStatus::Dispatched);

        let row = {
            let c = conn.lock().await;
            find_dispatch_by_handoff_event_id(&c, handoff.handoff_event_id)
                .unwrap()
                .unwrap()
        };
        assert_eq!(row.status, DispatchStatus::Dispatched);
        assert_eq!(row.target_solver, "ok-1,bad-1");
    }

    #[tokio::test]
    async fn partial_success_target_solver_preserves_send_loop_order() {
        // Regression guard for the DispatchOutcome ordering fix.
        // Attempt sequence: [early-fail, later-success]. The
        // `target_solver` column must read "early-fail,later-success",
        // NOT "later-success,early-fail" (which is what a naive
        // concatenation of succeeded + failed would produce).
        let (conn, handoff) = fresh_with_dispute_and_handoff().await;
        let outcome = DispatchOutcome::PartialSuccess {
            attempted: vec!["early-fail".into(), "later-success".into()],
            succeeded: vec!["later-success".into()],
            failed: vec!["early-fail".into()],
        };
        record_successful_dispatch(&conn, &handoff, "d-trk", &outcome, false, 200)
            .await
            .unwrap();
        let row = {
            let c = conn.lock().await;
            find_dispatch_by_handoff_event_id(&c, handoff.handoff_event_id)
                .unwrap()
                .unwrap()
        };
        assert_eq!(
            row.target_solver, "early-fail,later-success",
            "target_solver MUST preserve send-loop order for operator correlation \
             with notifications timestamps"
        );
    }

    #[tokio::test]
    async fn all_failed_outcome_records_send_failed_status() {
        let (conn, handoff) = fresh_with_dispute_and_handoff().await;
        let outcome = DispatchOutcome::AllFailed {
            attempted: vec!["dead-1".into(), "dead-2".into()],
        };
        let status = record_successful_dispatch(&conn, &handoff, "d-trk", &outcome, false, 200)
            .await
            .unwrap();
        assert_eq!(status, DispatchStatus::SendFailed);

        let row = {
            let c = conn.lock().await;
            find_dispatch_by_handoff_event_id(&c, handoff.handoff_event_id)
                .unwrap()
                .unwrap()
        };
        assert_eq!(row.status, DispatchStatus::SendFailed);
        assert_eq!(row.target_solver, "dead-1,dead-2");

        let payload: String = {
            let c = conn.lock().await;
            c.query_row(
                "SELECT payload_json FROM mediation_events
                 WHERE kind = 'escalation_dispatched'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["status"], "send_failed");
    }

    #[tokio::test]
    async fn fallback_broadcast_flag_flows_through_to_row_and_audit() {
        let (conn, handoff) = fresh_with_dispute_and_handoff().await;
        let outcome = DispatchOutcome::AllSucceeded {
            recipients: vec!["r1".into(), "r2".into()],
        };
        record_successful_dispatch(&conn, &handoff, "d-trk", &outcome, true, 200)
            .await
            .unwrap();
        let row = {
            let c = conn.lock().await;
            find_dispatch_by_handoff_event_id(&c, handoff.handoff_event_id)
                .unwrap()
                .unwrap()
        };
        assert!(row.fallback_broadcast);

        let payload: String = {
            let c = conn.lock().await;
            c.query_row(
                "SELECT payload_json FROM mediation_events
                 WHERE kind = 'escalation_dispatched'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["fallback_broadcast"], true);
    }

    #[tokio::test]
    async fn two_writes_land_atomically_in_same_transaction() {
        // Belt-and-braces pairing: the dispatch row and the audit
        // row MUST appear together (FR-211). After a successful
        // write, the count of dispatch rows equals the count of
        // `escalation_dispatched` audit rows AND the cross-join on
        // dispatch_id returns exactly one linked pair.
        let (conn, handoff) = fresh_with_dispute_and_handoff().await;
        let outcome = DispatchOutcome::AllSucceeded {
            recipients: vec!["pk-1".into()],
        };
        record_successful_dispatch(&conn, &handoff, "d-trk", &outcome, false, 200)
            .await
            .unwrap();

        let mismatch: i64 = {
            let c = conn.lock().await;
            c.query_row(
                "SELECT (
                    (SELECT COUNT(*) FROM escalation_dispatches)
                    - (SELECT COUNT(*) FROM mediation_events
                       WHERE kind = 'escalation_dispatched')
                 )",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            mismatch, 0,
            "FR-211: dispatch rows and audit rows must be paired 1:1"
        );

        let linked: i64 = {
            let c = conn.lock().await;
            c.query_row(
                "SELECT COUNT(*) FROM escalation_dispatches d
                 JOIN mediation_events e
                   ON e.kind = 'escalation_dispatched'
                  AND json_extract(e.payload_json, '$.dispatch_id') = d.dispatch_id",
                params![],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(linked, 1, "exactly one linked pair expected");
    }
}
