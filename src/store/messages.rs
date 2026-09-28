//! The `messages` table: every message exchanged with a party.
//!
//! Inbound dedup on the inner event id is durable, as the Mostro chat
//! protocol requires: an in-memory cache would let a party replay an old
//! message once it was evicted.

use std::fmt;
use std::str::FromStr;

use rusqlite::{Connection, params};

use super::sessions::Party;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: i64,
    pub session_id: String,
    pub direction: Direction,
    pub party: Party,
    pub template_id: Option<String>,
    pub lang: Option<String>,
    pub content: String,
    pub attachments: u32,
    pub inner_event_id: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewMessage<'a> {
    pub session_id: &'a str,
    pub direction: Direction,
    pub party: Party,
    /// Outbound only: the catalog template that produced the text.
    pub template_id: Option<&'a str>,
    /// Outbound only: the language it was rendered in.
    pub lang: Option<&'a str>,
    pub content: &'a str,
    pub attachments: u32,
    pub inner_event_id: &'a str,
    /// The validated inner event `created_at`.
    pub created_at: i64,
}

/// Stores a message unless its inner event id is already stored for the
/// session. Returns `false` for a duplicate (a replay or a re-wrap).
pub fn insert_if_new(conn: &Connection, message: &NewMessage<'_>) -> Result<bool> {
    let inserted = conn.execute(
        "INSERT INTO messages
             (session_id, direction, party, template_id, lang, content, attachments,
              inner_event_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (session_id, direction, party, inner_event_id) DO NOTHING",
        params![
            message.session_id,
            message.direction.to_string(),
            message.party.to_string(),
            message.template_id,
            message.lang,
            message.content,
            message.attachments,
            message.inner_event_id,
            message.created_at,
        ],
    )?;
    Ok(inserted == 1)
}

/// A session's transcript in conversation order.
pub fn list_for_session(conn: &Connection, session_id: &str) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, direction, party, template_id, lang, content, attachments,
                inner_event_id, created_at
         FROM messages WHERE session_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([session_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, u32>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, i64>(9)?,
        ))
    })?;
    rows.map(|row| {
        let (id, session_id, direction, party, template_id, lang, content, attachments, inner, at) =
            row?;
        Ok(Message {
            id,
            session_id,
            direction: direction.parse()?,
            party: party.parse()?,
            template_id,
            lang,
            content,
            attachments,
            inner_event_id: inner,
            created_at: at,
        })
    })
    .collect()
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::In => "in",
            Self::Out => "out",
        })
    }
}

impl FromStr for Direction {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "in" => Ok(Self::In),
            "out" => Ok(Self::Out),
            other => Err(Error::Schema(format!(
                "unknown message direction {other:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sessions::testing::store_with_session;

    fn inbound<'a>(inner: &'a str, content: &'a str, at: i64) -> NewMessage<'a> {
        NewMessage {
            session_id: "s1",
            direction: Direction::In,
            party: Party::Buyer,
            template_id: None,
            lang: None,
            content,
            attachments: 0,
            inner_event_id: inner,
            created_at: at,
        }
    }

    #[test]
    fn the_same_outbound_text_to_both_parties_is_stored_twice() {
        // The inner event of a chat message names no recipient, so the same
        // text sent to both parties in one second has one inner id.
        let store = store_with_session();
        let to = |party| NewMessage {
            direction: Direction::Out,
            party,
            template_id: Some("handoff_notice"),
            ..inbound("same-inner-id", "notice", 10)
        };

        assert!(insert_if_new(store.conn(), &to(Party::Buyer)).unwrap());
        assert!(insert_if_new(store.conn(), &to(Party::Seller)).unwrap());
        assert!(
            !insert_if_new(store.conn(), &to(Party::Seller)).unwrap(),
            "still deduplicated"
        );
        assert_eq!(list_for_session(store.conn(), "s1").unwrap().len(), 2);
    }

    #[test]
    fn stores_messages_in_conversation_order() {
        let store = store_with_session();
        insert_if_new(store.conn(), &inbound("e2", "second", 20)).unwrap();
        insert_if_new(
            store.conn(),
            &NewMessage {
                direction: Direction::Out,
                party: Party::Seller,
                template_id: Some("ask_seller_received"),
                lang: Some("es"),
                ..inbound("e1", "first", 10)
            },
        )
        .unwrap();

        let messages = list_for_session(store.conn(), "s1").unwrap();

        let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["first", "second"]);
        assert_eq!(messages[0].direction, Direction::Out);
        assert_eq!(
            messages[0].template_id.as_deref(),
            Some("ask_seller_received")
        );
    }

    #[test]
    fn duplicate_inner_event_id_is_ignored() {
        let store = store_with_session();
        assert!(insert_if_new(store.conn(), &inbound("e1", "I sent it", 10)).unwrap());

        let replayed = insert_if_new(store.conn(), &inbound("e1", "I sent it", 99)).unwrap();

        assert!(!replayed);
        assert_eq!(list_for_session(store.conn(), "s1").unwrap().len(), 1);
    }

    #[test]
    fn message_for_an_unknown_session_is_rejected() {
        let store = store_with_session();

        let orphan = NewMessage {
            session_id: "no-such-session",
            ..inbound("e1", "hi", 10)
        };

        assert!(insert_if_new(store.conn(), &orphan).is_err());
    }
}
