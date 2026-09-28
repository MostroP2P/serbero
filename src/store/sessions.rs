//! The `sessions` table: Serbero assisting one dispute (`docs/spec.md` §7).

use std::fmt;
use std::str::FromStr;

use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, params};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Opening,
    Active,
    Guiding,
    HandedOff,
    Closed,
    Superseded,
}

impl SessionState {
    /// Closed and superseded sessions never change again.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Superseded)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Party {
    Buyer,
    Seller,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub session_id: String,
    pub dispute_id: String,
    pub state: SessionState,
    pub buyer_trade_pubkey: String,
    pub seller_trade_pubkey: String,
    pub fiat_amount: Option<String>,
    pub fiat_code: Option<String>,
    pub payment_method: Option<String>,
    pub order_published_at: Option<i64>,
    pub buyer_lang: Option<String>,
    pub seller_lang: Option<String>,
    pub buyer_chat_cursor: Option<i64>,
    pub seller_chat_cursor: Option<i64>,
    pub rounds: u32,
    pub handoff_reason: Option<String>,
    pub opened_at: i64,
    pub updated_at: i64,
}

impl Session {
    pub fn trade_pubkey(&self, party: Party) -> &str {
        match party {
            Party::Buyer => &self.buyer_trade_pubkey,
            Party::Seller => &self.seller_trade_pubkey,
        }
    }

    /// The language the party is addressed in: detected, or `default`.
    pub fn language<'a>(&'a self, party: Party, default: &'a str) -> &'a str {
        match party {
            Party::Buyer => self.buyer_lang.as_deref(),
            Party::Seller => self.seller_lang.as_deref(),
        }
        .unwrap_or(default)
    }

    pub fn chat_cursor(&self, party: Party) -> Option<i64> {
        match party {
            Party::Buyer => self.buyer_chat_cursor,
            Party::Seller => self.seller_chat_cursor,
        }
    }
}

/// A session as opened after a successful take (`docs/spec.md` §7.2).
#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    pub session_id: &'a str,
    pub dispute_id: &'a str,
    pub buyer_trade_pubkey: &'a str,
    pub seller_trade_pubkey: &'a str,
    pub fiat_amount: Option<&'a str>,
    pub fiat_code: Option<&'a str>,
    /// As the take response gave it (`SolverDisputeInfo::payment_method`).
    pub payment_method: Option<&'a str>,
    pub order_published_at: Option<i64>,
    pub now: i64,
}

/// Inserts a session in state `opening`. Returns `false` when the dispute
/// already has a live session: at most one exists per dispute.
pub fn insert(conn: &Connection, session: &NewSession<'_>) -> Result<bool> {
    let result = conn.execute(
        "INSERT INTO sessions
             (session_id, dispute_id, state, buyer_trade_pubkey, seller_trade_pubkey,
              fiat_amount, fiat_code, payment_method, order_published_at, opened_at, updated_at)
         VALUES (?1, ?2, 'opening', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![
            session.session_id,
            session.dispute_id,
            session.buyer_trade_pubkey,
            session.seller_trade_pubkey,
            session.fiat_amount,
            session.fiat_code,
            session.payment_method,
            session.order_published_at,
            session.now,
        ],
    );
    match result {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::SqliteFailure(e, Some(message)))
            if e.code == ErrorCode::ConstraintViolation
                && message.contains("sessions.dispute_id") =>
        {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

const COLUMNS: &str = "session_id, dispute_id, state, buyer_trade_pubkey, seller_trade_pubkey,
     fiat_amount, fiat_code, payment_method, order_published_at, buyer_lang, seller_lang,
     buyer_chat_cursor, seller_chat_cursor, rounds, handoff_reason, opened_at, updated_at";

pub fn get(conn: &Connection, session_id: &str) -> Result<Option<Session>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM sessions WHERE session_id = ?1"),
        [session_id],
        from_row,
    )
    .optional()?
    .transpose()
}

/// The live (non-terminal) session of a dispute, if any.
pub fn live_for_dispute(conn: &Connection, dispute_id: &str) -> Result<Option<Session>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM sessions
             WHERE dispute_id = ?1 AND state NOT IN ('closed', 'superseded')"
        ),
        [dispute_id],
        from_row,
    )
    .optional()?
    .transpose()
}

/// Every live session, oldest first; used to resume after a restart.
pub fn list_live(conn: &Connection) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM sessions
         WHERE state NOT IN ('closed', 'superseded') ORDER BY opened_at, session_id"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    rows.map(|row| row?).collect()
}

