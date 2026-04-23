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

/// One candidate row returned by [`list_pending_handoffs`] — a
/// `handoff_prepared` audit event that has not yet been turned
/// into an `escalation_dispatches` row. Carries the fields the
/// dispatcher needs to decide whether to dispatch, supersede, or
/// record an unroutable: the event id (used as the dedup key), the
/// dispute it references, the optional session it came from, the
/// raw payload for `HandoffPackage` deserialization, and the
/// bundle pin so the Phase 4 audit rows can copy them forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingHandoff {
    pub handoff_event_id: i64,
    pub session_id: Option<String>,
    pub payload_json: String,
    pub prompt_bundle_id: Option<String>,
    pub policy_hash: Option<String>,
    pub occurred_at: i64,
}

/// Scan `mediation_events` for `handoff_prepared` rows that do not
/// yet have a matching `escalation_dispatches` row AND are not
/// already recorded as parse-failed audit rows.
///
/// The single `LEFT JOIN` against `escalation_dispatches` is the
/// consumer-side FR-203 / SC-205 dedup filter. The
/// `escalation_dispatch_parse_failed` check is the "mark consumed"
/// effect documented in T029: the FR-214 handler deliberately does
/// NOT write a dispatch row, so the LEFT JOIN alone would
/// re-surface the malformed event on every cycle and re-fire the
/// audit + ERROR log. The audit-event NOT EXISTS clause below keeps
/// it consumed from the scan's perspective.
///
/// Rows come back in ascending `id` order so the dispatcher
/// processes older handoffs first — matches the at-least-once,
/// FIFO-over-a-cycle semantics documented in research.md Decision 2.
///
/// `limit` caps the batch so a backlog after a daemon restart does
/// not starve other tokio tasks on the same cycle.
pub fn list_pending_handoffs(conn: &Connection, limit: i64) -> Result<Vec<PendingHandoff>> {
    let mut stmt = conn.prepare(
        "SELECT me.id, me.session_id, me.payload_json,
                me.prompt_bundle_id, me.policy_hash, me.occurred_at
         FROM mediation_events me
         LEFT JOIN escalation_dispatches d
                ON d.handoff_event_id = me.id
         WHERE me.kind = 'handoff_prepared'
           AND d.dispatch_id IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM mediation_events e2
                WHERE e2.kind = 'escalation_dispatch_parse_failed'
                  AND e2.id <> me.id
                  AND json_extract(e2.payload_json, '$.handoff_event_id') = me.id
           )
         ORDER BY me.id ASC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok(PendingHandoff {
                handoff_event_id: r.get::<_, i64>(0)?,
                session_id: r.get::<_, Option<String>>(1)?,
                payload_json: r.get::<_, String>(2)?,
                prompt_bundle_id: r.get::<_, Option<String>>(3)?,
                policy_hash: r.get::<_, Option<String>>(4)?,
                occurred_at: r.get::<_, i64>(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
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

    // --- list_pending_handoffs tests ---

    fn seed_handoff_event(conn: &Connection, _dispute_id: &str, payload: &str) -> i64 {
        // _dispute_id is kept in the signature so test call sites
        // stay readable ("seed a handoff for dispute X"), but the
        // event row is dispute-scoped via its payload_json, not
        // via a FK column, so the argument is deliberately unused.
        conn.query_row(
            "INSERT INTO mediation_events (
                session_id, kind, payload_json, prompt_bundle_id, policy_hash, occurred_at
             ) VALUES (NULL, 'handoff_prepared', ?1, 'phase3-default', 'hash-1', ?2)
             RETURNING id",
            params![payload, 100],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    }

    fn seed_dispute_row(conn: &Connection, dispute_id: &str) {
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES (?1, ?2, 'mostro', 'buyer', 'initiated', 10, 11, 'notified')",
            params![dispute_id, format!("evt-{dispute_id}")],
        )
        .unwrap();
    }

    #[test]
    fn list_pending_handoffs_returns_empty_when_no_handoff_events_exist() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        seed_dispute_row(&conn, "d-nohand");

        // Seed a non-handoff event (should be filtered out by
        // `kind = 'handoff_prepared'`).
        conn.execute(
            "INSERT INTO mediation_events (session_id, kind, payload_json, occurred_at)
             VALUES (NULL, 'reasoning_verdict', '{}', 100)",
            [],
        )
        .unwrap();

        let pending = list_pending_handoffs(&conn, 100).unwrap();
        assert!(
            pending.is_empty(),
            "only handoff_prepared rows should come back; got {pending:?}"
        );
    }

    #[test]
    fn list_pending_handoffs_returns_ascending_by_id() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        seed_dispute_row(&conn, "d-1");
        seed_dispute_row(&conn, "d-2");
        seed_dispute_row(&conn, "d-3");

        let id1 = seed_handoff_event(&conn, "d-1", "{\"dispute_id\":\"d-1\"}");
        let id2 = seed_handoff_event(&conn, "d-2", "{\"dispute_id\":\"d-2\"}");
        let id3 = seed_handoff_event(&conn, "d-3", "{\"dispute_id\":\"d-3\"}");

        let pending = list_pending_handoffs(&conn, 100).unwrap();
        let ids: Vec<i64> = pending.iter().map(|p| p.handoff_event_id).collect();
        assert_eq!(
            ids,
            vec![id1, id2, id3],
            "ascending id order required so the dispatcher processes oldest first"
        );
        assert!(pending[0].payload_json.contains("d-1"));
        assert_eq!(
            pending[0].prompt_bundle_id.as_deref(),
            Some("phase3-default"),
            "prompt bundle pin must flow through so Phase 4 audit rows can copy it"
        );
    }

    #[test]
    fn list_pending_handoffs_filters_already_dispatched() {
        // FR-203 / SC-205: the LEFT JOIN must filter out handoffs
        // whose dispatch row already exists.
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        seed_dispute_row(&conn, "d-consumed");
        seed_dispute_row(&conn, "d-fresh");

        let consumed_id =
            seed_handoff_event(&conn, "d-consumed", "{\"dispute_id\":\"d-consumed\"}");
        let fresh_id = seed_handoff_event(&conn, "d-fresh", "{\"dispute_id\":\"d-fresh\"}");

        let tx = conn.transaction().unwrap();
        insert_dispatch(
            &tx,
            &EscalationDispatch {
                dispatch_id: "dispatch-consumed".to_string(),
                dispute_id: "d-consumed".to_string(),
                session_id: None,
                handoff_event_id: consumed_id,
                target_solver: "solver-pk".to_string(),
                dispatched_at: 200,
                created_at: 200,
                status: DispatchStatus::Dispatched,
                fallback_broadcast: false,
            },
        )
        .unwrap();
        tx.commit().unwrap();

        let pending = list_pending_handoffs(&conn, 100).unwrap();
        let ids: Vec<i64> = pending.iter().map(|p| p.handoff_event_id).collect();
        assert_eq!(
            ids,
            vec![fresh_id],
            "already-dispatched handoff must be filtered out; only the fresh one remains"
        );
    }

    #[test]
    fn list_pending_handoffs_filters_parse_failed() {
        // FR-214 / T029 "mark consumed" effect: a
        // handoff_prepared event that has a corresponding
        // escalation_dispatch_parse_failed audit row (referencing
        // it via payload.handoff_event_id) MUST NOT re-surface in
        // the pending set. Otherwise the dispatcher would re-log
        // the ERROR and re-emit the audit row on every cycle.
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        seed_dispute_row(&conn, "d-malformed");
        seed_dispute_row(&conn, "d-fresh");

        let malformed_id = seed_handoff_event(&conn, "d-malformed", "not valid json");
        let fresh_id = seed_handoff_event(&conn, "d-fresh", "{\"dispute_id\":\"d-fresh\"}");

        // Seed a parse_failed audit row against the malformed
        // handoff. The payload references malformed_id so the
        // NOT EXISTS clause finds it.
        conn.execute(
            "INSERT INTO mediation_events (
                session_id, kind, payload_json, occurred_at
             ) VALUES (NULL, 'escalation_dispatch_parse_failed',
                       ?1, 200)",
            params![format!(
                r#"{{"dispute_id":"d-malformed","handoff_event_id":{malformed_id},"reason":"deserialize_failed","detail":"bad"}}"#
            )],
        )
        .unwrap();

        let pending = list_pending_handoffs(&conn, 100).unwrap();
        let ids: Vec<i64> = pending.iter().map(|p| p.handoff_event_id).collect();
        assert_eq!(
            ids,
            vec![fresh_id],
            "parse-failed handoff must not re-surface; only the fresh one remains"
        );
    }

    #[test]
    fn list_pending_handoffs_respects_limit() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        seed_dispute_row(&conn, "d-a");
        seed_dispute_row(&conn, "d-b");
        seed_dispute_row(&conn, "d-c");

        seed_handoff_event(&conn, "d-a", "{\"dispute_id\":\"d-a\"}");
        seed_handoff_event(&conn, "d-b", "{\"dispute_id\":\"d-b\"}");
        seed_handoff_event(&conn, "d-c", "{\"dispute_id\":\"d-c\"}");

        let pending = list_pending_handoffs(&conn, 2).unwrap();
        assert_eq!(
            pending.len(),
            2,
            "limit=2 must cap the batch so a restart backlog cannot starve other tasks"
        );
    }
}
