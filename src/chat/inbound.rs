//! Inbound mediation message unwrap.
//!
//! Ported from Mostrix `chat_utils.rs::unwrap_giftwrap_with_shared_key`.
//! The full fetch loop (subscribe, dedup, stale handling, round-
//! counter advance) is US2 and is deliberately NOT implemented in
//! this slice — that work needs the mediation session state
//! machine in its US2 shape. This module ships only the wrap-level
//! primitive so the outbound path can be roundtrip-tested locally.

use nostr_sdk::prelude::*;

use crate::error::{Error, Result};

/// Unwrap a custom mostro-chat gift-wrap event with the per-party
/// shared keys. Returns `(inner_content, inner_created_at_secs,
/// inner_sender_pubkey)`. The inner event's signature is verified;
/// the outer gift-wrap's timestamp is ignored for session-fact
/// ordering, per `contracts/mostro-chat.md`.
pub fn unwrap_with_shared_key(
    shared_keys: &Keys,
    event: &Event,
) -> Result<(String, i64, PublicKey)> {
    // Decrypt: the reader holds `shared.sk` and pairs it with the
    // outer event's signer (the sender's ephemeral pubkey).
    let decrypted = nip44::decrypt(shared_keys.secret_key(), &event.pubkey, &event.content)
        .map_err(|e| Error::ChatTransport(format!("NIP-44 decrypt failed: {e}")))?;
    let inner = Event::from_json(&decrypted)
        .map_err(|e| Error::ChatTransport(format!("invalid inner chat event JSON: {e}")))?;
    inner
        .verify()
        .map_err(|e| Error::ChatTransport(format!("inner chat event signature invalid: {e}")))?;
    Ok((
        inner.content,
        inner.created_at.as_secs() as i64,
        inner.pubkey,
    ))
}
