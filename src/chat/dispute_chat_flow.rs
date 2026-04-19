//! Dispute-chat interaction flow.
//!
//! Ported from Mostrix
//! `src/util/order_utils/execute_take_dispute.rs`. The flow is:
//!
//! 1. Subscribe to NIP-59 gift-wrapped DMs addressed to Serbero.
//! 2. Send an `AdminTakeDispute` mostro-core message to the Mostro
//!    pubkey via `Client::send_private_msg` (NIP-17, which uses
//!    NIP-59 gift-wrap under the hood).
//! 3. Poll the subscription for the `AdminTookDispute` response.
//!    The response carries a `Payload::Dispute(id, Some(SolverDisputeInfo))`
//!    with `buyer_pubkey` + `seller_pubkey` (trade-scoped).
//! 4. Derive the per-party chat-addressing keys via ECDH
//!    (`shared_key::derive_shared_keys`).
//!
//! This slice implements only the happy-path + timeout. It does
//! NOT:
//! - Re-request on relay replay / duplicate events (a retry policy
//!   belongs in the engine, not here).
//! - Persist the raw shared-key secret (`data-model.md` stores only
//!   the derived public keys; the secrets live in process memory
//!   for the session's lifetime). Restart-resume of live sessions
//!   is US2 scope and has an open verification point.
//!
//! # Key-lifecycle discipline
//!
//! `DisputeChatMaterial::{buyer,seller}_shared_keys` hold the ECDH
//! secrets required to decrypt inbound gift-wraps and (for the
//! outbound path) to key NIP-44 encryption toward the per-party
//! shared pubkey. Per `data-model.md` §Key lifecycle, those secrets
//! are **in-process only** for the duration of the session — the DB
//! persists only the derived `*_shared_pubkey` on `mediation_sessions`.
//! That choice is deliberate: the secrets are sensitive enough that
//! writing them to disk would widen the compromise blast radius of
//! a leaked SQLite file, and they can always be re-derived by
//! re-running the take-flow against a Mostro instance that still
//! has the dispute.
//!
//! [`load_chat_keys_for_session`] is the hook point for a future
//! restart-resume extension that either (a) re-runs the take-flow
//! at engine startup for every live session or (b) persists the
//! secret under a key-wrapping scheme. Until one of those lands,
//! the function returns an error and the T052 startup path handles
//! it gracefully — the session stays alive and the ingest tick
//! skips it until the process that originally derived the secrets
//! is still running.
//!
//! Verification discipline:
//! - Every non-trivial step has a code comment naming the Mostrix
//!   file + function it was ported from.
//! - If upstream changes any of these behaviors (e.g. Mostro emits
//!   extra fields we MUST forward), the port must be refreshed.

use std::time::Duration;

use mostro_core::message::{Action, Message, Payload};
use nostr_sdk::prelude::*;
use uuid::Uuid;

use crate::chat::shared_key;
use crate::error::{Error, Result};

/// Per-dispute chat-addressing material, returned by
/// [`run_take_flow`]. The `Keys` values hold the shared secrets in
/// process memory for the session's lifetime; they are not
/// persisted. The `*_shared_pubkey` fields are the addressing
/// pubkeys that are written to `mediation_sessions`.
#[derive(Debug, Clone)]
pub struct DisputeChatMaterial {
    pub buyer_shared_keys: Keys,
    pub seller_shared_keys: Keys,
    pub buyer_pubkey: String,
    pub seller_pubkey: String,
}

impl DisputeChatMaterial {
    pub fn buyer_shared_pubkey(&self) -> String {
        self.buyer_shared_keys.public_key().to_hex()
    }
    pub fn seller_shared_pubkey(&self) -> String {
        self.seller_shared_keys.public_key().to_hex()
    }
}

/// Parameters for [`run_take_flow`]. Grouped into a struct because
/// Clippy (correctly) flags more than a handful of positional
/// arguments.
pub struct TakeFlowParams<'a> {
    pub client: &'a Client,
    pub serbero_keys: &'a Keys,
    pub mostro_pubkey: &'a PublicKey,
    pub dispute_id: Uuid,
    /// Total wall-clock time the caller is willing to wait for the
    /// `AdminTookDispute` response before returning a timeout.
    pub timeout: Duration,
    /// How often to poll the subscription for a matching response.
    /// Small enough to keep session-open latency low; large enough
    /// to avoid busy-looping.
    pub poll_interval: Duration,
}

