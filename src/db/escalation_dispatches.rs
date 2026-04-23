//! Phase 4 dispatch-tracking table (`escalation_dispatches`).
//!
//! One row per dispatch attempt that reached the send step: the row
//! captures the dispatch id (UUID v4), the dispute and optional
//! session it belongs to, the `mediation_events.id` of the
//! `handoff_prepared` row that triggered it, the target solver
//! pubkey (or comma-joined pubkey list on the broadcast path), the
//! timestamps, a `status` of `dispatched` / `send_failed` (enforced
//! by a `CHECK` constraint at the schema level), and a
//! `fallback_broadcast` flag that marks rows written via the
//! FR-202 rule 3 fallback path.
//!
//! This module is append-only from Phase 4's perspective. FR-217
//! forbids UPDATE / DELETE against Phase 1/2/3 tables; there is no
//! corresponding constraint on `escalation_dispatches`, but the
//! current dispatch model has no need for mutation either — a
//! dispatch either succeeds (status stays `dispatched`) or fails
//! (status written as `send_failed` in the same insert). Later
//! phases that want to extend the status set should do so via a new
//! migration bumping the CHECK constraint.
//!
//! Dedup discipline: the `(handoff_event_id)` index makes FR-203 /
//! SC-205 a cheap probe. The consumer-side scan (Phase 4's
//! `src/escalation/consumer.rs`, T006 / T011) uses that index via
//! a `LEFT JOIN` and never re-dispatches a handoff whose row is
//! already present here.

use std::fmt;
use std::str::FromStr;

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::error::{Error, Result};

/// Lifecycle state of a single dispatch attempt.
///
/// Two values are legal today:
///
/// - `Dispatched` — at least one recipient in the target list
///   successfully received the gift-wrapped DM. Partial success
///   (some succeeded, some failed) also records `Dispatched`;
///   per-recipient forensic detail lives in the existing
///   `notifications` table.
/// - `SendFailed` — every recipient failed. See spec SC-208 for the
///   "no-JOIN" operator-query property this encoding is meant to
///   preserve.
///
/// Supersession (FR-208) is NOT represented here — the
/// `escalation_dispatches` row is never written on the supersession
/// path; the `escalation_superseded` event in `mediation_events`
/// carries the record instead (FR-212).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchStatus {
    Dispatched,
    SendFailed,
}

impl fmt::Display for DispatchStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DispatchStatus::Dispatched => "dispatched",
            DispatchStatus::SendFailed => "send_failed",
        };
        f.write_str(s)
    }
}

impl FromStr for DispatchStatus {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "dispatched" => Ok(DispatchStatus::Dispatched),
            "send_failed" => Ok(DispatchStatus::SendFailed),
            other => Err(Error::InvalidEvent(format!(
                "unknown dispatch status: {other}"
            ))),
        }
    }
}

/// In-memory view of an `escalation_dispatches` row.
///
/// Populated on INSERT from the dispatcher loop and read back for
/// dedup probes and operator queries. The `target_solver` column
/// carries a single hex pubkey on the targeted path (FR-202 rule 1)
/// and a comma-separated list of hex pubkeys on the broadcast path
/// (rule 2 / fallback rule 3). The column type in SQLite is plain
/// TEXT, not JSON — the comma encoding matches the existing
/// `notifications.solver_pubkey` shape and avoids pulling in JSON
/// functions for a simple read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationDispatch {
    pub dispatch_id: String,
    pub dispute_id: String,
    pub session_id: Option<String>,
    pub handoff_event_id: i64,
    pub target_solver: String,
    pub dispatched_at: i64,
    pub created_at: i64,
    pub status: DispatchStatus,
    pub fallback_broadcast: bool,
}

