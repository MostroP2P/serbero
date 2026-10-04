//! T2.3: against a simulated Mostro node on a local relay, Serbero checks the
//! node's protocol version, takes a dispute, reads the order facts, and
//! handles refusal and silence.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::time::Duration;

use mostro_core::dispute::SolverDisputeInfo;
use mostro_core::error::CantDoReason;
use mostro_core::message::{Action, Message, Payload};
use mostro_core::transport::WrapOptions;
use mostro_core::transport::{unwrap_message_nip44, wrap_message_nip44};
use nostr_sdk::prelude::*;
use serbero::mostro::take::{TakeError, take_dispute};
use serbero::mostro::{node, order};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Mode {
    Accept,
    Refuse,
    Silent,
}

struct FakeNode {
    keys: Keys,
    buyer: Keys,
    seller: Keys,
    order_id: Uuid,
}

fn tagged(kind: u16, tags: &[&[&str]], signer: &Keys) -> Event {
    let tags = tags.iter().map(|t| Tag::parse(t.iter().copied()).unwrap());
    EventBuilder::new(Kind::Custom(kind), "")
        .tags(tags)
        .finalize(signer)
        .unwrap()
}

/// Starts a node on `url` that publishes its info and one order, then
/// answers `admin-take-dispute` according to `mode`.
async fn start_node(url: &str, version: &str, mode: Mode) -> FakeNode {
    let node = FakeNode {
        keys: Keys::generate(),
        buyer: Keys::generate(),
        seller: Keys::generate(),
        order_id: Uuid::new_v4(),
    };
    let client = serbero::nostr::connect(&[url.to_owned()], WAIT)
        .await
        .unwrap();
    let me = node.keys.public_key().to_hex();
    let order_id = node.order_id.to_string();
    client
        .send_event(&tagged(
            38385,
            &[&["d", &me], &["protocol_version", version], &["pow", "0"]],
            &node.keys,
        ))
        .await
        .unwrap();
    client
        .send_event(&tagged(
            38383,
            &[
                &["d", &order_id],
                &["f", "ARS"],
                &["published_at", "1700"],
                &["s", "in-progress"],
            ],
            &node.keys,
        ))
        .await
        .unwrap();

    let mut notifications = client.notifications();
    client
        .subscribe(
            Filter::new()
                .kind(Kind::PrivateDirectMessage)
                .pubkey(node.keys.public_key()),
        )
        .await
        .unwrap();
    let keys = node.keys.clone();
    let (buyer, seller, order) = (
        node.buyer.public_key(),
        node.seller.public_key(),
        node.order_id,
    );
    tokio::spawn(async move {
        while let Some(n) = notifications.next().await {
            let ClientNotification::Event { event, .. } = n else {
                continue;
            };
            let Ok(Some(msg)) = unwrap_message_nip44(&event, &keys) else {
                continue;
            };
            let kind = msg.message.get_inner_message_kind();
            if kind.action != Action::AdminTakeDispute {
                continue;
            }
            let dispute = kind.id.unwrap();
            let reply = match mode {
                Mode::Silent => continue,
                Mode::Refuse => Message::cant_do(
                    Some(dispute),
                    kind.request_id,
                    Some(Payload::CantDo(Some(CantDoReason::NotAllowedByStatus))),
                ),
                Mode::Accept => {
                    let info = SolverDisputeInfo {
                        id: order,
                        buyer_pubkey: Some(buyer.to_hex()),
                        seller_pubkey: Some(seller.to_hex()),
                        fiat_amount: 50_000,
                        payment_method: "Mercado Pago".into(),
                        ..Default::default()
                    };
                    Message::new_dispute(
                        Some(dispute),
                        kind.request_id,
                        None,
                        Action::AdminTookDispute,
                        Some(Payload::Dispute(dispute, Some(info))),
                    )
                }
            };
            let event =
                wrap_message_nip44(&reply, &keys, &keys, msg.identity, WrapOptions::default())
                    .unwrap();
            client.send_event(&event).await.unwrap();
        }
    });
    node
}

#[tokio::test]
async fn takes_a_dispute_and_reads_the_order_facts() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, "2", Mode::Accept).await;
    let serbero = Keys::generate();
    let client = serbero::nostr::connect(&[url], WAIT).await.unwrap();

    let info = node::fetch(&client, node.keys.public_key(), WAIT)
        .await
        .unwrap()
        .unwrap();
    assert!(info.speaks_v2());
    let taken = take_dispute(
        &client,
        &serbero,
        node.keys.public_key(),
        Uuid::new_v4(),
        info.pow_for_serbero(),
        WAIT,
    )
    .await
    .unwrap();
    let facts = order::fetch(&client, node.keys.public_key(), &taken.id.to_string(), WAIT)
        .await
        .unwrap();

    assert_eq!(taken.buyer_pubkey, Some(node.buyer.public_key().to_hex()));
    assert_eq!(taken.seller_pubkey, Some(node.seller.public_key().to_hex()));
    assert_eq!(taken.fiat_amount, 50_000);
    assert_eq!(facts.fiat_code.as_deref(), Some("ARS"));
    assert_eq!(facts.published_at, Some(1_700));
}

#[tokio::test]
async fn a_refused_take_is_a_typed_error() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, "2", Mode::Refuse).await;
    let client = serbero::nostr::connect(&[url], WAIT).await.unwrap();

    let result = take_dispute(
        &client,
        &Keys::generate(),
        node.keys.public_key(),
        Uuid::new_v4(),
        0,
        WAIT,
    )
    .await;

    assert!(matches!(result, Err(TakeError::Refused(r)) if r.contains("NotAllowedByStatus")));
}

#[tokio::test]
async fn a_silent_node_times_out() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, "2", Mode::Silent).await;
    let client = serbero::nostr::connect(&[url], WAIT).await.unwrap();

    let limit = Duration::from_secs(1);
    let result = take_dispute(
        &client,
        &Keys::generate(),
        node.keys.public_key(),
        Uuid::new_v4(),
        0,
        limit,
    )
    .await;

    assert_eq!(result.unwrap_err(), TakeError::Timeout(limit));
}

#[tokio::test]
async fn a_node_on_protocol_v1_is_detected() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, "1", Mode::Silent).await;
    let client = serbero::nostr::connect(&[url], WAIT).await.unwrap();

    let info = node::fetch(&client, node.keys.public_key(), WAIT)
        .await
        .unwrap()
        .unwrap();

    assert!(!info.speaks_v2());
}

#[tokio::test]
async fn a_missing_order_event_leaves_facts_unknown() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, "2", Mode::Silent).await;
    let client = serbero::nostr::connect(&[url], WAIT).await.unwrap();

    let facts = order::fetch(
        &client,
        node.keys.public_key(),
        "no-such-order",
        Duration::from_secs(1),
    )
    .await
    .unwrap();

    assert_eq!(facts, order::OrderFacts::default());
}
