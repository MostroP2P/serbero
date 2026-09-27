//! Dispute detection, solver notifications, and the dispute lifecycle
//! (`docs/spec.md` §6).

pub mod send;

use std::sync::{Arc, Mutex};

use mostro_core::dispute::Status as DisputeStatus;
use nostr_sdk::prelude::{Event, PublicKey};
use serde_json::json;

pub use self::send::{Solver, notify_solvers};
use crate::error::{Error, Result};
use crate::mostro::dispute_event::{self, DisputeEvent};
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

pub struct Notifier {
    store: Arc<Mutex<Store>>,
    mostro: PublicKey,
}

impl Notifier {
    pub fn new(store: Arc<Mutex<Store>>, mostro: PublicKey) -> Self {
        Self { store, mostro }
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
        Ok(change)
    }
}

/// Stores a dispute event revision and records what changed, atomically.
pub fn record(store: &Mutex<Store>, dispute: &DisputeEvent, now: i64) -> Result<Change> {
    let store = store
        .lock()
        .map_err(|_| Error::Schema("store lock poisoned".into()))?;
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

    use super::testing::dispute_event;
    use super::*;

    fn notifier(mostro: &Keys) -> Notifier {
        Notifier::new(
            Arc::new(Mutex::new(Store::open_in_memory().unwrap())),
            mostro.public_key(),
        )
    }

    fn lifecycle(n: &Notifier, id: &str) -> Lifecycle {
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
        assert_eq!(lifecycle(&n, "d1"), Lifecycle::New);
        let store = n.store.lock().unwrap();
        let events = events::list_for_dispute(store.conn(), "d1").unwrap();
        assert_eq!(events[0].kind, "detected");
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
        assert_eq!(
            events::list_for_dispute(store.conn(), "d1").unwrap().len(),
            1
        );
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
