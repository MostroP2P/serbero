//! Relay connectivity (`docs/spec.md` §5): one client for all of Serbero's
//! subscriptions and messages.

use std::time::Duration;

use nostr_sdk::prelude::*;

use crate::config::Secret;
use crate::error::{Error, Result};

/// Serbero's keys, parsed from the configured private key.
pub fn keys_from_secret(secret: &Secret) -> Result<Keys> {
    Keys::parse(secret.expose())
        .map_err(|_| Error::Nostr("the configured private key is not a valid secp256k1 key".into()))
}

/// Parses a hex public key from the config.
pub fn public_key(field: &str, hex: &str) -> Result<PublicKey> {
    PublicKey::from_hex(hex)
        .and_then(|pk| pk.xonly().map(|_| pk))
        .map_err(|e| Error::Nostr(format!("{field} is not a valid public key: {e}")))
}

/// Adds every relay and connects, waiting at most `wait` for the first
/// connections. `nostr-sdk` keeps reconnecting with backoff in the
/// background, so an unreachable relay is not an error here.
pub async fn connect(relays: &[String], wait: Duration) -> Result<Client> {
    let client = Client::default();
    for url in relays {
        client
            .add_relay(url.as_str())
            .await
            .map_err(|e| Error::Nostr(format!("cannot add relay {url}: {e}")))?;
    }
    client.connect().and_wait(wait).await;
    Ok(client)
}

/// Closes every relay connection.
pub async fn shutdown(client: &Client) {
    client.shutdown().await;
}
