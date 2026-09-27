//! Inbound party messages, validated in the Mostro chat protocol's
//! cheapest-first order (`docs/spec.md` §5.1; step numbers are the
//! protocol's):
//!
//! - 1: the author is a known channel's `pub(K_sign)` (a lookup, no crypto);
//! - 5: the outer id was not seen recently (bounded LRU);
//! - 6: per-channel rate limit, before any crypto;
//! - 2–4, 7–11, 13: `mostro-core`'s `unwrap_chat_message` (`p` tag, clock
//!   skew, size, outer signature, decryption, inner signature, inner signer
//!   is that party's trade key, inner kind, inner/outer time agreement);
//! - 12: durable inner-id dedup (the `messages` table).
//!
//! Each session has its own subscription id, so opening or closing a session
//! never replaces a shared subscription (AGENTS.md, relay rule 6).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

use mostro_core::chat::unwrap_chat_message;
use nostr_sdk::prelude::*;

use crate::error::{Error, Result};
use crate::mostro::chat::ChannelKeys;
use crate::store::Store;
use crate::store::messages::{self, Direction, NewMessage};
use crate::store::sessions::{self, Party, Session};

/// Sustained messages per minute per channel, and the burst allowed
/// (the protocol's recommendation).
const RATE_PER_MINUTE: f64 = 30.0;
const RATE_BURST: f64 = 60.0;

/// Outer event ids remembered to drop duplicate relay deliveries cheaply.
const SEEN_OUTER_CAPACITY: usize = 4_096;

/// A message from a party, accepted and stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub session_id: String,
    pub party: Party,
    /// The text, or a marker such as `[image attachment]`.
    pub content: String,
    pub attachments: u32,
    pub created_at: i64,
}

/// Why an inbound event was not accepted. Nothing here is an error for the
/// daemon: invalid or abusive input is expected and simply dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    UnknownAuthor,
    DuplicateOuter,
    RateLimited,
    Invalid(String),
    DuplicateInner,
    /// The session is closed or superseded.
    SessionEnded,
}

struct Channel {
    session_id: String,
    party: Party,
    trade_pubkey: PublicKey,
    keys: ChannelKeys,
    bucket: TokenBucket,
}

/// Default for `[mediation].max_message_chars`.
pub const DEFAULT_MAX_MESSAGE_CHARS: usize = 2_000;

/// Every live chat channel, keyed by its author, `pub(K_sign)`.
pub struct Inbox {
    channels: HashMap<PublicKey, Channel>,
    seen_outer: SeenSet,
    max_message_chars: usize,
}

impl Default for Inbox {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_MESSAGE_CHARS)
    }
}

impl Inbox {
    /// An empty inbox that truncates stored text at `max_message_chars`
    /// (`[mediation].max_message_chars`, `docs/spec.md` §7.3).
    pub fn new(max_message_chars: usize) -> Self {
        Self {
            channels: HashMap::new(),
            seen_outer: SeenSet::default(),
            max_message_chars,
        }
    }

    /// Registers both channels of a session. Returns their subscription
    /// filter: the two authors, from the oldest stored cursor on.
    pub fn add_session(
        &mut self,
        serbero: &Keys,
        session: &Session,
        now: Instant,
    ) -> Result<Filter> {
        let mut authors = Vec::with_capacity(2);
        for party in [Party::Buyer, Party::Seller] {
            let trade_pubkey = crate::nostr::public_key(
                &format!("{party} trade key"),
                session.trade_pubkey(party),
            )?;
            let keys = ChannelKeys::derive(serbero, &trade_pubkey)?;
            authors.push(keys.author_pubkey());
            self.channels.insert(
                keys.author_pubkey(),
                Channel {
                    session_id: session.session_id.clone(),
                    party,
                    trade_pubkey,
                    keys,
                    bucket: TokenBucket::new(now),
                },
            );
        }
        // A party with no cursor yet has sent nothing Serbero stored, so its
        // channel is read from the session's opening; replies sent while
        // Serbero was down are never cut off by a rolling window.
        let since = [session.buyer_chat_cursor, session.seller_chat_cursor]
            .into_iter()
            .map(|c| c.unwrap_or(session.opened_at).max(0) as u64)
            .min()
            .unwrap_or(0);
        Ok(Filter::new()
            .kind(Kind::PrivateDirectMessage)
            .authors(authors)
            .since(Timestamp::from_secs(since)))
    }

    /// Forgets a session's channels.
    pub fn remove_session(&mut self, session_id: &str) {
        self.channels.retain(|_, c| c.session_id != session_id);
    }

