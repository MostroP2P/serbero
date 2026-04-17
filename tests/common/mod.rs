#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use nostr_relay_builder::MockRelay;
use nostr_sdk::{
    Alphabet, Client, Event, EventBuilder, Filter, Keys, Kind, RelayPoolNotification,
    SingleLetterTag, Tag, TagKind, Timestamp,
};
use serbero::models::{
    Config, MostroConfig, RelayConfig, SerberoConfig, SolverConfig, SolverPermission,
    TimeoutsConfig,
};
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
