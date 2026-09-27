//! Serbero's dispute chat with each party (`docs/spec.md` §5.1, §7.2–§7.3).
//!
//! Every party has its own channel with Serbero. Outbound messages are
//! `kind 1` events signed by Serbero, wrapped by `mostro-core` into a
//! `kind 14` signed with `K_sign` and addressed to `pub(K_conv)`. Inbound
//! messages are validated in the protocol's order (see `inbound`).

pub mod inbound;

use std::sync::Mutex;

use mostro_core::chat::{unwrap_chat_message, wrap_chat_message};
use nostr_sdk::prelude::*;

use crate::error::{Error, Result};
use crate::mostro::chat::ChannelKeys;
use crate::store::Store;
use crate::store::messages::{self, Direction, NewMessage};
use crate::store::sessions::{Party, Session};

/// The channel between Serbero and one party of a session.
pub fn channel(serbero: &Keys, session: &Session, party: Party) -> Result<ChannelKeys> {
    let trade =
        crate::nostr::public_key(&format!("{party} trade key"), session.trade_pubkey(party))?;
    ChannelKeys::derive(serbero, &trade)
}

/// An outbound message and what to record about it.
pub struct Outbound<'a> {
    pub party: Party,
    pub text: &'a str,
    /// Catalog template that produced `text`.
    pub template_id: Option<&'a str>,
    /// Language `text` is in.
    pub lang: Option<&'a str>,
}

/// Sends `message` to a party and records it in `messages`. Fails if no relay
/// accepts the event; nothing is recorded then. Returns the inner event id.
pub async fn send_to_party(
    client: &Client,
    store: &Mutex<Store>,
    serbero: &Keys,
    session: &Session,
    message: &Outbound<'_>,
) -> Result<EventId> {
    let keys = channel(serbero, session, message.party)?;
    let event = wrap_chat_message(serbero, keys.conv(), keys.sign(), message.text)
        .await
        .map_err(|e| Error::Nostr(format!("cannot wrap chat message: {e}")))?;
    // Read our own envelope back to learn the inner id we store for dedup.
    let inner = unwrap_chat_message(
        keys.conv(),
        &keys.author_pubkey(),
        &[serbero.public_key()],
        &event,
        Timestamp::now(),
    )
    .map_err(|e| Error::Nostr(format!("cannot read own chat message: {e}")))?;
    let output = client
        .send_event(&event)
        .await
        .map_err(|e| Error::Nostr(format!("cannot send chat message: {e}")))?;
    if output.success.is_empty() {
        return Err(Error::Nostr("no relay accepted the chat message".into()));
    }
    let store = store
        .lock()
        .map_err(|_| Error::Schema("store lock poisoned".into()))?;
    messages::insert_if_new(
        store.conn(),
        &NewMessage {
            session_id: &session.session_id,
            direction: Direction::Out,
            party: message.party,
            template_id: message.template_id,
            lang: message.lang,
            content: message.text,
            attachments: 0,
            inner_event_id: &inner.inner_event_id.to_hex(),
            created_at: inner.created_at.as_secs() as i64,
        },
    )?;
    Ok(inner.inner_event_id)
}
