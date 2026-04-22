use rusqlite::{params, Connection, Transaction};

use crate::error::Result;

#[cfg(test)]
const CURRENT_SCHEMA_VERSION: i64 = 5;

pub fn run_migrations(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY
        );",
    )?;

    // Propagate SELECT errors — swallowing them with unwrap_or would mask
    // a corrupt DB as "no migrations applied yet" and re-run Phase 1 DDL.
    let applied: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?
        .unwrap_or(0);

    if applied < 1 {
        run_versioned(conn, 1, apply_v1)?;
    }
    if applied < 2 {
        run_versioned(conn, 2, apply_v2)?;
    }
    if applied < 3 {
        run_versioned(conn, 3, apply_v3)?;
    }
    if applied < 4 {
        run_versioned(conn, 4, apply_v4)?;
    }
    if applied < 5 {
        run_versioned(conn, 5, apply_v5)?;
    }

    Ok(())
}

fn run_versioned<F>(conn: &mut Connection, version: i64, apply: F) -> Result<()>
where
    F: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let tx = conn.transaction()?;
    apply(&tx)?;
    tx.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        params![version],
    )?;
    tx.commit()?;
    Ok(())
}

fn apply_v1(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS disputes (
            dispute_id       TEXT PRIMARY KEY,
            event_id         TEXT NOT NULL UNIQUE,
            mostro_pubkey    TEXT NOT NULL,
            initiator_role   TEXT NOT NULL,
            dispute_status   TEXT NOT NULL DEFAULT 'initiated',
            event_timestamp  INTEGER NOT NULL,
            detected_at      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS notifications (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            dispute_id     TEXT NOT NULL,
            solver_pubkey  TEXT NOT NULL,
            sent_at        INTEGER NOT NULL,
            status         TEXT NOT NULL,
            error_message  TEXT,
            notif_type     TEXT NOT NULL DEFAULT 'initial',
            FOREIGN KEY (dispute_id) REFERENCES disputes(dispute_id)
        );

        CREATE INDEX IF NOT EXISTS idx_notifications_dispute_id
            ON notifications(dispute_id);",
    )?;
    Ok(())
}

fn apply_v2(tx: &Transaction<'_>) -> Result<()> {
    add_column_if_missing(
        tx,
        "disputes",
        "lifecycle_state",
        "TEXT NOT NULL DEFAULT 'new'",
    )?;
    add_column_if_missing(tx, "disputes", "assigned_solver", "TEXT")?;
    add_column_if_missing(tx, "disputes", "last_notified_at", "INTEGER")?;
    add_column_if_missing(tx, "disputes", "last_state_change", "INTEGER")?;

    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS dispute_state_transitions (
             id              INTEGER PRIMARY KEY AUTOINCREMENT,
             dispute_id      TEXT NOT NULL,
             from_state      TEXT,
             to_state        TEXT NOT NULL,
             transitioned_at INTEGER NOT NULL,
             trigger         TEXT,
             FOREIGN KEY (dispute_id) REFERENCES disputes(dispute_id)
         );

         CREATE INDEX IF NOT EXISTS idx_state_transitions_dispute_id
             ON dispute_state_transitions(dispute_id);
         CREATE INDEX IF NOT EXISTS idx_disputes_lifecycle_state
             ON disputes(lifecycle_state);",
    )?;
    Ok(())
}

fn add_column_if_missing(
    tx: &Transaction<'_>,
    table: &str,
    column: &str,
    column_def: &str,
) -> Result<()> {
    let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|name| name == column);
    drop(stmt);
    if !exists {
        tx.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {column_def};"
        ))?;
    }
    Ok(())
}

