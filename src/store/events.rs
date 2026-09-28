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

/// A mediated dispute that reached its final status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub dispute_id: String,
    pub status: String,
    pub by_parties: bool,
}

/// Mediated disputes resolved since `since` whose closing (thanks and final
/// report) was not completed, oldest first: a `resolved` event, a session,
/// and no `finished` event.
pub fn unfinished_resolutions(conn: &Connection, since: i64) -> Result<Vec<Resolution>> {
    let mut stmt = conn.prepare(
        "SELECT e.dispute_id,
                json_extract(e.payload_json, '$.status'),
                json_extract(e.payload_json, '$.resolved_by')
         FROM events e
         WHERE e.kind = 'resolved' AND e.created_at >= ?1
           AND EXISTS (SELECT 1 FROM sessions s WHERE s.dispute_id = e.dispute_id)
           AND NOT EXISTS (SELECT 1 FROM events f
                           WHERE f.dispute_id = e.dispute_id AND f.kind = 'finished')
         ORDER BY e.id",
    )?;
    let rows = stmt.query_map([since], |row| {
        Ok(Resolution {
            dispute_id: row.get(0)?,
            status: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            by_parties: row.get::<_, Option<String>>(2)?.as_deref() == Some("parties"),
        })
    })?;
    rows.map(|r| r.map_err(Into::into)).collect()
}

/// Whether a `solver_feedback` from this source DM was already recorded.
pub fn feedback_recorded(conn: &Connection, source: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM events
             WHERE kind = 'solver_feedback' AND json_extract(payload_json, '$.source') = ?1
             LIMIT 1",
            [source],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
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
    fn a_mediated_resolution_is_unfinished_until_finished() {
        let store = crate::store::sessions::testing::store_with_session();
        let resolved = |dispute_id, by, now| NewEvent {
            dispute_id,
            session_id: None,
            kind: "resolved",
            payload: json!({ "status": "seller-refunded", "resolved_by": by }),
            now,
        };
        append(store.conn(), &resolved("d1", "parties", 500)).unwrap();
        // No session: Serbero never mediated it.
        append(store.conn(), &resolved("d2", "solver", 500)).unwrap();

        assert_eq!(
            unfinished_resolutions(store.conn(), 400).unwrap(),
            [Resolution {
                dispute_id: "d1".into(),
                status: "seller-refunded".into(),
                by_parties: true,
            }]
        );
        assert!(
            unfinished_resolutions(store.conn(), 600)
                .unwrap()
                .is_empty(),
            "too old"
        );

        append(store.conn(), &event("d1", "finished", 510)).unwrap();

        assert!(
            unfinished_resolutions(store.conn(), 400)
                .unwrap()
                .is_empty()
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
