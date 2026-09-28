//! The judge state (`docs/judgments.md` §1): JSON built by code from the
//! session and its messages.
//!
//! It never contains pubkeys, event ids, or anything a party did not write,
//! apart from the order facts. Nostr identifiers are redacted even from
//! party text, in case a party pastes one: any 64-character hex word (a key
//! or event id) and any word that parses as NIP-19 (`npub`, `nsec`, `note`,
//! `nprofile`, `nevent`, `naddr`), from this session or any other. Messages get short ids (`m1`, `m2`, …) in
//! transcript order; they are the only link between answers and text, and
//! `State::message_id` maps them back to stored rows.

use nostr_sdk::prelude::{FromBech32, Nip19};
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
        let text = redact(&message.content);
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

/// Replaces every word that is a Nostr identifier. A word is a maximal run
/// of ASCII letters and digits, so `nostr:npub1…` and `(note1…)` are found.
fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word_start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if c.is_ascii_alphanumeric() && i < text.len() {
            word_start.get_or_insert(i);
            continue;
        }
        if let Some(start) = word_start.take() {
            let word = &text[start..i];
            out.push_str(if is_nostr_identifier(word) {
                REDACTED
            } else {
                word
            });
        }
        if i < text.len() {
            out.push(c);
        }
    }
    out
}

fn is_nostr_identifier(word: &str) -> bool {
    if word.len() == 64 && word.bytes().all(|b| b.is_ascii_hexdigit()) {
        return true;
    }
    // Bech32 may be all uppercase; NIP-19 parses the lowercase form.
    let lower = word.to_ascii_lowercase();
    lower.starts_with('n') && Nip19::from_bech32(&lower).is_ok()
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((cut, _)) => &text[..cut],
        None => text,
    }
}

#[cfg(test)]
mod tests;
