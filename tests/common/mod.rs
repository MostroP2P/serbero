#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mostro_core::dispute::SolverDisputeInfo;
use mostro_core::message::{Action, Message, Payload};
use nostr_relay_builder::MockRelay;
use nostr_sdk::{
    Alphabet, Client, Event, EventBuilder, Filter, Keys, Kind, PublicKey, RelayPoolNotification,
    SingleLetterTag, Tag, TagKind, Timestamp,
};
use serbero::models::mediation::ClassificationLabel;
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, RationaleText, ReasoningError, SuggestedAction,
    SummaryRequest, SummaryResponse,
};
use serbero::models::{
    Config, MostroConfig, RelayConfig, SerberoConfig, SolverConfig, SolverPermission,
    TimeoutsConfig,
};
use serbero::reasoning::ReasoningProvider;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub const DISPUTE_EVENT_KIND: u16 = 38386;

pub struct TestHarness {
    pub relay: MockRelay,
    pub relay_url: String,
    pub serbero_keys: Keys,
    pub mostro_keys: Keys,
    pub _tmpdir: TempDir,
    pub db_path: String,
}

impl TestHarness {
    pub async fn new() -> Self {
        let relay = MockRelay::run().await.expect("start mock relay");
        let relay_url = relay.url().await.to_string();
        let serbero_keys = Keys::generate();
        let mostro_keys = Keys::generate();
        let tmpdir = tempfile::tempdir().unwrap();
        let db_path = tmpdir
            .path()
            .join("serbero.db")
            .to_string_lossy()
            .into_owned();
        Self {
            relay,
            relay_url,
            serbero_keys,
            mostro_keys,
            _tmpdir: tmpdir,
            db_path,
        }
    }

    pub fn config(&self, solvers: Vec<SolverConfig>, timeouts: TimeoutsConfig) -> Config {
        Config {
            serbero: SerberoConfig {
                private_key: self.serbero_keys.secret_key().to_secret_hex(),
                db_path: self.db_path.clone(),
                log_level: "info".to_string(),
            },
            mostro: MostroConfig {
                pubkey: self.mostro_keys.public_key().to_hex(),
            },
            relays: vec![RelayConfig {
                url: self.relay_url.clone(),
            }],
            solvers,
            timeouts,
            // Phase 3 disabled by default for Phase 1/2 tests.
            mediation: Default::default(),
            reasoning: Default::default(),
            prompts: Default::default(),
            chat: Default::default(),
        }
    }
}

pub fn solver_cfg(pubkey: String, permission: SolverPermission) -> SolverConfig {
    SolverConfig { pubkey, permission }
}

pub async fn publisher(relay_url: &str, keys: Keys) -> Client {
    let client = Client::new(keys);
    client.add_relay(relay_url).await.unwrap();
    client.connect().await;
    client
}

pub async fn publish_dispute(
    client: &Client,
    mostro_keys: &Keys,
    dispute_id: &str,
    status: &str,
    initiator: &str,
    extra_tags: Vec<Tag>,
) -> Event {
    let mut tags = vec![
        Tag::identifier(dispute_id),
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::S)),
            [status],
        ),
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
            ["dispute"],
        ),
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Y)),
            [mostro_keys.public_key().to_hex().as_str()],
        ),
        Tag::custom(TagKind::Custom("initiator".into()), [initiator]),
    ];
    tags.extend(extra_tags);
    let event = EventBuilder::new(Kind::Custom(DISPUTE_EVENT_KIND), "")
        .tags(tags)
        .custom_created_at(Timestamp::now())
        .sign_with_keys(mostro_keys)
        .unwrap();
    client.send_event(&event).await.unwrap();
    event
}

/// A mock solver listener that records every decrypted gift-wrap DM it receives.
pub struct SolverListener {
    pub keys: Keys,
    pub client: Client,
    pub received: Arc<Mutex<Vec<String>>>,
    _handle: JoinHandle<()>,
}

impl SolverListener {
    pub async fn start(relay_url: &str) -> Self {
        let keys = Keys::generate();
        let client = Client::new(keys.clone());
        client.add_relay(relay_url).await.unwrap();
        client.connect().await;

        // Subscribe to gift-wrap events tagged to us.
        let filter = Filter::new().kind(Kind::GiftWrap).custom_tag(
            SingleLetterTag::lowercase(Alphabet::P),
            keys.public_key().to_hex(),
        );
        client.subscribe(filter, None).await.unwrap();

        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let received_for_task = received.clone();
        let client_for_task = client.clone();

        let handle = tokio::spawn(async move {
            let _ = client_for_task
                .handle_notifications(|notif| {
                    let received = received_for_task.clone();
                    let client = client_for_task.clone();
                    async move {
                        if let RelayPoolNotification::Event { event, .. } = notif {
                            if event.kind == Kind::GiftWrap {
                                if let Ok(unwrapped) = client.unwrap_gift_wrap(&event).await {
                                    received.lock().await.push(unwrapped.rumor.content.clone());
                                }
                            }
                        }
                        Ok(false)
                    }
                })
                .await;
        });

        SolverListener {
            keys,
            client,
            received,
            _handle: handle,
        }
    }

    pub fn pubkey_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }

    pub async fn count(&self) -> usize {
        self.received.lock().await.len()
    }

    pub async fn messages(&self) -> Vec<String> {
        self.received.lock().await.clone()
    }

    pub async fn wait_for(&self, expected: usize, timeout_secs: u64) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            if self.count().await >= expected {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// Spawn the Serbero daemon on a background task; returns a shutdown sender and the join handle.
pub fn spawn_daemon(
    config: Config,
) -> (oneshot::Sender<()>, JoinHandle<serbero::error::Result<()>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        serbero::daemon::run_with_shutdown(config, async move {
            let _ = rx.await;
        })
        .await
    });
    (tx, handle)
}

