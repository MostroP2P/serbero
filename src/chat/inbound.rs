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
//! - The inner event kind is pinned to `Kind::TextNote` (1). The
//!   mostro-chat contract ships inner payloads as `kind 1` text
//!   notes (Mostrix `build_custom_wrap_event` uses
//!   `EventBuilder::text_note`); anything else is rejected before
//!   it reaches the persistence layer.
//! - **Inner-signer authentication is enforced** against the
//!   expected per-party trade pubkey, not just "any signer whose
//!   signature is valid". The shared pubkey `p` tag on every
//!   outbound gift-wrap is public, and NIP-44 encryption requires
//!   only the recipient's *public* key — any third party can
//!   build a gift-wrap that decrypts with `shared.sk`. Without the
//!   author check, such a wrap would be attributed to Buyer /
//!   Seller. The caller passes the expected trade pubkey in
//!   [`PartyChatMaterial::expected_author`]; envelopes whose inner
//!   signer does not match are dropped at `warn!` level.
//! - "Implementation-verified against current Mostro/Mostrix
//!   behavior": the party's chat messages use `trade_keys` (not the
//!   shared keys) to sign the inner event, mirroring Mostrix
//!   `send_user_order_chat_message_via_shared_key`. Mostrix itself
//!   does not enforce the inner-author match; Serbero does, because
//!   attribution to a party is load-bearing for classification and
//!   later escalation (US3 / US4).

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
    /// The inner event's signer pubkey, already authenticated by
    /// `fetch_inbound` against `PartyChatMaterial.expected_author`
    /// (see module header). Retained on the envelope for forensic
    /// logging and for US3+ policy checks that want to correlate
    /// against the dispute-scoped trade pubkey.
    pub inner_sender: String,
}

/// Output of a successful gift-wrap unwrap: the full set of inner-
/// event facts every caller cares about. Keeping these together
/// means callers never re-decrypt the same wrap to extract an extra
/// field — and means the `event_id` that downstream dedup relies on
/// is always the one produced by the same verify pass that checked
/// the signature and the kind.
#[derive(Debug, Clone)]
pub struct UnwrappedInner {
    pub event_id: EventId,
    pub content: String,
    pub created_at: i64,
    pub sender: PublicKey,
}

/// Per-party chat material a caller must supply to [`fetch_inbound`].
/// Carries:
///
/// - `party`: the transcript-party label for attribution
///   (`Buyer` / `Seller`).
/// - `shared_keys`: the ECDH-derived shared key pair used to NIP-44
///   decrypt the gift-wrap. The data-model persists only the pubkey;
///   the secret lives in process memory for the session's lifetime
///   (see `data-model.md`).
/// - `expected_author`: the party's *trade-scoped* pubkey — the one
///   Mostro emits in `SolverDisputeInfo.buyer_pubkey` /
///   `seller_pubkey`. Used to authenticate the inner event's signer
///   so a third party who knows `shared_keys.public_key()` cannot
///   impersonate the party (NIP-44 encryption is public-key only;
///   the author check is what ties the envelope back to the
///   specific party identity).
#[derive(Debug, Clone)]
pub struct PartyChatMaterial<'a> {
    pub party: TranscriptParty,
    pub shared_keys: &'a Keys,
    pub expected_author: PublicKey,
}

