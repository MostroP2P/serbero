//! T1.4: dispute events published by the Mostro node reach the store
//! through a real relay subscription; other authors are ignored.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nostr_sdk::prelude::*;
use serbero::daemon;
use serbero::notifier::Notifier;
use serbero::store::{Store, disputes};

const WAIT: Duration = Duration::from_secs(5);

fn dispute_event(signer: &Keys, id: &str, status: &str, at: u64) -> Event {
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
        .custom_created_at(Timestamp::from_secs(at))
        .finalize(signer)
        .unwrap()
}

#[tokio::test]
async fn disputes_from_the_node_are_stored() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let mostro = Keys::generate();
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    daemon::subscribe_disputes(&client, mostro.public_key())
        .await
        .unwrap();
    let sender = serbero::nostr::dm::RelayDmSender::new(client.clone(), Keys::generate());
    let notifier = Notifier::new(Arc::clone(&store), sender, vec![], mostro.public_key());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let loop_client = client.clone();
    let task = tokio::spawn(async move {
        daemon::event_loop(&loop_client, &notifier, async {
            let _ = stopped.await;
        })
        .await
    });

    let publisher = serbero::nostr::connect(&[url], WAIT).await.unwrap();
    publisher
        .send_event(&dispute_event(&mostro, "from-node", "initiated", 100))
        .await
        .unwrap();
    publisher
        .send_event(&dispute_event(
            &Keys::generate(),
            "forged",
            "initiated",
            100,
        ))
        .await
        .unwrap();

    let stored = tokio::time::timeout(WAIT, async {
        loop {
            let found = disputes::get(store.lock().unwrap().conn(), "from-node").unwrap();
            if let Some(d) = found {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(stored.status, "initiated");
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
