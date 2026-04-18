//! Per-party chat-addressing key reconstruction.
//!
//! This is a direct port of the verified mechanism used by current
//! Mostro clients (Mostrix `src/util/chat_utils.rs`
//! `derive_shared_keys` / `derive_shared_key_hex` /
//! `keys_from_shared_hex`). The inputs are:
//!
//! - `admin_keys`: Serbero's configured private key (its operational
//!   solver identity).
//! - `counterparty_pubkey`: the party's **trade-scoped** pubkey as
//!   emitted by Mostro in `SolverDisputeInfo.buyer_pubkey` or
//!   `seller_pubkey` after a successful `AdminTookDispute`. This is
//!   **not** a user's long-term primary Nostr identity; the spec's
//!   rule against the "secret × primary-pubkey" shortcut still
//!   holds — the trade-scoped pubkey is what Mostro's protocol
//!   exposes for this purpose.
//!
//! The derivation is ECDH via `nostr::util::generate_shared_key`
//! followed by wrapping the resulting 32 bytes as a `SecretKey` and
//! building a `Keys`. The shared pubkey of that `Keys` is what
//! mediation chat events are addressed to.
//!
//! Ported against Mostrix at commit-time; re-verify if the
//! upstream `derive_shared_keys` signature changes.

use nostr_sdk::prelude::*;

use crate::error::{Error, Result};

/// Derive a per-party chat-addressing `Keys` from Serbero's secret
/// key and the counterparty's trade-scoped pubkey (`buyer_pubkey` /
/// `seller_pubkey` from `SolverDisputeInfo`).
pub fn derive_shared_keys(admin_keys: &Keys, counterparty_pubkey: &PublicKey) -> Result<Keys> {
    let shared_bytes =
        nostr_sdk::util::generate_shared_key(admin_keys.secret_key(), counterparty_pubkey)
            .map_err(|e| Error::ChatTransport(format!("ECDH failed: {e}")))?;
    let secret = SecretKey::from_slice(&shared_bytes).map_err(|e| {
        Error::ChatTransport(format!("shared-secret is not a valid SecretKey: {e}"))
    })?;
    Ok(Keys::new(secret))
}

/// Convenience: derive shared keys and hex-encode the resulting
/// secret for persistence-friendly handoff. The caller rebuilds
/// `Keys` later via [`keys_from_shared_hex`]. `data-model.md`
/// intentionally does NOT persist this hex — sessions only store
/// the derived public key. Provided for symmetry with the Mostrix
/// API and for test fixtures that want to exchange hex.
pub fn derive_shared_key_hex(admin_keys: &Keys, counterparty_pubkey: &PublicKey) -> Result<String> {
    let keys = derive_shared_keys(admin_keys, counterparty_pubkey)?;
    Ok(keys.secret_key().to_secret_hex())
}

/// Rebuild a `Keys` from a stored shared-key hex string. Ports
/// Mostrix `keys_from_shared_hex`. Used for tests that hold the hex
/// directly and for future workflows (e.g. restart resume) that may
/// need to reconstruct session keys from persisted material;
/// persisting the hex is an operator decision that is still open.
pub fn keys_from_shared_hex(hex: &str) -> Result<Keys> {
    let secret = SecretKey::parse(hex)
        .map_err(|e| Error::ChatTransport(format!("invalid shared-key hex: {e}")))?;
    Ok(Keys::new(secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ECDH symmetry: the pair (admin, counterparty) derives the
    /// same secret as (counterparty, admin). Mirrors Mostrix's
    /// `order_chat_counterparty_is_other_trade_side` test invariant
    /// at a lower level.
    #[test]
    fn ecdh_is_symmetric() {
        let admin = Keys::generate();
        let party = Keys::generate();
        let from_admin = derive_shared_keys(&admin, &party.public_key()).unwrap();
        let from_party = derive_shared_keys(&party, &admin.public_key()).unwrap();
        assert_eq!(
            from_admin.secret_key().to_secret_hex(),
            from_party.secret_key().to_secret_hex(),
            "ECDH must yield the same secret on both sides"
        );
        assert_eq!(from_admin.public_key(), from_party.public_key());
    }

    /// Different counterparties yield different shared secrets.
    /// Matches Mostrix's
    /// `derive_shared_key_hex_different_users_different_keys`.
    #[test]
    fn different_counterparties_produce_different_keys() {
        let admin = Keys::generate();
        let buyer = Keys::generate();
        let seller = Keys::generate();
        let b = derive_shared_key_hex(&admin, &buyer.public_key()).unwrap();
        let s = derive_shared_key_hex(&admin, &seller.public_key()).unwrap();
        assert_ne!(
            b, s,
            "different counterparties must yield distinct shared keys"
        );
    }

    /// Hex roundtrip preserves the derived keys exactly.
    #[test]
    fn hex_roundtrip_preserves_keys() {
        let admin = Keys::generate();
        let party = Keys::generate();
        let hex = derive_shared_key_hex(&admin, &party.public_key()).unwrap();
        let rebuilt = keys_from_shared_hex(&hex).unwrap();
        let direct = derive_shared_keys(&admin, &party.public_key()).unwrap();
        assert_eq!(
            rebuilt.secret_key().to_secret_hex(),
            direct.secret_key().to_secret_hex()
        );
    }

    /// Malformed hex fails loud.
    #[test]
    fn malformed_hex_errors() {
        assert!(keys_from_shared_hex("not-hex").is_err());
        assert!(keys_from_shared_hex("").is_err());
    }
}
