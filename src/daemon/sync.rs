//! Backlog sync: the disputes relays already store.
//!
//! `fetch_events` returns only once every relay has sent EOSE or the timeout
//! passes, so one slow relay holds the whole answer back. The sync therefore
//! runs in the background and never delays live dispute events. It cannot
//! simply take the first relay's answer either: the backlog is many disputes,
//! and a relay holding a stale `initiated` revision must not notify solvers
//! about a dispute another relay already shows as taken. So it waits for the
//! relays (bounded by `SYNC_TIMEOUT`) and applies only the newest revision of
//! each dispute. What a slow relay misses is caught by the periodic resync.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{Notify, watch};

use super::now;
use super::relays::dispute_filter;
use crate::error::{Error, Result};
use crate::nostr::dm::DmSender;
use crate::notifier::Notifier;

/// Upper bound for one backlog fetch.
pub const SYNC_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the backlog is fetched again even without reconnections.
pub const RESYNC_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Fetches the disputes the relays store and applies the newest revision of
/// each, oldest first. Returns how many disputes were applied.
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

/// Keeps the newest revision of each dispute and returns them oldest first.
/// Newest follows NIP-01: the later `created_at`, then the lowest id, so
/// which relay answered first never decides between two revisions.
pub fn newest_revisions(events: impl IntoIterator<Item = Event>) -> Vec<Event> {
    let rank = |e: &Event| (e.created_at, Reverse(e.id));
    let mut newest: HashMap<String, Event> = HashMap::new();
    for event in events {
        let Some(id) = event.tags.identifier() else {
            continue;
        };
        if newest.get(&id).is_none_or(|kept| rank(&event) > rank(kept)) {
            newest.insert(id, event);
        }
    }
    let mut revisions: Vec<Event> = newest.into_values().collect();
    revisions.sort_by_key(|e| (e.created_at, e.id));
    revisions
}

/// Syncs the backlog now, then again every `RESYNC_INTERVAL` or whenever a
/// resync is requested. Marks `synced` true after the first sync, so the
/// reminder timer never runs on state the relays have not confirmed.
pub async fn sync_loop<S: DmSender + Send + Sync + 'static>(
    client: Client,
    notifier: Arc<Notifier<S>>,
    mostro: PublicKey,
    resync: Arc<Notify>,
    synced: watch::Sender<bool>,
) {
    loop {
        match sync_backlog(&client, &notifier, mostro, SYNC_TIMEOUT).await {
            Ok(applied) => tracing::info!(disputes = applied, "backlog sync complete"),
            Err(e) => tracing::warn!(error = %e, "backlog sync failed"),
        }
        synced.send_replace(true);
        tokio::select! {
            () = tokio::time::sleep(RESYNC_INTERVAL) => {}
            () = resync.notified() => {}
        }
    }
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

    #[test]
    fn equal_timestamps_keep_the_lowest_id_whatever_the_order() {
        let mostro = Keys::generate();
        let x = revision(&mostro, "a", "in-progress", 200);
        let y = revision(&mostro, "a", "settled", 200);
        let lowest = if x.id < y.id { x.id } else { y.id };

        let forward = newest_revisions(vec![x.clone(), y.clone()]);
        let backward = newest_revisions(vec![y, x]);

        assert_eq!(forward[0].id, lowest);
        assert_eq!(backward[0].id, lowest);
    }
}
