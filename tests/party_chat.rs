//! T2.4–T2.5: Serbero and a party exchange dispute chat messages through a
//! real relay, in the Mostro protocol's kind 14 format.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::sync::Mutex;
use std::time::{Duration, Instant};

use mostro_core::chat::{unwrap_chat_message, wrap_chat_message};
use nostr_sdk::prelude::*;
use serbero::chat::inbound::Inbox;
use serbero::chat::{Outbound, send_to_party};
use serbero::mostro::chat::ChannelKeys;
use serbero::store::Store;
use serbero::store::disputes::{self, Initiator, NewDispute};
use serbero::store::messages::{self, Direction};
use serbero::store::sessions::{self, NewSession, Party};

const WAIT: Duration = Duration::from_secs(5);

fn store_with_session(buyer: &Keys, seller: &Keys) -> Mutex<Store> {
    let store = Store::open_in_memory().unwrap();
    disputes::insert_if_new(
        store.conn(),
        &NewDispute {
            dispute_id: "d1",
            initiator: Initiator::Buyer,
            status: "in-progress",
            status_at: 1,
            now: 1,
        },
    )
    .unwrap();
    let (b, s) = (buyer.public_key().to_hex(), seller.public_key().to_hex());
    sessions::insert(
        store.conn(),
        &NewSession {
            session_id: "s1",
            dispute_id: "d1",
            buyer_trade_pubkey: &b,
            seller_trade_pubkey: &s,
            fiat_amount: Some("50000"),
            fiat_code: Some("ARS"),
            payment_method: None,
            order_published_at: None,
            now: 1,
        },
    )
    .unwrap();
    Mutex::new(store)
}

#[tokio::test]
async fn serbero_and_a_party_exchange_messages_through_a_relay() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let (serbero, buyer, seller) = (Keys::generate(), Keys::generate(), Keys::generate());
    let store = store_with_session(&buyer, &seller);
    let session = sessions::get(store.lock().unwrap().conn(), "s1")
        .unwrap()
        .unwrap();
    let serbero_client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let buyer_client = serbero::nostr::connect(&[url], WAIT).await.unwrap();

    // Both sides listen before anything is sent.
    let buyer_side = ChannelKeys::derive(&buyer, &serbero.public_key()).unwrap();
    let mut buyer_inbox = buyer_client.notifications();
    buyer_client
        .subscribe(
            Filter::new()
                .kind(Kind::PrivateDirectMessage)
                .author(buyer_side.author_pubkey()),
        )
        .await
        .unwrap();
    let mut inbox = Inbox::default();
    let filter = inbox
        .add_session(&serbero, &session, Instant::now())
        .unwrap();
    let mut serbero_inbox = serbero_client.notifications();
    serbero_client
        .subscribe(filter)
        .with_id(SubscriptionId::new("serbero-chat-s1"))
        .await
        .unwrap();

    // Outbound: Serbero asks the buyer.
    send_to_party(
        &serbero_client,
        &store,
        &serbero,
        &session,
        &Outbound {
            party: Party::Buyer,
            text: "¿Enviaste el pago de 50.000 ARS?",
            template_id: Some("ask_buyer_sent"),
            lang: Some("es"),
        },
    )
    .await
    .unwrap();
    let delivered = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(ClientNotification::Event { event, .. }) = buyer_inbox.next().await {
                return event;
            }
        }
    })
    .await
    .unwrap();
    let read = unwrap_chat_message(
        buyer_side.conv(),
        &buyer_side.author_pubkey(),
        &[serbero.public_key()],
        &delivered,
        Timestamp::now(),
    )
    .unwrap();
    assert_eq!(read.content, "¿Enviaste el pago de 50.000 ARS?");
    assert_eq!(read.sender, serbero.public_key());

    // Inbound: the buyer answers on the same channel.
    let answer = wrap_chat_message(
        &buyer,
        buyer_side.conv(),
        buyer_side.sign(),
        "sí, a las 14:10",
    )
    .await
    .unwrap();
    buyer_client.send_event(&answer).await.unwrap();
    let received = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(ClientNotification::Event { event, .. }) = serbero_inbox.next().await
                && let Ok(received) = inbox
                    .handle(&store, &event, Timestamp::now(), Instant::now())
                    .unwrap()
            {
                return received;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(received.party, Party::Buyer);
    assert_eq!(received.content, "sí, a las 14:10");

    let transcript = messages::list_for_session(store.lock().unwrap().conn(), "s1").unwrap();
    let summary: Vec<_> = transcript
        .iter()
        .map(|m| (m.direction, m.party, m.template_id.as_deref()))
        .collect();
    assert_eq!(
        summary,
        [
            (Direction::Out, Party::Buyer, Some("ask_buyer_sent")),
            (Direction::In, Party::Buyer, None)
        ]
    );
}