/// Insert one `escalation_dispatches` row.
///
/// Takes `&Transaction<'_>` (not `&Connection`) because FR-211
/// requires this row and the matching `escalation_dispatched`
/// audit event to land atomically. Forcing the transaction at the
/// type level makes the atomicity invariant impossible to bypass
/// by accident — a future caller cannot "just call insert_dispatch"
/// without first opening a transaction that also covers the audit
/// write.
pub fn insert_dispatch(tx: &Transaction<'_>, row: &EscalationDispatch) -> Result<()> {
    tx.execute(
        "INSERT INTO escalation_dispatches (
            dispatch_id, dispute_id, session_id, handoff_event_id,
            target_solver, dispatched_at, created_at, status, fallback_broadcast
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.dispatch_id,
            row.dispute_id,
            row.session_id,
            row.handoff_event_id,
            row.target_solver,
            row.dispatched_at,
            row.created_at,
            row.status.to_string(),
            row.fallback_broadcast as i64,
        ],
    )?;
    Ok(())
}

/// Dedup probe keyed by `handoff_event_id` (FR-203 / SC-205).
///
/// Returns `Ok(Some(_))` when a dispatch row already exists for the
/// given `handoff_prepared` event id; returns `Ok(None)` when it
/// does not. The dispatcher's consumer scan uses this to filter the
/// pending set and guarantees a handoff is dispatched at most once
/// per row (barring the crash-between-send-and-audit mode
/// explicitly allowed by the spec).
pub fn find_dispatch_by_handoff_event_id(
    conn: &Connection,
    handoff_event_id: i64,
) -> Result<Option<EscalationDispatch>> {
    // `OptionalExtension::optional()` is the idiomatic rusqlite form
    // for "`QueryReturnedNoRows` becomes `Ok(None)`, everything else
    // propagates" — matches the `disputes::get_dispute` pattern
    // already used in the Phase 1/2 layer. The earlier `.ok()` shape
    // was a bug: it flattened every rusqlite error (missing table,
    // lock/busy, row-decode failure) to `None`, which in the
    // dispatcher's consumer path would silently bypass the FR-203
    // dedup probe and risk a duplicate send.
    let tuple = conn
        .query_row(
            "SELECT dispatch_id, dispute_id, session_id, handoff_event_id,
                    target_solver, dispatched_at, created_at, status, fallback_broadcast
             FROM escalation_dispatches
             WHERE handoff_event_id = ?1
             LIMIT 1",
            params![handoff_event_id],
            |r| {
                let status_s: String = r.get(7)?;
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    status_s,
                    r.get::<_, i64>(8)? != 0,
                ))
            },
        )
        .optional()?;

    let Some(tuple) = tuple else {
        return Ok(None);
    };

    let status = DispatchStatus::from_str(&tuple.7)?;
    Ok(Some(EscalationDispatch {
        dispatch_id: tuple.0,
        dispute_id: tuple.1,
        session_id: tuple.2,
        handoff_event_id: tuple.3,
        target_solver: tuple.4,
        dispatched_at: tuple.5,
        created_at: tuple.6,
        status,
        fallback_broadcast: tuple.8,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    fn seed_parent_rows(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-1', 'evt-1', 'mostro', 'buyer',
                       'initiated', 10, 11, 'notified')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                started_at, last_transition_at
             ) VALUES ('sess-1', 'd-1', 'escalation_recommended', 0,
                       'phase3-default', 'test-hash', 100, 100)",
            [],
        )
        .unwrap();
        conn.query_row(
            "INSERT INTO mediation_events (session_id, kind, payload_json, occurred_at)
             VALUES (NULL, 'handoff_prepared', '{\"dispute_id\":\"d-1\"}', 100)
             RETURNING id",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    }

    #[test]
    fn status_roundtrips_display_and_from_str() {
        for (token, variant) in [
            ("dispatched", DispatchStatus::Dispatched),
            ("send_failed", DispatchStatus::SendFailed),
        ] {
            let parsed: DispatchStatus = token.parse().unwrap();
            assert_eq!(parsed, variant);
            assert_eq!(parsed.to_string(), token);
        }
    }

    #[test]
    fn status_from_str_rejects_unknown_token() {
        let err = DispatchStatus::from_str("bogus").unwrap_err();
        match err {
            Error::InvalidEvent(msg) => {
                assert!(
                    msg.contains("bogus"),
                    "error message should include the bad token: {msg}"
                );
            }
            other => panic!("expected InvalidEvent, got {other:?}"),
        }
    }

    #[test]
    fn insert_and_lookup_dispatched_row() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let handoff_event_id = seed_parent_rows(&conn);

        let row = EscalationDispatch {
            dispatch_id: "dispatch-a".to_string(),
            dispute_id: "d-1".to_string(),
            session_id: None,
            handoff_event_id,
            target_solver: "solver-pk".to_string(),
            dispatched_at: 200,
            created_at: 200,
            status: DispatchStatus::Dispatched,
            fallback_broadcast: false,
        };
        let tx = conn.transaction().unwrap();
        insert_dispatch(&tx, &row).unwrap();
        tx.commit().unwrap();

        let got = find_dispatch_by_handoff_event_id(&conn, handoff_event_id)
            .unwrap()
            .expect("row must exist after insert");
        assert_eq!(got, row);
    }

    #[test]
    fn insert_and_lookup_send_failed_row_with_broadcast() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let handoff_event_id = seed_parent_rows(&conn);

        let row = EscalationDispatch {
            dispatch_id: "dispatch-b".to_string(),
            dispute_id: "d-1".to_string(),
            session_id: Some("sess-1".to_string()),
            handoff_event_id,
            target_solver: "pk-1,pk-2,pk-3".to_string(),
            dispatched_at: 300,
            created_at: 300,
            status: DispatchStatus::SendFailed,
            fallback_broadcast: true,
        };
        let tx = conn.transaction().unwrap();
        insert_dispatch(&tx, &row).unwrap();
        tx.commit().unwrap();

        let got = find_dispatch_by_handoff_event_id(&conn, handoff_event_id)
            .unwrap()
            .expect("row must exist after insert");
        assert_eq!(got, row);
        assert!(got.fallback_broadcast);
        assert_eq!(got.status, DispatchStatus::SendFailed);
    }

    #[test]
    fn lookup_returns_none_when_no_row_exists() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let handoff_event_id = seed_parent_rows(&conn);

        let got = find_dispatch_by_handoff_event_id(&conn, handoff_event_id).unwrap();
        assert!(got.is_none(), "no row should exist before insert");
    }

    #[test]
    fn lookup_propagates_db_errors_instead_of_swallowing_them() {
        // Regression guard: the consumer path relies on this probe
        // to prevent duplicate dispatches. If the probe flattened
        // every rusqlite error to `Ok(None)` (for example via a
        // naive `.ok()`), a missing table or a lock/busy failure
        // would look exactly like "no dispatch exists" and the
        // caller would re-send the DM. We exercise the "missing
        // table" shape by calling the probe on a connection that
        // never ran migration v5.
        let conn = open_in_memory().unwrap();
        // No run_migrations here — the `escalation_dispatches`
        // table does not exist, so SQLite errors with "no such
        // table".
        let err = find_dispatch_by_handoff_event_id(&conn, 42)
            .expect_err("missing table must surface as Err, not Ok(None)");
        match err {
            Error::Db(rusqlite::Error::SqliteFailure(_, Some(msg))) => {
                assert!(
                    msg.contains("no such table"),
                    "expected 'no such table' SQLite failure; got {msg}"
                );
            }
            Error::Db(rusqlite::Error::SqlInputError { msg, .. }) => {
                assert!(
                    msg.contains("no such table"),
                    "expected 'no such table' in SQL input error; got {msg}"
                );
            }
            other => panic!(
                "expected Error::Db(SqliteFailure | SqlInputError) for missing table, got {other:?}"
            ),
        }
    }
}
