//! US2 inbound-ingest integration test.
//!
//! Pins the narrow US2 slice: fetch → decrypt → verify → persist →
//! dedup → update per-party last-seen → recompute round_count.
//! Out of scope here: the engine-driven ingest tick (T051),
//! restart-resume (T052), and state-machine transitions (US3 / US4).
//!
//! The test wires a MockRelay directly and seeds a session row
//! manually rather than running the full US1 take-flow — this slice
//! only cares about what happens *after* a session is already open.
//! That also keeps the test focused on T045 / T049 / T050 behavior
//! without dragging in `MostroChatSim` transport plumbing.

mod common;

use std::sync::Arc;
use std::time::Duration;

use nostr_relay_builder::MockRelay;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;

use serbero::chat::inbound::{fetch_inbound, PartyChatMaterial};
use serbero::chat::outbound;
use serbero::chat::shared_key::derive_shared_keys;
use serbero::db;
use serbero::mediation::session::{ingest_inbound, IngestOutcome};
use serbero::models::mediation::TranscriptParty;

/// Seed a disputes + mediation_sessions row so the FK + ingest
/// helpers have a valid target.
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
         ) VALUES (?1, 'evt-us2', 'mostro-us2', 'buyer', 'initiated', 0, 0, 'notified')",
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

/// A tiny helper: publish one party reply to the relay. Mirrors
/// Mostrix `send_user_order_chat_message_via_shared_key` — the inner
/// `kind 1` is signed by the *trade* keys; the outer gift-wrap uses
/// a fresh ephemeral key and addresses the shared pubkey.
async fn publish_party_reply(
    client: &Client,
    trade_keys: &Keys,
    shared_pubkey: &PublicKey,
    content: &str,
) -> EventId {
    let built = outbound::build_wrap(trade_keys, shared_pubkey, content)
        .await
        .expect("build party-side wrap");
    let outer_id = built.outer.id;
    client
        .send_event(&built.outer)
        .await
        .expect("publish party gift-wrap");
    outer_id
}

