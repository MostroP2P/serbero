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
    /// A solver took a dispute that was waiting for one.
    Taken,
    /// Another `in-progress` revision of a dispute already taken: a
    /// takeover by another solver (`docs/spec.md` §5.1).
    TakenAgain,
    /// The dispute reached a final status.
    Resolved(DisputeStatus),
    /// Any other newer revision of a known dispute.
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
        match &change {
            Change::New => self.notify_new(&dispute, now).await?,
            Change::Taken => {
                // Serbero does not take disputes yet (Phase 5), so the
                // taker is always another solver.
                let text = text::taken(&dispute.dispute_id, false);
                notify_solvers(
                    &self.store,
                    &self.sender,
                    &self.solvers,
                    &dispute.dispute_id,
                    "taken",
                    &text,
                    now,
                )
                .await?;
            }
            _ => {}
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
                    transition(&tx, dispute, existing, now)?
                }
                StatusUpdate::Stale | StatusUpdate::NotFound => Change::Unchanged,
            }
        }
    };
    tx.commit()?;
    Ok(change)
}

/// Moves the lifecycle for a newer revision of a known dispute.
fn transition(
    conn: &rusqlite::Connection,
    dispute: &DisputeEvent,
    existing: disputes::Dispute,
    now: i64,
) -> Result<Change> {
    let id = &dispute.dispute_id;
    Ok(match &dispute.status {
        DisputeStatus::InProgress => match existing.lifecycle {
            Lifecycle::New | Lifecycle::Notified => {
                disputes::set_lifecycle(conn, id, Lifecycle::Taken, now)?;
                Change::Taken
            }
            Lifecycle::Taken => Change::TakenAgain,
            Lifecycle::Resolved => Change::Updated {
                from: existing.status,
                to: dispute.status.clone(),
            },
        },
        status if dispute_event::is_final(status) => {
            disputes::set_lifecycle(conn, id, Lifecycle::Resolved, now)?;
            let by = if dispute_event::resolved_by_parties(status) {
                "parties"
            } else {
                "solver"
            };
            append(
                conn,
                dispute,
                "resolved",
                json!({ "status": status.to_string(), "resolved_by": by }),
                now,
            )?;
            Change::Resolved(status.clone())
        }
        other => Change::Updated {
            from: existing.status,
            to: other.clone(),
        },
    })
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
mod tests;
