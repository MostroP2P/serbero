//! Dispute detection, solver notifications, and the dispute lifecycle
//! (`docs/spec.md` §6).

pub mod send;
pub mod text;

use std::sync::{Arc, Mutex};

use mostro_core::dispute::Status as DisputeStatus;
use nostr_sdk::prelude::{Event, PublicKey};
use serde_json::json;

pub use self::send::{Solver, notify_solvers};
use crate::error::{Error, Result};
use crate::mostro::dispute_event::{self, DisputeEvent};
use crate::nostr::dm::DmSender;
use crate::store::disputes::{self, Lifecycle, NewDispute, StatusUpdate};
use crate::store::{Store, events};

/// What a dispute event changed in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A dispute seen for the first time while still `initiated`.
    New,
    /// A dispute seen for the first time already past `initiated`, for
    /// example after Serbero was offline. Stored without notifying.
    FirstSeen(DisputeStatus),
    /// A newer revision of a known dispute.
    Updated { from: String, to: DisputeStatus },
    /// A replay, an older revision, or an invalid event.
    Unchanged,
}

pub struct Notifier<S> {
    store: Arc<Mutex<Store>>,
    sender: S,
    solvers: Vec<Solver>,
    mostro: PublicKey,
}

impl<S: DmSender> Notifier<S> {
    pub fn new(
        store: Arc<Mutex<Store>>,
        sender: S,
        solvers: Vec<Solver>,
        mostro: PublicKey,
    ) -> Self {
        Self {
            store,
            sender,
            solvers,
            mostro,
        }
    }

    /// Handles one event from the dispute subscription. Events that are not
    /// valid dispute events from the configured node are logged and ignored.
    pub async fn handle_event(&self, event: &Event, now: i64) -> Result<Change> {
        let dispute = match dispute_event::parse(event, &self.mostro) {
            Ok(dispute) => dispute,
            Err(e) => {
                tracing::warn!(event_id = %event.id, error = %e, "ignoring event");
                return Ok(Change::Unchanged);
            }
        };
        let change = record(&self.store, &dispute, now)?;
        if change != Change::Unchanged {
            tracing::info!(dispute_id = %dispute.dispute_id, status = %dispute.status, ?change, "dispute event");
        }
        if change == Change::New {
            self.notify_new(&dispute, now).await?;
        }
        Ok(change)
    }

    /// Reminds solvers of disputes nobody has taken `renotify_after` after
    /// their last notification, and retries disputes whose first
    /// notification never got through. Returns how many disputes were
    /// processed.
    pub async fn remind(&self, renotify_after: i64, now: i64) -> Result<usize> {
        let due = {
            let store = lock(&self.store)?;
            disputes::list_awaiting_solver(store.conn(), now - renotify_after)?
        };
        let mut processed = 0;
        for dispute in &due {
            // Events are handled concurrently with this loop: skip disputes
            // taken or resolved since the due list was read.
            let current = {
                let store = lock(&self.store)?;
                disputes::get(store.conn(), &dispute.dispute_id)?
            };
            let Some(dispute) =
                current.filter(|d| matches!(d.lifecycle, Lifecycle::New | Lifecycle::Notified))
            else {
                continue;
            };
            processed += 1;
            let (notification, text) = match dispute.lifecycle {
                Lifecycle::New => (
                    "new_dispute",
                    text::new_dispute(&dispute.dispute_id, dispute.initiator),
                ),
                _ => (
                    "reminder",
                    text::reminder(&dispute.dispute_id, now - dispute.first_seen_at),
                ),
            };
            let delivered = notify_solvers(
                &self.store,
                &self.sender,
                &self.solvers,
                &dispute.dispute_id,
                notification,
                &text,
                now,
            )
            .await?;
            if delivered > 0 {
                let store = lock(&self.store)?;
                disputes::mark_notified(store.conn(), &dispute.dispute_id, now)?;
            }
        }
        if processed > 0 {
            tracing::info!(count = processed, "reminder tick");
        }
        Ok(processed)
    }

