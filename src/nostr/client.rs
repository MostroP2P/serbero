use nostr_sdk::{Client, Keys};
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::models::Config;

pub async fn build_client(config: &Config) -> Result<Client> {
    let keys = Keys::parse(&config.serbero.private_key)
        .map_err(|e| Error::InvalidKey(format!("failed to parse serbero private key: {e}")))?;

    info!(
        serbero_pubkey = %keys.public_key().to_hex(),
        "built nostr client keys (serbero's own identity for gift-wrap signing)"
    );

    let client = Client::new(keys);

    if config.relays.is_empty() {
        warn!("no relays configured; client will not receive any events");
    }

    for relay in &config.relays {
        match client.add_relay(&relay.url).await {
            Ok(added) => {
                if added {
                    info!(relay = %relay.url, "added relay");
                } else {
                    info!(relay = %relay.url, "relay already added");
                }
            }
            Err(e) => {
                return Err(Error::Nostr(format!(
                    "failed to add relay {}: {e}",
                    relay.url
                )))
            }
        }
    }

    client.connect().await;
    info!(
        relay_count = config.relays.len(),
        "nostr client issued connect() to all configured relays \
         (reconnection and backoff are handled automatically by nostr-sdk)"
    );

    Ok(client)
}