fn apply_v3(tx: &Transaction<'_>) -> Result<()> {
    // Phase 3 mediation schema. Mirrors
    // `specs/003-guided-mediation/data-model.md`. All tables are new;
    // no Phase 1/2 rows are backfilled.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS mediation_sessions (
             session_id                 TEXT PRIMARY KEY,
             dispute_id                 TEXT NOT NULL,
             state                      TEXT NOT NULL,
             round_count                INTEGER NOT NULL DEFAULT 0,
             prompt_bundle_id           TEXT NOT NULL,
             policy_hash                TEXT NOT NULL,
             instructions_version       TEXT,
             assigned_solver            TEXT,
             current_classification     TEXT,
             current_confidence         REAL,
             buyer_shared_pubkey        TEXT,
             seller_shared_pubkey       TEXT,
             buyer_last_seen_inner_ts   INTEGER,
             seller_last_seen_inner_ts  INTEGER,
             started_at                 INTEGER NOT NULL,
             last_transition_at         INTEGER NOT NULL,
             FOREIGN KEY (dispute_id) REFERENCES disputes(dispute_id)
         );

         CREATE INDEX IF NOT EXISTS idx_mediation_sessions_dispute_id
             ON mediation_sessions(dispute_id);
         CREATE INDEX IF NOT EXISTS idx_mediation_sessions_state
             ON mediation_sessions(state);

         CREATE TABLE IF NOT EXISTS mediation_messages (
             id                       INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id               TEXT NOT NULL,
             direction                TEXT NOT NULL,
             party                    TEXT NOT NULL,
             shared_pubkey            TEXT NOT NULL,
             inner_event_id           TEXT NOT NULL,
             inner_event_created_at   INTEGER NOT NULL,
             outer_event_id           TEXT,
             content                  TEXT NOT NULL,
             prompt_bundle_id         TEXT,
             policy_hash              TEXT,
             persisted_at             INTEGER NOT NULL,
             stale                    INTEGER NOT NULL DEFAULT 0,
             FOREIGN KEY (session_id) REFERENCES mediation_sessions(session_id)
         );

         CREATE UNIQUE INDEX IF NOT EXISTS uq_mediation_messages_inner_event
             ON mediation_messages(session_id, inner_event_id);
         CREATE INDEX IF NOT EXISTS idx_mediation_messages_session
             ON mediation_messages(session_id);
         CREATE INDEX IF NOT EXISTS idx_mediation_messages_direction
             ON mediation_messages(direction);

         CREATE TABLE IF NOT EXISTS reasoning_rationales (
             rationale_id         TEXT PRIMARY KEY,
             session_id           TEXT,
             provider             TEXT NOT NULL,
             model                TEXT NOT NULL,
             prompt_bundle_id     TEXT NOT NULL,
             policy_hash          TEXT NOT NULL,
             rationale_text       TEXT NOT NULL,
             generated_at         INTEGER NOT NULL,
             FOREIGN KEY (session_id) REFERENCES mediation_sessions(session_id)
         );

         CREATE INDEX IF NOT EXISTS idx_reasoning_rationales_session
             ON reasoning_rationales(session_id);

         CREATE TABLE IF NOT EXISTS mediation_summaries (
             id                   INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id           TEXT NOT NULL,
             dispute_id           TEXT NOT NULL,
             classification       TEXT NOT NULL,
             confidence           REAL NOT NULL,
             suggested_next_step  TEXT NOT NULL,
             summary_text         TEXT NOT NULL,
             prompt_bundle_id     TEXT NOT NULL,
             policy_hash          TEXT NOT NULL,
             rationale_id         TEXT,
             generated_at         INTEGER NOT NULL,
             FOREIGN KEY (session_id) REFERENCES mediation_sessions(session_id),
             FOREIGN KEY (dispute_id) REFERENCES disputes(dispute_id),
             FOREIGN KEY (rationale_id) REFERENCES reasoning_rationales(rationale_id)
         );

         CREATE INDEX IF NOT EXISTS idx_mediation_summaries_session
             ON mediation_summaries(session_id);

         CREATE TABLE IF NOT EXISTS mediation_events (
             id                INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id        TEXT,
             kind              TEXT NOT NULL,
             payload_json      TEXT NOT NULL DEFAULT '{}',
             rationale_id      TEXT,
             prompt_bundle_id  TEXT,
             policy_hash       TEXT,
             occurred_at       INTEGER NOT NULL,
             FOREIGN KEY (session_id) REFERENCES mediation_sessions(session_id),
             FOREIGN KEY (rationale_id) REFERENCES reasoning_rationales(rationale_id)
         );

         CREATE INDEX IF NOT EXISTS idx_mediation_events_session_kind
             ON mediation_events(session_id, kind);",
    )?;
    Ok(())
}