    /// Tells every solver about a new dispute; the dispute becomes
    /// `notified` once at least one DM was delivered.
    async fn notify_new(&self, dispute: &DisputeEvent, now: i64) -> Result<()> {
        let text = text::new_dispute(&dispute.dispute_id, dispute.initiator);
        let delivered = notify_solvers(
            &self.store,
            &self.sender,
            &self.solvers,
            &dispute.dispute_id,
            "new_dispute",
            &text,
            now,
        )
        .await?;
        if delivered > 0 {
            let store = lock(&self.store)?;
            disputes::mark_notified(store.conn(), &dispute.dispute_id, now)?;
        }
        Ok(())
    }
}

/// Stores a dispute event revision and records what changed, atomically.
pub fn record(store: &Mutex<Store>, dispute: &DisputeEvent, now: i64) -> Result<Change> {
    let store = lock(store)?;
    let tx = store.conn().unchecked_transaction()?;
    let status = dispute.status.to_string();
    let change = match disputes::get(&tx, &dispute.dispute_id)? {
        None => {
            disputes::insert_if_new(
                &tx,
                &NewDispute {
                    dispute_id: &dispute.dispute_id,
                    initiator: dispute.initiator,
                    status: &status,
                    status_at: dispute.revision_at,
                    now,
                },
            )?;
            let change = match &dispute.status {
                DisputeStatus::Initiated => Change::New,
                other => {
                    let lifecycle = if dispute_event::is_final(other) {
                        Lifecycle::Resolved
                    } else {
                        Lifecycle::Taken
                    };
                    disputes::set_lifecycle(&tx, &dispute.dispute_id, lifecycle, now)?;
                    Change::FirstSeen(other.clone())
                }
            };
            append(
                &tx,
                dispute,
                "detected",
                json!({ "status": status, "initiator": dispute.initiator.to_string() }),
                now,
            )?;
            change
        }
        Some(existing) => {
            match disputes::apply_status(
                &tx,
                &dispute.dispute_id,
                &status,
                dispute.revision_at,
                now,
            )? {
                StatusUpdate::Applied => {
                    append(
                        &tx,
                        dispute,
                        "status_changed",
                        json!({ "from": existing.status, "to": status }),
                        now,
                    )?;
                    Change::Updated {
                        from: existing.status,
                        to: dispute.status.clone(),
                    }
                }
                StatusUpdate::Stale | StatusUpdate::NotFound => Change::Unchanged,
            }
        }
    };
    tx.commit()?;
    Ok(change)
}

fn lock(store: &Mutex<Store>) -> Result<std::sync::MutexGuard<'_, Store>> {
    store
        .lock()
        .map_err(|_| Error::Schema("store lock poisoned".into()))
}

fn append(
    conn: &rusqlite::Connection,
    dispute: &DisputeEvent,
    kind: &str,
    payload: serde_json::Value,
    now: i64,
) -> Result<i64> {
    events::append(
        conn,
        &events::NewEvent {
            dispute_id: &dispute.dispute_id,
            session_id: None,
            kind,
            payload,
            now,
        },
    )
}

#[cfg(test)]
pub(crate) mod testing {
    use nostr_sdk::prelude::*;

    /// A signed dispute event revision from `mostro`.
    pub fn dispute_event(mostro: &Keys, id: &str, status: &str, at: u64) -> Event {
        let tags = [
            vec!["d", id],
            vec!["s", status],
            vec!["initiator", "seller"],
            vec!["y", "mostro"],
            vec!["z", "dispute"],
        ]
        .into_iter()
        .map(|t| Tag::parse(t).unwrap());
        EventBuilder::new(Kind::Custom(38386), "")
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(at))
            .finalize(mostro)
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use nostr_sdk::prelude::Keys;

    use super::send::testing::FakeSender;
    use super::testing::dispute_event;
    use super::*;