/// Run the Mostro take-dispute exchange and return the per-party
/// chat-addressing material.
///
/// The caller owns the relay subscription lifecycle for the wider
/// daemon; this function performs its own short-lived subscribe so
/// it does not depend on the caller having already subscribed to
/// gift-wraps for Serbero's own pubkey.
pub async fn run_take_flow(p: TakeFlowParams<'_>) -> Result<DisputeChatMaterial> {
    let admin_pubkey = p.serbero_keys.public_key();

    // NIP-59 deliberately tweaks gift-wrap `created_at` up to 2 days
    // into the past to hide real send time (see
    // nostr::nips::nip59::RANGE_RANDOM_TIMESTAMP_TWEAK, 0..172800s).
    // A `since(now)` filter would therefore drop the very response
    // we're waiting for. Mostrix widens the window to 7 days; we
    // match that to stay compatible with any relay that enforces
    // `since` at the REQ level.
    let now = Timestamp::now();
    let since_window = Timestamp::from_secs(now.as_secs().saturating_sub(7 * 24 * 60 * 60));

    // (1) Subscribe to gift-wraps addressed to Serbero. Filter by
    //     `#p` = Serbero's pubkey.
    let filter = Filter::new()
        .kind(Kind::GiftWrap)
        .custom_tag(
            SingleLetterTag::lowercase(Alphabet::P),
            admin_pubkey.to_hex(),
        )
        .since(since_window);
    let sub = p
        .client
        .subscribe(filter.clone(), None)
        .await
        .map_err(|e| Error::ChatTransport(format!("failed to subscribe for take-flow: {e}")))?;

    // Everything that can return early lives in this inner block so
    // the caller can always call `unsubscribe` on the way out (happy
    // path, timeout, send / fetch errors). The earlier implementation
    // held the subscription for the life of the client, leaking one
    // REQ per session-open attempt.
    let result: Result<DisputeChatMaterial> = async {
        // (2) Build and send the AdminTakeDispute mostro-core message.
        //     Mirrors Mostrix execute_take_dispute.rs lines 63-85.
        let take_msg = Message::new_dispute(
            Some(p.dispute_id),
            None,
            None,
            Action::AdminTakeDispute,
            None,
        );
        let json = take_msg
            .as_json()
            .map_err(|_| Error::ChatTransport("failed to serialize AdminTakeDispute".into()))?;
        let send_out = p
            .client
            .send_private_msg(*p.mostro_pubkey, json, [])
            .await
            .map_err(|e| {
                Error::ChatTransport(format!("failed to send AdminTakeDispute DM: {e}"))
            })?;
        tracing::info!(
            mostro = %p.mostro_pubkey.to_hex(),
            outer_event_id = %send_out.val,
            "sent AdminTakeDispute to Mostro"
        );

        // (3) Poll for the AdminTookDispute response. We use
        //     client.fetch_events (blocking with a short timeout) rather
        //     than handle_notifications so this function remains a
        //     self-contained one-shot. Both the fetch and the
        //     between-rounds sleep are clamped to the remaining budget
        //     so the overall wall-clock never exceeds `p.timeout` by
        //     more than a scheduler tick.
        let deadline = tokio::time::Instant::now() + p.timeout;
        let timed_out = || {
            Error::ChatTransport(
                "timed out waiting for AdminTookDispute response from Mostro".into(),
            )
        };
        loop {
            let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
            else {
                return Err(timed_out());
            };
            if remaining.is_zero() {
                return Err(timed_out());
            }
            let fetch_budget = remaining.min(p.poll_interval);
            let events = p
                .client
                .fetch_events(filter.clone(), fetch_budget)
                .await
                .map_err(|e| {
                    Error::ChatTransport(format!("fetch_events failed during take-flow: {e}"))
                })?;
            tracing::trace!(count = events.len(), "take-flow: fetched candidate events");

            for wrapped in events.iter() {
                let Ok(unwrapped) = p.client.unwrap_gift_wrap(wrapped).await else {
                    continue;
                };
                if unwrapped.sender != *p.mostro_pubkey {
                    continue;
                }
                // The rumor content is the JSON-encoded mostro-core
                // Message. Mirrors Mostrix parse_dm_events.
                let Ok(response) = Message::from_json(&unwrapped.rumor.content) else {
                    continue;
                };
                let kind = response.get_inner_message_kind();
                if kind.action != Action::AdminTookDispute {
                    continue;
                }
                let Some(Payload::Dispute(id, Some(info))) = &kind.payload else {
                    continue;
                };
                if *id != p.dispute_id {
                    continue;
                }
                return material_from_solver_info(p.serbero_keys, info);
            }
            // Cooperative yield before the next poll round, clamped
            // to whatever budget is left.
            let sleep_budget = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or(Duration::ZERO)
                .min(p.poll_interval);
            if sleep_budget.is_zero() {
                return Err(timed_out());
            }
            tokio::time::sleep(sleep_budget).await;
        }
    }
    .await;

    // Drop the subscription regardless of outcome. `unsubscribe` is
    // infallible by signature (no Result), so there is no error to
    // propagate; any failure to reach the relay still leaves the
    // client-side subscription map cleaned up.
    p.client.unsubscribe(&sub.val).await;
    result
}

