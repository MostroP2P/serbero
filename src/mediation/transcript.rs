//! Transcript loader for the mid-session follow-up loop (T117 / FR-128).
//!
//! [`load_transcript_for_session`] reads `mediation_messages` for one
//! session, tags each row with its [`TranscriptParty`] by matching
//! the row's `shared_pubkey` against the session's
//! `buyer_shared_pubkey` / `seller_shared_pubkey`, excludes rows
//! flagged as stale by the ingest tick, and caps at the most recent
//! `max_rows` entries so a runaway transcript cannot blow the
//! reasoning provider's token budget.
//!
//! Called by `mediation::advance_session_round` (T120); that is the
//! only production consumer. The function is a pure DB read — no
//! writes, no network — so the transaction / lock boundary is owned
//! entirely by the caller.
//!
//! # Ordering
//!
//! Entries are returned **ascending** by `inner_event_created_at`
//! (oldest first) per the reasoning-provider contract
//! (`src/models/reasoning.rs` — `TranscriptEntry` doc). The SQL uses
//! `ORDER BY inner_event_created_at DESC LIMIT ?max` to keep only
//! the most recent window, then the Rust side reverses so the caller
//! sees ascending order.
//!
//! # Party-tag rules
//!
//! - `direction = 'outbound'` → [`TranscriptParty::Serbero`] — Serbero
//!   authored the message; `shared_pubkey` is the *recipient* party's
//!   shared pubkey (not needed for role tagging).
//! - `direction = 'inbound'` → map `shared_pubkey` against the
//!   session's pair:
//!   - matches `buyer_shared_pubkey` → [`TranscriptParty::Buyer`]
//!   - matches `seller_shared_pubkey` → [`TranscriptParty::Seller`]
//!   - matches neither → log `warn!` and drop the row. This is
//!     pathological (ingest should have enforced the invariant) but
//!     worth being defensive about: silently re-classifying a
//!     message under the wrong party role would feed misleading
//!     context to the model.
//! - Any other `direction` value → log `warn!` and drop. SQLite has
//!   no CHECK constraint on the column today; we don't want a
//!   typo'd row to bring the loop down.
//!
//! # What the cap excludes
//!
//! Stale rows (`mediation_messages.stale = 1`) are never included.
//! They exist for audit purposes (see `stale_inbound_is_persisted_but_
//! does_not_advance_session` in the Phase 3 integration suite) but
//! MUST NOT participate in classification: the ingest pipeline
//! already decided they fell outside the time window Serbero can
//! reasonably reason about.

use rusqlite::{params, Connection};
use tracing::warn;

use crate::error::Result;
use crate::models::mediation::TranscriptParty;
use crate::models::reasoning::TranscriptEntry;

