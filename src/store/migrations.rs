//! Ordered schema migrations. Version `n` is `MIGRATIONS[n - 1]`.
//!
//! Migrations are append-only: never edit one that has been released, add a
//! new one instead.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};

pub struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every migration Serbero ships, in order.
pub const MIGRATIONS: &[Migration] = &[];

/// Brings `conn` up to the latest version in `migrations`. Each migration
/// runs in its own transaction together with its version record, so a
/// failure leaves the database at the previous version.
pub fn apply(conn: &mut Connection, migrations: &[Migration]) -> Result<u32> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             applied_at INTEGER NOT NULL
         );",
    )?;
    let current = current_version(conn)?;
    let latest =
        u32::try_from(migrations.len()).map_err(|_| Error::Schema("too many migrations".into()))?;
    if current > latest {
        return Err(Error::Schema(format!(
            "database is at schema version {current}, but this Serbero only knows up to \
             {latest}; upgrade Serbero or use another database"
        )));
    }
    for (version, migration) in (1..).zip(migrations).skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql).map_err(|e| {
            Error::Schema(format!(
                "migration {version} ({}) failed: {e}",
                migration.name
            ))
        })?;
        tx.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, unixepoch())",
            params![version, migration.name],
        )?;
        tx.commit()?;
        tracing::info!(version, name = migration.name, "applied migration");
    }
    Ok(latest)
}

pub fn current_version(conn: &Connection) -> Result<u32> {
    let version: Option<u32> = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    Ok(version.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: Migration = Migration {
        name: "create_a",
        sql: "CREATE TABLE a (id INTEGER PRIMARY KEY);",
    };
    const SECOND: Migration = Migration {
        name: "create_b",
        sql: "CREATE TABLE b (id INTEGER PRIMARY KEY);",
    };
    const BROKEN: Migration = Migration {
        name: "broken",
        sql: "CREATE TABLE c (id INTEGER PRIMARY KEY); INSERT INTO missing VALUES (1);",
    };

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }

    #[test]
    fn fresh_database_reaches_latest_version() {
        let mut conn = Connection::open_in_memory().unwrap();

        let version = apply(&mut conn, &[FIRST, SECOND]).unwrap();

        assert_eq!(version, 2);
        assert_eq!(current_version(&conn).unwrap(), 2);
        assert!(table_exists(&conn, "a") && table_exists(&conn, "b"));
    }

    #[test]
    fn rerunning_is_a_no_op() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn, &[FIRST, SECOND]).unwrap();

        let version = apply(&mut conn, &[FIRST, SECOND]).unwrap();

        assert_eq!(version, 2);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2);
    }

    #[test]
    fn only_pending_migrations_run() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn, &[FIRST]).unwrap();

        apply(&mut conn, &[FIRST, SECOND]).unwrap();

        assert_eq!(current_version(&conn).unwrap(), 2);
    }

    #[test]
    fn failed_migration_rolls_back_to_previous_version() {
        let mut conn = Connection::open_in_memory().unwrap();

        let err = apply(&mut conn, &[FIRST, BROKEN]).unwrap_err();

        assert!(
            err.to_string().contains("migration 2 (broken) failed"),
            "{err}"
        );
        assert_eq!(current_version(&conn).unwrap(), 1);
        assert!(!table_exists(&conn, "c"));
    }

    #[test]
    fn newer_database_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn, &[FIRST, SECOND]).unwrap();

        let err = apply(&mut conn, &[FIRST]).unwrap_err();

        assert!(err.to_string().contains("schema version 2"), "{err}");
    }
}
