//! T1.1: the client connects to a relay, receives subscribed events, and
//! shuts down cleanly.

use std::time::Duration;

use nostr_sdk::prelude::*;
use serbero::nostr;

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn connects_receives_and_shuts_down() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let client = nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let author = Keys::generate();
    client
        .subscribe(
            Filter::new()
                .author(author.public_key())
                .kind(Kind::TextNote),
        )
        .await
        .unwrap();
    let mut notifications = client.notifications();

    let event = EventBuilder::new(Kind::TextNote, "hello")
        .finalize(&author)
        .unwrap();
    let publisher = nostr::connect(&[url], WAIT).await.unwrap();
    publisher.send_event(&event).await.unwrap();

    let received = tokio::time::timeout(WAIT, async {
        while let Some(n) = notifications.next().await {
            if let ClientNotification::Event { event: e, .. } = n {
                return e.id;
            }
        }
        panic!("notification stream ended");
    })
    .await
    .unwrap();
    assert_eq!(received, event.id);

    nostr::shutdown(&client).await;
    nostr::shutdown(&publisher).await;
    assert!(client.is_shutdown());
}

#[tokio::test]
async fn invalid_relay_url_is_an_error() {
    let err = nostr::connect(&["not a url".to_owned()], WAIT)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("cannot add relay"), "{err}");
}