fn material_from_solver_info(
    serbero_keys: &Keys,
    info: &mostro_core::dispute::SolverDisputeInfo,
) -> Result<DisputeChatMaterial> {
    // Mostrix validates non-None on both sides and logs; we return
    // a loud error because mediation cannot start without both.
    let buyer_hex = info
        .buyer_pubkey
        .as_deref()
        .ok_or_else(|| Error::ChatTransport("SolverDisputeInfo missing buyer_pubkey".into()))?;
    let seller_hex = info
        .seller_pubkey
        .as_deref()
        .ok_or_else(|| Error::ChatTransport("SolverDisputeInfo missing seller_pubkey".into()))?;
    let buyer_pk = PublicKey::parse(buyer_hex)
        .map_err(|e| Error::ChatTransport(format!("invalid buyer_pubkey: {e}")))?;
    let seller_pk = PublicKey::parse(seller_hex)
        .map_err(|e| Error::ChatTransport(format!("invalid seller_pubkey: {e}")))?;

    // A dispute whose buyer and seller trade pubkeys are identical
    // is malformed — Mostro cannot match a trade against itself, and
    // mediating would produce two `mediation_messages` rows against
    // the same `shared_pubkey`, collapsing the per-party routing.
    // Reject it before deriving any keys.
    if buyer_pk == seller_pk {
        return Err(Error::ChatTransport(format!(
            "SolverDisputeInfo has identical buyer and seller trade pubkey ({buyer_hex}); \
             refusing to start mediation on a malformed dispute"
        )));
    }

    let buyer_shared_keys = shared_key::derive_shared_keys(serbero_keys, &buyer_pk)?;
    let seller_shared_keys = shared_key::derive_shared_keys(serbero_keys, &seller_pk)?;

    // Belt-and-braces: even with distinct trade pubkeys, ECDH must
    // produce distinct shared secrets. Ported from Mostrix.
    if buyer_shared_keys.secret_key().to_secret_hex()
        == seller_shared_keys.secret_key().to_secret_hex()
    {
        return Err(Error::ChatTransport(
            "buyer and seller shared secrets are identical for different trade pubkeys; \
             chat would be broken"
                .into(),
        ));
    }

    Ok(DisputeChatMaterial {
        buyer_shared_keys,
        seller_shared_keys,
        buyer_pubkey: buyer_hex.to_string(),
        seller_pubkey: seller_hex.to_string(),
    })
}

