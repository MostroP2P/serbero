pub mod disputes;
pub mod mediation;
pub mod migrations;
pub mod notifications;
pub mod state_transitions;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

pub fn open_connection<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(conn)
}
