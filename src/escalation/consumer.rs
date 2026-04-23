//! Phase 4 consumer — scans `mediation_events` for pending
//! `handoff_prepared` rows via
//! [`crate::db::escalation_dispatches::list_pending_handoffs`].
//!
//! Thin async wrapper that holds the connection mutex only for the
//! length of the single `LEFT JOIN` scan and returns the pending
//! set to the dispatcher loop. The SQL heavy-lifting (dedup filter,
//! parse-failed exclusion, ascending-id order) lives in
//! `db::escalation_dispatches`; keeping it there means the
//! `run_dispatcher` cycle stays readable as a pipeline.

use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;

use crate::db::escalation_dispatches::{list_pending_handoffs, PendingHandoff};
use crate::error::Result;

/// Return the pending-handoff batch for one dispatcher cycle.
///
/// `limit` is clamped to `>= 1` inside `list_pending_handoffs` so
/// a mis-configured caller cannot trigger an unbounded scan.
pub async fn scan_pending(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    limit: i64,
) -> Result<Vec<PendingHandoff>> {
    let guard = conn.lock().await;
    list_pending_handoffs(&guard, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use rusqlite::params;

    async fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
        let mut c = open_in_memory().unwrap();
        run_migrations(&mut c).unwrap();
        c.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-consumer', 'evt-c', 'mostro', 'buyer',
                       'initiated', 10, 11, 'notified')",
            [],
        )
        .unwrap();
        Arc::new(AsyncMutex::new(c))
    }

    fn seed_handoff(conn: &rusqlite::Connection, payload: &str) -> i64 {
        conn.query_row(
            "INSERT INTO mediation_events (
                session_id, kind, payload_json, occurred_at
             ) VALUES (NULL, 'handoff_prepared', ?1, 100)
             RETURNING id",
            params![payload],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn scan_pending_returns_only_handoff_prepared_rows() {
        let conn = fresh_conn().await;
        {
            let c = conn.lock().await;
            c.execute(
                "INSERT INTO mediation_events (session_id, kind, payload_json, occurred_at)
                 VALUES (NULL, 'reasoning_verdict', '{}', 100)",
                [],
            )
            .unwrap();
            seed_handoff(&c, r#"{"dispute_id":"d-consumer"}"#);
        }

        let pending = scan_pending(&conn, 100).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].payload_json.contains("d-consumer"));
    }

    #[tokio::test]
    async fn scan_pending_returns_empty_when_no_handoffs() {
        let conn = fresh_conn().await;
        let pending = scan_pending(&conn, 100).await.unwrap();
        assert!(pending.is_empty());
    }
}
