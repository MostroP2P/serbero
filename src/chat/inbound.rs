//! Inbound mediation message unwrap + fetch.
//!
//! Ported from Mostrix `chat_utils.rs::unwrap_giftwrap_with_shared_key`
//! and `chat_utils.rs::fetch_gift_wraps_for_shared_key`. This module
//! owns the read side of the mediation chat transport:
//!
//! - [`unwrap_with_shared_key`]: decrypt one NIP-44 gift-wrap with the
//!   per-party shared key, parse the inner `kind 1` event, verify its
//!   signature, and return the authoritative `(content, created_at,
//!   sender_pubkey)` tuple. This is the primitive the outbound-path
//!   roundtrip tests already exercise.
//! - [`fetch_inbound`]: pull every candidate gift-wrap on the relay
//!   whose `p` tag matches either party's shared pubkey, unwrap it,
//!   and emit [`InboundEnvelope`] rows tagged with the authoring
//!   party. Ordered ascending by inner `created_at` so downstream
//!   ingest advances the session's per-party last-seen marker
//!   monotonically.
//!
//! Verification discipline:
//! - The **inner** event's `created_at` is the authoritative
//!   timestamp for session facts. The outer gift-wrap's `created_at`
//!   is tweaked by NIP-59 into a ±2-day window and MUST NOT be used
//!   for ordering or last-seen markers (see `contracts/mostro-chat.md`
//!   §Inner-event timestamps are authoritative).
//! - Inner event signatures are verified before the envelope is
//!   returned. A tampered or re-signed inner payload is dropped on
//!   the floor with a `ChatTransport` error on that specific event;
//!   the rest of the batch still flows.
//! - "Implementation-verified against current Mostro/Mostrix
//!   behavior": the party's chat messages use `trade_keys` (not the
//!   shared keys) to sign the inner event, mirroring Mostrix
//!   `send_user_order_chat_message_via_shared_key`. Serbero's own
//!   messages are signed by its solver identity. We do NOT pin the
//!   inner signer to any specific expected pubkey at this layer —
//!   that is a policy-layer concern (US3+) since a dispute-scoped
//!   trade pubkey is not persisted in the US1 session row.

use std::time::Duration;

use nostr_sdk::prelude::*;

use crate::error::{Error, Result};
use crate::models::mediation::TranscriptParty;

/// One inbound mediation chat envelope as observed on the relay.
///
/// Contains the minimum the persistence layer needs to insert a
/// `mediation_messages` row plus the metadata the session-state
/// update needs to advance last-seen markers. The envelope is
/// caller-neutral: it doesn't know the session_id — callers zip
/// that in from the session they're ingesting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEnvelope {
    pub party: TranscriptParty,
    pub shared_pubkey: String,
    pub inner_event_id: String,
    pub inner_created_at: i64,
    pub outer_event_id: String,
    pub content: String,
    /// The inner event's signer pubkey. Not used for gating in US2
    /// (we don't have a persisted trade-pubkey to compare against),
    /// but captured for future policy checks + forensic logging.
    pub inner_sender: String,
}

/// Per-party chat material a caller must supply to [`fetch_inbound`].
/// Carries both the shared pubkey (used as the `p` tag filter) and
/// the shared secret `Keys` (used to NIP-44 decrypt). The data-model
/// persists only the pubkey; the secret lives in process memory for
/// the session's lifetime (see `data-model.md`).
#[derive(Debug, Clone)]
pub struct PartyChatMaterial<'a> {
    pub party: TranscriptParty,
    pub shared_keys: &'a Keys,
}

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

