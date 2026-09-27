//! Wiring: configuration, store, relays, the event loop, and the background
//! tasks (backlog sync, relay watcher, reminders).

pub mod relays;
pub mod sync;

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

pub use self::relays::{dispute_filter, subscribe_disputes};
pub use self::sync::{newest_revisions, sync_backlog};
use crate::config::Settings;
use crate::error::Result;
use crate::nostr::dm::{DmSender, RelayDmSender};
use crate::notifier::{Notifier, Solver};
use crate::store::Store;

/// How long startup waits for the first relay connections.
const RELAY_CONNECT_WAIT: Duration = Duration::from_secs(10);

/// How often the reminder timer looks for unattended disputes.
const REMINDER_TICK: Duration = Duration::from_secs(60);

/// Runs Serbero until Ctrl-C or SIGTERM.
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
    let notifier = Arc::new(Notifier::new(Arc::clone(&store), sender, solvers, mostro));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pubkey = %keys.public_key(),
        mostro = %mostro,
        relays = config.mostro.relays.len(),
        solvers = config.solvers.len(),
        mediation = config.mediation.enabled,
        "serbero starting"
    );

    let renotify_after = config.notify.renotify_after.as_secs() as i64;
    let (notifications, background) = start(&client, &notifier, mostro, renotify_after).await?;
    let result = event_loop(notifications, &notifier, crate::signal::shutdown()).await;
    drop(background);
    tracing::info!("shutting down");
    crate::nostr::shutdown(&client).await;
    result
}

/// Background tasks started by `start`; dropping it stops them.
pub struct Background {
    tasks: Vec<JoinHandle<()>>,
    synced: watch::Receiver<bool>,
}

impl Background {
    /// Waits until the first backlog sync has been applied.
    pub async fn first_sync(&mut self) {
        let _ = self.synced.wait_for(|done| *done).await;
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Subscribes to live dispute events at once and starts the background
/// tasks. Nothing relay-bound runs before the live subscription, so a slow
/// relay can never delay a new dispute (`docs/spec.md` §6). Returns the
/// notification stream to feed `event_loop`.
pub async fn start<S: DmSender + Send + Sync + 'static>(
    client: &Client,
    notifier: &Arc<Notifier<S>>,
    mostro: PublicKey,
    renotify_after: i64,
) -> Result<(
    impl StreamExt<Item = ClientNotification> + Unpin + Send + use<S>,
    Background,
)> {
    // Opened before any REQ, so nothing a relay sends can be missed.
    let notifications = client.notifications();
    let since = Timestamp::now();
    subscribe_disputes(client, mostro, since).await?;

    let resync = Arc::new(Notify::new());
    let (synced_tx, synced_rx) = watch::channel(false);
    let tasks = vec![
        tokio::spawn(sync::sync_loop(
            client.clone(),
            Arc::clone(notifier),
            mostro,
            Arc::clone(&resync),
            synced_tx,
        )),
        tokio::spawn(relays::watch(client.clone(), mostro, since, resync)),
        tokio::spawn(reminder_loop(
            Arc::clone(notifier),
            renotify_after,
            synced_rx.clone(),
        )),
    ];
    Ok((
        notifications,
        Background {
            tasks,
            synced: synced_rx,
        },
    ))
}

/// Feeds relay events to the notifier until `shutdown` completes or the
/// client shuts down. A failure handling one event is logged and does not
/// stop the loop.
pub async fn event_loop<S: DmSender>(
    mut notifications: impl StreamExt<Item = ClientNotification> + Unpin,
    notifier: &Notifier<S>,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
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

/// Runs the reminder timer in its own task, so reminder delivery never
/// delays dispute events. It waits for the first backlog sync, then checks
/// every `REMINDER_TICK`.
pub async fn reminder_loop<S: DmSender + Send + Sync + 'static>(
    notifier: Arc<Notifier<S>>,
    renotify_after: i64,
    mut synced: watch::Receiver<bool>,
) {
    let _ = synced.wait_for(|done| *done).await;
    let first = tokio::time::Instant::now() + REMINDER_TICK;
    let mut ticks = tokio::time::interval_at(first, REMINDER_TICK);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        if let Err(e) = notifier.remind(renotify_after, now()).await {
            tracing::error!(error = %e, "reminder tick failed");
        }
    }
}

/// Current Unix time in seconds.
pub fn now() -> i64 {
    Timestamp::now().as_secs() as i64
}
