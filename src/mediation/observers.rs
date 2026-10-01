//! The observer notices of mediation (`crate::notifier::observers`):
//! queued with the state they report, delivered by their own task.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

use super::Mediator;
use crate::error::Result;
use crate::nostr::dm::DmSender;
use crate::notifier::observers;

/// How long one observer DM may take; nostr-sdk itself gives up waiting
/// for relays after 10 s.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// How often queued notices are checked without a wake, for retries.
const RETRY_EVERY: Duration = Duration::from_secs(30);

impl<S: DmSender> Mediator<S> {
    /// Queues `subject` about `dispute_id` for the observers, inside the
    /// caller's transaction. Returns whether the delivery task has work.
    pub(crate) fn queue_for_observers(
        &self,
        conn: &Connection,
        dispute_id: &str,
        subject: &str,
        now: i64,
    ) -> Result<bool> {
        observers::queue(conn, &self.observers, dispute_id, subject, now)
    }

    /// Wakes the delivery task once a transaction that queued a notice
    /// committed.
    pub(crate) fn wake_observers(&self, queued: bool) {
        if queued {
            self.observer_wake.notify_one();
        }
    }

    /// Delivers the notices that are due. Returns how many were delivered.
    pub async fn deliver_observer_notices(&self, now: i64) -> Result<usize> {
        observers::deliver_due(
            &self.store,
            &self.sender,
            &self.observers,
            SEND_TIMEOUT,
            now,
        )
        .await
    }
}

impl<S: DmSender + Send + Sync + 'static> Mediator<S> {
    /// Delivers queued observer notices as soon as one is queued, and
    /// retries failed ones. The only task that sends to observers.
    pub async fn run_observer_notices(self: Arc<Self>) {
        loop {
            let _ = tokio::time::timeout(RETRY_EVERY, self.observer_wake.notified()).await;
            if let Err(e) = self.deliver_observer_notices(crate::daemon::now()).await {
                tracing::warn!(error = %e, "cannot deliver observer notices");
            }
        }
    }
}