/// Load the transcript for one mediation session.
///
/// Returns the `max_rows` most recent non-stale messages in
/// ascending `inner_event_created_at` order. An unknown `session_id`
/// returns an empty vector (not an error) — the caller can interpret
/// that as "nothing to classify yet", symmetric to a session that
/// exists but has no messages on file.
pub fn load_transcript_for_session(
    conn: &Connection,
    session_id: &str,
    max_rows: usize,
) -> Result<Vec<TranscriptEntry>> {
    // (1) Resolve the session's per-party shared pubkeys. Unknown
    //     session id → empty transcript rather than an error:
    //     race-free default when called from a caller that already
    //     knows the session existed a moment ago but the row may
    //     have been deleted by a parallel cleanup. Current code
    //     never deletes sessions, so this is defensive.
    let (buyer_sp, seller_sp): (Option<String>, Option<String>) = match conn.query_row(
        "SELECT buyer_shared_pubkey, seller_shared_pubkey
         FROM mediation_sessions
         WHERE session_id = ?1",
        params![session_id],
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        },
    ) {
        Ok(pair) => pair,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    // (2) Pull at most `max_rows` rows, most recent first. We do
    //     the cap in SQL (instead of unbounded SELECT + truncate
    //     in Rust) so the worst-case transfer size is bounded even
    //     when the DB has somehow accumulated a runaway transcript.
    //     `max_rows == 0` collapses to a zero-LIMIT query that
    //     returns no rows; the caller can short-circuit on that.
    let mut stmt = conn.prepare(
        "SELECT direction, shared_pubkey, inner_event_created_at, content
         FROM mediation_messages
         WHERE session_id = ?1 AND stale = 0
         ORDER BY inner_event_created_at DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![session_id, max_rows as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;

    // (3) Tag each row. Drop (with a warn) anything we cannot
    //     attribute to a known role — feeding the model a message
    //     tagged "Serbero" that really came from one of the parties
    //     would corrupt the reasoning context.
    let mut out: Vec<TranscriptEntry> = Vec::with_capacity(max_rows);
    for row in rows {
        let (direction, shared_pubkey, ts, content) = row?;
        let party = match direction.as_str() {
            "outbound" => TranscriptParty::Serbero,
            "inbound" => {
                if Some(shared_pubkey.as_str()) == buyer_sp.as_deref() {
                    TranscriptParty::Buyer
                } else if Some(shared_pubkey.as_str()) == seller_sp.as_deref() {
                    TranscriptParty::Seller
                } else {
                    warn!(
                        session_id = %session_id,
                        shared_pubkey = %shared_pubkey,
                        "transcript: dropping inbound row with unknown shared_pubkey"
                    );
                    continue;
                }
            }
            other => {
                warn!(
                    session_id = %session_id,
                    direction = other,
                    "transcript: dropping row with unrecognised direction"
                );
                continue;
            }
        };
        out.push(TranscriptEntry {
            party,
            inner_event_created_at: ts,
            content,
        });
    }

    // (4) Reverse so the caller gets ascending order. FR-128 says
    //     "ordered by inner_created_at ascending (outer event time
    //     is not authoritative)".
    out.reverse();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    /// Shared harness: open an in-memory DB, apply migrations, seed
    /// a dispute + a session row with known buyer/seller shared
    /// pubkeys. Returns the connection so the test can seed
    /// messages and then call `load_transcript_for_session`.
    fn seeded_conn(buyer_sp: &str, seller_sp: &str) -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-t117', 'evt-t117', 'mostro', 'buyer',
                       'initiated', 1, 2, 'notified')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                buyer_shared_pubkey, seller_shared_pubkey,
                started_at, last_transition_at
             ) VALUES ('sess-t117', 'd-t117', 'awaiting_response', 0,
                       'phase3-default', 'hash',
                       ?1, ?2, 100, 100)",
            params![buyer_sp, seller_sp],
        )
        .unwrap();
        conn
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_msg(
        conn: &Connection,
        direction: &str,
        party: &str,
        shared_pubkey: &str,
        inner_event_id: &str,
        inner_event_created_at: i64,
        content: &str,
        stale: bool,
    ) {
        conn.execute(
            "INSERT INTO mediation_messages (
                session_id, direction, party, shared_pubkey,
                inner_event_id, inner_event_created_at,
                content, persisted_at, stale
             ) VALUES ('sess-t117', ?1, ?2, ?3, ?4, ?5, ?6, ?5, ?7)",
            params![
                direction,
                party,
                shared_pubkey,
                inner_event_id,
                inner_event_created_at,
                content,
                if stale { 1 } else { 0 }
            ],
        )
        .unwrap();
    }

    #[test]
    fn unknown_session_returns_empty_vec() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let got = load_transcript_for_session(&conn, "does-not-exist", 40).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn session_with_no_messages_returns_empty_vec() {
        let conn = seeded_conn("buyer-sp", "seller-sp");
        let got = load_transcript_for_session(&conn, "sess-t117", 40).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn normal_flow_returns_ascending_order_with_party_tags() {
        let conn = seeded_conn("buyer-sp", "seller-sp");
        // Interleaved: outbound (Serbero) → inbound (Buyer) → outbound
        // → inbound (Seller). Timestamps strictly increasing.
        insert_msg(&conn, "outbound", "buyer", "buyer-sp", "e-out-1", 100, "hello buyer", false);
        insert_msg(&conn, "outbound", "seller", "seller-sp", "e-out-2", 101, "hello seller", false);
        insert_msg(&conn, "inbound", "buyer", "buyer-sp", "e-in-1", 200, "buyer reply", false);
        insert_msg(
            &conn,
            "inbound",
            "seller",
            "seller-sp",
            "e-in-2",
            300,
            "seller reply",
            false,
        );

        let got = load_transcript_for_session(&conn, "sess-t117", 40).unwrap();
        assert_eq!(got.len(), 4);
        // Ascending by inner_event_created_at.
        assert_eq!(got[0].inner_event_created_at, 100);
        assert_eq!(got[0].party, TranscriptParty::Serbero);
        assert_eq!(got[0].content, "hello buyer");
        assert_eq!(got[1].party, TranscriptParty::Serbero);
        assert_eq!(got[1].content, "hello seller");
        assert_eq!(got[2].party, TranscriptParty::Buyer);
        assert_eq!(got[2].content, "buyer reply");
        assert_eq!(got[3].party, TranscriptParty::Seller);
        assert_eq!(got[3].content, "seller reply");
    }

    #[test]
    fn stale_rows_are_excluded() {
        let conn = seeded_conn("buyer-sp", "seller-sp");
        insert_msg(&conn, "inbound", "buyer", "buyer-sp", "e-fresh", 100, "fresh", false);
        insert_msg(&conn, "inbound", "seller", "seller-sp", "e-stale", 150, "stale", true);
        let got = load_transcript_for_session(&conn, "sess-t117", 40).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "fresh");
        assert_eq!(got[0].party, TranscriptParty::Buyer);
    }

    #[test]
    fn inbound_with_unknown_shared_pubkey_is_dropped() {
        let conn = seeded_conn("buyer-sp", "seller-sp");
        insert_msg(&conn, "inbound", "buyer", "buyer-sp", "e-ok", 100, "ok", false);
        // shared_pubkey doesn't match buyer-sp or seller-sp — a
        // pathological ingest-side bug; we drop the row.
        insert_msg(
            &conn,
            "inbound",
            "buyer",
            "other-sp",
            "e-mystery",
            101,
            "mystery",
            false,
        );
        let got = load_transcript_for_session(&conn, "sess-t117", 40).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "ok");
    }

    #[test]
    fn cap_returns_the_last_n_entries_in_ascending_order() {
        let conn = seeded_conn("buyer-sp", "seller-sp");
        // 10 inbound buyer replies at t = 0..9. Cap at 3 → we want
        // the last three (t = 7, 8, 9) in ascending order.
        for i in 0..10 {
            insert_msg(
                &conn,
                "inbound",
                "buyer",
                "buyer-sp",
                &format!("e-{i}"),
                i,
                &format!("msg {i}"),
                false,
            );
        }
        let got = load_transcript_for_session(&conn, "sess-t117", 3).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].inner_event_created_at, 7);
        assert_eq!(got[1].inner_event_created_at, 8);
        assert_eq!(got[2].inner_event_created_at, 9);
    }

    #[test]
    fn zero_max_rows_returns_empty_vec() {
        // Edge case: the caller asks for "0 rows". Rather than
        // making the caller special-case it before calling, we
        // honor it: an empty transcript is legal and means "nothing
        // to classify".
        let conn = seeded_conn("buyer-sp", "seller-sp");
        insert_msg(&conn, "inbound", "buyer", "buyer-sp", "e", 100, "x", false);
        let got = load_transcript_for_session(&conn, "sess-t117", 0).unwrap();
        assert!(got.is_empty());
    }
}