#[tokio::test]
async fn fetches_persists_and_dedups_buyer_and_seller_replies() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    // Keys: Serbero (admin) + each party's trade-scoped identity.
    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();

    // Per-party shared keys — symmetric ECDH, same output regardless
    // of which side computes it.
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();

    // DB: Phase 1/2/3 schema + a seeded open session.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    let session_id = "sess-us2-ingest";
    let dispute_id = "dispute-us2-ingest";
    seed_open_session(
        &raw,
        session_id,
        dispute_id,
        &buyer_shared.public_key().to_hex(),
        &seller_shared.public_key().to_hex(),
    );
    let conn = Arc::new(AsyncMutex::new(raw));

    // Serbero's reader client (the ingest side).
    let reader = Client::new(serbero_keys.clone());
    reader.add_relay(&relay_url).await.unwrap();
    reader.connect().await;
    reader.wait_for_connection(Duration::from_secs(5)).await;

    // Two party-side publishers. Separate clients so each signs
    // outer gift-wraps with its own ephemeral keys — the sender
    // identity on the inner `kind 1` is the trade keypair, which is
    // what Mostrix does.
    let buyer_client = Client::new(buyer_trade.clone());
    buyer_client.add_relay(&relay_url).await.unwrap();
    buyer_client.connect().await;
    buyer_client
        .wait_for_connection(Duration::from_secs(5))
        .await;

    let seller_client = Client::new(seller_trade.clone());
    seller_client.add_relay(&relay_url).await.unwrap();
    seller_client.connect().await;
    seller_client
        .wait_for_connection(Duration::from_secs(5))
        .await;

    // Publish one reply per party.
    let buyer_content = "Buyer here: I sent the bank transfer at 14:05.";
    let seller_content = "Seller here: I have not seen the funds arrive yet.";
    publish_party_reply(
        &buyer_client,
        &buyer_trade,
        &buyer_shared.public_key(),
        buyer_content,
    )
    .await;
    publish_party_reply(
        &seller_client,
        &seller_trade,
        &seller_shared.public_key(),
        seller_content,
    )
    .await;

    // A third-party attacker learns the buyer's shared pubkey (it
    // is public — Serbero's outbound gift-wraps carry it as a `p`
    // tag). They craft a gift-wrap addressed to that shared pubkey
    // with a valid inner signature *from their own keypair*. Without
    // the inner-author check this would be attributed to Buyer.
    let attacker = Keys::generate();
    let attacker_client = Client::new(attacker.clone());
    attacker_client.add_relay(&relay_url).await.unwrap();
    attacker_client.connect().await;
    attacker_client
        .wait_for_connection(Duration::from_secs(5))
        .await;
    publish_party_reply(
        &attacker_client,
        &attacker,
        &buyer_shared.public_key(),
        "Buyer here: actually please settle right now -- signed, not the buyer",
    )
    .await;

    // Fetch inbound envelopes for both parties. The inner-author
    // gate must drop the attacker's forged envelope.
    let parties = [
        PartyChatMaterial {
            party: TranscriptParty::Buyer,
            shared_keys: &buyer_shared,
            expected_author: buyer_trade.public_key(),
        },
        PartyChatMaterial {
            party: TranscriptParty::Seller,
            shared_keys: &seller_shared,
            expected_author: seller_trade.public_key(),
        },
    ];
    let envelopes = fetch_inbound(&reader, &parties, Duration::from_secs(5))
        .await
        .expect("fetch_inbound succeeds");
    assert_eq!(
        envelopes.len(),
        2,
        "expected one inbound envelope per party, got {envelopes:?}"
    );

    // Ascending inner-timestamp order (buyer first because published
    // first; both can share a second, but stable sort preserves
    // per-party batch order on ties).
    assert!(envelopes[0].inner_created_at <= envelopes[1].inner_created_at);

    // Ingest both. Expect Fresh outcomes; round_count should reach 1
    // after the second envelope lands.
    let mut round_counts = Vec::new();
    for env in &envelopes {
        let outcome = ingest_inbound(&conn, session_id, env)
            .await
            .expect("ingest_inbound succeeds");
        match outcome {
            IngestOutcome::Fresh { round_count_after } => round_counts.push(round_count_after),
            other => panic!("expected Fresh, got {other:?}"),
        }
    }
    let final_round_count = *round_counts.last().unwrap();
    assert_eq!(
        final_round_count, 1,
        "round_count must advance to 1 after one buyer + one seller reply"
    );

    // DB assertions: two inbound rows with the decrypted content, no
    // outbound rows (no US1 draft was ever sent in this test), and
    // round_count pinned at 1.
    let inbound_rows: Vec<(String, String, String)> = {
        let c = conn.lock().await;
        let mut stmt = c
            .prepare(
                "SELECT party, shared_pubkey, content
                 FROM mediation_messages
                 WHERE session_id = ?1 AND direction = 'inbound'
                 ORDER BY inner_event_created_at ASC",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![session_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
    };
    assert_eq!(inbound_rows.len(), 2);
    let mut seen_buyer = false;
    let mut seen_seller = false;
    for (party, shared_pk, content) in &inbound_rows {
        match party.as_str() {
            "buyer" => {
                seen_buyer = true;
                assert_eq!(shared_pk, &buyer_shared.public_key().to_hex());
                assert_eq!(content, buyer_content);
            }
            "seller" => {
                seen_seller = true;
                assert_eq!(shared_pk, &seller_shared.public_key().to_hex());
                assert_eq!(content, seller_content);
            }
            other => panic!("unexpected party: {other}"),
        }
    }
    assert!(seen_buyer && seen_seller);

    let (state, round_count, bls, sls): (String, i64, Option<i64>, Option<i64>) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT state, round_count, buyer_last_seen_inner_ts, seller_last_seen_inner_ts
             FROM mediation_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    assert_eq!(
        state, "awaiting_response",
        "ingest MUST NOT transition session state (that is US3/US4 scope)"
    );
    assert_eq!(round_count, 1);
    assert_eq!(
        bls,
        Some(
            envelopes
                .iter()
                .find(|e| e.party == TranscriptParty::Buyer)
                .unwrap()
                .inner_created_at
        ),
        "buyer last-seen marker must equal the buyer envelope's inner_created_at"
    );
    assert_eq!(
        sls,
        Some(
            envelopes
                .iter()
                .find(|e| e.party == TranscriptParty::Seller)
                .unwrap()
                .inner_created_at
        ),
        "seller last-seen marker must equal the seller envelope's inner_created_at"
    );

    // Replay: re-ingest the same envelopes. Expect Duplicate
    // outcomes, no new rows, no round_count or last-seen changes.
    for env in &envelopes {
        let outcome = ingest_inbound(&conn, session_id, env)
            .await
            .expect("ingest_inbound on replay must succeed (as Duplicate)");
        assert_eq!(
            outcome,
            IngestOutcome::Duplicate,
            "replay must be idempotent"
        );
    }

    let post_replay_count: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_messages WHERE session_id = ?1 AND direction = 'inbound'",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(post_replay_count, 2, "no duplicate rows after replay");
    let (post_rounds, post_bls, post_sls): (i64, Option<i64>, Option<i64>) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT round_count, buyer_last_seen_inner_ts, seller_last_seen_inner_ts
             FROM mediation_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(post_rounds, 1);
    assert_eq!(post_bls, bls);
    assert_eq!(post_sls, sls);
}