/// Fetch inbound mediation chat envelopes addressed to either party's
/// shared pubkey.
///
/// Each candidate gift-wrap is unwrapped + verified with the
/// corresponding party's shared keys; events that fail decrypt or
/// signature verification are skipped at `warn!` level and do NOT
/// fail the batch (one corrupt event from a misbehaving peer must
/// not poison the entire ingest cycle).
///
/// Returned envelopes are sorted ascending by inner `created_at` so
/// the caller can apply them in chronological order against the
/// per-party last-seen marker. Ordering uses the **inner** timestamp
/// exclusively (the outer NIP-59 tweaked timestamp is not reliable —
/// see module header).
pub async fn fetch_inbound(
    client: &Client,
    parties: &[PartyChatMaterial<'_>],
    fetch_timeout: Duration,
) -> Result<Vec<InboundEnvelope>> {
    // NIP-59 deliberately tweaks gift-wrap `created_at` up to 2 days
    // into the past, so `since(now)` would drop real events. We
    // widen the window to 7 days to match Mostrix's
    // `fetch_gift_wraps_for_shared_key`.
    let now = Timestamp::now();
    let since_window = Timestamp::from_secs(now.as_secs().saturating_sub(7 * 24 * 60 * 60));

    let mut out: Vec<InboundEnvelope> = Vec::new();

    for party in parties {
        let shared_pubkey = party.shared_keys.public_key();
        let filter = Filter::new()
            .kind(Kind::GiftWrap)
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::P),
                shared_pubkey.to_hex(),
            )
            .since(since_window);
        let events = client
            .fetch_events(filter, fetch_timeout)
            .await
            .map_err(|e| {
                Error::ChatTransport(format!(
                    "fetch_events failed for party shared pubkey {}: {}",
                    shared_pubkey.to_hex(),
                    e
                ))
            })?;
        tracing::trace!(
            party = %party.party,
            count = events.len(),
            "inbound fetch: candidates for shared pubkey"
        );
        for wrapped in events.iter() {
            match unwrap_with_shared_key(party.shared_keys, wrapped) {
                Ok((content, inner_ts, inner_sender)) => {
                    out.push(InboundEnvelope {
                        party: party.party,
                        shared_pubkey: shared_pubkey.to_hex(),
                        inner_event_id: extract_inner_event_id(wrapped, party.shared_keys)?,
                        inner_created_at: inner_ts,
                        outer_event_id: wrapped.id.to_hex(),
                        content,
                        inner_sender: inner_sender.to_hex(),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        party = %party.party,
                        outer_event_id = %wrapped.id.to_hex(),
                        error = %e,
                        "dropping inbound gift-wrap that failed decrypt / verify"
                    );
                }
            }
        }
    }

    // Monotonic inner-timestamp order. Stable sort keeps per-party
    // original order when two events share a timestamp — relevant
    // on the rare same-second boundary.
    out.sort_by_key(|e| e.inner_created_at);
    Ok(out)
}

/// Re-derive the inner event's id. We need the stable inner id (not
/// the outer gift-wrap id) because the unique DB index is on
/// `(session_id, inner_event_id)`. `unwrap_with_shared_key` already
/// computes the inner event internally; exposing the id would add a
/// public API surface. For simplicity we re-decrypt here; the cost
/// is one extra NIP-44 call per candidate, which is negligible
/// relative to the network I/O of `fetch_events`. If this becomes
/// hot, merging the two decrypt paths is a mechanical follow-up.
fn extract_inner_event_id(wrapped: &Event, shared_keys: &Keys) -> Result<String> {
    let decrypted = nip44::decrypt(shared_keys.secret_key(), &wrapped.pubkey, &wrapped.content)
        .map_err(|e| Error::ChatTransport(format!("NIP-44 decrypt (for id) failed: {e}")))?;
    let inner = Event::from_json(&decrypted)
        .map_err(|e| Error::ChatTransport(format!("invalid inner chat event JSON: {e}")))?;
    Ok(inner.id.to_hex())
}

#[cfg(test)]
mod tests {
    //! T048 — inner-event verification discipline.
    //!
    //! These tests exercise `unwrap_with_shared_key` directly against
    //! fixture wraps built with the outbound module, so the pair
    //! stays in sync with what Mostrix-style senders produce on the
    //! wire.

