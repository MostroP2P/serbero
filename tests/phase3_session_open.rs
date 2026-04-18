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

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use mostro_core::dispute::SolverDisputeInfo;
use mostro_core::message::{Action, Message, Payload};

use serbero::chat::inbound::unwrap_with_shared_key;
use serbero::chat::shared_key::derive_shared_keys;
use serbero::db;
use serbero::mediation;
use serbero::models::dispute::InitiatorRole;
use serbero::models::mediation::ClassificationLabel;
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, RationaleText, ReasoningError, SuggestedAction,
    SummaryRequest, SummaryResponse,
};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

use nostr_relay_builder::MockRelay;

// ---------------------------------------------------------------------------
// MockReasoningProvider
// ---------------------------------------------------------------------------

struct MockReasoningProvider {
    clarification: String,
}

#[async_trait]
impl ReasoningProvider for MockReasoningProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        Ok(ClassificationResponse {
            classification: ClassificationLabel::CoordinationFailureResolvable,
            confidence: 0.9,
            suggested_action: SuggestedAction::AskClarification(self.clarification.clone()),
            rationale: RationaleText("both parties seem cooperative".into()),
            flags: Vec::new(),
        })
    }

    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        // Out of US1 scope — summary is US3.
        Err(ReasoningError::Unreachable(
            "summary not expected in US1 test".into(),
        ))
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MostroChatSim
// ---------------------------------------------------------------------------

/// Plays Mostro: listens for Serbero's `AdminTakeDispute` DM and
/// replies with a canned `AdminTookDispute` carrying the test's
/// chosen buyer/seller trade pubkeys.
struct MostroChatSim {
    keys: Keys,
    // The relay-pool client has its own lifecycle; we only keep the
    // handle alive so the task below stays connected.
    _client: Client,
    _task: tokio::task::JoinHandle<()>,
}

impl MostroChatSim {
    async fn start(relay_url: &str, buyer_trade_pk: PublicKey, seller_trade_pk: PublicKey) -> Self {
        let keys = Keys::generate();
        let client = Client::new(keys.clone());
        client.add_relay(relay_url).await.unwrap();
        client.connect().await;
        // Wait for the MockRelay handshake before subscribing, so
        // our REQ is guaranteed to be registered before Serbero
        // publishes the AdminTakeDispute DM. Replaces the earlier
        // fixed `sleep(200ms)` in the test body, which was racy.
        client.wait_for_connection(Duration::from_secs(5)).await;

        // Subscribe to gift-wraps addressed to us. The `since`
        // window must be wide enough to cover NIP-59's random
        // timestamp tweak (up to 2 days in the past); otherwise
        // the relay will drop incoming events at the REQ filter.
        let seven_days_ago =
            Timestamp::from_secs(Timestamp::now().as_secs().saturating_sub(7 * 24 * 60 * 60));
        let filter = Filter::new()
            .kind(Kind::GiftWrap)
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::P),
                keys.public_key().to_hex(),
            )
            .since(seven_days_ago);
        client.subscribe(filter, None).await.unwrap();

        let client_loop = client.clone();
        let client_for_inner = client.clone();
        let task = tokio::spawn(async move {
            let _ = client_loop
                .handle_notifications(move |notif| {
                    let client = client_for_inner.clone();
                    let buyer = buyer_trade_pk;
                    let seller = seller_trade_pk;
                    async move {
                        let RelayPoolNotification::Event { event, .. } = notif else {
                            return Ok(false);
                        };
                        if event.kind != Kind::GiftWrap {
                            return Ok(false);
                        }
                        let Ok(unwrapped) = client.unwrap_gift_wrap(&event).await else {
                            return Ok(false);
                        };
                        let Ok(msg) = Message::from_json(&unwrapped.rumor.content) else {
                            return Ok(false);
                        };
                        let kind = msg.get_inner_message_kind();
                        if kind.action != Action::AdminTakeDispute {
                            return Ok(false);
                        }
                        let Some(dispute_id) = kind.id else {
                            return Ok(false);
                        };
                        // Build the SolverDisputeInfo reply.
                        let info = SolverDisputeInfo {
                            id: dispute_id,
                            kind: "buy".into(),
                            status: "in-progress".into(),
                            hash: None,
                            preimage: None,
                            order_previous_status: "active".into(),
                            initiator_pubkey: buyer.to_hex(),
                            buyer_pubkey: Some(buyer.to_hex()),
                            seller_pubkey: Some(seller.to_hex()),
                            initiator_full_privacy: false,
                            counterpart_full_privacy: false,
                            initiator_info: None,
                            counterpart_info: None,
                            premium: 0,
                            payment_method: "".into(),
                            amount: 0,
                            fiat_amount: 0,
                            fee: 0,
                            routing_fee: 0,
                            buyer_invoice: None,
                            invoice_held_at: 0,
                            taken_at: 0,
                            created_at: 0,
                        };
                        let reply = Message::new_dispute(
                            Some(dispute_id),
                            None,
                            None,
                            Action::AdminTookDispute,
                            Some(Payload::Dispute(dispute_id, Some(info))),
                        );
                        let json = reply.as_json().unwrap();
                        let _ = client.send_private_msg(unwrapped.sender, json, []).await;
                        Ok(false)
                    }
                })
                .await;
        });

        Self {
            keys,
            _client: client,
            _task: task,
        }
    }

    fn pubkey(&self) -> PublicKey {
        self.keys.public_key()
    }
}

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
    )
    .await
    .expect("open_session must succeed in the happy-path fixture");

    let session_id = match outcome {
        mediation::session::OpenOutcome::Opened { session_id } => session_id,
        other => panic!("expected Opened, got {other:?}"),
    };

    // ---- Assertions ---------------------------------------------------

    // (a) The session row exists at awaiting_response with the
    //     pinned bundle + both shared pubkeys set.
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
    assert!(bsp.is_some() && ssp.is_some());

    // (b) Exactly two outbound mediation_messages rows, addressed
    //     to the computed per-party shared pubkeys.
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();
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
                Ok((content, _ts, inner_sender)) => {
                    if content.contains(base) {
                        assert_eq!(
                            inner_sender,
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
