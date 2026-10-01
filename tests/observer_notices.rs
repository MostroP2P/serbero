//! Observer notices over real relays: what an observer such as
//! mostro-watchdog receives, and a silent relay costing one bounded attempt
//! (AGENTS.md, relays rule 7).

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::sync::Mutex;
use std::time::{Duration, Instant};

use mostro_core::message::{Action, Payload};
use mostro_core::transport::unwrap_message_nip44;
use nostr_sdk::prelude::*;
use serbero::nostr::dm::RelayDmSender;
use serbero::notifier::observers;
use serbero::store::{Store, events};
use uuid::Uuid;

const WAIT: Duration = Duration::from_millis(500);

fn kinds(store: &Mutex<Store>, dispute_id: &str) -> Vec<String> {
    events::list_for_dispute(store.lock().unwrap().conn(), dispute_id)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn an_observer_receives_one_send_dm_line_naming_the_dispute() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let serbero = Keys::generate();
    let observer = Keys::generate();
    let sender = RelayDmSender::new(client.clone(), serbero.clone());
    let store = Mutex::new(Store::open_in_memory().unwrap());
    let dispute_id = Uuid::new_v4();
    let id = dispute_id.to_string();
    observers::queue(
        store.lock().unwrap().conn(),
        &[observer.public_key()],
        &id,
        "handed off: conflicting_claims",
        1_000,
    )
    .unwrap();

    let delivered = observers::deliver_due(
        &store,
        &sender,
        &[observer.public_key()],
        Duration::from_secs(5),
        1_001,
    )
    .await
    .unwrap();

    assert_eq!(delivered, 1);
    let received = client
        .fetch_events(
            Filter::new()
                .kind(Kind::PrivateDirectMessage)
                .author(serbero.public_key())
                .pubkey(observer.public_key()),
        )
        .timeout(Duration::from_secs(2))
        .await
        .unwrap();
    let event = received.first().expect("the DM reached the relay");
    let opened = unwrap_message_nip44(event, &observer).unwrap().unwrap();
    let kind = opened.message.get_inner_message_kind();
    assert_eq!(kind.action, Action::SendDm);
    assert_eq!(kind.id, Some(dispute_id));
    assert!(
        matches!(&kind.payload, Some(Payload::TextMessage(text))
            if *text == format!("Dispute {id} · handed off: conflicting_claims")),
        "{:?}",
        kind.payload
    );
}

#[tokio::test]
async fn a_silent_relay_costs_one_bounded_attempt_and_leaves_the_notice_queued() {
    let silent = MockRelay::run_with_opts(LocalRelayTestOptions {
        unresponsive_connection: Some(Duration::from_secs(60)),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = serbero::nostr::connect(&[silent.url().await.to_string()], WAIT)
        .await
        .unwrap();
    let sender = RelayDmSender::new(client, Keys::generate());
    let store = Mutex::new(Store::open_in_memory().unwrap());
    let observer = Keys::generate().public_key();
    let id = Uuid::new_v4().to_string();
    observers::queue(
        store.lock().unwrap().conn(),
        &[observer],
        &id,
        "mediating",
        1_000,
    )
    .unwrap();

    let started = Instant::now();
    let delivered = observers::deliver_due(
        &store,
        &sender,
        &[observer],
        Duration::from_millis(300),
        1_001,
    )
    .await
    .unwrap();

    assert_eq!(delivered, 0);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the silent relay held the queue for {:?}",
        started.elapsed()
    );
    assert_eq!(
        kinds(&store, &id),
        ["observer_pending", "observer_failed"],
        "recorded, so the backoff retries it"
    );
}
