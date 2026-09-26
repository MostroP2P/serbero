//! Wiring: configuration, store, relays, and the event loop.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mostro_core::prelude::NOSTR_DISPUTE_EVENT_KIND;
use nostr_sdk::prelude::*;

use crate::config::Settings;
use crate::error::{Error, Result};
use crate::nostr::dm::{DmSender, RelayDmSender};
use crate::notifier::{Notifier, Solver};
use crate::store::Store;

/// How long startup waits for the first relay connections.
const RELAY_CONNECT_WAIT: Duration = Duration::from_secs(10);

/// Runs Serbero until Ctrl-C.
pub async fn run(settings: &Settings) -> Result<()> {
    let config = &settings.config;
    let store = Arc::new(Mutex::new(Store::open(&config.serbero.db_path)?));
    let keys = crate::nostr::keys_from_secret(&settings.secrets.private_key)?;
    let mostro = crate::nostr::public_key("mostro.pubkey", &config.mostro.pubkey)?;
    let client = crate::nostr::connect(&config.mostro.relays, RELAY_CONNECT_WAIT).await?;
    let solvers = config
        .solvers
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Ok(Solver {
                pubkey: crate::nostr::public_key(&format!("solvers[{i}].pubkey"), &s.pubkey)?,
                permission: s.permission,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let sender = RelayDmSender::new(client.clone(), keys.clone());
    let notifier = Notifier::new(Arc::clone(&store), sender, solvers, mostro);
    subscribe_disputes(&client, mostro).await?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pubkey = %keys.public_key(),
        mostro = %mostro,
        relays = config.mostro.relays.len(),
        solvers = config.solvers.len(),
        mediation = config.mediation.enabled,
        "serbero started"
    );

    let shutdown = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for shutdown signal");
        }
    };
    let result = event_loop(&client, &notifier, shutdown).await;
    tracing::info!("shutting down");
    crate::nostr::shutdown(&client).await;
    result
}

/// Filter for the configured node's dispute events (`docs/spec.md` §6).
pub fn dispute_filter(mostro: PublicKey) -> Result<Filter> {
    let z = SingleLetterTag::from_char('z').map_err(|e| Error::Nostr(e.to_string()))?;
    Ok(Filter::new()
        .author(mostro)
        .kind(Kind::Custom(NOSTR_DISPUTE_EVENT_KIND))
        .custom_tag(z, "dispute"))
}

pub async fn subscribe_disputes(client: &Client, mostro: PublicKey) -> Result<()> {
    client
        .subscribe(dispute_filter(mostro)?)
        .await
        .map_err(|e| Error::Nostr(format!("cannot subscribe to disputes: {e}")))?;
    Ok(())
}

/// Feeds relay events to the notifier until `shutdown` completes or the
/// client shuts down. A failure handling one event is logged and does not
/// stop the loop.
pub async fn event_loop<S: DmSender>(
    client: &Client,
    notifier: &Notifier<S>,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let mut notifications = client.notifications();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            notification = notifications.next() => match notification {
                Some(ClientNotification::Event { event, .. }) => {
                    if let Err(e) = notifier.handle_event(&event, now()).await {
                        tracing::error!(event_id = %event.id, error = %e, "failed to handle event");
                    }
                }
                Some(ClientNotification::Shutdown) | None => return Ok(()),
                Some(_) => {}
            },
        }
    }
}

/// Current Unix time in seconds.
pub fn now() -> i64 {
    Timestamp::now().as_secs() as i64
}
