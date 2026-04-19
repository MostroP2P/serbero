//! T046 — US2 inbound dedup, both within a single daemon run and
//! across a simulated restart.
//!
//! The first test confirms that calling `ingest_inbound` twice with
//! the same envelope is a no-op: the unique index on
//! `(session_id, inner_event_id)` keeps the row count at 1 and
//! `round_count` does not advance.
//!
//! The second test pins that invariant across a daemon restart:
//! open a named temp-file SQLite DB, ingest once, drop the
//! connection (simulating shutdown), re-open the same path, ingest
//! the same envelope again, assert the DB state is unchanged.
//!
//! Both tests use the direct-seed pattern from T045: no take-flow,
//! no relay. We build the envelope directly off `outbound::build_wrap`
//! so its `inner_event_id` + `outer_event_id` + `inner_sender` all
//! match what `fetch_inbound` would produce if this event were ever
//! observed on a real relay — the structural invariants the DB
//! dedup relies on are therefore identical.

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
         ) VALUES (?1, 'evt-us2-dedup', 'mostro-us2', 'buyer',
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

/// Build a synthetic [`InboundEnvelope`] for a buyer reply. Mirrors
/// the shape `fetch_inbound` produces after unwrapping a real
/// relay event: `inner_event_id` / `outer_event_id` / `inner_sender`
/// come from a freshly-built outbound wrap, so the
/// `(session_id, inner_event_id)` dedup key is the exact same id
/// the relay would emit.
async fn build_buyer_envelope(
    buyer_trade: &Keys,
    buyer_shared_pubkey: &PublicKey,
    content: &str,
) -> InboundEnvelope {
    let built = outbound::build_wrap(buyer_trade, buyer_shared_pubkey, content)
        .await
        .expect("build buyer wrap");
    InboundEnvelope {
        party: TranscriptParty::Buyer,
        shared_pubkey: buyer_shared_pubkey.to_hex(),
        inner_event_id: built.inner_event_id.to_hex(),
        inner_created_at: built.inner_created_at,
        outer_event_id: built.outer.id.to_hex(),
        content: content.to_string(),
        inner_sender: buyer_trade.public_key().to_hex(),
    }
}

#[tokio::test]
async fn ingest_inbound_is_idempotent_within_a_single_run() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();

    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();

    let session_id = "sess-dedup-1";
    let dispute_id = "dispute-dedup-1";
    seed_open_session(
        &raw,
        session_id,
        dispute_id,
        &buyer_shared.public_key().to_hex(),
        &seller_shared.public_key().to_hex(),
    );
    let conn = Arc::new(AsyncMutex::new(raw));

    let env = build_buyer_envelope(
        &buyer_trade,
        &buyer_shared.public_key(),
        "I sent the fiat at 14:05 — reference 12345",
    )
    .await;

    // First ingest: Fresh, round_count stays at 0 (only one party).
    match ingest_inbound(&conn, session_id, &env).await.unwrap() {
        IngestOutcome::Fresh { round_count_after } => {
            assert_eq!(
                round_count_after, 0,
                "a single buyer reply must not complete a round"
            );
        }
        other => panic!("expected Fresh on first ingest, got {other:?}"),
    }

    // Second ingest of the SAME envelope: Duplicate, no new row, no
    // round_count / last-seen drift.
    match ingest_inbound(&conn, session_id, &env).await.unwrap() {
        IngestOutcome::Duplicate => {}
        other => panic!("expected Duplicate on replay, got {other:?}"),
    }

    let (rows, round_count): (i64, i64) = {
        let c = conn.lock().await;
        let rows = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_messages WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let rc = c
            .query_row(
                "SELECT round_count FROM mediation_sessions WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        (rows, rc)
    };
    assert_eq!(rows, 1, "dedup must keep mediation_messages at 1 row");
    assert_eq!(round_count, 0, "round_count must not advance on replay");
}

#[tokio::test]
async fn ingest_inbound_dedups_across_daemon_restart() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();

    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();

    let session_id = "sess-dedup-restart";
    let dispute_id = "dispute-dedup-restart";

    // Build the envelope once; its inner_event_id is the stable
    // key the DB dedup relies on across restarts.
    let env = build_buyer_envelope(
        &buyer_trade,
        &buyer_shared.public_key(),
        "Replayed buyer reply across daemon restart",
    )
    .await;

    // ---- run 1: fresh DB, seed + ingest once ------------------------
    {
        let mut raw = db::open_connection(&db_path).unwrap();
        db::migrations::run_migrations(&mut raw).unwrap();
        seed_open_session(
            &raw,
            session_id,
            dispute_id,
            &buyer_shared.public_key().to_hex(),
            &seller_shared.public_key().to_hex(),
        );
        let conn = Arc::new(AsyncMutex::new(raw));
        match ingest_inbound(&conn, session_id, &env).await.unwrap() {
            IngestOutcome::Fresh { .. } => {}
            other => panic!("expected Fresh on first ingest, got {other:?}"),
        }
        // Connection (and its rusqlite Connection) drops at end of
        // block, simulating a clean daemon shutdown.
    }

    // Snapshot DB state for post-restart comparison.
    let (rows_before, rc_before, bls_before): (i64, i64, Option<i64>) = {
        let mut raw = db::open_connection(&db_path).unwrap();
        // Migrations are idempotent — the re-open in the same run
        // here just refreshes the connection for the snapshot.
        db::migrations::run_migrations(&mut raw).unwrap();
        let rows = raw
            .query_row(
                "SELECT COUNT(*) FROM mediation_messages WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let rc = raw
            .query_row(
                "SELECT round_count FROM mediation_sessions WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let bls = raw
            .query_row(
                "SELECT buyer_last_seen_inner_ts FROM mediation_sessions WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        (rows, rc, bls)
    };
    assert_eq!(rows_before, 1);
    assert_eq!(rc_before, 0);
    assert_eq!(bls_before, Some(env.inner_created_at));

    // ---- run 2: re-open same DB, replay same envelope --------------
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    let conn = Arc::new(AsyncMutex::new(raw));
    match ingest_inbound(&conn, session_id, &env).await.unwrap() {
        IngestOutcome::Duplicate => {}
        other => panic!("expected Duplicate after restart, got {other:?}"),
    }

    let (rows_after, rc_after, bls_after): (i64, i64, Option<i64>) = {
        let c = conn.lock().await;
        let rows = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_messages WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let rc = c
            .query_row(
                "SELECT round_count FROM mediation_sessions WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let bls = c
            .query_row(
                "SELECT buyer_last_seen_inner_ts FROM mediation_sessions WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        (rows, rc, bls)
    };
    assert_eq!(
        rows_after, 1,
        "dedup must survive a daemon restart — still exactly one row"
    );
    assert_eq!(rc_after, rc_before, "round_count must not drift on restart");
    assert_eq!(
        bls_after, bls_before,
        "buyer_last_seen_inner_ts must not drift on restart"
    );
}
