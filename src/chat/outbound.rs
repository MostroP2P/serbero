//! Outbound mediation message construction.
//!
//! Ported from Mostrix `chat_utils.rs::build_custom_wrap_event` +
//! `send_admin_chat_message_via_shared_key`. Do NOT swap this out
//! for nostr-sdk's standard `send_private_msg` — the mostro-chat
//! wrap is a *custom* variant of NIP-59:
//!
//! - Inner event: `kind 1` text note, signed by the **sender's
//!   keys** (e.g. Serbero's solver identity).
//! - Encryption: NIP-44 between a fresh ephemeral key and the
//!   `shared_pubkey` (i.e. the per-party chat pubkey produced by
//!   [`crate::chat::shared_key::derive_shared_keys`]).
//! - Outer event: `Kind::GiftWrap` (1059), signed by the ephemeral
//!   key, with a `p` tag pointing at `shared_pubkey`, and a
//!   NIP-59 tweaked `created_at` timestamp.
//!
//! Addressing mediation content to a party's **primary** pubkey is
//! forbidden. The function signature enforces this: the caller
//! cannot pass a party pubkey directly — it must pass the
//! `shared_keys` returned by `derive_shared_keys`.

use nostr_sdk::prelude::*;

use crate::error::{Error, Result};

/// Result of building a mediation chat wrap. The caller needs the
/// `inner_event_id` and `inner_created_at` to persist the outbound
/// row in `mediation_messages` (the unique index on
/// `(session_id, inner_event_id)` is the dedup primary key).
#[derive(Debug)]
pub struct BuiltWrap {
    pub outer: Event,
    pub inner_event_id: EventId,
    pub inner_created_at: i64,
}

/// Send a mediation chat message from Serbero to a party via the
/// per-party shared key. Returns the outer gift-wrap event id so
/// callers can persist it in `mediation_messages.outer_event_id`.
pub async fn send_chat_message(
    client: &Client,
    sender_keys: &Keys,
    shared_keys: &Keys,
    content: &str,
) -> Result<EventId> {
    let built = build_wrap(sender_keys, &shared_keys.public_key(), content).await?;
    let outer_id = built.outer.id;
    client
        .send_event(&built.outer)
        .await
        .map_err(|e| Error::ChatTransport(format!("failed to publish chat gift-wrap: {e}")))?;
    Ok(outer_id)
}

/// Build the custom gift-wrap without publishing. Returns both the
/// outer event (for transmission) and the inner event's id +
/// timestamp (for persistence). Exposed for tests and for
/// `session::open_session` which needs the inner metadata to
/// insert the `mediation_messages` rows.
pub async fn build_wrap(
    sender_keys: &Keys,
    shared_pubkey: &PublicKey,
    message: &str,
) -> Result<BuiltWrap> {
    // Guard empty / whitespace-only content here (not just in
    // `send_chat_message`) so direct callers like
    // `mediation::session::open_session` cannot persist a
    // mediation_messages row for a message that will never be a
    // meaningful clarification.
    if message.trim().is_empty() {
        return Err(Error::ChatTransport(
            "refusing to build mediation chat wrap with empty content".into(),
        ));
    }

    // Inner event: a plain kind-1 text note, signed by the sender's
    // keys (Mostrix comment: "Message is just sent inside rumor as
    // per https://mostro.network/protocol/chat.html please check
    // that.").
    let inner_event = EventBuilder::text_note(message)
        .build(sender_keys.public_key())
        .sign(sender_keys)
        .await
        .map_err(|e| Error::ChatTransport(format!("failed to sign inner chat event: {e}")))?;
    let inner_event_id = inner_event.id;
    let inner_created_at = inner_event.created_at.as_secs() as i64;

    // Ephemeral key for the outer wrap. Regenerated per message so
    // the outer event is unlinkable across messages.
    let ephem_key = Keys::generate();

    // NIP-44 v2 encrypt the inner JSON toward the shared pubkey.
    // Both sides derive the same symmetric key: the sender holds
    // `ephem.sk` and the reader holds `shared.sk`; ECDH of
    // `(ephem.sk, shared.pk)` equals `(shared.sk, ephem.pk)`.
    let encrypted = nip44::encrypt(
        ephem_key.secret_key(),
        shared_pubkey,
        inner_event.as_json(),
        nip44::Version::V2,
    )
    .map_err(|e| Error::ChatTransport(format!("NIP-44 encrypt failed: {e}")))?;

    let outer = EventBuilder::new(Kind::GiftWrap, encrypted)
        .tag(Tag::public_key(*shared_pubkey))
        // NIP-59 random timestamp tweak — prevents the outer event's
        // `created_at` from leaking the real send time.
        .custom_created_at(Timestamp::tweaked(nip59::RANGE_RANDOM_TIMESTAMP_TWEAK))
        .sign_with_keys(&ephem_key)
        .map_err(|e| Error::ChatTransport(format!("failed to sign gift-wrap: {e}")))?;

    Ok(BuiltWrap {
        outer,
        inner_event_id,
        inner_created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::inbound::unwrap_with_shared_key;
    use crate::chat::shared_key::derive_shared_keys;

    /// End-to-end roundtrip against a live `nostr-sdk` keypair /
    /// NIP-44 implementation: build an outbound wrap with
    /// sender=Serbero and receiver=shared-key-for-buyer, then unwrap
    /// with the same shared key and verify the inner event's
    /// content and signer.
    #[tokio::test]
    async fn outbound_roundtrips_through_inbound() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let shared = derive_shared_keys(&serbero, &buyer.public_key()).unwrap();

        let built = build_wrap(&serbero, &shared.public_key(), "first clarifying question")
            .await
            .unwrap();
        let event = &built.outer;

        // Outer event shape matches the contract (GiftWrap signed by
        // ephemeral key, `p` tag = shared pubkey, content is encrypted).
        assert_eq!(event.kind, Kind::GiftWrap);
        assert_ne!(
            event.pubkey,
            serbero.public_key(),
            "outer signer must be ephemeral"
        );
        assert!(event
            .tags
            .iter()
            .any(|t| matches!(t.kind(), TagKind::SingleLetter(slt) if slt.as_char() == 'p')));

        // The returned inner metadata matches what the reader will
        // compute after decrypt — no re-derivation needed elsewhere.
        let inner = unwrap_with_shared_key(&shared, event).unwrap();
        assert_eq!(inner.content, "first clarifying question");
        assert_eq!(inner.created_at, built.inner_created_at);
        assert_eq!(inner.sender, serbero.public_key());
        assert_eq!(
            inner.event_id, built.inner_event_id,
            "reader-computed inner event id must match the builder's report"
        );
    }

    /// Empty / whitespace-only content is refused by `build_wrap`
    /// itself — both the top-level send entry and the direct-build
    /// call sites in `mediation::session::open_session` are covered
    /// without needing a live relay.
    #[tokio::test]
    async fn empty_content_is_rejected_by_build_wrap() {
        let sender = Keys::generate();
        let shared = Keys::generate();
        for bad in ["", "   ", "\n\t"] {
            let err = build_wrap(&sender, &shared.public_key(), bad)
                .await
                .expect_err("build_wrap must refuse empty / whitespace-only content");
            let msg = err.to_string();
            assert!(
                msg.contains("empty content"),
                "unexpected error for {bad:?}: {msg}"
            );
        }
    }
}
