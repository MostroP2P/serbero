//! Dispute chat keys (`docs/spec.md` §5.1).
//!
//! Each party talks to Serbero on its own channel, keyed by the ECDH secret
//! between Serbero's key and the party's trade pubkey, split by HKDF into
//! `K_conv` (NIP-44 encryption, `p` tag) and `K_sign` (outer event author).
//! Derivation is delegated to `mostro-core`; nothing here re-implements it.

use mostro_core::chat::derive_chat_keys;
use nostr_sdk::prelude::*;

use crate::error::{Error, Result};

/// The keys of one party's dispute chat channel. They are never stored:
/// they are re-derived from Serbero's key and the stored trade pubkey.
#[derive(Clone)]
pub struct ChannelKeys {
    conv: Keys,
    sign: Keys,
}

impl ChannelKeys {
    /// Derives the channel between Serbero and a party's trade key. The
    /// party derives the same keys from its trade key and Serbero's pubkey.
    pub fn derive(serbero: &Keys, party_trade_pubkey: &PublicKey) -> Result<Self> {
        let (conv, sign) = derive_chat_keys(serbero, party_trade_pubkey)
            .map_err(|e| Error::Nostr(format!("cannot derive chat keys: {e}")))?;
        Ok(Self { conv, sign })
    }

    /// `K_conv`: encrypts messages; its pubkey is the channel's `p` tag.
    pub fn conv(&self) -> &Keys {
        &self.conv
    }

    /// `K_sign`: signs the outer events; its pubkey is the channel's author.
    pub fn sign(&self) -> &Keys {
        &self.sign
    }

    /// `pub(K_conv)`, the value of the channel's `p` tag.
    pub fn conversation_pubkey(&self) -> PublicKey {
        self.conv.public_key()
    }

    /// `pub(K_sign)`, the author to subscribe to.
    pub fn author_pubkey(&self) -> PublicKey {
        self.sign.public_key()
    }
}

impl std::fmt::Debug for ChannelKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelKeys")
            .field("conversation", &self.conversation_pubkey())
            .field("author", &self.author_pubkey())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector from the Mostro protocol, chat.md § Test vector.
    const ALICE_SECRET: &str = "548f68890c49fa42f104c60352395e60ff030b0b407e955f1eed1400d6c0347a";
    const BOB_SECRET: &str = "f258e73f07386d37133718b6127f873dd7c391b8f43b331ff8254034a13d2943";
    const CONV_PUBKEY: &str = "bceb1cd2a8e98ee9729122a1693edcc39c3ace04582ff96a26705c5e4078a6f2";
    const SIGN_PUBKEY: &str = "1dba04571059183f76b148119cfa6f8004dad30cb4e810180a6df17386a7f0b4";

    #[test]
    fn matches_the_protocol_test_vector_from_both_sides() {
        let alice = Keys::parse(ALICE_SECRET).unwrap();
        let bob = Keys::parse(BOB_SECRET).unwrap();

        let from_alice = ChannelKeys::derive(&alice, &bob.public_key()).unwrap();
        let from_bob = ChannelKeys::derive(&bob, &alice.public_key()).unwrap();

        for keys in [&from_alice, &from_bob] {
            assert_eq!(keys.conversation_pubkey().to_hex(), CONV_PUBKEY);
            assert_eq!(keys.author_pubkey().to_hex(), SIGN_PUBKEY);
        }
    }

    #[test]
    fn each_party_gets_its_own_channel() {
        let serbero = Keys::generate();
        let buyer = Keys::generate();
        let seller = Keys::generate();

        let with_buyer = ChannelKeys::derive(&serbero, &buyer.public_key()).unwrap();
        let with_seller = ChannelKeys::derive(&serbero, &seller.public_key()).unwrap();

        assert_ne!(with_buyer.author_pubkey(), with_seller.author_pubkey());
        assert_ne!(
            with_buyer.conversation_pubkey(),
            with_seller.conversation_pubkey()
        );
    }

    #[test]
    fn debug_output_shows_no_secrets() {
        let serbero = Keys::generate();
        let party = Keys::generate();
        let keys = ChannelKeys::derive(&serbero, &party.public_key()).unwrap();

        let debug = format!("{keys:?}");

        assert!(!debug.contains(&keys.conv().secret_key().to_secret_hex()));
        assert!(!debug.contains(&keys.sign().secret_key().to_secret_hex()));
    }
}
