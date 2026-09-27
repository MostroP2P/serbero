//! T1.4–T1.6: dispute events from the Mostro node reach the store through
//! real relays; other authors are ignored; the backlog sync applies only the
//! newest revision when relays disagree; a slow or late relay never delays
//! live disputes and gets the subscription when it connects.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nostr_sdk::prelude::*;
use serbero::config::Permission;
use serbero::daemon;
use serbero::nostr::dm::RelayDmSender;
use serbero::notifier::{Notifier, Solver};
use serbero::store::{Store, disputes, events};

const WAIT: Duration = Duration::from_secs(5);

fn dispute_event(signer: &Keys, id: &str, status: &str, at: Timestamp) -> Event {
    let tags = [
        vec!["d", id],
        vec!["s", status],
        vec!["initiator", "buyer"],
        vec!["y", "mostro"],
        vec!["z", "dispute"],
    ]
    .into_iter()
    .map(|t| Tag::parse(t).unwrap());
    EventBuilder::new(Kind::Custom(38386), "")
        .tags(tags)
        .custom_created_at(at)
        .finalize(signer)
        .unwrap()
}

fn notifier(
    client: &Client,
    store: &Arc<Mutex<Store>>,
    mostro: &Keys,
) -> Arc<Notifier<RelayDmSender>> {
    let solver = Solver {
        pubkey: Keys::generate().public_key(),
        permission: Permission::Write,
    };
    let sender = RelayDmSender::new(client.clone(), Keys::generate());
    Arc::new(Notifier::new(
        Arc::clone(store),
        sender,
        vec![solver],
        mostro.public_key(),
    ))
}

