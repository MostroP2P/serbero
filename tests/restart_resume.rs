//! T2.7: after a restart, Serbero re-derives the chat keys of live sessions
//! from the stored trade pubkeys, re-subscribes from the stored cursors,
//! receives what was sent while it was down, and ignores what it already had.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mostro_core::chat::wrap_chat_message;
use nostr_sdk::prelude::*;
use serbero::chat::channels::Chats;
use serbero::mostro::chat::ChannelKeys;
use serbero::store::Store;
use serbero::store::disputes::{self, Initiator, NewDispute};
use serbero::store::messages::{self, Direction};
use serbero::store::sessions::{self, NewSession, SessionState};

const WAIT: Duration = Duration::from_secs(5);

fn store_with_live_session(buyer: &Keys, seller: &Keys) -> Arc<Mutex<Store>> {
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
            fiat_amount: None,
            fiat_code: None,
            payment_method: None,
            order_published_at: None,
            now: Timestamp::now().as_secs() as i64 - 60,
        },
    )
    .unwrap();
    sessions::set_state(store.conn(), "s1", SessionState::Active, 1).unwrap();
    Arc::new(Mutex::new(store))
}

fn inbound(store: &Arc<Mutex<Store>>) -> Vec<String> {
    messages::list_for_session(store.lock().unwrap().conn(), "s1")
        .unwrap()
        .into_iter()
        .filter(|m| m.direction == Direction::In)
        .map(|m| m.content)
        .collect()
}

async fn wait_for(store: &Arc<Mutex<Store>>, count: usize) {
    tokio::time::timeout(WAIT, async {
        while inbound(store).len() < count {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}

/// Starts a Serbero instance on `store`: a fresh client and a fresh inbox.
async fn start_serbero(
    url: &str,
    serbero: &Keys,
    store: &Arc<Mutex<Store>>,
) -> (Client, tokio::task::JoinHandle<()>) {
    let client = serbero::nostr::connect(&[url.to_owned()], WAIT)
        .await
        .unwrap();
    let chats = Arc::new(Chats::new(
        client.clone(),
        serbero.clone(),
        Arc::clone(store),
        2_000,
        Default::default(),
    ));
    let notifications = client.notifications();
    assert_eq!(chats.resume().await.unwrap(), 1);
    let task = tokio::spawn(async move { chats.run(notifications).await });
    (client, task)
}

#[tokio::test]
async fn messages_sent_while_down_arrive_after_restart_without_duplicates() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let (serbero, buyer, seller) = (Keys::generate(), Keys::generate(), Keys::generate());
    let store = store_with_live_session(&buyer, &seller);
    let buyer_client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let channel = ChannelKeys::derive(&buyer, &serbero.public_key()).unwrap();
    let send = |text: &'static str| {
        let (buyer, channel, client) = (buyer.clone(), channel.clone(), buyer_client.clone());
        async move {
            let event = wrap_chat_message(&buyer, channel.conv(), channel.sign(), text)
                .await
                .unwrap();
            client.send_event(&event).await.unwrap();
        }
    };

    // Before the restart: the first message arrives.
    let (client, task) = start_serbero(&url, &serbero, &store).await;
    send("ya envié el pago").await;
    wait_for(&store, 1).await;
    task.abort();
    client.shutdown().await;

    // While Serbero is down, the buyer writes again.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    send("¿alguien me lee?").await;

    // After the restart: nothing in memory, only the database.
    let (client, task) = start_serbero(&url, &serbero, &store).await;
    wait_for(&store, 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(inbound(&store), ["ya envié el pago", "¿alguien me lee?"]);
    task.abort();
    client.shutdown().await;
}

#[tokio::test]
async fn a_session_that_cannot_be_resumed_does_not_block_the_others() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let (serbero, buyer, seller) = (Keys::generate(), Keys::generate(), Keys::generate());
    let store = store_with_live_session(&buyer, &seller);
    {
        // Listed first, and its stored trade pubkey is not a key.
        let store = store.lock().unwrap();
        disputes::insert_if_new(
            store.conn(),
            &NewDispute {
                dispute_id: "d0",
                initiator: Initiator::Buyer,
                status: "in-progress",
                status_at: 1,
                now: 1,
            },
        )
        .unwrap();
        sessions::insert(
            store.conn(),
            &NewSession {
                session_id: "s0",
                dispute_id: "d0",
                buyer_trade_pubkey: "not-a-key",
                seller_trade_pubkey: "not-a-key",
                fiat_amount: None,
                fiat_code: None,
                payment_method: None,
                order_published_at: None,
                now: 1,
            },
        )
        .unwrap();
        sessions::set_state(store.conn(), "s0", SessionState::Active, 1).unwrap();
    }
    let client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let chats = Arc::new(Chats::new(
        client.clone(),
        serbero.clone(),
        Arc::clone(&store),
        2_000,
        Default::default(),
    ));
    let notifications = client.notifications();

    assert_eq!(chats.resume().await.unwrap(), 1, "s1 still opened");

    let task = tokio::spawn(async move { chats.run(notifications).await });
    let channel = ChannelKeys::derive(&buyer, &serbero.public_key()).unwrap();
    let event = wrap_chat_message(&buyer, channel.conv(), channel.sign(), "hola")
        .await
        .unwrap();
    let buyer_client = serbero::nostr::connect(&[url], WAIT).await.unwrap();
    buyer_client.send_event(&event).await.unwrap();
    wait_for(&store, 1).await;
    task.abort();
    client.shutdown().await;
}
