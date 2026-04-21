//! Composed mediation-eligibility predicate (FR-123).
//!
//! A dispute is mediation-eligible if and only if ALL of the
//! following hold:
//!
//! - The dispute's Phase 1/2 `lifecycle_state` is not a resolved
//!   terminal state AND not `escalated`. Eligible lifecycle states
//!   are `new`, `notified`, `taken`, `waiting`. The dispute has NOT
//!   been closed by Mostro (no `resolved`) and has NOT been handed
//!   off to a human escalation path (no `escalated`).
//!
//! - There is no currently-active (non-terminal) `mediation_sessions`
//!   row for this dispute. "Active" means any state other than
//!   `closed` and `superseded_by_human` (both terminal). A session
//!   in `escalation_recommended` is treated as ineligible too — once
//!   Serbero recommended escalation for a session, the engine MUST
//!   NOT silently pull the dispute back into a fresh mediation.
//!
//! - There is no dispute-scoped `escalation_recommended` audit row
//!   for this dispute. Under FR-122 option (b), the opening-path
//!   escalation writes a `session_id = NULL` handoff whose payload
//!   references `dispute_id`; eligibility MUST honor that handoff
//!   the same way it honors a session-scoped one.
//!
//! The predicate is shared by two call sites:
//!
//! - The event-driven start path in `handlers::dispute_detected`
//!   (FR-121) asks [`is_mediation_eligible`] for a single
//!   `dispute_id` before invoking `mediation::start::try_start_for`.
//! - The resumption/retry tick in `mediation::run_engine_tick` calls
//!   [`list_mediation_eligible`] to batch-walk every currently
//!   eligible dispute. Both paths MUST use the same predicate so a
//!   dispute cannot be skipped because it transitioned through a
//!   short-lived intermediate state between the two passes.
//!
//! Implementation shape: the single-row predicate and the batch
//! listing share the SQL fragment below so the two paths cannot
//! diverge accidentally.
//!
//! NOTE on the 2026-04-20 spec correction: the predecessor code
//! (`mediation::list_eligible_disputes` as of commit `84bc6a1`)
//! pinned eligibility to `lifecycle_state = 'notified'` exclusively.
//! That formulation is explicitly rejected by FR-123 — it produces
//! the race the gap analysis calls out (a dispute that transitioned
//! out of `notified` before the next tick would never be picked up).

use std::str::FromStr;

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::models::dispute::InitiatorRole;

/// Lifecycle states a dispute may occupy while remaining eligible
/// for a fresh mediation session. Explicitly excludes `resolved`
/// (Mostro closed the dispute) and `escalated` (Phase 2 / Phase 4
/// has handed the dispute off to a human path).
///
/// The list is authored against the enum variants defined in
/// `crate::models::dispute::LifecycleState`; any new variant added
/// there MUST be classified here (eligible or not) in the same
/// commit, and the compiler does not enforce that — so the
/// [`is_eligible_lifecycle`] match below is exhaustive on the enum
/// precisely to surface forgotten variants as a compile error.
const ELIGIBLE_LIFECYCLE_STATES_SQL: &str = "('new', 'notified', 'taken', 'waiting')";

/// Exhaustive classifier for [`crate::models::dispute::LifecycleState`].
/// Kept as a `match` (no wildcard arm) so adding a new variant to
/// `LifecycleState` without updating this file is a compile error.
pub fn is_eligible_lifecycle(state: crate::models::LifecycleState) -> bool {
    use crate::models::LifecycleState::*;
    match state {
        New | Notified | Taken | Waiting => true,
        Escalated | Resolved => false,
    }
}

/// SQL fragment that encodes every "no live session / no escalated
/// session / no dispute-scoped escalation handoff" rejection. Used
/// by both [`is_mediation_eligible`] and [`list_mediation_eligible`]
/// so the two paths cannot drift. `?1` is the `dispute_id` binding.
///
/// The dispute-scoped handoff check uses `json_extract` on
/// `payload_json.$.dispute_id`. SQLite has built-in JSON support;
/// rusqlite uses the same runtime. If the JSON functions are ever
/// disabled the check will fail loudly rather than silently return
/// a wrong answer.
const INELIGIBLE_REASONS_SQL: &str = "\
    EXISTS (\
        SELECT 1 FROM mediation_sessions s \
        WHERE s.dispute_id = ?1 \
          AND s.state NOT IN ('closed', 'superseded_by_human')\
    ) \
    OR EXISTS (\
        SELECT 1 FROM mediation_events e \
        WHERE e.session_id IS NULL \
          AND e.kind = 'escalation_recommended' \
          AND json_extract(e.payload_json, '$.dispute_id') = ?1\
    )";

