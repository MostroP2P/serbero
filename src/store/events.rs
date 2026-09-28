//! The append-only `events` table: the audit trail of everything Serbero
//! observed or did (`docs/spec.md` §8).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: i64,
    pub dispute_id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub payload: Value,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewEvent<'a> {
    pub dispute_id: &'a str,
    pub session_id: Option<&'a str>,
    /// snake_case identifier, e.g. `notification_sent`.
    pub kind: &'a str,
    /// Structured details. Never message text or secrets.
    pub payload: Value,
    pub now: i64,
}

/// Appends an event and returns its id.
pub fn append(conn: &Connection, event: &NewEvent<'_>) -> Result<i64> {
    if event.kind.is_empty() {
        return Err(Error::Schema("event kind must not be empty".into()));
    }
    conn.execute(
        "INSERT INTO events (dispute_id, session_id, kind, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.dispute_id,
            event.session_id,
            event.kind,
            event.payload.to_string(),
            event.now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The newest `notification_sent` event of one kind to one solver (hex
/// pubkey), across disputes: what a solver's reply most likely answers.
pub fn last_notification_to(
    conn: &Connection,
    solver: &str,
    notification: &str,
) -> Result<Option<Event>> {
    let id: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, dispute_id FROM events
             WHERE kind = 'notification_sent'
               AND json_extract(payload_json, '$.solver') = ?1
               AND json_extract(payload_json, '$.notification') = ?2
             ORDER BY id DESC LIMIT 1",
            [solver, notification],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((id, dispute_id)) = id else {
        return Ok(None);
    };
    Ok(list_for_dispute(conn, &dispute_id)?
        .into_iter()
        .find(|event| event.id == id))
}

/// Events for one dispute, oldest first.
pub fn list_for_dispute(conn: &Connection, dispute_id: &str) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id, dispute_id, session_id, kind, payload_json, created_at
         FROM events WHERE dispute_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([dispute_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    rows.map(|row| {
        let (id, dispute_id, session_id, kind, payload_json, created_at) = row?;
        let payload = serde_json::from_str(&payload_json)
            .map_err(|e| Error::Schema(format!("event {id} has invalid payload: {e}")))?;
        Ok(Event {
            id,
            dispute_id,
            session_id,
            kind,
            payload,
            created_at,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::store::Store;

    fn event<'a>(dispute_id: &'a str, kind: &'a str, now: i64) -> NewEvent<'a> {
        NewEvent {
            dispute_id,
            session_id: None,
            kind,
            payload: json!({ "solver": "abc", "ok": true }),
            now,
        }
    }

    #[test]
    fn the_last_notification_of_a_kind_to_a_solver_is_found() {
        let store = Store::open_in_memory().unwrap();
        let sent = |dispute: &str, notification: &str, solver: &str, at: i64| {
            append(
                store.conn(),
                &NewEvent {
                    dispute_id: dispute,
                    session_id: None,
                    kind: "notification_sent",
                    payload: json!({ "notification": notification, "solver": solver }),
                    now: at,
                },
            )
            .unwrap();
        };
        sent("d1", "brief", "aa", 10);
        sent("d2", "brief", "aa", 20);
        sent("d3", "update", "aa", 30);
        sent("d4", "brief", "bb", 40);

        let last = last_notification_to(store.conn(), "aa", "brief")
            .unwrap()
            .unwrap();

        assert_eq!(last.dispute_id, "d2");
        assert!(
            last_notification_to(store.conn(), "cc", "brief")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn appended_events_are_listed_in_order() {
        let store = Store::open_in_memory().unwrap();
        append(store.conn(), &event("d1", "detected", 10)).unwrap();
        append(store.conn(), &event("d1", "notification_sent", 11)).unwrap();
        append(store.conn(), &event("d2", "detected", 12)).unwrap();

        let events = list_for_dispute(store.conn(), "d1").unwrap();

        let kinds: Vec<_> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["detected", "notification_sent"]);
        assert_eq!(events[1].payload, json!({ "solver": "abc", "ok": true }));
        assert_eq!(events[1].created_at, 11);
    }

    #[test]
    fn session_id_is_kept() {
        let store = Store::open_in_memory().unwrap();
        let new = NewEvent {
            session_id: Some("s1"),
            ..event("d1", "session_opened", 10)
        };

        append(store.conn(), &new).unwrap();

        let events = list_for_dispute(store.conn(), "d1").unwrap();
        assert_eq!(events[0].session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn empty_kind_is_rejected() {
        let store = Store::open_in_memory().unwrap();

        assert!(append(store.conn(), &event("d1", "", 10)).is_err());
    }

    #[test]
    fn unknown_dispute_has_no_events() {
        let store = Store::open_in_memory().unwrap();

        assert!(list_for_dispute(store.conn(), "nope").unwrap().is_empty());
    }
}
