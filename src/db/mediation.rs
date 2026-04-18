//! SQLite helpers for Phase 3 mediation_sessions and
//! mediation_messages rows.
//!
//! Scope today (US1 + narrow US2 slice): insert a session at
//! `awaiting_response` with the pinned bundle, append outbound
//! messages, idempotently persist inbound messages, advance the
//! per-party last-seen marker, and recompute `round_count` from the
//! persisted transcript. Lifecycle transitions beyond that (US3,
//! US4) and the engine-driven ingest tick (T040 / T051) remain
//! deferred.

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

/// Inbound mediation message payload. Separate struct from
/// [`NewOutboundMessage`] because the two carry different columns —
/// outbound rows carry `prompt_bundle_id` + `policy_hash` (provenance
/// of the bundle that produced the draft); inbound rows do not,
/// because parties did not author against a bundle.
pub struct NewInboundMessage<'a> {
    pub session_id: &'a str,
    pub party: TranscriptParty,
    pub shared_pubkey: &'a str,
    pub inner_event_id: &'a str,
    pub inner_event_created_at: i64,
    pub outer_event_id: Option<&'a str>,
    pub content: &'a str,
    pub persisted_at: i64,
    /// `1` iff this inbound's inner `created_at` predated the
    /// session's last-seen marker for its party at ingest time.
    pub stale: bool,
}

/// Idempotent inbound insert. Returns `true` when a row actually
/// landed and `false` when the unique index on
/// `(session_id, inner_event_id)` rejected the insert because the
/// same inbound event had already been persisted. The callsite uses
/// this to distinguish fresh ingest (advance last-seen / round
/// count) from replay (no session-state change).
pub fn insert_inbound_message(conn: &Connection, m: &NewInboundMessage<'_>) -> Result<bool> {
    let rows = conn.execute(
        "INSERT OR IGNORE INTO mediation_messages (
            session_id, direction, party,
            shared_pubkey, inner_event_id, inner_event_created_at,
            outer_event_id, content, prompt_bundle_id, policy_hash,
            persisted_at, stale
         ) VALUES (?1, 'inbound', ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9)",
        params![
            m.session_id,
            m.party.to_string(),
            m.shared_pubkey,
            m.inner_event_id,
            m.inner_event_created_at,
            m.outer_event_id,
            m.content,
            m.persisted_at,
            if m.stale { 1 } else { 0 },
        ],
    )?;
    Ok(rows > 0)
}

/// Read the per-party last-seen inner timestamps for a session.
/// Returns `(buyer_last_seen, seller_last_seen)` in seconds; either
/// side may be `None` if that party has never replied.
pub fn get_last_seen(conn: &Connection, session_id: &str) -> Result<(Option<i64>, Option<i64>)> {
    match conn.query_row(
        "SELECT buyer_last_seen_inner_ts, seller_last_seen_inner_ts
         FROM mediation_sessions WHERE session_id = ?1",
        params![session_id],
        |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
    ) {
        Ok(pair) => Ok(pair),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok((None, None)),
        Err(e) => Err(e.into()),
    }
}

/// Update the last-seen inner-event timestamp for one party on a
/// session. Only the column matching `party` is written.
pub fn update_last_seen_inner_ts(
    conn: &Connection,
    session_id: &str,
    party: TranscriptParty,
    ts: i64,
) -> Result<()> {
    let column = match party {
        TranscriptParty::Buyer => "buyer_last_seen_inner_ts",
        TranscriptParty::Seller => "seller_last_seen_inner_ts",
        TranscriptParty::Serbero => {
            // Serbero never authors inbound rows, so this branch is
            // unreachable in the ingest path. Guard defensively —
            // this is the kind of enum-widening mistake that is
            // cheap to catch at the DB boundary.
            return Err(crate::error::Error::InvalidEvent(
                "serbero is not a valid last-seen party".into(),
            ));
        }
    };
    let sql = format!(
        "UPDATE mediation_sessions SET {column} = ?1 WHERE session_id = ?2",
        column = column
    );
    conn.execute(&sql, params![ts, session_id])?;
    Ok(())
}

/// Recompute `round_count` from the persisted inbound transcript.
///
/// Rule (from `data-model.md` §mediation_sessions + T050): one
/// completed round = one buyer reply + one seller reply. The count
/// is therefore `min(fresh_buyer_inbound_count, fresh_seller_inbound_count)`,
/// where "fresh" excludes rows marked `stale = 1`. Recomputing from
/// the transcript each time keeps the counter deterministic under
/// replay and idempotent under duplicate ingest.
///
/// Returns the new round count. Does NOT transition session state —
/// state transitions belong to the policy layer (US3/US4).
pub fn recompute_round_count(conn: &Connection, session_id: &str) -> Result<i64> {
    let buyer_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM mediation_messages
         WHERE session_id = ?1 AND direction = 'inbound' AND party = 'buyer' AND stale = 0",
        params![session_id],
        |r| r.get(0),
    )?;
    let seller_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM mediation_messages
         WHERE session_id = ?1 AND direction = 'inbound' AND party = 'seller' AND stale = 0",
        params![session_id],
        |r| r.get(0),
    )?;
    let new_rounds = buyer_count.min(seller_count);
    conn.execute(
        "UPDATE mediation_sessions SET round_count = ?1 WHERE session_id = ?2",
        params![new_rounds, session_id],
    )?;
    Ok(new_rounds)
}

