//! T1.4–T1.6: dispute events from the Mostro node reach the store through
//! real relays; other authors are ignored; the startup sync applies only the
//! newest revision when relays disagree.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::sync::{Arc, Mutex};
use std::time::Duration;

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

fn notifier(client: &Client, store: &Arc<Mutex<Store>>, mostro: &Keys) -> Notifier<RelayDmSender> {
    let solver = Solver {
        pubkey: Keys::generate().public_key(),
        permission: Permission::Write,
    };
    let sender = RelayDmSender::new(client.clone(), Keys::generate());
    Notifier::new(Arc::clone(store), sender, vec![solver], mostro.public_key())
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

#[tokio::test]
async fn live_disputes_from_the_node_are_stored() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let mostro = Keys::generate();
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let notifier = notifier(&client, &store, &mostro);
    let notifications = daemon::start(&client, &notifier, mostro.public_key())
        .await
        .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        daemon::event_loop(notifications, &notifier, async {
            let _ = stopped.await;
        })
        .await
    });

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
    task.await.unwrap().unwrap();
    client.shutdown().await;
    publisher.shutdown().await;
}

#[tokio::test]
async fn startup_sync_applies_only_the_newest_revision_across_relays() {
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
    let notifier = notifier(&client, &store, &mostro);

    let _live = daemon::start(&client, &notifier, mostro.public_key())
        .await
        .unwrap();

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