    use super::*;
    use crate::chat::outbound::build_wrap;
    use crate::chat::shared_key::derive_shared_keys;

    #[tokio::test]
    async fn unwrap_returns_inner_content_and_signer_not_outer_metadata() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let shared = derive_shared_keys(&serbero, &buyer.public_key()).unwrap();

        let built = build_wrap(&serbero, &shared.public_key(), "buyer, please confirm")
            .await
            .unwrap();

        let (content, ts, signer) = unwrap_with_shared_key(&shared, &built.outer).unwrap();
        assert_eq!(content, "buyer, please confirm");
        assert_eq!(ts, built.inner_created_at);
        assert_eq!(
            signer,
            serbero.public_key(),
            "inner signer must be the sender's keys, not the ephemeral outer signer"
        );
        assert_ne!(
            signer, built.outer.pubkey,
            "the outer signer is the NIP-59 ephemeral key and must not be reported as the inner sender"
        );
    }

    #[tokio::test]
    async fn unwrap_rejects_tampered_inner_ciphertext() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let shared = derive_shared_keys(&serbero, &buyer.public_key()).unwrap();

        let built = build_wrap(&serbero, &shared.public_key(), "original message")
            .await
            .unwrap();

        // Corrupt the outer `content` (the NIP-44 ciphertext). The
        // simplest way to get a deterministic-but-invalid ciphertext
        // is to flip a byte in the middle. Build the corrupted event
        // by re-signing with a fresh ephemeral key so the outer
        // signature itself is still valid — we want decrypt or inner
        // verify to fail, not outer signature verification.
        let mut corrupted_content = built.outer.content.clone();
        let mid = corrupted_content.len() / 2;
        let bytes = unsafe { corrupted_content.as_bytes_mut() };
        bytes[mid] ^= 0x01;

        let ephem = Keys::generate();
        let tampered = EventBuilder::new(Kind::GiftWrap, corrupted_content)
            .tag(Tag::public_key(shared.public_key()))
            .custom_created_at(built.outer.created_at)
            .sign_with_keys(&ephem)
            .unwrap();

        let err = unwrap_with_shared_key(&shared, &tampered)
            .expect_err("tampered ciphertext must not produce a verified inner event");
        let msg = err.to_string();
        assert!(
            msg.contains("NIP-44 decrypt failed")
                || msg.contains("invalid inner chat event JSON")
                || msg.contains("signature invalid"),
            "error should name the verification stage that failed: {msg}"
        );
    }

    #[tokio::test]
    async fn unwrap_rejects_inner_resigned_by_wrong_key() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let attacker = Keys::generate();
        let shared = derive_shared_keys(&serbero, &buyer.public_key()).unwrap();

        // Build a legitimate-looking wrap but with an inner event
        // whose `pubkey` field names Serbero's key while the
        // signature was produced by the attacker. The inner event's
        // `verify()` pairs the content hash + signature against the
        // declared pubkey, so this must fail.
        let inner = EventBuilder::text_note("forged content")
            .build(serbero.public_key())
            .sign(&attacker)
            .await
            .unwrap();
        let ephem = Keys::generate();
        let encrypted = nip44::encrypt(
            ephem.secret_key(),
            &shared.public_key(),
            inner.as_json(),
            nip44::Version::V2,
        )
        .unwrap();
        let forged = EventBuilder::new(Kind::GiftWrap, encrypted)
            .tag(Tag::public_key(shared.public_key()))
            .custom_created_at(Timestamp::tweaked(nip59::RANGE_RANDOM_TIMESTAMP_TWEAK))
            .sign_with_keys(&ephem)
            .unwrap();

        let err = unwrap_with_shared_key(&shared, &forged)
            .expect_err("inner event signed by a different key must be rejected");
        assert!(
            err.to_string().contains("signature invalid"),
            "expected signature-invalid error, got {err}"
        );
    }
}
