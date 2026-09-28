//! The judge state (`docs/judgments.md` §1): JSON built by code from the
//! session and its messages.
//!
//! It never contains pubkeys, event ids, or anything a party did not write,
//! apart from the order facts. Identifiers Serbero knows (the parties' trade
//! pubkeys and the messages' event ids, as hex, `npub` or `note`) are
//! redacted even from party text, in case a party pastes one. Messages get short ids (`m1`, `m2`, …) in
//! transcript order; they are the only link between answers and text, and
//! `State::message_id` maps them back to stored rows.

use nostr_sdk::prelude::{EventId, PublicKey, ToBech32};
use serde_json::{Map, Value, json};

use crate::store::disputes::Initiator;
use crate::store::messages::{Direction, Message};
use crate::store::sessions::{Party, Session};

/// Fixed descriptions of who is who, in English (AGENTS.md: Jev is always
/// addressed in English).
const ROLE_BUYER: &str =
    "Pays the fiat money to the seller outside Mostro, then waits for the bitcoin.";
const ROLE_SELLER: &str = "Has bitcoin locked in Mostro escrow and must confirm the fiat arrived before the trade can finish.";
const ROLE_SERBERO: &str = "Automated assistant that asks both parties questions for the human solver. It cannot move funds or decide the dispute.";

/// A built state and the stored message behind each short id.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub value: Value,
    /// `message_ids[i]` is the stored id of `m{i+1}`.
    message_ids: Vec<i64>,
}

impl State {
    /// The stored message id behind a short id such as `m3`.
    pub fn message_id(&self, short_id: &str) -> Option<i64> {
        let index: usize = short_id.strip_prefix('m')?.parse().ok()?;
        self.message_ids.get(index.checked_sub(1)?).copied()
    }
}

/// Builds the state from a session, who opened the dispute, and the
/// session's messages in transcript order. Text is cut at `max_chars`
/// characters; attachments are counted, never included.
pub fn build(
    session: &Session,
    opened_by: Initiator,
    messages: &[Message],
    max_chars: usize,
) -> State {
    let known_ids = known_identifiers(session, messages);
    let mut transcript = Vec::with_capacity(messages.len());
    let mut latest_buyer = Vec::new();
    let mut latest_seller = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let id = format!("m{}", index + 1);
        let mut entry = Map::new();
        entry.insert("id".into(), json!(id));
        match message.direction {
            Direction::Out => {
                entry.insert("from".into(), json!("serbero"));
                entry.insert("to".into(), json!(message.party.to_string()));
            }
            Direction::In => {
                entry.insert("from".into(), json!(message.party.to_string()));
            }
        }
        let text = redact(&message.content, &known_ids);
        entry.insert("text".into(), json!(truncate(&text, max_chars)));
        if message.attachments > 0 {
            entry.insert("attachments".into(), json!(message.attachments));
        }
        transcript.push(Value::Object(entry));

        // `latest.<party>`: the party's messages since Serbero last wrote
        // to them.
        let latest = match message.party {
            Party::Buyer => &mut latest_buyer,
            Party::Seller => &mut latest_seller,
        };
        match message.direction {
            Direction::Out => latest.clear(),
            Direction::In => latest.push(id),
        }
    }

    State {
        value: json!({
            "roles": {
                "buyer": ROLE_BUYER,
                "seller": ROLE_SELLER,
                "serbero": ROLE_SERBERO,
            },
            "order": order(session, opened_by),
            "transcript": transcript,
            "latest": { "buyer": latest_buyer, "seller": latest_seller },
        }),
        message_ids: messages.iter().map(|m| m.id).collect(),
    }
}

/// The order facts Serbero may show; unknown facts are left out.
fn order(session: &Session, opened_by: Initiator) -> Value {
    let mut order = Map::new();
    let facts = [
        ("fiat_amount", &session.fiat_amount),
        ("fiat_code", &session.fiat_code),
        ("payment_method", &session.payment_method),
    ];
    for (name, value) in facts {
        if let Some(value) = value {
            order.insert(name.into(), json!(value));
        }
    }
    if opened_by != Initiator::Unknown {
        order.insert("dispute_opened_by".into(), json!(opened_by.to_string()));
    }
    Value::Object(order)
}

/// Placeholder for a redacted identifier.
const REDACTED: &str = "[redacted]";

/// Every form of the identifiers this session knows, lowercase.
fn known_identifiers(session: &Session, messages: &[Message]) -> Vec<String> {
    let mut ids = Vec::new();
    for key in [&session.buyer_trade_pubkey, &session.seller_trade_pubkey] {
        ids.push(key.to_ascii_lowercase());
        ids.extend(PublicKey::parse(key).ok().and_then(|k| k.to_bech32().ok()));
    }
    for message in messages {
        ids.push(message.inner_event_id.to_ascii_lowercase());
        ids.extend(
            EventId::parse(&message.inner_event_id)
                .ok()
                .and_then(|id| id.to_bech32().ok()),
        );
    }
    ids.retain(|id| !id.is_empty());
    ids
}

/// Replaces every occurrence of a known identifier, ignoring ASCII case.
fn redact(text: &str, identifiers: &[String]) -> String {
    let mut text = text.to_owned();
    for id in identifiers {
        // Identifiers are ASCII, so lowercasing keeps byte offsets.
        while let Some(start) = text.to_ascii_lowercase().find(id.as_str()) {
            text.replace_range(start..start + id.len(), REDACTED);
        }
    }
    text
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((cut, _)) => &text[..cut],
        None => text,
    }
}

#[cfg(test)]
mod tests;
