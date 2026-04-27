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
    build_wrap_with_audience(sender_keys, shared_pubkey, message, None).await
}

/// Variant of [`build_wrap`] that adds a per-party `audience` tag to
/// the inner event. The tag is non-content metadata and is NOT
/// rendered by Mostro chat clients (which display only `inner.content`),
/// but it changes the inner event id — so two simultaneous outbound
/// messages whose text is identical between buyer and seller still
/// produce distinct ids and don't violate the
/// `(session_id, inner_event_id)` uniqueness invariant on
/// `mediation_messages`. This is what lets the mediation drafters
/// drop the visible `Buyer: ` / `Seller: ` / `Round N.` content
/// prefixes that previously leaked transcript scaffolding into the
/// user-facing message stream (observed 2026-04-27).
pub async fn build_wrap_with_audience(
    sender_keys: &Keys,
    shared_pubkey: &PublicKey,
    message: &str,
    audience: Option<&str>,
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
    let mut inner_builder = EventBuilder::text_note(message);
    if let Some(aud) = audience {
        // `m-aud` = "mediation audience". Two-letter prefix avoids
        // collisions with single-letter NIP-defined tag kinds and
        // signals "metadata, not user-visible content" to anything
        // walking the event tags.
        inner_builder = inner_builder.tag(Tag::custom(
            TagKind::custom("m-aud"),
            [aud.to_string()],
        ));
    }
    let inner_event = inner_builder
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

    /// The whole point of [`build_wrap_with_audience`] is to keep the
    /// `(session_id, inner_event_id)` uniqueness invariant on
    /// `mediation_messages` even when the user-visible body is
    /// identical between the buyer and seller wraps. The `m-aud`
    /// tag is metadata-only (Mostro chat clients render only
    /// `inner.content`), but it changes the inner event id because
    /// the id is `sha256(serialized_event)` and the tag is part of
    /// the serialization.
    #[tokio::test]
    async fn audience_tag_makes_inner_event_id_unique_for_identical_body() {
        let sender = Keys::generate();
        let shared = Keys::generate();
        let body = "Please confirm the fiat payment timing for this trade.";

        let buyer_wrap = build_wrap_with_audience(&sender, &shared.public_key(), body, Some("buyer"))
            .await
            .unwrap();
        let seller_wrap =
            build_wrap_with_audience(&sender, &shared.public_key(), body, Some("seller"))
                .await
                .unwrap();

        assert_ne!(
            buyer_wrap.inner_event_id, seller_wrap.inner_event_id,
            "different audience values MUST yield distinct inner event ids; otherwise the \
             (session_id, inner_event_id) uniqueness on mediation_messages would collide \
             when buyer and seller receive identical body text in the same round"
        );
    }

    /// Decrypt the outer wrap with the shared key and re-parse the
    /// inner Event so the test can inspect tags directly. Mirrors
    /// what `unwrap_with_shared_key` does internally, but returns
    /// the raw `Event` so the caller can poke at `event.tags`.
    fn decrypt_inner(shared: &Keys, outer: &Event) -> Event {
        let decrypted = nip44::decrypt(shared.secret_key(), &outer.pubkey, &outer.content)
            .expect("NIP-44 decrypt must succeed");
        Event::from_json(&decrypted).expect("inner JSON must parse")
    }

    /// Round-trip a wrap built with `Some("buyer")`: decrypt with the
    /// shared key and verify (a) the user-visible body is unchanged
    /// (the `m-aud` tag is metadata, not content) and (b) the inner
    /// event carries an `m-aud` tag whose value matches the
    /// audience we passed in.
    #[tokio::test]
    async fn audience_tag_is_attached_to_inner_event_and_body_is_unchanged() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let shared = derive_shared_keys(&serbero, &buyer.public_key()).unwrap();
        let body = "Could you share the fiat reference id and timezone?";

        let built = build_wrap_with_audience(&serbero, &shared.public_key(), body, Some("buyer"))
            .await
            .unwrap();

        // Body is verbatim — `m-aud` does not leak into the
        // user-visible content rendered by Mostro chat clients.
        let inner = unwrap_with_shared_key(&shared, &built.outer).unwrap();
        assert_eq!(inner.content, body);
        assert_eq!(inner.event_id, built.inner_event_id);

        // The `m-aud` tag is present on the inner event with the
        // expected audience value. Mostro chat clients ignore it,
        // but the audit pipeline can read it back if needed.
        let raw_inner = decrypt_inner(&shared, &built.outer);
        let aud_values: Vec<String> = raw_inner
            .tags
            .iter()
            .filter_map(|t| match t.kind() {
                TagKind::Custom(name) if name.as_ref() == "m-aud" => {
                    t.content().map(|s| s.to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            aud_values,
            vec!["buyer".to_string()],
            "inner event must carry exactly one `m-aud` tag with the audience we passed in"
        );
    }

    /// Backward compatibility: `build_wrap_with_audience(.., None)`
    /// MUST keep the user-visible body identical to the legacy
    /// [`build_wrap`] entry point and MUST NOT attach an `m-aud`
    /// tag, so older callers that don't care about per-party
    /// uniqueness keep observing the historical behaviour.
    /// (The exact `inner_event_id` cannot match across two calls
    /// because `created_at` is wall-clock and the ephemeral signer
    /// keys are regenerated per call; the contract we need here is
    /// "no `m-aud` tag and same content".)
    #[tokio::test]
    async fn audience_none_matches_legacy_build_wrap_output() {
        let sender = Keys::generate();
        let shared = Keys::generate();
        let body = "shared body for both calls";

        let legacy = build_wrap(&sender, &shared.public_key(), body).await.unwrap();
        let same = build_wrap_with_audience(&sender, &shared.public_key(), body, None)
            .await
            .unwrap();

        let legacy_inner = unwrap_with_shared_key(&shared, &legacy.outer).unwrap();
        let same_inner = unwrap_with_shared_key(&shared, &same.outer).unwrap();
        assert_eq!(legacy_inner.content, same_inner.content);
        assert_eq!(legacy_inner.content, body);

        for outer in [&legacy.outer, &same.outer] {
            let raw_inner = decrypt_inner(&shared, outer);
            assert!(
                !raw_inner.tags.iter().any(|t| matches!(
                    t.kind(),
                    TagKind::Custom(name) if name.as_ref() == "m-aud"
                )),
                "audience=None must NOT attach an `m-aud` tag (legacy compat)"
            );
        }
    }
}
