//! The live dispute subscription and its repair when relays reconnect.
//!
//! nostr-sdk 0.45 can drop a REQ from a relay's registry when it is refused
//! (for example a CLOSE + REQ while the relay is offline, as the app hit in
//! the field; see appv2 `docs/RELAYS.md`). Serbero therefore watches relay
//! status and re-sends its subscription, under a fixed id, to every relay
//! that becomes connected, and asks for a backlog resync (`docs/spec.md` §6).
//! A relay that already has the subscription refuses the duplicate id, which
//! is harmless. This is defensive: in Serbero's own start-up sequence the SDK
//! re-sent the REQ by itself in every scenario tested.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mostro_core::prelude::NOSTR_DISPUTE_EVENT_KIND;
use nostr_sdk::prelude::*;
use tokio::sync::Notify;

use crate::error::{Error, Result};

/// Id of the live dispute subscription on every relay.
pub const SUBSCRIPTION_ID: &str = "serbero-disputes";

/// How often relay status is checked for reconnections.
pub const RELAY_CHECK: Duration = Duration::from_secs(5);

/// Filter for the configured node's dispute events.
pub fn dispute_filter(mostro: PublicKey) -> Result<Filter> {
    let z = SingleLetterTag::from_char('z').map_err(|e| Error::Nostr(e.to_string()))?;
    Ok(Filter::new()
        .author(mostro)
        .kind(Kind::Custom(NOSTR_DISPUTE_EVENT_KIND))
        .custom_tag(z, "dispute"))
}

/// Subscribes every relay to dispute events published from `since` on.
/// Relays that are down are covered later by `watch`.
pub async fn subscribe_disputes(
    client: &Client,
    mostro: PublicKey,
    since: Timestamp,
) -> Result<()> {
    let output = client
        .subscribe(dispute_filter(mostro)?.since(since))
        .with_id(SubscriptionId::new(SUBSCRIPTION_ID))
        .await
        .map_err(|e| Error::Nostr(format!("cannot subscribe to disputes: {e}")))?;
    for (relay, reason) in &output.failed {
        tracing::info!(%relay, %reason, "dispute subscription deferred until the relay connects");
    }
    Ok(())
}

/// Re-sends the dispute subscription to one relay.
/// Every long-lived subscription Serbero holds, by id: the dispute feed and
/// each live session's chat. The relay watcher re-sends all of them to any
/// relay that connects (AGENTS.md, relay rule 6).
pub type Registry = Arc<std::sync::Mutex<HashMap<String, Filter>>>;

/// Records a long-lived subscription so reconnections re-send it.
pub fn register(registry: &Registry, id: &str, filter: Filter) {
    if let Ok(mut subscriptions) = registry.lock() {
        subscriptions.insert(id.to_owned(), filter);
    }
}

/// Forgets a subscription that was closed.
pub fn unregister(registry: &Registry, id: &str) {
    if let Ok(mut subscriptions) = registry.lock() {
        subscriptions.remove(id);
    }
}

/// Re-sends every registered subscription to one relay. A relay that still
/// has one refuses the duplicate id, which is harmless.
async fn resubscribe_all(client: &Client, relay: &RelayUrl, registry: &Registry) {
    let subscriptions: Vec<(String, Filter)> = match registry.lock() {
        Ok(s) => s.iter().map(|(id, f)| (id.clone(), f.clone())).collect(),
        Err(_) => return,
    };
    for (id, filter) in subscriptions {
        let target = vec![(relay.clone(), vec![filter])];
        match client
            .subscribe(target)
            .with_id(SubscriptionId::new(&id))
            .await
        {
            Ok(output) if output.success.contains_key(relay) => {
                tracing::info!(%relay, subscription = %id, "subscription re-sent");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(%relay, subscription = %id, error = %e, "cannot re-send subscription")
            }
        }
    }
}

/// Watches relay status forever. Whenever a relay becomes connected, every
/// registered subscription is re-sent to it and a backlog resync is
/// requested, so nothing published while it was unreachable is lost.
pub async fn watch(client: Client, registry: Registry, resync: Arc<Notify>) {
    let mut connected: HashMap<RelayUrl, bool> = client
        .relays()
        .await
        .into_iter()
        .map(|(url, relay)| (url, relay.status() == RelayStatus::Connected))
        .collect();
    loop {
        tokio::time::sleep(RELAY_CHECK).await;
        for (url, relay) in client.relays().await {
            let now_connected = relay.status() == RelayStatus::Connected;
            let was_connected = connected
                .insert(url.clone(), now_connected)
                .unwrap_or(false);
            if now_connected && !was_connected {
                tracing::info!(relay = %url, "relay connected");
                resubscribe_all(&client, &url, &registry).await;
                resync.notify_one();
            }
        }
    }
}
