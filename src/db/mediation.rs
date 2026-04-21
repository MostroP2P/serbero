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

use rusqlite::{params, Connection, Transaction};

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

/// Snapshot of a live mediation_sessions row, used by the engine's
/// startup-resume pass and its per-tick ingest loop (T051 / T052).
/// Only the fields both paths need — keep thin.
#[derive(Debug, Clone)]
pub struct LiveSession {
    pub session_id: String,
    pub dispute_id: String,
    pub state: MediationSessionState,
    pub prompt_bundle_id: String,
    pub policy_hash: String,
    pub buyer_shared_pubkey: Option<String>,
    pub seller_shared_pubkey: Option<String>,
}

/// List all mediation sessions that are NOT in a terminal or
/// handed-off state. Same exclusion set as
/// [`latest_open_session_for`]: `closed`, `summary_delivered`,
/// `escalation_recommended`, `superseded_by_human`. The engine uses
/// this to decide which sessions to poll for inbound replies on each
/// tick and to rebuild in-memory chat material at startup.
pub fn list_live_sessions(conn: &Connection) -> Result<Vec<LiveSession>> {
    use std::str::FromStr;

    let mut stmt = conn.prepare(
        "SELECT session_id, dispute_id, state,
                prompt_bundle_id, policy_hash,
                buyer_shared_pubkey, seller_shared_pubkey
         FROM mediation_sessions
         WHERE state NOT IN (
             'closed',
             'summary_delivered',
             'escalation_recommended',
             'superseded_by_human'
         )
         ORDER BY started_at ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (session_id, dispute_id, state_s, bundle, hash, bsp, ssp) = row?;
        let state = MediationSessionState::from_str(&state_s)?;
        out.push(LiveSession {
            session_id,
            dispute_id,
            state,
            prompt_bundle_id: bundle,
            policy_hash: hash,
            buyer_shared_pubkey: bsp,
            seller_shared_pubkey: ssp,
        });
    }
    Ok(out)
}

