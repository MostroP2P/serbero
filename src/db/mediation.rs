//! SQLite helpers for Phase 3 mediation_sessions and
//! mediation_messages rows.
//!
//! Scope for this slice (US1): insert a session at `awaiting_response`
//! with the pinned bundle, and append outbound messages. Lifecycle
//! transitions and inbound ingest helpers (US2) are deferred.

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::models::mediation::{MediationSessionState, TranscriptParty};

/// Minimal session-open payload. US2+ will extend with inbound
/// last-seen markers, assigned_solver writes, etc.
pub struct NewMediationSession<'a> {
    pub session_id: &'a str,
    pub dispute_id: &'a str,
    pub prompt_bundle_id: &'a str,
    pub policy_hash: &'a str,
    pub buyer_shared_pubkey: Option<&'a str>,
    pub seller_shared_pubkey: Option<&'a str>,
    pub started_at: i64,
}

pub fn insert_session(conn: &Connection, s: &NewMediationSession<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO mediation_sessions (
            session_id, dispute_id, state, round_count,
            prompt_bundle_id, policy_hash,
            buyer_shared_pubkey, seller_shared_pubkey,
            started_at, last_transition_at
         ) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            s.session_id,
            s.dispute_id,
            MediationSessionState::AwaitingResponse.to_string(),
            s.prompt_bundle_id,
            s.policy_hash,
            s.buyer_shared_pubkey,
            s.seller_shared_pubkey,
            s.started_at,
        ],
    )?;
    Ok(())
}

pub struct NewOutboundMessage<'a> {
    pub session_id: &'a str,
    pub party: TranscriptParty,
    pub shared_pubkey: &'a str,
    pub inner_event_id: &'a str,
    pub inner_event_created_at: i64,
    pub outer_event_id: Option<&'a str>,
    pub content: &'a str,
    pub prompt_bundle_id: &'a str,
    pub policy_hash: &'a str,
    pub persisted_at: i64,
}

pub fn insert_outbound_message(conn: &Connection, m: &NewOutboundMessage<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO mediation_messages (
            session_id, direction, party,
            shared_pubkey, inner_event_id, inner_event_created_at,
            outer_event_id, content, prompt_bundle_id, policy_hash,
            persisted_at, stale
         ) VALUES (?1, 'outbound', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0)",
        params![
            m.session_id,
            m.party.to_string(),
            m.shared_pubkey,
            m.inner_event_id,
            m.inner_event_created_at,
            m.outer_event_id,
            m.content,
            m.prompt_bundle_id,
            m.policy_hash,
            m.persisted_at,
        ],
    )?;
    Ok(())
}

/// Lookup the most recently opened mediation_sessions row for a
/// given dispute_id, if any. Used by the engine to gate session
/// opens (don't open a new session if one is already live).
pub fn latest_open_session_for(
    conn: &Connection,
    dispute_id: &str,
) -> Result<Option<(String, MediationSessionState)>> {
    use std::str::FromStr;

    match conn.query_row(
        "SELECT session_id, state FROM mediation_sessions
         WHERE dispute_id = ?1
         ORDER BY started_at DESC
         LIMIT 1",
        params![dispute_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    ) {
        Ok((sid, st)) => Ok(Some((sid, MediationSessionState::from_str(&st)?))),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    fn fresh() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        // Session insert requires a parent disputes row because the
        // FK is enforced (PRAGMA foreign_keys = ON).
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES (?1, 'e1', 'm1', 'buyer', 'initiated', 1, 2, 'new')",
            params!["dispute-xyz"],
        )
        .unwrap();
        conn
    }

    fn new_session(policy: &str) -> NewMediationSession<'_> {
        NewMediationSession {
            session_id: "sess-1",
            dispute_id: "dispute-xyz",
            prompt_bundle_id: "phase3-test",
            policy_hash: policy,
            buyer_shared_pubkey: Some("buyer-shared-pk"),
            seller_shared_pubkey: Some("seller-shared-pk"),
            started_at: 100,
        }
    }

    #[test]
    fn insert_session_row_carries_pinned_bundle() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-1")).unwrap();

        let (state, ph, bsp, ssp, rc): (String, String, Option<String>, Option<String>, i64) = conn
            .query_row(
                "SELECT state, policy_hash, buyer_shared_pubkey, seller_shared_pubkey, round_count
                 FROM mediation_sessions WHERE session_id = 'sess-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(state, "awaiting_response");
        assert_eq!(ph, "pol-hash-1");
        assert_eq!(bsp.as_deref(), Some("buyer-shared-pk"));
        assert_eq!(ssp.as_deref(), Some("seller-shared-pk"));
        assert_eq!(rc, 0);
    }

    #[test]
    fn insert_outbound_messages_honor_unique_inner_event_id() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-2")).unwrap();

        let buyer_msg = NewOutboundMessage {
            session_id: "sess-1",
            party: TranscriptParty::Buyer,
            shared_pubkey: "buyer-shared-pk",
            inner_event_id: "inner-1",
            inner_event_created_at: 200,
            outer_event_id: Some("outer-1"),
            content: "first clarifying question",
            prompt_bundle_id: "phase3-test",
            policy_hash: "pol-hash-2",
            persisted_at: 210,
        };
        insert_outbound_message(&conn, &buyer_msg).unwrap();

        // Second insert with the same (session_id, inner_event_id)
        // must be rejected by the unique index — this is the
        // dedup primary mechanism that US2 will rely on.
        assert!(insert_outbound_message(&conn, &buyer_msg).is_err());

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mediation_messages WHERE session_id='sess-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn latest_open_session_returns_none_when_absent() {
        let conn = fresh();
        assert!(latest_open_session_for(&conn, "nope").unwrap().is_none());
    }

    #[test]
    fn latest_open_session_returns_most_recent() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-3")).unwrap();
        let found = latest_open_session_for(&conn, "dispute-xyz").unwrap();
        assert!(found.is_some());
        let (sid, state) = found.unwrap();
        assert_eq!(sid, "sess-1");
        assert_eq!(state, MediationSessionState::AwaitingResponse);
    }
}