/// Migration v4 — Phase 11 mid-session follow-up loop.
///
/// Adds two columns to `mediation_sessions` so the ingest tick can
/// drive the mid-session evaluator idempotently and track bounded
/// consecutive failures:
///
/// - `round_count_last_evaluated` (FR-127): counts the total number
///   of fresh (non-stale) inbound rows Serbero has already
///   classified for this session. The column name is historical —
///   an earlier draft stored the `round_count` min-rule value here;
///   see the 2026-04-21 gate-fix commit for why fresh-inbound-count
///   is the right idempotency primitive (a single-party reply
///   never advances min-rule `round_count`, so the gate would never
///   open in the common mid-session case). The evaluator skips
///   when `count_fresh_inbounds &lt;= round_count_last_evaluated`, i.e.
///   there is nothing new since the last classification. On a
///   successful `advance_session_round` the value is rewritten to
///   the current total-fresh-inbound count as part of the same
///   atomic commit that writes the outbound rows (or, for the
///   Summarize branch, as part of a short post-deliver transaction).
///   Backfilled to `0` for existing rows, which forces any in-flight
///   session to be re-evaluated once after the daemon restarts —
///   acceptable because the alternative (skipping pre-existing
///   sessions) keeps them silent.
///
/// - `consecutive_eval_failures` (FR-130): monotonic counter of
///   back-to-back reasoning-call or commit failures for the
///   mid-session evaluator. Incremented on failure, reset to `0` on
///   any successful evaluation. At value `3` the session escalates
///   with `ReasoningUnavailable` and the counter is reset by the
///   escalation path.
///
/// SQLite's `ALTER TABLE ADD COLUMN` supports the `NOT NULL DEFAULT`
/// combo used here and rewrites the page lazily. The version guard
/// in `run_migrations` protects against re-application.
fn apply_v4(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE mediation_sessions
            ADD COLUMN round_count_last_evaluated INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE mediation_sessions
            ADD COLUMN consecutive_eval_failures INTEGER NOT NULL DEFAULT 0;",
    )?;
    Ok(())
}

