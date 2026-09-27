use serde_json::json;

use super::*;
use crate::store::sessions::SessionState;

const BUYER_KEY: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const SELLER_KEY: &str = "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";

fn session() -> Session {
    Session {
        session_id: "s1".into(),
        dispute_id: "d1".into(),
        state: SessionState::Active,
        buyer_trade_pubkey: BUYER_KEY.into(),
        seller_trade_pubkey: SELLER_KEY.into(),
        fiat_amount: Some("50000".into()),
        fiat_code: Some("ARS".into()),
        payment_method: Some("Mercado Pago".into()),
        order_published_at: Some(1_700_000_000),
        buyer_lang: None,
        seller_lang: None,
        buyer_chat_cursor: None,
        seller_chat_cursor: None,
        rounds: 0,
        handoff_reason: None,
        opened_at: 1_700_000_100,
        updated_at: 1_700_000_100,
    }
}

fn message(id: i64, direction: Direction, party: Party, content: &str) -> Message {
    Message {
        id,
        session_id: "s1".into(),
        direction,
        party,
        template_id: None,
        lang: None,
        content: content.into(),
        attachments: 0,
        inner_event_id: format!("{id:064x}"),
        created_at: 1_700_000_100 + id,
    }
}

fn conversation() -> Vec<Message> {
    let mut paid = message(
        14,
        Direction::In,
        Party::Buyer,
        "ya envié el pago a las 14:10",
    );
    paid.attachments = 1;
    vec![
        message(
            11,
            Direction::Out,
            Party::Buyer,
            "Did you send the payment?",
        ),
        message(
            12,
            Direction::Out,
            Party::Seller,
            "Has the payment arrived?",
        ),
        message(
            13,
            Direction::In,
            Party::Seller,
            "hola, no me ha llegado el dinero",
        ),
        paid,
    ]
}

#[test]
fn state_matches_the_documented_shape() {
    let state = build(&session(), Initiator::Seller, &conversation(), 2_000);

    assert_eq!(
        state.value,
        json!({
            "roles": {
                "buyer": ROLE_BUYER,
                "seller": ROLE_SELLER,
                "serbero": ROLE_SERBERO,
            },
            "order": {
                "fiat_amount": "50000",
                "fiat_code": "ARS",
                "payment_method": "Mercado Pago",
                "dispute_opened_by": "seller"
            },
            "transcript": [
                { "id": "m1", "from": "serbero", "to": "buyer", "text": "Did you send the payment?" },
                { "id": "m2", "from": "serbero", "to": "seller", "text": "Has the payment arrived?" },
                { "id": "m3", "from": "seller", "text": "hola, no me ha llegado el dinero" },
                { "id": "m4", "from": "buyer", "text": "ya envié el pago a las 14:10", "attachments": 1 }
            ],
            "latest": { "buyer": ["m4"], "seller": ["m3"] }
        })
    );
}

#[test]
fn latest_only_holds_messages_since_serbero_last_wrote_to_that_party() {
    let mut messages = conversation();
    messages.push(message(15, Direction::Out, Party::Buyer, "Any receipt?"));
    messages.push(message(16, Direction::In, Party::Seller, "still nothing"));

    let state = build(&session(), Initiator::Seller, &messages, 2_000);

    assert_eq!(
        state.value["latest"],
        json!({ "buyer": [], "seller": ["m3", "m6"] })
    );
}

#[test]
fn short_ids_map_back_to_stored_messages() {
    let state = build(&session(), Initiator::Seller, &conversation(), 2_000);

    assert_eq!(state.message_id("m1"), Some(11));
    assert_eq!(state.message_id("m4"), Some(14));
    assert_eq!(state.message_id("m5"), None);
    assert_eq!(state.message_id("m0"), None);
    assert_eq!(state.message_id("x1"), None);
}

#[test]
fn text_is_truncated_on_a_character_boundary() {
    let messages = [message(1, Direction::In, Party::Buyer, "añoñoño")];

    let state = build(&session(), Initiator::Buyer, &messages, 3);

    assert_eq!(state.value["transcript"][0]["text"], json!("año"));
}

#[test]
fn unknown_order_facts_are_left_out() {
    let mut bare = session();
    bare.fiat_amount = None;
    bare.fiat_code = None;
    bare.payment_method = None;

    let state = build(&bare, Initiator::Unknown, &[], 2_000);

    assert_eq!(state.value["order"], json!({}));
    assert_eq!(state.value["transcript"], json!([]));
    assert_eq!(state.value["latest"], json!({ "buyer": [], "seller": [] }));
}

#[test]
fn no_pubkey_or_event_id_can_appear_in_the_state() {
    let messages = conversation();

    let text = build(&session(), Initiator::Seller, &messages, 2_000)
        .value
        .to_string();

    for key in [BUYER_KEY, SELLER_KEY] {
        assert!(!text.contains(key), "state leaks {key}");
    }
    for message in &messages {
        assert!(!text.contains(&message.inner_event_id));
    }
    for field in [
        "session_id",
        "dispute_id",
        "pubkey",
        "created_at",
        "published_at",
    ] {
        assert!(!text.contains(field), "state has field {field}");
    }
}
