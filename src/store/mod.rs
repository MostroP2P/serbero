//! SQLite persistence (`docs/spec.md` §8). No ORM: typed functions over
//! plain SQL.

pub mod migrations;

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::error::Result;

/// How long a statement waits for a lock held by another connection.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// An open, migrated database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) the database at `path` and migrates it.
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// A private in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        // WAL keeps readers from blocking the writer; in-memory databases
        // silently stay in "memory" mode, which is fine.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        migrations::apply(&mut conn, migrations::MIGRATIONS)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn schema_version(&self) -> Result<u32> {
        migrations::current_version(&self.conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_database_uses_wal_and_foreign_keys() {
        let dir = std::env::temp_dir().join(format!("serbero-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");

        let store = Store::open(&path).unwrap();

        let mode: String = store
            .conn()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let fk: i64 = store
            .conn()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(fk, 1);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reopening_keeps_the_schema_version() {
        let dir = std::env::temp_dir().join(format!("serbero-reopen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let first = Store::open(&path).unwrap().schema_version().unwrap();

        let second = Store::open(&path).unwrap().schema_version().unwrap();

        assert_eq!(first, second);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