/// Records the language a party is addressed in.
pub fn set_language(
    conn: &Connection,
    session_id: &str,
    party: Party,
    lang: &str,
    now: i64,
) -> Result<bool> {
    let column = match party {
        Party::Buyer => "buyer_lang",
        Party::Seller => "seller_lang",
    };
    let updated = conn.execute(
        &format!("UPDATE sessions SET {column} = ?2, updated_at = ?3 WHERE session_id = ?1"),
        params![session_id, lang, now],
    )?;
    Ok(updated == 1)
}

/// Counts one question round (`docs/judgments.md` §4.1).
pub fn increment_rounds(conn: &Connection, session_id: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET rounds = rounds + 1, updated_at = ?2 WHERE session_id = ?1",
        params![session_id, now],
    )?;
    Ok(())
}

/// The dispute's most recent session, live or ended.
pub fn latest_for_dispute(conn: &Connection, dispute_id: &str) -> Result<Option<Session>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM sessions WHERE dispute_id = ?1
             ORDER BY opened_at DESC, rowid DESC LIMIT 1"
        ),
        [dispute_id],
        from_row,
    )
    .optional()?
    .transpose()
}

/// Whether the dispute ever had a session, live or ended. Mediation opens
/// at most once per dispute (`docs/spec.md` §7.1).
pub fn exists_for_dispute(conn: &Connection, dispute_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sessions WHERE dispute_id = ?1)",
        [dispute_id],
        |row| row.get(0),
    )?)
}

/// Claims a session for a handoff: moves it to `handed_off` and records
/// why, only while it is still opening, active or guiding. Returns `false`
/// if it ended or was already handed off, so each session is handed off
/// once.
pub fn hand_off(conn: &Connection, session_id: &str, reason: &str, now: i64) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE sessions SET state = 'handed_off', handoff_reason = ?2, updated_at = ?3
         WHERE session_id = ?1 AND state IN ('opening', 'active', 'guiding')",
        params![session_id, reason, now],
    )?;
    Ok(updated == 1)
}

/// Moves a live session to `state`. A terminal session is never changed.
/// Returns `true` if the session was updated.
pub fn set_state(
    conn: &Connection,
    session_id: &str,
    state: SessionState,
    now: i64,
) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE sessions SET state = ?2, updated_at = ?3
         WHERE session_id = ?1 AND state NOT IN ('closed', 'superseded')",
        params![session_id, state.to_string(), now],
    )?;
    Ok(updated == 1)
}

/// Moves an active session to `guiding`. Returns false when it is no longer
/// active, so guidance is sent once and never after the session moved on.
pub fn start_guiding(conn: &Connection, session_id: &str, now: i64) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE sessions SET state = 'guiding', updated_at = ?2
         WHERE session_id = ?1 AND state = 'active'",
        params![session_id, now],
    )?;
    Ok(updated == 1)
}

/// Advances a party's chat cursor. It never moves backwards; clamping to the
/// local clock is the caller's job (`docs/spec.md` §5.1).
pub fn advance_chat_cursor(
    conn: &Connection,
    session_id: &str,
    party: Party,
    cursor: i64,
    now: i64,
) -> Result<()> {
    let column = match party {
        Party::Buyer => "buyer_chat_cursor",
        Party::Seller => "seller_chat_cursor",
    };
    conn.execute(
        &format!(
            "UPDATE sessions SET {column} = MAX(COALESCE({column}, 0), ?2), updated_at = ?3
             WHERE session_id = ?1"
        ),
        params![session_id, cursor, now],
    )?;
    Ok(())
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Result<Session>> {
    let state: String = row.get(2)?;
    let state = match state.parse() {
        Ok(state) => state,
        Err(e) => return Ok(Err(e)),
    };
    Ok(Ok(Session {
        session_id: row.get(0)?,
        dispute_id: row.get(1)?,
        state,
        buyer_trade_pubkey: row.get(3)?,
        seller_trade_pubkey: row.get(4)?,
        fiat_amount: row.get(5)?,
        fiat_code: row.get(6)?,
        payment_method: row.get(7)?,
        order_published_at: row.get(8)?,
        buyer_lang: row.get(9)?,
        seller_lang: row.get(10)?,
        buyer_chat_cursor: row.get(11)?,
        seller_chat_cursor: row.get(12)?,
        rounds: row.get(13)?,
        handoff_reason: row.get(14)?,
        opened_at: row.get(15)?,
        updated_at: row.get(16)?,
    }))
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Opening => "opening",
            Self::Active => "active",
            Self::Guiding => "guiding",
            Self::HandedOff => "handed_off",
            Self::Closed => "closed",
            Self::Superseded => "superseded",
        })
    }
}