    fn notifier(mostro: &Keys) -> Notifier<FakeSender> {
        notifier_with(mostro, FakeSender::default(), vec![solver()])
    }

    fn notifier_with(
        mostro: &Keys,
        sender: FakeSender,
        solvers: Vec<Solver>,
    ) -> Notifier<FakeSender> {
        Notifier::new(
            Arc::new(Mutex::new(Store::open_in_memory().unwrap())),
            sender,
            solvers,
            mostro.public_key(),
        )
    }

    fn solver() -> Solver {
        Solver {
            pubkey: Keys::generate().public_key(),
            permission: crate::config::Permission::Write,
        }
    }

    fn lifecycle(n: &Notifier<FakeSender>, id: &str) -> Lifecycle {
        let store = n.store.lock().unwrap();
        disputes::get(store.conn(), id).unwrap().unwrap().lifecycle
    }

    #[tokio::test]
    async fn new_initiated_dispute_is_stored_as_new() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);

        let change = n
            .handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();

        assert_eq!(change, Change::New);
        let store = n.store.lock().unwrap();
        let events = events::list_for_dispute(store.conn(), "d1").unwrap();
        assert_eq!(events[0].kind, "detected");
    }

    #[tokio::test]
    async fn new_dispute_is_sent_to_every_solver_and_marked_notified() {
        let mostro = Keys::generate();
        let n = notifier_with(&mostro, FakeSender::default(), vec![solver(), solver()]);

        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();

        assert_eq!(n.sender.texts().len(), 2);
        assert_eq!(
            n.sender.texts()[0],
            "New Mostro dispute\ndispute: d1\nopened by: seller"
        );
        let store = n.store.lock().unwrap();
        let dispute = disputes::get(store.conn(), "d1").unwrap().unwrap();
        assert_eq!(dispute.lifecycle, Lifecycle::Notified);
        assert_eq!(dispute.last_notified_at, Some(1_000));
    }

    #[tokio::test]
    async fn partial_failure_still_marks_notified() {
        let mostro = Keys::generate();
        let solvers = vec![solver(), solver()];
        let sender = FakeSender {
            failing: [solvers[0].pubkey].into(),
            ..Default::default()
        };
        let n = notifier_with(&mostro, sender, solvers);

        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();

        assert_eq!(lifecycle(&n, "d1"), Lifecycle::Notified);
        let store = n.store.lock().unwrap();
        let kinds: Vec<_> = events::list_for_dispute(store.conn(), "d1")
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            ["detected", "notification_failed", "notification_sent"]
        );
    }

    #[tokio::test]
    async fn total_failure_leaves_the_dispute_new() {
        let mostro = Keys::generate();
        let solvers = vec![solver()];
        let sender = FakeSender {
            failing: [solvers[0].pubkey].into(),
            ..Default::default()
        };
        let n = notifier_with(&mostro, sender, solvers);

        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();

        assert_eq!(lifecycle(&n, "d1"), Lifecycle::New);
    }

    #[tokio::test]
    async fn reminder_fires_once_per_interval() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();

        let early = n.remind(900, 1_899).await.unwrap();
        let due = n.remind(900, 1_900).await.unwrap();
        let again = n.remind(900, 1_901).await.unwrap();
        let next = n.remind(900, 2_800).await.unwrap();

        assert_eq!((early, due, again, next), (0, 1, 0, 1));
        let texts = n.sender.texts();
        assert_eq!(texts.len(), 3);
        assert_eq!(texts[1], "Dispute still unattended (15 min)\ndispute: d1");
        assert_eq!(texts[2], "Dispute still unattended (30 min)\ndispute: d1");
    }

    #[tokio::test]
    async fn taken_disputes_get_no_reminders() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();
        {
            let store = n.store.lock().unwrap();
            disputes::set_lifecycle(store.conn(), "d1", Lifecycle::Taken, 1_100).unwrap();
        }

        let reminded = n.remind(900, 5_000).await.unwrap();

        assert_eq!(reminded, 0);
    }

    #[tokio::test]
    async fn failed_first_notification_is_retried_as_new_dispute() {
        let mostro = Keys::generate();
        let solvers = vec![solver()];
        let failing = FakeSender {
            failing: [solvers[0].pubkey].into(),
            ..Default::default()
        };
        let n = notifier_with(&mostro, failing, solvers.clone());
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();
        let n = Notifier::new(
            Arc::clone(&n.store),
            FakeSender::default(),
            solvers,
            mostro.public_key(),
        );

        n.remind(900, 1_900).await.unwrap();

        assert_eq!(
            n.sender.texts(),
            ["New Mostro dispute\ndispute: d1\nopened by: seller"]
        );
        assert_eq!(lifecycle(&n, "d1"), Lifecycle::Notified);
    }

    #[tokio::test]
    async fn replayed_new_dispute_is_not_notified_twice() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        let event = dispute_event(&mostro, "d1", "initiated", 100);
        n.handle_event(&event, 1_000).await.unwrap();

        n.handle_event(&event, 1_001).await.unwrap();

        assert_eq!(n.sender.texts().len(), 1);
    }

    #[tokio::test]
    async fn disputes_first_seen_past_initiated_are_not_notified() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);

        n.handle_event(&dispute_event(&mostro, "d1", "in-progress", 100), 1_000)
            .await
            .unwrap();

        assert!(n.sender.texts().is_empty());
    }

    #[tokio::test]
    async fn replayed_event_is_a_no_op() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        let event = dispute_event(&mostro, "d1", "initiated", 100);
        n.handle_event(&event, 1_000).await.unwrap();

        let change = n.handle_event(&event, 1_001).await.unwrap();

        assert_eq!(change, Change::Unchanged);
        let store = n.store.lock().unwrap();
        let detected = events::list_for_dispute(store.conn(), "d1")
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "detected")
            .count();
        assert_eq!(detected, 1);
    }

    #[tokio::test]
    async fn older_revision_after_newer_is_a_no_op() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();
        n.handle_event(&dispute_event(&mostro, "d1", "settled", 300), 1_001)
            .await
            .unwrap();

        let change = n
            .handle_event(&dispute_event(&mostro, "d1", "in-progress", 200), 1_002)
            .await
            .unwrap();

        assert_eq!(change, Change::Unchanged);
    }

    #[tokio::test]
    async fn newer_revision_reports_the_transition() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();

        let change = n
            .handle_event(&dispute_event(&mostro, "d1", "in-progress", 200), 1_001)
            .await
            .unwrap();

        assert_eq!(
            change,
            Change::Updated {
                from: "initiated".into(),
                to: DisputeStatus::InProgress
            }
        );
    }

    #[tokio::test]
    async fn dispute_first_seen_in_progress_or_final_is_not_new() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);

        let taken = n
            .handle_event(&dispute_event(&mostro, "d1", "in-progress", 100), 1_000)
            .await
            .unwrap();
        let done = n
            .handle_event(&dispute_event(&mostro, "d2", "released", 100), 1_000)
            .await
            .unwrap();

        assert_eq!(taken, Change::FirstSeen(DisputeStatus::InProgress));
        assert_eq!(done, Change::FirstSeen(DisputeStatus::Released));
        assert_eq!(lifecycle(&n, "d1"), Lifecycle::Taken);
        assert_eq!(lifecycle(&n, "d2"), Lifecycle::Resolved);
    }

    #[tokio::test]
    async fn events_from_other_authors_are_ignored() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);

        let change = n
            .handle_event(
                &dispute_event(&Keys::generate(), "d1", "initiated", 100),
                1_000,
            )
            .await
            .unwrap();

        assert_eq!(change, Change::Unchanged);
        let store = n.store.lock().unwrap();
        assert!(disputes::get(store.conn(), "d1").unwrap().is_none());
    }
}
