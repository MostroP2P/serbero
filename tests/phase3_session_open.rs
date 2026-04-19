//! US1 integration test — Serbero opens a mediation session.
//!
//! Exercises the full slice against a nostr-relay-builder MockRelay:
//!
//! - A `MostroChatSim` plays the Mostro side: it decrypts Serbero's
//!   `AdminTakeDispute` gift-wrapped DM, verifies the payload, and
//!   responds with a canned `AdminTookDispute` carrying a
//!   `SolverDisputeInfo` that names real trade-scoped pubkeys.
//! - A `MockReasoningProvider` returns a scripted
//!   `CoordinationFailureResolvable` classification whose
//!   `SuggestedAction::AskClarification` text drives the first
//!   outbound message.
//! - Two `PartyListener`s hold the buyer / seller trade keys; they
//!   derive the same per-party shared key as Serbero (ECDH
//!   symmetry) and fetch + decrypt the outbound gift-wraps from the
//!   relay to verify content + inner-event signer.
//!
//! Assertions mirror the US1 acceptance criteria:
//! - `mediation_sessions` row exists at `awaiting_response` with
//!   the pinned `prompt_bundle_id` + `policy_hash`.
//! - Exactly two outbound `mediation_messages` rows, addressed to
//!   the buyer and seller shared pubkeys.
//! - Each outbound inner event decrypts with the reconstructed
//!   shared keys and contains the clarifying-question text drawn
//!   from the reasoning provider.

mod common;

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use serbero::chat::inbound::unwrap_with_shared_key;
use serbero::chat::shared_key::derive_shared_keys;
use serbero::db;
use serbero::mediation;
use serbero::mediation::auth_retry::AuthRetryHandle;
use serbero::models::dispute::InitiatorRole;
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

use common::{MockReasoningProvider, MostroChatSim};
use nostr_relay_builder::MockRelay;

// ---------------------------------------------------------------------------
// Prompt bundle fixture
// ---------------------------------------------------------------------------

fn fixture_bundle() -> Arc<PromptBundle> {
    // The daemon's startup path would load these from the fixtures
    // directory; the integration test can use the same paths so
    // the policy_hash is stable and matches the production loader.
    let cfg = serbero::models::PromptsConfig {
        system_instructions_path: "./tests/fixtures/prompts/phase3-system.md".into(),
        classification_policy_path: "./tests/fixtures/prompts/phase3-classification.md".into(),
        escalation_policy_path: "./tests/fixtures/prompts/phase3-escalation-policy.md".into(),
        mediation_style_path: "./tests/fixtures/prompts/phase3-mediation-style.md".into(),
        message_templates_path: "./tests/fixtures/prompts/phase3-message-templates.md".into(),
    };
    Arc::new(prompts::load_bundle(&cfg).expect("fixture bundle must load"))
}