impl FromStr for SessionState {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "opening" => Ok(Self::Opening),
            "active" => Ok(Self::Active),
            "guiding" => Ok(Self::Guiding),
            "handed_off" => Ok(Self::HandedOff),
            "closed" => Ok(Self::Closed),
            "superseded" => Ok(Self::Superseded),
            other => Err(Error::Schema(format!("unknown session state {other:?}"))),
        }
    }
}

impl fmt::Display for Party {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Buyer => "buyer",
            Self::Seller => "seller",
        })
    }
}

impl FromStr for Party {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "buyer" => Ok(Self::Buyer),
            "seller" => Ok(Self::Seller),
            other => Err(Error::Schema(format!("unknown party {other:?}"))),
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::store::Store;
    use crate::store::disputes::{self, Initiator, NewDispute};

    /// A store with dispute `d1` and a session `s1` for it.
    pub fn store_with_session() -> Store {
        let store = Store::open_in_memory().unwrap();
        disputes::insert_if_new(
            store.conn(),
            &NewDispute {
                dispute_id: "d1",
                initiator: Initiator::Seller,
                status: "initiated",
                status_at: 100,
                now: 100,
            },
        )
        .unwrap();
        assert!(insert(store.conn(), &new_session("s1", "d1")).unwrap());
        store
    }