/// Write a new lifecycle state onto a session row.
///
/// In debug builds this helper asserts that the current → next
/// transition is legal per
/// [`MediationSessionState::can_transition_to`], surfacing
/// callers that forget the state-machine check before writing.
/// Release builds skip the assert and just issue the UPDATE, so
/// the check adds zero cost on the hot path.
///
/// Used by the T052 restart-resume pass to flip a session to
/// `escalation_recommended` when its pinned prompt bundle is no
/// longer loadable (trigger `policy_bundle_missing`), and by the
/// session-open path to mark non-AskClarification opens escalated.
pub fn set_session_state(
    conn: &Connection,
    session_id: &str,
    new_state: MediationSessionState,
    at: i64,
) -> Result<()> {
    #[cfg(debug_assertions)]
    {
        use std::str::FromStr;
        let current: Option<String> = conn
            .query_row(
                "SELECT state FROM mediation_sessions WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .ok();
        if let Some(current) = current {
            let current = MediationSessionState::from_str(&current)
                .expect("set_session_state: persisted state must parse");
            debug_assert!(
                current.can_transition_to(new_state),
                "set_session_state: illegal transition {current} -> {new_state} \
                 for session_id={session_id}"
            );
        }
    }
    conn.execute(
        "UPDATE mediation_sessions
         SET state = ?1, last_transition_at = ?2
         WHERE session_id = ?3",
        params![new_state.to_string(), at, session_id],
    )?;
    Ok(())
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

/// Phase 11 idempotency-marker writer (FR-127).
///
/// Sets `round_count_last_evaluated = new_round_count` AND resets
/// `consecutive_eval_failures = 0` in a single UPDATE. Takes a
/// `&Transaction` so the caller can commit the marker advance
/// atomically with whatever side effect the mid-session dispatch
/// produced (two `mediation_messages` rows + a state transition for
/// `AskClarification`, or a `classification_produced` event for
/// `Summarize`). A commit of the enclosing transaction makes both
/// visible together; a rollback loses both. That atomicity is the
/// whole point — a crash between "dispatched outbound" and "marked
/// the round evaluated" would otherwise re-dispatch on the next
/// tick and double-message the parties.
///
/// Resetting `consecutive_eval_failures` to 0 is paired with the
/// marker advance on purpose: any successful evaluation clears the
/// failure streak by definition (FR-130 "Any successful evaluation
/// resets consecutive_eval_failures to 0").
pub fn advance_evaluator_marker(
    tx: &Transaction<'_>,
    session_id: &str,
    new_round_count: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE mediation_sessions
         SET round_count_last_evaluated = ?1,
             consecutive_eval_failures = 0
         WHERE session_id = ?2",
        params![new_round_count, session_id],
    )?;
    Ok(())
}

/// Phase 11 bounded-failure counter (FR-130).
///
/// Increments `consecutive_eval_failures` for the session and
/// returns the new value. The caller uses the return value to
/// decide whether to escalate with `ReasoningUnavailable`
/// (threshold: value `>= 3` per FR-130).
///
/// Takes `&Connection` (not `&Transaction`) because the failure
/// path writes this increment OUTSIDE whatever transaction the
/// dispatch attempted — the dispatch's transaction has already
/// been dropped by the time we're in the error branch. The single
/// UPDATE is still atomic (SQLite's default isolation handles it);
/// there is no "increment + read back" race within the lock that
/// the caller holds around `advance_session_round`.
pub fn bump_consecutive_eval_failures(conn: &Connection, session_id: &str) -> Result<i64> {
    conn.execute(
        "UPDATE mediation_sessions
         SET consecutive_eval_failures = consecutive_eval_failures + 1
         WHERE session_id = ?1",
        params![session_id],
    )?;
    let new_value: i64 = conn.query_row(
        "SELECT consecutive_eval_failures
         FROM mediation_sessions
         WHERE session_id = ?1",
        params![session_id],
        |r| r.get(0),
    )?;
    Ok(new_value)
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

    #[test]
    fn list_live_sessions_returns_only_non_terminal_rows() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-live")).unwrap();

        // Alive row: should show up.
        let live = list_live_sessions(&conn).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].session_id, "sess-1");
        assert_eq!(live[0].state, MediationSessionState::AwaitingResponse);
        assert_eq!(live[0].prompt_bundle_id, "phase3-test");
        assert_eq!(live[0].policy_hash, "pol-hash-live");
        assert_eq!(
            live[0].buyer_shared_pubkey.as_deref(),
            Some("buyer-shared-pk")
        );

        // Flip to each terminal / handed-off state; the row must
        // disappear from the live list.
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
            let live = list_live_sessions(&conn).unwrap();
            assert!(
                live.is_empty(),
                "state '{terminal}' must be excluded from list_live_sessions; got {live:?}"
            );
        }
    }

    #[test]
    fn set_session_state_updates_state_and_transition_ts() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-hash-trans")).unwrap();

        set_session_state(
            &conn,
            "sess-1",
            MediationSessionState::EscalationRecommended,
            555,
        )
        .unwrap();

        let (state, ts): (String, i64) = conn
            .query_row(
                "SELECT state, last_transition_at FROM mediation_sessions WHERE session_id = 'sess-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "escalation_recommended");
        assert_eq!(ts, 555);
    }

    // ---------------------------------------------------------
    // Phase 11 — idempotency-marker + failure-counter helpers (T118)
    // ---------------------------------------------------------

    fn read_eval_columns(conn: &Connection, session_id: &str) -> (i64, i64) {
        conn.query_row(
            "SELECT round_count_last_evaluated, consecutive_eval_failures
             FROM mediation_sessions WHERE session_id = ?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn advance_evaluator_marker_sets_marker_and_resets_failure_counter() {
        let mut conn = fresh();
        insert_session(&conn, &new_session("pol-adv-1")).unwrap();
        // Seed a non-zero failure counter to prove the reset.
        conn.execute(
            "UPDATE mediation_sessions SET consecutive_eval_failures = 2
             WHERE session_id = 'sess-1'",
            [],
        )
        .unwrap();

        let tx = conn.transaction().unwrap();
        advance_evaluator_marker(&tx, "sess-1", 3).unwrap();
        tx.commit().unwrap();

        let (marker, failures) = read_eval_columns(&conn, "sess-1");
        assert_eq!(marker, 3, "marker must advance to the supplied round count");
        assert_eq!(
            failures, 0,
            "any successful evaluation resets the failure streak"
        );
    }

    #[test]
    fn advance_evaluator_marker_rolls_back_on_transaction_rollback() {
        // FR-127: a rollback of the enclosing transaction MUST
        // leave the marker untouched. Otherwise a crash between
        // "dispatched outbound" and "marked evaluated" could
        // re-dispatch — the whole point of the marker.
        let mut conn = fresh();
        insert_session(&conn, &new_session("pol-adv-2")).unwrap();

        let tx = conn.transaction().unwrap();
        advance_evaluator_marker(&tx, "sess-1", 7).unwrap();
        drop(tx); // implicit rollback — no commit call

        let (marker, failures) = read_eval_columns(&conn, "sess-1");
        assert_eq!(
            marker, 0,
            "rollback must leave the marker at its pre-tx value"
        );
        assert_eq!(failures, 0);
    }

    #[test]
    fn bump_consecutive_eval_failures_increments_and_returns_new_value() {
        let conn = fresh();
        insert_session(&conn, &new_session("pol-bump-1")).unwrap();

        let v1 = bump_consecutive_eval_failures(&conn, "sess-1").unwrap();
        assert_eq!(v1, 1);
        let v2 = bump_consecutive_eval_failures(&conn, "sess-1").unwrap();
        assert_eq!(v2, 2);
        let v3 = bump_consecutive_eval_failures(&conn, "sess-1").unwrap();
        assert_eq!(v3, 3, "third bump crosses the FR-130 escalation threshold");

        let (_marker, failures) = read_eval_columns(&conn, "sess-1");
        assert_eq!(failures, 3);
    }

    #[test]
    fn advance_evaluator_marker_after_bumps_resets_failure_streak() {
        // Lifecycle check: fail twice → succeed once → streak
        // resets to 0. Validates the pairing FR-130 describes
        // ("any successful evaluation resets").
        let mut conn = fresh();
        insert_session(&conn, &new_session("pol-reset")).unwrap();

        assert_eq!(bump_consecutive_eval_failures(&conn, "sess-1").unwrap(), 1);
        assert_eq!(bump_consecutive_eval_failures(&conn, "sess-1").unwrap(), 2);

        let tx = conn.transaction().unwrap();
        advance_evaluator_marker(&tx, "sess-1", 1).unwrap();
        tx.commit().unwrap();

        let (marker, failures) = read_eval_columns(&conn, "sess-1");
        assert_eq!(marker, 1);
        assert_eq!(failures, 0);
    }
}