/// Attempt to re-derive the in-memory chat material for an existing
/// session on engine startup (T053 / restart-resume hook).
///
/// # Limitations (US2)
///
/// The ECDH shared-key secret is **not** persisted — only the derived
/// pubkeys appear on `mediation_sessions` (see the module header's
/// key-lifecycle paragraph and `data-model.md`). Re-deriving therefore
/// requires either (a) re-running the take-flow against the relay,
/// which is network I/O not available synchronously at engine startup,
/// or (b) a key-wrapping scheme that stores the secret at rest.
/// Neither path ships in US2, so this function always returns
/// `Err(Error::ChatTransport(…))` with a message that names the
/// limitation. The restart-resume path in
/// `mediation::run_engine` handles that `Err` gracefully — the
/// session row stays alive and the ingest tick skips it until the
/// process that originally derived the secrets is still running.
///
/// Kept as a documented hook so the US2+ extension can be slotted
/// in without touching every call site — the engine already treats
/// the error path as "skip this session for now" and the success
/// path as "populate the in-memory cache and proceed with ingest".
pub fn load_chat_keys_for_session(
    _buyer_shared_pubkey: &str,
    _seller_shared_pubkey: &str,
) -> Result<DisputeChatMaterial> {
    Err(Error::ChatTransport(
        "shared-key secret not persisted; restart-resume requires relay re-fetch \
         (deferred to US2+)"
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mostro_core::dispute::SolverDisputeInfo;

    fn info(buyer_hex: &str, seller_hex: &str) -> SolverDisputeInfo {
        SolverDisputeInfo {
            id: Uuid::nil(),
            kind: "buy".into(),
            status: "in-progress".into(),
            hash: None,
            preimage: None,
            order_previous_status: "active".into(),
            initiator_pubkey: buyer_hex.into(),
            buyer_pubkey: Some(buyer_hex.into()),
            seller_pubkey: Some(seller_hex.into()),
            initiator_full_privacy: false,
            counterpart_full_privacy: false,
            initiator_info: None,
            counterpart_info: None,
            premium: 0,
            payment_method: "".into(),
            amount: 0,
            fiat_amount: 0,
            fee: 0,
            routing_fee: 0,
            buyer_invoice: None,
            invoice_held_at: 0,
            taken_at: 0,
            created_at: 0,
        }
    }

    #[test]
    fn builds_material_from_solver_info() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let seller = Keys::generate();
        let material = material_from_solver_info(
            &serbero,
            &info(&buyer.public_key().to_hex(), &seller.public_key().to_hex()),
        )
        .unwrap();
        assert_eq!(material.buyer_pubkey, buyer.public_key().to_hex());
        assert_eq!(material.seller_pubkey, seller.public_key().to_hex());
        // The two shared pubkeys differ, proving ECDH used the right inputs.
        assert_ne!(
            material.buyer_shared_pubkey(),
            material.seller_shared_pubkey()
        );
    }

    #[test]
    fn errors_when_buyer_pubkey_missing() {
        let serbero = Keys::generate();
        let seller = Keys::generate();
        let mut bad = info("", &seller.public_key().to_hex());
        bad.buyer_pubkey = None;
        let err = material_from_solver_info(&serbero, &bad).unwrap_err();
        match err {
            Error::ChatTransport(m) => assert!(m.contains("buyer_pubkey")),
            other => panic!("expected ChatTransport error, got {other}"),
        }
    }

    #[test]
    fn errors_when_buyer_and_seller_trade_pubkeys_are_identical() {
        let serbero = Keys::generate();
        let shared = Keys::generate();
        let hex = shared.public_key().to_hex();
        let err = material_from_solver_info(&serbero, &info(&hex, &hex)).unwrap_err();
        match err {
            Error::ChatTransport(m) => {
                assert!(
                    m.contains("identical buyer and seller"),
                    "unexpected error text: {m}"
                );
            }
            other => panic!("expected ChatTransport, got {other}"),
        }
    }

    #[test]
    fn errors_when_seller_pubkey_malformed() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let bad = info(&buyer.public_key().to_hex(), "not-a-pubkey");
        let err = material_from_solver_info(&serbero, &bad).unwrap_err();
        assert!(
            matches!(err, Error::ChatTransport(_)),
            "expected ChatTransport, got {err}"
        );
    }

    #[test]
    fn load_chat_keys_for_session_documents_the_us2_limitation() {
        // T053 stub: always returns `Err` in US2. The T052 startup
        // pass relies on this — pinning it here keeps a future slice
        // that accidentally implements half of the restart-resume
        // path (and flips this to `Ok` without updating callers)
        // from silently changing engine behavior.
        let err = load_chat_keys_for_session("buyer-shared-pk", "seller-shared-pk").unwrap_err();
        match err {
            Error::ChatTransport(msg) => {
                assert!(
                    msg.contains("not persisted"),
                    "error message should name the key-lifecycle limitation: {msg}"
                );
            }
            other => panic!("expected ChatTransport, got {other}"),
        }
    }
}