/// Return `Ok(true)` iff the dispute is currently eligible for a
/// fresh mediation session open under FR-123.
///
/// Use from the event-driven start path; the batch form
/// [`list_mediation_eligible`] exists for the engine-tick retry/
/// resumption pass.
pub fn is_mediation_eligible(conn: &Connection, dispute_id: &str) -> Result<bool> {
    // Lifecycle check is typed (not stringly): we read the state
    // column, parse into the enum, and delegate to the exhaustive
    // `is_eligible_lifecycle` matcher. If the column carries a
    // value unknown to the enum (shouldn't happen), the FromStr
    // impl returns `Error::InvalidEvent` — which surfaces loudly
    // rather than silently treating the unknown as ineligible.
    // Only a genuine missing-row maps to "not eligible". Every other
    // `rusqlite` error (I/O, schema, lock busy, …) must propagate so
    // the caller can log it loudly instead of silently treating it
    // as an ineligibility verdict.
    let lifecycle_s: String = match conn.query_row(
        "SELECT lifecycle_state FROM disputes WHERE dispute_id = ?1",
        params![dispute_id],
        |r| r.get::<_, String>(0),
    ) {
        Ok(s) => s,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            // Unknown dispute — not eligible. The caller probably
            // raced ahead of Phase 1/2 persistence; the next tick
            // will reconsider.
            return Ok(false);
        }
        Err(e) => return Err(e.into()),
    };
    let lifecycle = crate::models::LifecycleState::from_str(&lifecycle_s)?;
    if !is_eligible_lifecycle(lifecycle) {
        return Ok(false);
    }
    // Rejection subqueries.
    let ineligible: i64 = conn.query_row(
        &format!("SELECT CASE WHEN {INELIGIBLE_REASONS_SQL} THEN 1 ELSE 0 END"),
        params![dispute_id],
        |r| r.get(0),
    )?;
    Ok(ineligible == 0)
}

/// One row returned by [`list_mediation_eligible`].
pub struct EligibleDispute {
    pub dispute_id: String,
    pub initiator_role: InitiatorRole,
}

