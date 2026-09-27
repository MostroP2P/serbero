use std::time::Duration;

use mostro_core::chat::wrap_chat_message;

use super::*;
use crate::store::disputes::{self, Initiator, NewDispute};
use crate::store::sessions::NewSession;

struct Fixture {
    store: Mutex<Store>,
    serbero: Keys,
    buyer: Keys,
    seller: Keys,
    inbox: Inbox,
}

fn fixture() -> Fixture {
    let store = Store::open_in_memory().unwrap();
    let (serbero, buyer, seller) = (Keys::generate(), Keys::generate(), Keys::generate());
    disputes::insert_if_new(
        store.conn(),
        &NewDispute {
            dispute_id: "d1",
            initiator: Initiator::Seller,
            status: "in-progress",
            status_at: 100,
            now: 100,
        },
    )
    .unwrap();
    let (buyer_hex, seller_hex) = (buyer.public_key().to_hex(), seller.public_key().to_hex());
    sessions::insert(
        store.conn(),
        &NewSession {
            session_id: "s1",
            dispute_id: "d1",
            buyer_trade_pubkey: &buyer_hex,
            seller_trade_pubkey: &seller_hex,
            fiat_amount: None,
            fiat_code: None,
            order_published_at: None,
            now: 100,
        },
    )
    .unwrap();
    let session = sessions::get(store.conn(), "s1").unwrap().unwrap();
    let mut inbox = Inbox::default();
    inbox
        .add_session(&serbero, &session, Instant::now())
        .unwrap();
    Fixture {
        store: Mutex::new(store),
        serbero,
        buyer,
        seller,
        inbox,
    }
}

/// A message on `party`'s channel with Serbero, inner-signed by `signer`.
async fn on_channel(f: &Fixture, party: &Keys, signer: &Keys, text: &str) -> Event {
    let keys = ChannelKeys::derive(party, &f.serbero.public_key()).unwrap();
    wrap_chat_message(signer, keys.conv(), keys.sign(), text)
        .await
        .unwrap()
}

fn handle(f: &mut Fixture, event: &Event) -> std::result::Result<Received, Rejected> {
    f.inbox
        .handle(&f.store, event, Timestamp::now(), Instant::now())
        .unwrap()
}

#[tokio::test]
async fn a_party_message_is_stored_and_advances_the_cursor() {
    let mut f = fixture();
    let event = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), "ya envié el pago").await;

    let received = handle(&mut f, &event).unwrap();

    assert_eq!(received.party, Party::Buyer);
    assert_eq!(received.content, "ya envié el pago");
    let store = f.store.lock().unwrap();
    let stored = messages::list_for_session(store.conn(), "s1").unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].direction, Direction::In);
    let session = sessions::get(store.conn(), "s1").unwrap().unwrap();
    assert_eq!(session.buyer_chat_cursor, Some(received.created_at));
}

#[tokio::test]
async fn each_party_is_recognised_on_its_own_channel() {
    let mut f = fixture();
    let event = on_channel(&f, &f.seller.clone(), &f.seller.clone(), "no me llegó").await;

    assert_eq!(handle(&mut f, &event).unwrap().party, Party::Seller);
}

#[tokio::test]
async fn an_unknown_author_is_rejected_without_crypto() {
    let mut f = fixture();
    let stranger = Keys::generate();
    let event = on_channel(&f, &stranger, &stranger, "hi").await;

    assert_eq!(handle(&mut f, &event), Err(Rejected::UnknownAuthor));
}

#[tokio::test]
async fn a_duplicate_delivery_is_dropped() {
    let mut f = fixture();
    let event = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), "hola").await;
    handle(&mut f, &event).unwrap();

    assert_eq!(handle(&mut f, &event), Err(Rejected::DuplicateOuter));
}

#[tokio::test]
async fn a_rewrapped_old_message_is_rejected_by_inner_id() {
    let mut f = fixture();
    // Wrapping the same text twice within one second yields the same inner
    // event (same author, time, content) in two different envelopes.
    let (first, second) = loop {
        let a = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), "I sent the fiat").await;
        let b = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), "I sent the fiat").await;
        if a.created_at == b.created_at {
            break (a, b);
        }
    };
    assert_ne!(first.id, second.id);
    handle(&mut f, &first).unwrap();

    assert_eq!(handle(&mut f, &second), Err(Rejected::DuplicateInner));
}

#[tokio::test]
async fn a_forged_inner_signer_is_rejected() {
    let mut f = fixture();
    let forger = Keys::generate();
    let event = on_channel(&f, &f.buyer.clone(), &forger, "seller says release").await;

    assert!(matches!(handle(&mut f, &event), Err(Rejected::Invalid(_))));
}

#[tokio::test]
async fn a_flood_is_rate_limited_before_decryption() {
    let mut f = fixture();
    let now = Instant::now();
    let mut outcomes = Vec::new();
    for i in 0..61 {
        let event = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), &format!("spam {i}")).await;
        outcomes.push(
            f.inbox
                .handle(&f.store, &event, Timestamp::now(), now)
                .unwrap(),
        );
    }

    assert!(outcomes[..60].iter().all(Result::is_ok));
    assert_eq!(outcomes[60], Err(Rejected::RateLimited));
    let later = now + Duration::from_secs(10);
    let event = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), "after a pause").await;
    assert!(
        f.inbox
            .handle(&f.store, &event, Timestamp::now(), later)
            .unwrap()
            .is_ok()
    );
}

#[tokio::test]
async fn attachments_are_counted_and_their_urls_never_stored() {
    let mut f = fixture();
    let json = r#"{"type":"image_encrypted","blossom_url":"https://cdn.example/abc","nonce":"0102","mime_type":"image/jpeg"}"#;
    let event = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), json).await;

    let received = handle(&mut f, &event).unwrap();

    assert_eq!(received.attachments, 1);
    assert_eq!(received.content, "[image attachment]");
    let store = f.store.lock().unwrap();
    let stored = messages::list_for_session(store.conn(), "s1").unwrap();
    assert!(!stored[0].content.contains("cdn.example"));
}

#[tokio::test]
async fn the_cursor_never_passes_the_local_clock() {
    let mut f = fixture();
    let event = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), "hola").await;
    // Serbero's clock runs 30 s behind the sender's (inside the 60 s skew).
    let local_now = Timestamp::from_secs(event.created_at.as_secs() - 30);

    f.inbox
        .handle(&f.store, &event, local_now, Instant::now())
        .unwrap()
        .unwrap();

    let store = f.store.lock().unwrap();
    let session = sessions::get(store.conn(), "s1").unwrap().unwrap();
    assert_eq!(session.buyer_chat_cursor, Some(local_now.as_secs() as i64));
}

#[tokio::test]
async fn removed_sessions_stop_being_accepted() {
    let mut f = fixture();
    f.inbox.remove_session("s1");
    let event = on_channel(&f, &f.buyer.clone(), &f.buyer.clone(), "hola").await;

    assert_eq!(handle(&mut f, &event), Err(Rejected::UnknownAuthor));
}
