use rusqlite::{params, Connection, Transaction};

use crate::error::Result;

#[cfg(test)]
const CURRENT_SCHEMA_VERSION: i64 = 2;

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
        // First pass applies v2; second pass should be a no-op.
        run_migrations(&mut conn).unwrap();
        run_migrations(&mut conn).unwrap();
    }
}