// ---------------------------------------------------------------------------
// The test itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn opens_session_and_dispatches_first_clarifying_message_to_both_parties() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    // Serbero + Mostro + party trade keys.
    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();

    // Start the fake Mostro.
    let mostro_sim = MostroChatSim::start(
        &relay_url,
        buyer_trade.public_key(),
        seller_trade.public_key(),
    )
    .await;

    // Serbero's Nostr client.
    let serbero_client = Client::new(serbero_keys.clone());
    serbero_client.add_relay(&relay_url).await.unwrap();
    serbero_client.connect().await;

    // Reasoning provider fixture.
    let reasoning: Arc<dyn ReasoningProvider> = Arc::new(MockReasoningProvider {
        clarification: "Please confirm the fiat payment timing for this trade.".into(),
    });

    // Prompt bundle fixture (same hash pinned into the session row).
    let bundle = fixture_bundle();

    // SQLite in a temp file with Phase 1 / 2 / 3 schema.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    // The session-open flow requires a parent `disputes` row (FK).
    // Phase 1/2 would have created this on event detection; here
    // we seed it manually because the test is focused on US1.
    let dispute_uuid = Uuid::new_v4();
    let dispute_id = dispute_uuid.to_string();
    raw.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state
         ) VALUES (?1, 'evt-1', ?2, 'buyer', 'initiated', 0, 0, 'notified')",
        rusqlite::params![dispute_id, mostro_sim.pubkey().to_hex()],
    )
    .unwrap();
    let conn = Arc::new(AsyncMutex::new(raw));

    // Also gate Serbero's own client on relay readiness so the REQ
    // for the AdminTookDispute response lands before the sim could
    // otherwise reply. `MostroChatSim::start` already awaited its
    // own `wait_for_connection`.
    serbero_client
        .wait_for_connection(Duration::from_secs(5))
        .await;

    // Call the session-open entry point.
    let auth_handle = AuthRetryHandle::new_authorized();
    let outcome = mediation::open_dispute_session(
        &conn,
        &serbero_client,
        &serbero_keys,
        &mostro_sim.pubkey(),
        reasoning.as_ref(),
        &bundle,
        &dispute_id,
        InitiatorRole::Buyer,
        dispute_uuid,
        "mock-provider",
        "mock-model",
        &auth_handle,
    )
    .await
    .expect("open_session must succeed in the happy-path fixture");

    let session_id = match outcome {
        mediation::session::OpenOutcome::Opened { session_id } => session_id,
        other => panic!("expected Opened, got {other:?}"),
    };

    // ---- Assertions ---------------------------------------------------

    // Independently re-derive the per-party shared keys up-front so
    // every subsequent DB / relay assertion compares against them,
    // not against a weaker "is set" check.
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();

    // (a) The session row exists at awaiting_response with the
    //     pinned bundle, and the persisted shared pubkeys match
    //     the independently-computed ECDH outputs.
    let (state, ph, bid, bsp, ssp): (String, String, String, Option<String>, Option<String>) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT state, policy_hash, prompt_bundle_id, buyer_shared_pubkey, seller_shared_pubkey
             FROM mediation_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
    };
    assert_eq!(state, "awaiting_response");
    assert_eq!(ph, bundle.policy_hash);
    assert_eq!(bid, bundle.id);
    assert_eq!(
        bsp.as_deref(),
        Some(buyer_shared.public_key().to_hex().as_str()),
        "session row's buyer_shared_pubkey must equal the ECDH-derived buyer shared pubkey"
    );
    assert_eq!(
        ssp.as_deref(),
        Some(seller_shared.public_key().to_hex().as_str()),
        "session row's seller_shared_pubkey must equal the ECDH-derived seller shared pubkey"
    );

    // (a.1) Exactly one session_opened audit event landed in the
    //       same transaction as the session row, carrying the
    //       pinned bundle provenance (T033 wiring of T037). The
    //       count assertion guards against a regression where the
    //       transaction retries or a later slice adds a second
    //       writer.
    let session_opened_count: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_events
             WHERE session_id = ?1 AND kind = 'session_opened'",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        session_opened_count, 1,
        "exactly one session_opened row expected per session open"
    );
    let (evt_kind, evt_bundle, evt_hash): (String, String, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT kind, prompt_bundle_id, policy_hash
             FROM mediation_events WHERE session_id = ?1 AND kind = 'session_opened'",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(evt_kind, "session_opened");
    assert_eq!(evt_bundle, bundle.id);
    assert_eq!(evt_hash, bundle.policy_hash);

    // (b) Exactly two outbound mediation_messages rows, addressed
    //     to the computed per-party shared pubkeys.
    let rows: Vec<(String, String, String)> = {
        let c = conn.lock().await;
        let mut stmt = c
            .prepare(
                "SELECT party, shared_pubkey, content
                 FROM mediation_messages WHERE session_id = ?1 AND direction = 'outbound'
                 ORDER BY party ASC",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![session_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
    };
    assert_eq!(rows.len(), 2, "expected one outbound row per party");
    let base = "Please confirm the fiat payment timing for this trade.";
    for (party, sp, content) in &rows {
        // Each row's content must contain the clarifying question and
        // identify the party. The exact prefix is a US1 implementation
        // detail; the assertion is behavioral: the base text is there
        // and the row's party label is honored.
        assert!(
            content.contains(base),
            "content missing base text: {content}"
        );
        match party.as_str() {
            "buyer" => {
                assert_eq!(sp, &buyer_shared.public_key().to_hex());
                assert!(content.to_lowercase().contains("buyer"));
            }
            "seller" => {
                assert_eq!(sp, &seller_shared.public_key().to_hex());
                assert!(content.to_lowercase().contains("seller"));
            }
            other => panic!("unexpected party {other}"),
        }
    }

    // (c) Each outbound gift-wrap on the relay decrypts with the
    //     shared key and carries the clarifying text. Fetches
    //     using the party's shared keypair prove that ECDH
    //     symmetry works end-to-end.
    let reader = Client::new(Keys::generate());
    reader.add_relay(&relay_url).await.unwrap();
    reader.connect().await;
    // Same readiness gate as MostroChatSim + the Serbero client,
    // so `fetch_events` below isn't racing the relay handshake on
    // slow CI.
    reader.wait_for_connection(Duration::from_secs(5)).await;

    for shared in [&buyer_shared, &seller_shared] {
        let filter = Filter::new()
            .kind(Kind::GiftWrap)
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::P),
                shared.public_key().to_hex(),
            )
            .limit(10);
        let events = reader
            .fetch_events(filter, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            !events.is_empty(),
            "no gift-wrap events addressed to shared pubkey {}",
            shared.public_key().to_hex()
        );
        let mut any_decrypted = false;
        let mut first_err: Option<String> = None;
        for ev in events.iter() {
            match unwrap_with_shared_key(shared, ev) {
                Ok(inner) => {
                    if inner.content.contains(base) {
                        assert_eq!(
                            inner.sender,
                            serbero_keys.public_key(),
                            "inner event must be signed by Serbero's keys"
                        );
                        any_decrypted = true;
                        break;
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e.to_string());
                    }
                }
            }
        }
        assert!(
            any_decrypted,
            "shared-key {} could not decrypt an event containing the clarifying text \
             (events tried: {}; first unwrap error: {})",
            shared.public_key().to_hex(),
            events.len(),
            first_err
                .as_deref()
                .unwrap_or("<no decrypt errors recorded>")
        );
    }
}