    /// Validates and stores one inbound event. `local_now` is Serbero's clock.
    pub fn handle(
        &mut self,
        store: &Mutex<Store>,
        event: &Event,
        local_now: Timestamp,
        now: Instant,
    ) -> Result<std::result::Result<Received, Rejected>> {
        // 1. Author: a lookup, no crypto.
        let Some(channel) = self.channels.get_mut(&event.pubkey) else {
            return Ok(Err(Rejected::UnknownAuthor));
        };
        // A session closed or superseded since the channel was opened gets
        // nothing more: forget its channels and drop the event.
        let session_id = channel.session_id.clone();
        let ended = {
            let store = store
                .lock()
                .map_err(|_| Error::Schema("store lock poisoned".into()))?;
            sessions::get(store.conn(), &session_id)?.is_none_or(|s| s.state.is_terminal())
        };
        if ended {
            self.remove_session(&session_id);
            return Ok(Err(Rejected::SessionEnded));
        }
        let Some(channel) = self.channels.get_mut(&event.pubkey) else {
            return Ok(Err(Rejected::UnknownAuthor));
        };
        // 5. Duplicate relay delivery.
        if !self.seen_outer.insert(event.id) {
            return Ok(Err(Rejected::DuplicateOuter));
        }
        // 6. Rate limit before any crypto.
        if !channel.bucket.take(now) {
            return Ok(Err(Rejected::RateLimited));
        }
        // 2–4, 7–11, 13.
        let message = match unwrap_chat_message(
            channel.keys.conv(),
            &channel.keys.author_pubkey(),
            &[channel.trade_pubkey],
            event,
            local_now,
        ) {
            Ok(message) => message,
            Err(e) => return Ok(Err(Rejected::Invalid(e.to_string()))),
        };
        let (content, attachments) = describe(&message.content);
        let content = truncate(&content, self.max_message_chars);
        let created_at = message.created_at.as_secs() as i64;
        let store = store
            .lock()
            .map_err(|_| Error::Schema("store lock poisoned".into()))?;
        // 12. Durable inner-id dedup.
        let stored = messages::insert_if_new(
            store.conn(),
            &NewMessage {
                session_id: &channel.session_id,
                direction: Direction::In,
                party: channel.party,
                template_id: None,
                lang: None,
                content: &content,
                attachments,
                inner_event_id: &message.inner_event_id.to_hex(),
                created_at,
            },
        )?;
        if !stored {
            return Ok(Err(Rejected::DuplicateInner));
        }
        // Relays filter `since` on the outer `created_at`, so that is what the
        // cursor tracks; the inner time only orders the transcript. The
        // cursor never passes Serbero's own clock: a peer setting both
        // timestamps in the future must not blind the subscription.
        let cursor = (event.created_at.as_secs() as i64).min(local_now.as_secs() as i64);
        sessions::advance_chat_cursor(
            store.conn(),
            &channel.session_id,
            channel.party,
            cursor,
            local_now.as_secs() as i64,
        )?;
        Ok(Ok(Received {
            session_id: channel.session_id.clone(),
            party: channel.party,
            content,
            attachments,
            created_at,
        }))
    }
}

/// Mostro clients send attachments as JSON (`image_encrypted` /
/// `file_encrypted`, with a Blossom URL and a nonce). Serbero counts them and
/// keeps only a marker, never the URL or the nonce.
fn describe(content: &str) -> (String, u32) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return (content.to_owned(), 0);
    };
    match value.get("type").and_then(|t| t.as_str()) {
        Some("image_encrypted") => ("[image attachment]".into(), 1),
        Some("file_encrypted") => {
            let kind = value
                .get("file_type")
                .and_then(|t| t.as_str())
                .unwrap_or("file");
            (format!("[{kind} attachment]"), 1)
        }
        _ => (content.to_owned(), 0),
    }
}

/// Keeps at most `max` characters, cutting on a character boundary.
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => text[..cut].to_owned(),
        None => text.to_owned(),
    }
}

struct TokenBucket {
    tokens: f64,
    refilled: Instant,
}

impl TokenBucket {
    fn new(now: Instant) -> Self {
        Self {
            tokens: RATE_BURST,
            refilled: now,
        }
    }

    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.refilled).as_secs_f64();
        self.tokens = (self.tokens + elapsed * RATE_PER_MINUTE / 60.0).min(RATE_BURST);
        self.refilled = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
struct SeenSet {
    ids: HashSet<EventId>,
    order: VecDeque<EventId>,
}

impl SeenSet {
    /// Returns `false` if `id` was already seen.
    fn insert(&mut self, id: EventId) -> bool {
        if !self.ids.insert(id) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > SEEN_OUTER_CAPACITY
            && let Some(oldest) = self.order.pop_front()
        {
            self.ids.remove(&oldest);
        }
        true
    }
}

#[cfg(test)]
mod tests;