/// Return every dispute currently eligible for a mediation open,
/// ordered by Phase 1/2 `event_timestamp` ascending (oldest first).
///
/// This is the batch variant used by the engine tick. It MUST
/// evaluate the same predicate as [`is_mediation_eligible`] so the
/// event-driven and tick paths cannot disagree about eligibility.
pub fn list_mediation_eligible(conn: &Connection) -> Result<Vec<EligibleDispute>> {
    // The SQL joins the `ELIGIBLE_LIFECYCLE_STATES_SQL` whitelist
    // against the `INELIGIBLE_REASONS_SQL` rejection clauses. We
    // pass the `?1` binding for each rejection subquery per row
    // via a correlated reference to `d.dispute_id`.
    //
    // Rewriting the subquery binding: we cannot just inline
    // `INELIGIBLE_REASONS_SQL` as-is because it uses `?1` for a
    // single prepared-statement parameter. For the batch case we
    // want the subquery to reference `d.dispute_id` directly, so
    // we build a batch-specific string that substitutes
    // `d.dispute_id` for `?1`.
    let batch_rejection = INELIGIBLE_REASONS_SQL.replace("?1", "d.dispute_id");
    let sql = format!(
        "SELECT dispute_id, initiator_role
         FROM disputes d
         WHERE d.lifecycle_state IN {lifecycle}
           AND NOT ({rejection})
         ORDER BY d.event_timestamp ASC",
        lifecycle = ELIGIBLE_LIFECYCLE_STATES_SQL,
        rejection = batch_rejection,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (dispute_id, initiator_role_s) = row?;
        match InitiatorRole::from_str(&initiator_role_s) {
            Ok(initiator_role) => out.push(EligibleDispute {
                dispute_id,
                initiator_role,
            }),
            Err(e) => {
                tracing::warn!(
                    dispute_id = %dispute_id,
                    role = %initiator_role_s,
                    error = %e,
                    "eligibility: skipping dispute with unrecognised initiator_role"
                );
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::models::LifecycleState;

    fn fresh() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    fn insert_dispute(conn: &Connection, dispute_id: &str, lifecycle: LifecycleState) {
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES (?1, 'e', 'm', 'buyer', 'initiated', 1, 2, ?2)",
            params![dispute_id, lifecycle.to_string()],
        )
        .unwrap();
    }

    fn insert_session(conn: &Connection, session_id: &str, dispute_id: &str, state: &str) {
        conn.execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                started_at, last_transition_at
             ) VALUES (?1, ?2, ?3, 0, 'bundle', 'hash', 100, 100)",
            params![session_id, dispute_id, state],
        )
        .unwrap();
    }

    #[test]
    fn eligible_lifecycle_accepts_every_non_terminal_non_escalated() {
        assert!(is_eligible_lifecycle(LifecycleState::New));
        assert!(is_eligible_lifecycle(LifecycleState::Notified));
        assert!(is_eligible_lifecycle(LifecycleState::Taken));
        assert!(is_eligible_lifecycle(LifecycleState::Waiting));
    }

    #[test]
    fn eligible_lifecycle_rejects_resolved_and_escalated() {
        assert!(!is_eligible_lifecycle(LifecycleState::Resolved));
        assert!(!is_eligible_lifecycle(LifecycleState::Escalated));
    }

    #[test]
    fn unknown_dispute_is_not_eligible() {
        let conn = fresh();
        assert!(!is_mediation_eligible(&conn, "no-such-dispute").unwrap());
    }

    #[test]
    fn notified_dispute_without_sessions_is_eligible() {
        let conn = fresh();
        insert_dispute(&conn, "d1", LifecycleState::Notified);
        assert!(is_mediation_eligible(&conn, "d1").unwrap());
    }

    #[test]
    fn taken_dispute_without_sessions_is_eligible() {
        // Regression: the predecessor pinned eligibility to
        // `lifecycle_state = 'notified'`. Under FR-123 a dispute
        // that has been taken (e.g. Phase 2 observed `s=in-progress`
        // from Serbero's own solver identity) but has no live
        // session is still eligible for a fresh session.
        let conn = fresh();
        insert_dispute(&conn, "d-taken", LifecycleState::Taken);
        assert!(is_mediation_eligible(&conn, "d-taken").unwrap());
    }

    #[test]
    fn waiting_dispute_is_eligible() {
        let conn = fresh();
        insert_dispute(&conn, "d-waiting", LifecycleState::Waiting);
        assert!(is_mediation_eligible(&conn, "d-waiting").unwrap());
    }

    #[test]
    fn resolved_dispute_is_not_eligible() {
        let conn = fresh();
        insert_dispute(&conn, "d-resolved", LifecycleState::Resolved);
        assert!(!is_mediation_eligible(&conn, "d-resolved").unwrap());
    }

    #[test]
    fn escalated_dispute_is_not_eligible() {
        let conn = fresh();
        insert_dispute(&conn, "d-esc", LifecycleState::Escalated);
        assert!(!is_mediation_eligible(&conn, "d-esc").unwrap());
    }

    #[test]
    fn active_session_blocks_eligibility() {
        let conn = fresh();
        insert_dispute(&conn, "d-active", LifecycleState::Notified);
        insert_session(&conn, "s-active", "d-active", "awaiting_response");
        assert!(!is_mediation_eligible(&conn, "d-active").unwrap());
    }

    #[test]
    fn closed_session_does_not_block_eligibility() {
        let conn = fresh();
        insert_dispute(&conn, "d-closed", LifecycleState::Notified);
        insert_session(&conn, "s-closed", "d-closed", "closed");
        // Closed means a prior session ended; a fresh session can
        // open again (e.g. re-dispute).
        assert!(is_mediation_eligible(&conn, "d-closed").unwrap());
    }

    #[test]
    fn escalation_recommended_session_blocks_eligibility() {
        let conn = fresh();
        insert_dispute(&conn, "d-escrec", LifecycleState::Notified);
        insert_session(&conn, "s-escrec", "d-escrec", "escalation_recommended");
        assert!(!is_mediation_eligible(&conn, "d-escrec").unwrap());
    }

    #[test]
    fn dispute_scoped_escalation_event_blocks_eligibility() {
        // FR-122 option (b): the opening-path escalation fires
        // dispute-scoped (no session row). Eligibility MUST still
        // reject the dispute after that handoff is written.
        let conn = fresh();
        insert_dispute(&conn, "d-disp-esc", LifecycleState::Notified);
        conn.execute(
            "INSERT INTO mediation_events (
                session_id, kind, payload_json,
                rationale_id, prompt_bundle_id, policy_hash, occurred_at
             ) VALUES (NULL, 'escalation_recommended',
                       '{\"dispute_id\":\"d-disp-esc\",\"trigger\":\"conflicting_claims\"}',
                       NULL, NULL, NULL, 500)",
            [],
        )
        .unwrap();
        assert!(!is_mediation_eligible(&conn, "d-disp-esc").unwrap());
    }

    #[test]
    fn list_mediation_eligible_returns_only_eligible_in_event_timestamp_order() {
        let conn = fresh();
        // Three disputes with distinct event_timestamps so ordering is deterministic.
        conn.execute(
            "INSERT INTO disputes (dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state)
             VALUES ('d-old', 'e', 'm', 'buyer', 'initiated', 10, 11, 'notified')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO disputes (dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state)
             VALUES ('d-mid', 'e2', 'm', 'buyer', 'initiated', 20, 21, 'waiting')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO disputes (dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state)
             VALUES ('d-new', 'e3', 'm', 'buyer', 'initiated', 30, 31, 'resolved')",
            [],
        )
        .unwrap();
        let got = list_mediation_eligible(&conn).unwrap();
        let ids: Vec<_> = got.into_iter().map(|e| e.dispute_id).collect();
        assert_eq!(ids, vec!["d-old".to_string(), "d-mid".to_string()]);
    }
}