/// Lookup the most recently opened *live* mediation_sessions row for
/// a given dispute_id, if any. Rows in terminal / handed-off states
/// (`closed`, `summary_delivered`, `escalation_recommended`,
/// `superseded_by_human`) are excluded — a dispute that was closed
/// or escalated earlier must not block a later session open.
///
/// Used by the engine to gate session opens and, crucially, re-checked
/// inside the final open-session DB transaction to close the
/// check-then-act race.
pub fn latest_open_session_for(
    conn: &Connection,
    dispute_id: &str,
) -> Result<Option<(String, MediationSessionState)>> {
    use std::str::FromStr;

    match conn.query_row(
        "SELECT session_id, state FROM mediation_sessions
         WHERE dispute_id = ?1
           AND state NOT IN (
               'closed',
               'summary_delivered',
               'escalation_recommended',
               'superseded_by_human'
           )
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

    fn new_inbound(party: TranscriptParty, inner_id: &str, ts: i64) -> NewInboundMessage<'_> {
        let shared_pubkey = match party {
            TranscriptParty::Buyer => "buyer-shared-pk",
            TranscriptParty::Seller => "seller-shared-pk",
            TranscriptParty::Serbero => unreachable!(),
        };
        NewInboundMessage {
            session_id: "sess-1",
            party,
            shared_pubkey,
            inner_event_id: inner_id,
            inner_event_created_at: ts,
            outer_event_id: None,
            content: "party reply text",
            persisted_at: ts + 1,
            stale: false,
        }
    }

    #[test]
    fn insert_inbound_message_is_idempotent_on_inner_event_id() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-inb")).unwrap();

        let msg = new_inbound(TranscriptParty::Buyer, "inner-a", 300);
        assert!(insert_inbound_message(&conn, &msg).unwrap(), "first insert");
        assert!(
            !insert_inbound_message(&conn, &msg).unwrap(),
            "replay must be a no-op"
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mediation_messages WHERE session_id='sess-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "no duplicate row on replay");
    }

    #[test]
    fn update_last_seen_writes_only_the_matching_party_column() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-ls")).unwrap();
        update_last_seen_inner_ts(&conn, "sess-1", TranscriptParty::Buyer, 500).unwrap();
        let (bls, sls) = get_last_seen(&conn, "sess-1").unwrap();
        assert_eq!(bls, Some(500));
        assert_eq!(sls, None, "seller column must stay untouched");

        update_last_seen_inner_ts(&conn, "sess-1", TranscriptParty::Seller, 700).unwrap();
        let (bls, sls) = get_last_seen(&conn, "sess-1").unwrap();
        assert_eq!(bls, Some(500));
        assert_eq!(sls, Some(700));
    }

    #[test]
    fn recompute_round_count_counts_min_of_fresh_per_party_replies() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-rc")).unwrap();

        // Zero replies → zero rounds.
        assert_eq!(recompute_round_count(&conn, "sess-1").unwrap(), 0);

        // One buyer, zero seller → still zero rounds.
        insert_inbound_message(&conn, &new_inbound(TranscriptParty::Buyer, "b1", 100)).unwrap();
        assert_eq!(recompute_round_count(&conn, "sess-1").unwrap(), 0);

        // One buyer + one seller → one completed round.
        insert_inbound_message(&conn, &new_inbound(TranscriptParty::Seller, "s1", 110)).unwrap();
        assert_eq!(recompute_round_count(&conn, "sess-1").unwrap(), 1);

        // Extra buyer reply without matching seller → still one.
        insert_inbound_message(&conn, &new_inbound(TranscriptParty::Buyer, "b2", 120)).unwrap();
        assert_eq!(recompute_round_count(&conn, "sess-1").unwrap(), 1);

        // Matching seller → round 2.
        insert_inbound_message(&conn, &new_inbound(TranscriptParty::Seller, "s2", 130)).unwrap();
        assert_eq!(recompute_round_count(&conn, "sess-1").unwrap(), 2);
    }

    #[test]
    fn recompute_round_count_ignores_stale_rows() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-stale")).unwrap();

        let mut b = new_inbound(TranscriptParty::Buyer, "b1", 100);
        b.stale = true;
        insert_inbound_message(&conn, &b).unwrap();
        let mut s = new_inbound(TranscriptParty::Seller, "s1", 110);
        s.stale = false;
        insert_inbound_message(&conn, &s).unwrap();

        // Buyer's only reply is stale → no completed round.
        assert_eq!(recompute_round_count(&conn, "sess-1").unwrap(), 0);
    }

    #[test]
    fn latest_open_session_skips_terminal_states() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-4")).unwrap();
        // Flip the session to a terminal / handed-off state; a
        // subsequent open attempt must NOT be blocked by it.
        for terminal in [
            "closed",
            "summary_delivered",
            "escalation_recommended",
            "superseded_by_human",
        ] {
            conn.execute(
                "UPDATE mediation_sessions SET state = ?1 WHERE session_id = 'sess-1'",
                params![terminal],
            )
            .unwrap();
            let found = latest_open_session_for(&conn, "dispute-xyz").unwrap();
            assert!(
                found.is_none(),
                "latest_open_session_for must skip state '{terminal}', got {found:?}"
            );
        }
    }
}