    pub fn new_session<'a>(session_id: &'a str, dispute_id: &'a str) -> NewSession<'a> {
        NewSession {
            session_id,
            dispute_id,
            buyer_trade_pubkey: "buyer-trade",
            seller_trade_pubkey: "seller-trade",
            fiat_amount: Some("50000"),
            fiat_code: Some("ARS"),
            payment_method: Some("Mercado Pago"),
            order_published_at: Some(90),
            now: 200,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{new_session, store_with_session};
    use super::*;

    #[test]
    fn a_partys_language_is_stored_separately() {
        let store = store_with_session();

        assert!(set_language(store.conn(), "s1", Party::Seller, "es", 300).unwrap());

        let session = get(store.conn(), "s1").unwrap().unwrap();
        assert_eq!(session.seller_lang.as_deref(), Some("es"));
        assert_eq!(session.buyer_lang, None);
        assert_eq!(session.language(Party::Seller, "en"), "es");
        assert_eq!(
            session.language(Party::Buyer, "en"),
            "en",
            "the default until detected"
        );
    }

    #[test]
    fn rounds_count_up() {
        let store = store_with_session();

        increment_rounds(store.conn(), "s1", 300).unwrap();
        increment_rounds(store.conn(), "s1", 301).unwrap();

        assert_eq!(get(store.conn(), "s1").unwrap().unwrap().rounds, 2);
    }

    #[test]
    fn the_latest_session_is_found_even_after_it_ended() {
        let store = store_with_session();
        set_state(store.conn(), "s1", SessionState::Closed, 300).unwrap();

        let latest = latest_for_dispute(store.conn(), "d1").unwrap().unwrap();

        assert_eq!(latest.session_id, "s1");
        assert_eq!(latest.state, SessionState::Closed);
        assert!(latest_for_dispute(store.conn(), "d2").unwrap().is_none());
    }

    #[test]
    fn any_session_counts_for_a_dispute_even_after_it_ended() {
        let store = store_with_session();

        assert!(exists_for_dispute(store.conn(), "d1").unwrap());
        set_state(store.conn(), "s1", SessionState::Closed, 300).unwrap();
        assert!(exists_for_dispute(store.conn(), "d1").unwrap());
        assert!(!exists_for_dispute(store.conn(), "d2").unwrap());
    }

    #[test]
    fn a_handoff_records_its_reason() {
        let store = store_with_session();

        assert!(hand_off(store.conn(), "s1", "opening_failed", 300).unwrap());

        let session = get(store.conn(), "s1").unwrap().unwrap();
        assert_eq!(session.state, SessionState::HandedOff);
        assert_eq!(session.handoff_reason.as_deref(), Some("opening_failed"));
        assert_eq!(session.updated_at, 300);
    }

    #[test]
    fn a_session_is_handed_off_only_once() {
        let store = store_with_session();

        assert!(hand_off(store.conn(), "s1", "uncertain", 300).unwrap());
        assert!(!hand_off(store.conn(), "s1", "unresponsive", 400).unwrap());

        let session = get(store.conn(), "s1").unwrap().unwrap();
        assert_eq!(
            session.handoff_reason.as_deref(),
            Some("uncertain"),
            "the first claim stands"
        );
    }

    #[test]
    fn only_an_active_session_starts_guiding() {
        let store = store_with_session();
        assert!(!start_guiding(store.conn(), "s1", 200).unwrap(), "opening");
        set_state(store.conn(), "s1", SessionState::Active, 250).unwrap();

        assert!(start_guiding(store.conn(), "s1", 300).unwrap());
        assert!(!start_guiding(store.conn(), "s1", 400).unwrap(), "once");
        assert_eq!(
            get(store.conn(), "s1").unwrap().unwrap().state,
            SessionState::Guiding
        );
    }

    #[test]
    fn an_ended_session_is_never_handed_off() {
        let store = store_with_session();
        set_state(store.conn(), "s1", SessionState::Superseded, 300).unwrap();

        assert!(!hand_off(store.conn(), "s1", "uncertain", 400).unwrap());
        assert_eq!(
            get(store.conn(), "s1").unwrap().unwrap().state,
            SessionState::Superseded
        );
    }

    #[test]
    fn inserted_session_starts_opening_with_its_facts() {
        let store = store_with_session();

        let session = get(store.conn(), "s1").unwrap().unwrap();

        assert_eq!(session.state, SessionState::Opening);
        assert_eq!(session.trade_pubkey(Party::Seller), "seller-trade");
        assert_eq!(session.fiat_code.as_deref(), Some("ARS"));
        assert_eq!(session.payment_method.as_deref(), Some("Mercado Pago"));
        assert_eq!(session.order_published_at, Some(90));
        assert_eq!(session.rounds, 0);
    }

    #[test]
    fn second_live_session_for_the_same_dispute_is_rejected() {
        let store = store_with_session();

        let inserted = insert(store.conn(), &new_session("s2", "d1")).unwrap();

        assert!(!inserted);
        assert!(get(store.conn(), "s2").unwrap().is_none());
    }

    #[test]
    fn a_new_session_is_allowed_once_the_previous_one_ended() {
        let store = store_with_session();
        set_state(store.conn(), "s1", SessionState::Superseded, 300).unwrap();

        let inserted = insert(store.conn(), &new_session("s2", "d1")).unwrap();

        assert!(inserted);
        assert_eq!(
            live_for_dispute(store.conn(), "d1")
                .unwrap()
                .unwrap()
                .session_id,
            "s2"
        );
    }

    #[test]
    fn session_for_an_unknown_dispute_is_rejected() {
        let store = store_with_session();

        assert!(insert(store.conn(), &new_session("s2", "no-such-dispute")).is_err());
    }

    #[test]
    fn terminal_sessions_never_change_state() {
        let store = store_with_session();
        set_state(store.conn(), "s1", SessionState::Closed, 300).unwrap();

        let updated = set_state(store.conn(), "s1", SessionState::Active, 400).unwrap();

        assert!(!updated);
        assert_eq!(
            get(store.conn(), "s1").unwrap().unwrap().state,
            SessionState::Closed
        );
    }

    #[test]
    fn list_live_skips_terminal_sessions() {
        let store = store_with_session();
        set_state(store.conn(), "s1", SessionState::Active, 300).unwrap();

        assert_eq!(list_live(store.conn()).unwrap().len(), 1);
        set_state(store.conn(), "s1", SessionState::Closed, 400).unwrap();
        assert!(list_live(store.conn()).unwrap().is_empty());
    }

    #[test]
    fn chat_cursor_never_moves_backwards() {
        let store = store_with_session();

        advance_chat_cursor(store.conn(), "s1", Party::Buyer, 500, 600).unwrap();
        advance_chat_cursor(store.conn(), "s1", Party::Buyer, 450, 601).unwrap();

        let session = get(store.conn(), "s1").unwrap().unwrap();
        assert_eq!(session.chat_cursor(Party::Buyer), Some(500));
        assert_eq!(session.chat_cursor(Party::Seller), None);
    }

    #[test]
    fn enums_round_trip_through_text() {
        for state in [
            SessionState::Opening,
            SessionState::Active,
            SessionState::Guiding,
            SessionState::HandedOff,
            SessionState::Closed,
            SessionState::Superseded,
        ] {
            assert_eq!(state.to_string().parse::<SessionState>().unwrap(), state);
        }
        for party in [Party::Buyer, Party::Seller] {
            assert_eq!(party.to_string().parse::<Party>().unwrap(), party);
        }
    }
}