/// Unwrap a custom mostro-chat gift-wrap event with the per-party
/// shared keys. Returns a fully-verified [`UnwrappedInner`] — the
/// caller never needs to re-decrypt or re-verify to extract an
/// additional field, which keeps the inbound event id that the DB
/// dedup relies on in lock-step with the signature + kind checks
/// that accepted the wrap in the first place.
///
/// Verification performed, in order:
///
/// 1. NIP-44 decrypt against `shared.sk`. The reader pairs its
///    shared secret with the outer event's signer (the ephemeral
///    pubkey) to recover the inner event JSON.
/// 2. Inner event parse.
/// 3. Inner event kind = [`Kind::TextNote`]. Mostro-chat ships only
///    kind-1 text notes; anything else is dropped before we run
///    signature verification, so a misbehaving peer cannot push
///    e.g. a channel-create or a replaceable-kind event through
///    the mediation transport.
/// 4. Inner event signature verify.
///
/// The outer gift-wrap's timestamp is ignored for session-fact
/// ordering, per `contracts/mostro-chat.md`.
pub fn unwrap_with_shared_key(shared_keys: &Keys, event: &Event) -> Result<UnwrappedInner> {
    let decrypted = nip44::decrypt(shared_keys.secret_key(), &event.pubkey, &event.content)
        .map_err(|e| Error::ChatTransport(format!("NIP-44 decrypt failed: {e}")))?;
    let inner = Event::from_json(&decrypted)
        .map_err(|e| Error::ChatTransport(format!("invalid inner chat event JSON: {e}")))?;
    if inner.kind != Kind::TextNote {
        return Err(Error::ChatTransport(format!(
            "inner chat event must be kind TextNote, got {}",
            inner.kind.as_u16()
        )));
    }
    inner
        .verify()
        .map_err(|e| Error::ChatTransport(format!("inner chat event signature invalid: {e}")))?;
    Ok(UnwrappedInner {
        event_id: inner.id,
        created_at: inner.created_at.as_secs() as i64,
        sender: inner.pubkey,
        content: inner.content,
    })
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
        // We cannot pre-filter at the relay by the expected inner
        // author: the outer gift-wrap is signed by a fresh
        // ephemeral key per wrap, so `authors()` on the outer
        // filter only selects ephemeral identities — which are
        // unpredictable by design. We therefore pay decrypt +
        // verify cost for every `p`-tag-matching candidate, then
        // drop wraps whose authenticated inner author does not
        // match. On a spammy relay this is a CPU hit; a smarter
        // relay-side filter would require changing the wire format
        // (e.g. a tag that commits to the inner author), which is
        // out of scope here.
        for wrapped in events.iter() {
            match unwrap_with_shared_key(party.shared_keys, wrapped) {
                Ok(inner) => {
                    // Authenticate the inner event's author against
                    // the expected trade pubkey. See module header —
                    // without this, any third party who has seen
                    // `shared_pubkey` on the relay could craft a
                    // decryptable wrap and have it attributed to the
                    // party.
                    if inner.sender != party.expected_author {
                        tracing::warn!(
                            party = %party.party,
                            outer_event_id = %wrapped.id.to_hex(),
                            expected_author = %party.expected_author.to_hex(),
                            actual_author = %inner.sender.to_hex(),
                            "dropping inbound gift-wrap: inner signer does not match expected party trade pubkey"
                        );
                        continue;
                    }
                    out.push(InboundEnvelope {
                        party: party.party,
                        shared_pubkey: shared_pubkey.to_hex(),
                        inner_event_id: inner.event_id.to_hex(),
                        inner_created_at: inner.created_at,
                        outer_event_id: wrapped.id.to_hex(),
                        content: inner.content,
                        inner_sender: inner.sender.to_hex(),
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

        let inner = unwrap_with_shared_key(&shared, &built.outer).unwrap();
        assert_eq!(inner.content, "buyer, please confirm");
        assert_eq!(inner.created_at, built.inner_created_at);
        assert_eq!(inner.event_id, built.inner_event_id);
        assert_eq!(
            inner.sender,
            serbero.public_key(),
            "inner signer must be the sender's keys, not the ephemeral outer signer"
        );
        assert_ne!(
            inner.sender, built.outer.pubkey,
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
        // is to flip a byte in the middle. We go through a Vec<u8>
        // so we never mutate the String's bytes under `unsafe`, and
        // then re-validate as UTF-8 on the way back. NIP-44 v2
        // ciphertext is base64 — still UTF-8 after a single-bit
        // flip in base64's ASCII range — but we surface any
        // pathological case loudly rather than relying on that.
        let mut bytes = built.outer.content.as_bytes().to_vec();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        let corrupted_content =
            String::from_utf8(bytes).expect("bit flip in base64 must stay valid UTF-8");

        // Re-sign with a fresh ephemeral key so the outer signature
        // is still valid — we want decrypt or inner verify to fail,
        // not outer signature verification.
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
    async fn unwrap_rejects_non_text_note_inner_kinds() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let shared = derive_shared_keys(&serbero, &buyer.public_key()).unwrap();

        // Build an inner event that is a channel-create (kind 40)
        // instead of a kind-1 text note, sign it legitimately, wrap
        // it normally. Decrypt + signature verification would both
        // succeed; only the kind guard should reject it.
        let inner = EventBuilder::new(Kind::Custom(40), "{\"name\":\"evil\"}")
            .build(serbero.public_key())
            .sign(&serbero)
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
        let wrap = EventBuilder::new(Kind::GiftWrap, encrypted)
            .tag(Tag::public_key(shared.public_key()))
            .custom_created_at(Timestamp::tweaked(nip59::RANGE_RANDOM_TIMESTAMP_TWEAK))
            .sign_with_keys(&ephem)
            .unwrap();

        let err = unwrap_with_shared_key(&shared, &wrap)
            .expect_err("non-TextNote inner must be rejected");
        assert!(
            err.to_string().contains("must be kind TextNote"),
            "error should flag the kind guard: {err}"
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