/// Phase 4 migration — add the `escalation_dispatches` table that
/// tracks one row per dispatch attempt of a `handoff_prepared`
/// audit event. Schema matches
/// `specs/004-escalation-execution/data-model.md` §escalation_dispatches.
///
/// The `status` CHECK constraint is enforced at the schema level so
/// a mis-spelled value surfaces as a SQL error, not a silent audit
/// drift. The two indexes cover the dispute-lookup and the
/// dedup-probe query shapes (FR-203 / SC-205). FKs reference Phase
/// 1/2/3 tables but the caller still writes Phase 4 rows
/// append-only — FR-217 forbids Phase 4 from mutating any existing
/// Phase 1/2/3 row.
fn apply_v5(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS escalation_dispatches (
            dispatch_id         TEXT PRIMARY KEY,
            dispute_id          TEXT NOT NULL,
            session_id          TEXT,
            handoff_event_id    INTEGER NOT NULL,
            target_solver       TEXT NOT NULL,
            dispatched_at       INTEGER NOT NULL,
            created_at          INTEGER NOT NULL,
            status              TEXT NOT NULL DEFAULT 'dispatched'
                                CHECK (status IN ('dispatched', 'send_failed')),
            fallback_broadcast  INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (dispute_id) REFERENCES disputes(dispute_id),
            FOREIGN KEY (session_id) REFERENCES mediation_sessions(session_id),
            FOREIGN KEY (handoff_event_id) REFERENCES mediation_events(id)
        );

        CREATE INDEX IF NOT EXISTS idx_escalation_dispatches_dispute
            ON escalation_dispatches(dispute_id);
        CREATE INDEX IF NOT EXISTS idx_escalation_dispatches_handoff
            ON escalation_dispatches(handoff_event_id);",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn migrations_are_idempotent() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        run_migrations(&mut conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn phase1_and_phase2_tables_exist() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        for table in [
            "disputes",
            "notifications",
            "dispute_state_transitions",
            "schema_version",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} should exist");
        }
    }

    #[test]
    fn phase2_columns_present_on_disputes() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(disputes)").unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for col in [
            "lifecycle_state",
            "assigned_solver",
            "last_notified_at",
            "last_state_change",
        ] {
            assert!(
                names.iter().any(|n| n == col),
                "disputes should have column {col} but only has {:?}",
                names
            );
        }
    }

    #[test]
    fn applying_phase2_over_existing_phase1_schema_is_idempotent() {
        // Simulate upgrading a pre-existing Phase 1 DB.
        let mut conn = open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE schema_version (version INTEGER PRIMARY KEY);")
            .unwrap();
        {
            let tx = conn.transaction().unwrap();
            apply_v1(&tx).unwrap();
            tx.execute("INSERT INTO schema_version (version) VALUES (1)", [])
                .unwrap();
            tx.commit().unwrap();
        }
        // First pass applies v2 and v3; second pass should be a no-op.
        run_migrations(&mut conn).unwrap();
        run_migrations(&mut conn).unwrap();
    }

    #[test]
    fn phase3_tables_exist_after_migration() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        for table in [
            "mediation_sessions",
            "mediation_messages",
            "mediation_summaries",
            "mediation_events",
            "reasoning_rationales",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} should exist after v3");
        }
    }

    #[test]
    fn applying_phase3_over_existing_phase2_schema_is_idempotent() {
        // Simulate upgrading a pre-existing Phase 2 DB (v2 already
        // applied) to v3. Running twice should not produce errors.
        let mut conn = open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE schema_version (version INTEGER PRIMARY KEY);")
            .unwrap();
        {
            let tx = conn.transaction().unwrap();
            apply_v1(&tx).unwrap();
            tx.execute("INSERT INTO schema_version (version) VALUES (1)", [])
                .unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.transaction().unwrap();
            apply_v2(&tx).unwrap();
            tx.execute("INSERT INTO schema_version (version) VALUES (2)", [])
                .unwrap();
            tx.commit().unwrap();
        }
        run_migrations(&mut conn).unwrap();
        run_migrations(&mut conn).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        // Running twice walks a v2 DB forward through v3 and v4 on
        // the first pass and no-ops on the second; the final version
        // is whatever `CURRENT_SCHEMA_VERSION` pins.
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn phase11_columns_present_on_mediation_sessions() {
        // After a clean migration to the current schema, the two
        // Phase 11 columns MUST exist on `mediation_sessions` and
        // default to 0. The test inserts no rows and only inspects
        // the schema — column presence plus default — because the
        // backfill behaviour is covered by the idempotency test
        // below.
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let mut stmt = conn
            .prepare("PRAGMA table_info(mediation_sessions)")
            .unwrap();
        let cols: Vec<(String, String, String)> = stmt
            .query_map([], |row| {
                // PRAGMA table_info columns: cid, name, type,
                // notnull, dflt_value, pk. We pick name, type,
                // dflt_value (which is NULL-or-text).
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                ))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for col_name in ["round_count_last_evaluated", "consecutive_eval_failures"] {
            let hit = cols
                .iter()
                .find(|(n, _, _)| n == col_name)
                .unwrap_or_else(|| {
                    panic!(
                        "mediation_sessions should have column {col_name} but only has {:?}",
                        cols
                    )
                });
            assert_eq!(
                hit.1.to_uppercase(),
                "INTEGER",
                "column {col_name} must be INTEGER"
            );
            assert_eq!(hit.2, "0", "column {col_name} must default to 0");
        }
    }

    #[test]
    fn applying_phase11_over_existing_phase3_schema_backfills_zero() {
        // Simulate a v3 DB with a session row already on file (an
        // in-flight mediation) and confirm the v4 migration adds the
        // two new columns with value `0` on that row, AND that a
        // second pass is a no-op.
        let mut conn = open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE schema_version (version INTEGER PRIMARY KEY);")
            .unwrap();
        for (v, apply) in [
            (1_i64, apply_v1 as fn(&Transaction<'_>) -> Result<()>),
            (2, apply_v2),
            (3, apply_v3),
        ] {
            let tx = conn.transaction().unwrap();
            apply(&tx).unwrap();
            tx.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                params![v],
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Seed a minimal in-flight session + its parent dispute.
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-pre-v4', 'evt-pre-v4', 'mostro', 'buyer',
                       'initiated', 10, 11, 'notified')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                started_at, last_transition_at
             ) VALUES ('sess-pre-v4', 'd-pre-v4', 'awaiting_response',
                       2, 'phase3-default', 'hash-pre', 100, 200)",
            [],
        )
        .unwrap();

        // Walk forward to v4.
        run_migrations(&mut conn).unwrap();
        // Second pass should be a no-op; both columns already exist.
        run_migrations(&mut conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);

        let (rcl, cef): (i64, i64) = conn
            .query_row(
                "SELECT round_count_last_evaluated, consecutive_eval_failures
                 FROM mediation_sessions WHERE session_id = 'sess-pre-v4'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(rcl, 0, "backfill of round_count_last_evaluated must be 0");
        assert_eq!(cef, 0, "backfill of consecutive_eval_failures must be 0");
    }

    #[test]
    fn phase4_escalation_dispatches_table_and_indexes_exist() {
        // After a clean migration, v5 must have created the Phase 4
        // `escalation_dispatches` table plus both indexes (dispute
        // lookup + handoff-event dedup probe).
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'escalation_dispatches'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            table_count, 1,
            "escalation_dispatches table should exist after v5"
        );

        for idx in [
            "idx_escalation_dispatches_dispute",
            "idx_escalation_dispatches_handoff",
        ] {
            let hit: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'index' AND name = ?1",
                    params![idx],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(hit, 1, "index {idx} should exist after v5");
        }
    }

    #[test]
    fn phase4_status_check_constraint_rejects_unknown_token() {
        // The CHECK constraint on `status` must reject any value
        // other than `dispatched` or `send_failed`. A mis-spelled
        // token surfaces as a SQL error, not a silent audit drift.
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();

        // Seed a minimal parent dispute + mediation_event so FKs
        // are satisfied.
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-check', 'evt-check', 'mostro', 'buyer',
                       'initiated', 10, 11, 'notified')",
            [],
        )
        .unwrap();
        let handoff_event_id: i64 = conn
            .query_row(
                "INSERT INTO mediation_events (session_id, kind, payload_json, occurred_at)
             VALUES (NULL, 'handoff_prepared', '{}', 100)
             RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Valid statuses succeed.
        for status in ["dispatched", "send_failed"] {
            conn.execute(
                "INSERT INTO escalation_dispatches (
                    dispatch_id, dispute_id, handoff_event_id,
                    target_solver, dispatched_at, created_at, status
                 ) VALUES (?1, 'd-check', ?2, 'solver-pk', 200, 200, ?3)",
                params![format!("dispatch-{status}"), handoff_event_id, status],
            )
            .unwrap();
        }

        // Any other token must error.
        let err = conn.execute(
            "INSERT INTO escalation_dispatches (
                dispatch_id, dispute_id, handoff_event_id,
                target_solver, dispatched_at, created_at, status
             ) VALUES ('dispatch-bogus', 'd-check', ?1, 'solver-pk', 200, 200, 'bogus')",
            params![handoff_event_id],
        );
        assert!(
            err.is_err(),
            "CHECK constraint must reject status = 'bogus', but insert succeeded"
        );
    }

    #[test]
    fn applying_phase4_over_existing_phase11_schema_is_idempotent() {
        // Simulate upgrading a v4 DB (Phase 11 mid-session columns
        // already present, no Phase 4 table yet) to v5. Running
        // twice should not produce errors, and the
        // `escalation_dispatches` table MUST land on the first run.
        let mut conn = open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE schema_version (version INTEGER PRIMARY KEY);")
            .unwrap();
        for (v, apply) in [
            (1_i64, apply_v1 as fn(&Transaction<'_>) -> Result<()>),
            (2, apply_v2),
            (3, apply_v3),
            (4, apply_v4),
        ] {
            let tx = conn.transaction().unwrap();
            apply(&tx).unwrap();
            tx.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                params![v],
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Walk forward to v5.
        run_migrations(&mut conn).unwrap();
        // Second pass should be a no-op (CREATE TABLE IF NOT EXISTS
        // + CREATE INDEX IF NOT EXISTS).
        run_migrations(&mut conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'escalation_dispatches'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);
    }
}