/// Poll a COUNT-style SQL query until it returns a value >= `expected`,
/// or the timeout elapses. Returns true on success, false on timeout.
///
/// SQL errors fail the test fast via `expect` — a malformed query is a
/// test bug, not a missing-row signal, and we should not silently
/// convert it into a timeout.
pub async fn wait_for_row_count(
    db_path: &str,
    query: &str,
    expected: i64,
    timeout_secs: u64,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let conn = rusqlite::Connection::open(db_path).expect("open test db");
        match conn.query_row(query, [], |r| r.get::<_, i64>(0)) {
            Ok(c) if c >= expected => return true,
            Ok(_) => { /* not there yet — keep polling */ }
            Err(rusqlite::Error::QueryReturnedNoRows) => { /* aggregate-less query — keep polling */
            }
            Err(e) => panic!("wait_for_row_count: SQL error for query `{query}`: {e}"),
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---------------------------------------------------------------------------
// Phase 3 shared fixtures — MostroChatSim + reasoning-provider stubs.
//
// These were previously inline inside individual Phase 3 integration
// tests. Promoted here (T024 / T025) so new US2+ integration tests
// can reuse them without copy-pasting the full Mostro take-flow
// simulation or the provider scripted behaviors.
// ---------------------------------------------------------------------------

/// Plays the Mostro side of the dispute-chat take-flow for tests.
///
/// Subscribes to gift-wraps addressed to its own keys, detects
/// `Action::AdminTakeDispute`, and replies with a canned
/// `AdminTookDispute` that carries a `SolverDisputeInfo` naming the
/// configured buyer / seller trade pubkeys. The `dispute_id`
/// echoed back is taken verbatim from the request's `kind.id`, so
/// callers can correlate request ↔ reply without coordinating UUIDs
/// out of band.
pub struct MostroChatSim {
    keys: Keys,
    // The relay-pool client has its own lifecycle; we keep the
    // handle alive so the notification task below stays connected.
    _client: Client,
    _task: JoinHandle<()>,
}

impl MostroChatSim {
    pub async fn start(
        relay_url: &str,
        buyer_trade_pk: PublicKey,
        seller_trade_pk: PublicKey,
    ) -> Self {
        let keys = Keys::generate();
        let client = Client::new(keys.clone());
        client.add_relay(relay_url).await.unwrap();
        client.connect().await;
        // Wait for the MockRelay handshake before subscribing, so
        // our REQ is guaranteed to be registered before Serbero
        // publishes its AdminTakeDispute DM.
        client.wait_for_connection(Duration::from_secs(5)).await;

        // Subscribe to gift-wraps addressed to us. The `since`
        // window must be wide enough to cover NIP-59's random
        // timestamp tweak (up to 2 days in the past); otherwise
        // the relay drops incoming events at the REQ filter.
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
                        // Match the real Mostro wire format: the rumor
                        // content is a JSON 2-tuple `[<Message>, <sig?>]`.
                        // The production `run_take_flow` signs; this
                        // sim accepts either the signed or the unsigned
                        // variant (the signature bytes are not verified
                        // here — the test trusts the gift-wrap sender).
                        let Ok((msg, _sig)) = serde_json::from_str::<(
                            Message,
                            Option<nostr_sdk::secp256k1::schnorr::Signature>,
                        )>(&unwrapped.rumor.content) else {
                            return Ok(false);
                        };
                        let kind = msg.get_inner_message_kind();
                        if kind.action != Action::AdminTakeDispute {
                            return Ok(false);
                        }
                        let Some(dispute_id) = kind.id else {
                            return Ok(false);
                        };
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
                        // Reply wire format matches production: the
                        // Mostro side may omit its own signature, so
                        // we send `[<Message>, null]` here. Serbero's
                        // take-flow deserializes with
                        // `Option<Signature>` and does not verify.
                        let content = serde_json::to_string(&(
                            &reply,
                            Option::<nostr_sdk::secp256k1::schnorr::Signature>::None,
                        ))
                        .unwrap();
                        let _ = client.send_private_msg(unwrapped.sender, content, []).await;
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

    pub fn pubkey(&self) -> PublicKey {
        self.keys.public_key()
    }
}

/// Scripted reasoning provider used by US1 happy-path tests.
///
/// `classify` always returns `CoordinationFailureResolvable` with
/// confidence `0.9` and `SuggestedAction::AskClarification(clarification)`.
/// `summarize` returns `ReasoningError::Unreachable` (US3 scope).
/// `health_check` always succeeds.
pub struct MockReasoningProvider {
    pub clarification: String,
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
        Err(ReasoningError::Unreachable(
            "summary not expected in US1 test".into(),
        ))
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Ok(())
    }
}

/// Reasoning provider whose `health_check` always returns
/// `Unreachable`. `classify` and `summarize` panic if ever
/// reached — any call on those paths is a regression of the T044
/// reasoning-health gate.
pub struct UnhealthyReasoningProvider;

#[async_trait]
impl ReasoningProvider for UnhealthyReasoningProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        panic!("classify must not be reached when the reasoning-health gate refuses")
    }

    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        panic!("summarize must not be reached when the reasoning-health gate refuses")
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Err(ReasoningError::Unreachable(
            "provider scripted as unhealthy for the US1 gating test".into(),
        ))
    }
}
