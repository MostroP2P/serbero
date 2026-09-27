//! Wiring: configuration, store, relays, and the event loop.

use std::collections::HashMap;
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

/// How often the reminder timer looks for unattended disputes.
const REMINDER_TICK: Duration = Duration::from_secs(60);

/// How long the startup sync waits for relays to return stored disputes.
const SYNC_TIMEOUT: Duration = Duration::from_secs(15);

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

    let notifications = start(&client, &notifier, mostro).await?;
    let renotify_after = config.notify.renotify_after.as_secs() as i64;
    let reminders = tokio::spawn(reminder_loop(Arc::clone(&notifier), renotify_after));
    let result = event_loop(notifications, &notifier, crate::signal::shutdown()).await;
    reminders.abort();
    tracing::info!("shutting down");
    crate::nostr::shutdown(&client).await;
    result
}

/// Brings the store up to date with the relays, then subscribes to new
/// dispute events. Returns the notification stream to feed `event_loop`.
///
/// The stream is created before any request so nothing the relays send can
/// be missed. The backlog is applied newest revision first per dispute, so
/// relays that disagree cannot make an old `initiated` revision notify
/// solvers about a dispute that was already taken (`docs/spec.md` §6).
pub async fn start<S: DmSender>(
    client: &Client,
    notifier: &Notifier<S>,
    mostro: PublicKey,
) -> Result<impl StreamExt<Item = ClientNotification> + Unpin + Send + use<S>> {
    let notifications = client.notifications();
    let sync_started = Timestamp::now();
    let applied = sync_backlog(client, notifier, mostro, SYNC_TIMEOUT).await?;
    tracing::info!(disputes = applied, "startup sync complete");
    subscribe_disputes(client, mostro, sync_started).await?;
    Ok(notifications)
}

/// Fetches the disputes the relays already store and applies the newest
/// revision of each, oldest first. Returns how many disputes were applied.
pub async fn sync_backlog<S: DmSender>(
    client: &Client,
    notifier: &Notifier<S>,
    mostro: PublicKey,
    timeout: Duration,
) -> Result<usize> {
    let events = client
        .fetch_events(dispute_filter(mostro)?)
        .timeout(timeout)
        .await
        .map_err(|e| Error::Nostr(format!("cannot fetch stored disputes: {e}")))?;
    let newest = newest_revisions(events);
    for event in &newest {
        if let Err(e) = notifier.handle_event(event, now()).await {
            tracing::error!(event_id = %event.id, error = %e, "failed to apply stored dispute");
        }
    }
    Ok(newest.len())
}

/// Keeps the newest revision of each dispute (by the event's `created_at`,
/// then id) and returns them oldest first.
pub fn newest_revisions(events: impl IntoIterator<Item = Event>) -> Vec<Event> {
    let mut newest: HashMap<String, Event> = HashMap::new();
    for event in events {
        let Some(id) = event.tags.identifier() else {
            continue;
        };
        let replace = newest
            .get(&id)
            .is_none_or(|kept| (event.created_at, event.id) > (kept.created_at, kept.id));
        if replace {
            newest.insert(id, event);
        }
    }
    let mut revisions: Vec<Event> = newest.into_values().collect();
    revisions.sort_by_key(|e| (e.created_at, e.id));
    revisions
}

/// Filter for the configured node's dispute events (`docs/spec.md` §6).
pub fn dispute_filter(mostro: PublicKey) -> Result<Filter> {
    let z = SingleLetterTag::from_char('z').map_err(|e| Error::Nostr(e.to_string()))?;
    Ok(Filter::new()
        .author(mostro)
        .kind(Kind::Custom(NOSTR_DISPUTE_EVENT_KIND))
        .custom_tag(z, "dispute"))
}

/// Subscribes to dispute events published from `since` on.
pub async fn subscribe_disputes(
    client: &Client,
    mostro: PublicKey,
    since: Timestamp,
) -> Result<()> {
    client
        .subscribe(dispute_filter(mostro)?.since(since))
        .await
        .map_err(|e| Error::Nostr(format!("cannot subscribe to disputes: {e}")))?;
    Ok(())
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
/// delays dispute events. The first check happens one tick after startup,
/// once the backlog has been applied.
pub async fn reminder_loop<S: DmSender + Send + Sync + 'static>(
    notifier: Arc<Notifier<S>>,
    renotify_after: i64,
) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(signer: &Keys, id: &str, status: &str, at: u64) -> Event {
        let tags = [vec!["d", id], vec!["s", status], vec!["z", "dispute"]]
            .into_iter()
            .map(|t| Tag::parse(t).unwrap());
        EventBuilder::new(Kind::Custom(38386), "")
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(at))
            .finalize(signer)
            .unwrap()
    }

    #[test]
    fn keeps_only_the_newest_revision_per_dispute_oldest_first() {
        let mostro = Keys::generate();
        let events = vec![
            revision(&mostro, "a", "in-progress", 200),
            revision(&mostro, "b", "initiated", 150),
            revision(&mostro, "a", "initiated", 100),
            revision(&mostro, "a", "settled", 300),
        ];

        let newest = newest_revisions(events);

        let picked: Vec<_> = newest
            .iter()
            .map(|e| (e.tags.identifier().unwrap(), e.created_at.as_secs()))
            .collect();
        assert_eq!(picked, [("b".to_owned(), 150), ("a".to_owned(), 300)]);
    }
}