async fn wait_for(store: &Arc<Mutex<Store>>, id: &str) -> disputes::Dispute {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(d) = disputes::get(store.lock().unwrap().conn(), id).unwrap() {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

async fn run_loop(
    client: &Client,
    notifier: Arc<Notifier<RelayDmSender>>,
    mostro: &Keys,
) -> (
    daemon::Background,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (notifications, background) = daemon::start(client, &notifier, mostro.public_key(), 900)
        .await
        .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        daemon::event_loop(notifications, &notifier, async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    (background, stop, task)
}

#[tokio::test]
async fn live_disputes_from_the_node_are_stored() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let mostro = Keys::generate();
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let (_background, stop, task) =
        run_loop(&client, notifier(&client, &store, &mostro), &mostro).await;

    let publisher = serbero::nostr::connect(&[url], WAIT).await.unwrap();
    let now = Timestamp::now();
    publisher
        .send_event(&dispute_event(&mostro, "from-node", "initiated", now))
        .await
        .unwrap();
    publisher
        .send_event(&dispute_event(
            &Keys::generate(),
            "forged",
            "initiated",
            now,
        ))
        .await
        .unwrap();

    assert_eq!(wait_for(&store, "from-node").await.status, "initiated");
    assert!(
        disputes::get(store.lock().unwrap().conn(), "forged")
            .unwrap()
            .is_none()
    );

    stop.send(()).unwrap();
    task.await.unwrap();
    client.shutdown().await;
    publisher.shutdown().await;
}

#[tokio::test]
async fn backlog_sync_applies_only_the_newest_revision_across_relays() {
    let stale_relay = MockRelay::run().await.unwrap();
    let fresh_relay = MockRelay::run().await.unwrap();
    let stale_url = stale_relay.url().await.to_string();
    let fresh_url = fresh_relay.url().await.to_string();
    let mostro = Keys::generate();
    let now = Timestamp::now().as_secs();
    let publisher = serbero::nostr::connect(&[stale_url.clone(), fresh_url.clone()], WAIT)
        .await
        .unwrap();
    let opened = dispute_event(&mostro, "d1", "initiated", Timestamp::from_secs(now - 120));
    let taken = dispute_event(&mostro, "d1", "in-progress", Timestamp::from_secs(now - 60));
    publisher
        .send_event(&opened)
        .to([stale_url.as_str()])
        .await
        .unwrap();
    publisher
        .send_event(&taken)
        .to([fresh_url.as_str()])
        .await
        .unwrap();
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let client = serbero::nostr::connect(&[stale_url, fresh_url], WAIT)
        .await
        .unwrap();

    let (_notifications, mut background) = daemon::start(
        &client,
        &notifier(&client, &store, &mostro),
        mostro.public_key(),
        900,
    )
    .await
    .unwrap();
    background.first_sync().await;

    let dispute = disputes::get(store.lock().unwrap().conn(), "d1")
        .unwrap()
        .unwrap();
    assert_eq!(dispute.status, "in-progress");
    assert_eq!(dispute.lifecycle, disputes::Lifecycle::Taken);
    let kinds: Vec<String> = events::list_for_dispute(store.lock().unwrap().conn(), "d1")
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        ["detected"],
        "no solver was told about a new dispute"
    );
    client.shutdown().await;
    publisher.shutdown().await;
}

#[tokio::test]
async fn a_silent_relay_delays_neither_startup_nor_live_disputes() {
    let healthy = MockRelay::run().await.unwrap();
    let silent = MockRelay::run_with_opts(LocalRelayTestOptions {
        unresponsive_connection: Some(Duration::from_secs(60)),
        ..Default::default()
    })
    .await
    .unwrap();
    let healthy_url = healthy.url().await.to_string();
    let silent_url = silent.url().await.to_string();
    let mostro = Keys::generate();
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let client = serbero::nostr::connect(
        &[healthy_url.clone(), silent_url],
        Duration::from_millis(500),
    )
    .await
    .unwrap();

    let started = Instant::now();
    let (_background, stop, task) =
        run_loop(&client, notifier(&client, &store, &mostro), &mostro).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "startup waited for the silent relay"
    );

    let publisher = serbero::nostr::connect(&[healthy_url], WAIT).await.unwrap();
    let published = Instant::now();
    publisher
        .send_event(&dispute_event(
            &mostro,
            "fresh",
            "initiated",
            Timestamp::now(),
        ))
        .await
        .unwrap();
    wait_for(&store, "fresh").await;
    assert!(
        published.elapsed() < Duration::from_secs(2),
        "live dispute waited for the silent relay"
    );

    stop.send(()).unwrap();
    task.await.unwrap();
    client.shutdown().await;
    publisher.shutdown().await;
}

#[tokio::test]
async fn a_relay_down_at_startup_delivers_disputes_once_it_comes_up() {
    // The relay does not exist yet when Serbero starts; once it comes up, a
    // dispute published only there must still reach the store.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let late_url = format!("ws://127.0.0.1:{port}");
    let healthy = MockRelay::run().await.unwrap();
    let healthy_url = healthy.url().await.to_string();
    let mostro = Keys::generate();
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let client =
        serbero::nostr::connect(&[healthy_url, late_url.clone()], Duration::from_millis(500))
            .await
            .unwrap();
    let (_background, stop, task) =
        run_loop(&client, notifier(&client, &store, &mostro), &mostro).await;

    let late = LocalRelay::builder()
        .addr("127.0.0.1".parse().unwrap())
        .port(port)
        .build();
    late.run().await.unwrap();
    let publisher = serbero::nostr::connect(std::slice::from_ref(&late_url), WAIT)
        .await
        .unwrap();
    // Let Serbero's client reconnect to the relay before publishing.
    tokio::time::sleep(Duration::from_secs(12)).await;
    publisher
        .send_event(&dispute_event(
            &mostro,
            "late",
            "initiated",
            Timestamp::now(),
        ))
        .await
        .unwrap();

    let stored = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if disputes::get(store.lock().unwrap().conn(), "late")
                .unwrap()
                .is_some()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(stored.is_ok(), "a dispute on the late relay never arrived");

    stop.send(()).unwrap();
    task.await.unwrap();
    client.shutdown().await;
    publisher.shutdown().await;
}
