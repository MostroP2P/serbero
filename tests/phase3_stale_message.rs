//! T047 — US2 stale-message ingest.
//!
//! Pins the behavior when an inbound envelope's inner
//! `created_at` predates the party's current last-seen marker:
//!
//! - The row IS persisted (we keep forensic evidence of what was
//!   seen on the relay) but with `stale = 1`.
//! - The per-party last-seen marker is NOT regressed.
//! - `round_count` excludes the stale row (the SQL filter in
//!   `recompute_round_count` checks `stale = 0`).
//!
//! Uses the same direct-seed pattern as T045 / T046: no take-flow,
//! no relay; envelopes are built off `outbound::build_wrap` with a
//! custom `inner_created_at` substituted in so the stale timestamp
//! ordering is deterministic and independent of wall-clock drift.

mod common;

use std::sync::Arc;

use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;

use serbero::chat::inbound::InboundEnvelope;
use serbero::chat::outbound;
use serbero::chat::shared_key::derive_shared_keys;
use serbero::db;
use serbero::mediation::session::{ingest_inbound, IngestOutcome};
use serbero::models::mediation::TranscriptParty;

fn seed_open_session(
    conn: &rusqlite::Connection,
    session_id: &str,
    dispute_id: &str,
    buyer_shared_pk: &str,
    seller_shared_pk: &str,
) {
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state
         ) VALUES (?1, 'evt-us2-stale', 'mostro-us2', 'buyer',
                   'initiated', 0, 0, 'notified')",
        rusqlite::params![dispute_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mediation_sessions (
            session_id, dispute_id, state, round_count,
            prompt_bundle_id, policy_hash,
            buyer_shared_pubkey, seller_shared_pubkey,
            started_at, last_transition_at
         ) VALUES (?1, ?2, 'awaiting_response', 0,
                   'phase3-us2', 'pol-hash-us2',
                   ?3, ?4, 100, 100)",
        rusqlite::params![session_id, dispute_id, buyer_shared_pk, seller_shared_pk],
    )
    .unwrap();
}

/// Build a buyer envelope with a **forced** `inner_created_at`.
/// Two concerns drive this helper:
///
/// - We want deterministic timestamp ordering for the stale check —
///   relying on wall-clock between calls would be flaky.
/// - `outbound::build_wrap` drives `inner_created_at` off
///   `Timestamp::now()` under the hood, so to pin a specific value
///   we override the field on the envelope after construction. The
///   `inner_event_id` still comes from the real event so the DB's
///   unique index sees a real, distinct id per envelope.
async fn build_buyer_envelope_at(
    buyer_trade: &Keys,
    buyer_shared_pubkey: &PublicKey,
    content: &str,
    forced_inner_ts: i64,
) -> InboundEnvelope {
    let built = outbound::build_wrap(buyer_trade, buyer_shared_pubkey, content)
        .await
        .expect("build buyer wrap");
    InboundEnvelope {
        party: TranscriptParty::Buyer,
        shared_pubkey: buyer_shared_pubkey.to_hex(),
        inner_event_id: built.inner_event_id.to_hex(),
        inner_created_at: forced_inner_ts,
        outer_event_id: built.outer.id.to_hex(),
        content: content.to_string(),
        inner_sender: buyer_trade.public_key().to_hex(),
    }
}

#[tokio::test]
async fn stale_inbound_is_persisted_but_does_not_advance_session() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();

    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();

    let session_id = "sess-stale-1";
    let dispute_id = "dispute-stale-1";
    seed_open_session(
        &raw,
        session_id,
        dispute_id,
        &buyer_shared.public_key().to_hex(),
        &seller_shared.public_key().to_hex(),
    );
    let conn = Arc::new(AsyncMutex::new(raw));

    // (1) Fresh reply at ts=1000 advances buyer_last_seen to 1000.
    let fresh = build_buyer_envelope_at(
        &buyer_trade,
        &buyer_shared.public_key(),
        "Fresh buyer reply at t=1000",
        1000,
    )
    .await;
    match ingest_inbound(&conn, session_id, &fresh).await.unwrap() {
        IngestOutcome::Fresh { .. } => {}
        other => panic!("expected Fresh for the first reply, got {other:?}"),
    }

    // (2) Stale reply at ts=500 — distinct inner_event_id (different
    //     content → different inner hash), so the DB will NOT dedup
    //     it. Ingest MUST classify it as Stale.
    let stale = build_buyer_envelope_at(
        &buyer_trade,
        &buyer_shared.public_key(),
        "Stale buyer reply at t=500 — reordered by relay replay",
        500,
    )
    .await;
    match ingest_inbound(&conn, session_id, &stale).await.unwrap() {
        IngestOutcome::Stale => {}
        other => panic!("expected Stale for the back-dated reply, got {other:?}"),
    }

    // Assertions.
    let (total_rows, stale_rows, fresh_rows): (i64, i64, i64) = {
        let c = conn.lock().await;
        let t = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_messages WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let s = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_messages WHERE session_id = ?1 AND stale = 1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let f = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_messages WHERE session_id = ?1 AND stale = 0",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        (t, s, f)
    };
    assert_eq!(
        total_rows, 2,
        "both rows must be persisted — stale messages keep forensic evidence"
    );
    assert_eq!(stale_rows, 1, "exactly one row must carry stale = 1");
    assert_eq!(
        fresh_rows, 1,
        "the original fresh row must remain stale = 0"
    );

    // last-seen did not regress to 500.
    let (buyer_last_seen, round_count): (Option<i64>, i64) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT buyer_last_seen_inner_ts, round_count
             FROM mediation_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(
        buyer_last_seen,
        Some(1000),
        "last-seen marker must not regress on a stale message"
    );
    assert_eq!(
        round_count, 0,
        "stale rows must be excluded from round_count (recompute filters stale = 0)"
    );
}
