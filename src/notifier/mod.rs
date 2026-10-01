//! Dispute detection, solver notifications, and the dispute lifecycle
//! (`docs/spec.md` §6).

pub mod send;
pub mod text;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use mostro_core::dispute::Status as DisputeStatus;
use mostro_core::prelude::NOSTR_DISPUTE_EVENT_KIND;
use nostr_sdk::prelude::{Event, Kind, PublicKey};
use serde_json::json;

pub use self::send::{Solver, notify_observers, notify_solvers};
use crate::error::{Error, Result};
use crate::mostro::dispute_event::{self, DisputeEvent};
use crate::nostr::dm::DmSender;
use crate::store::disputes::{self, Lifecycle, NewDispute, StatusUpdate};
use crate::store::sessions::{self, SessionState};
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

/// Disputes Serbero is taking right now. Mostro may publish the
/// `in-progress` revision of Serbero's own take before its session exists;
/// this set tells the notifier that take was Serbero's.
pub type OwnTakes = Arc<Mutex<HashSet<String>>>;

/// Called with the id of each new dispute once solvers were notified.
pub type NewDisputeHook = Box<dyn Fn(&str) + Send + Sync>;

/// Called with a dispute's id, its final status, and whether the parties
/// resolved it themselves, once it resolved.
pub type ResolvedHook = Box<dyn Fn(&str, &str, bool) + Send + Sync>;

pub struct Notifier<S> {
    store: Arc<Mutex<Store>>,
    gate: Arc<crate::chat::OutboundGate>,
    sender: S,
    solvers: Vec<Solver>,
    mostro: PublicKey,
    own_takes: OwnTakes,
    on_new_dispute: OnceLock<NewDisputeHook>,
    on_resolved: OnceLock<ResolvedHook>,
}

impl<S: DmSender> Notifier<S> {
    /// The gate chat senders must hold while sending (`chat::send_to_party`).
    pub fn outbound_gate(&self) -> Arc<crate::chat::OutboundGate> {
        Arc::clone(&self.gate)
    }

    pub fn new(
        store: Arc<Mutex<Store>>,
        sender: S,
        solvers: Vec<Solver>,
        mostro: PublicKey,
    ) -> Self {
        Self {
            store,
            gate: Arc::default(),
            sender,
            solvers,
            mostro,
            own_takes: OwnTakes::default(),
            on_new_dispute: OnceLock::new(),
            on_resolved: OnceLock::new(),
        }
    }

    /// The disputes Serbero is taking; mediation adds one before its take.
    pub fn own_takes(&self) -> OwnTakes {
        Arc::clone(&self.own_takes)
    }

    /// Installs the hook called for each new dispute once a solver was
    /// notified of it, at the first DM or at a later retry. Only the first
    /// hook is kept.
    pub fn on_new_dispute(&self, hook: NewDisputeHook) {
        if self.on_new_dispute.set(hook).is_err() {
            tracing::warn!("a new-dispute hook is already installed");
        }
    }

    /// Installs the hook called once a dispute reached its final status
    /// (its live session, if any, is already closed). Only the first hook
    /// is kept.
    pub fn on_resolved(&self, hook: ResolvedHook) {
        if self.on_resolved.set(hook).is_err() {
            tracing::warn!("a resolution hook is already installed");
        }
    }

    /// Serbero took the dispute if a take of its own is in flight or one of
    /// its sessions is live: a human's take would have superseded it.
    fn taken_by_serbero(&self, dispute_id: &str) -> Result<bool> {
        let taking = self
            .own_takes
            .lock()
            .map_err(|_| Error::Schema("own takes lock poisoned".into()))?
            .contains(dispute_id);
        if taking {
            return Ok(true);
        }
        let store = lock(&self.store)?;
        Ok(sessions::live_for_dispute(store.conn(), dispute_id)?.is_some())
    }

    /// The dispute an event carries. `None` for events of other kinds: the
    /// client's notification stream carries every subscription's events
    /// (party chats, node info, orders), and those are not worth a warning.
    pub(crate) fn dispute_of(&self, event: &Event) -> Option<Result<DisputeEvent>> {
        (event.kind == Kind::Custom(NOSTR_DISPUTE_EVENT_KIND))
            .then(|| dispute_event::parse(event, &self.mostro))
    }

    /// Handles one event from the dispute subscription. Events that are not
    /// valid dispute events from the configured node are logged and ignored.
    pub async fn handle_event(&self, event: &Event, now: i64) -> Result<Change> {
        let dispute = match self.dispute_of(event) {
            None => return Ok(Change::Unchanged),
            Some(Ok(dispute)) => dispute,
            Some(Err(e)) => {
                tracing::warn!(error = %e, "ignoring malformed dispute event");
                return Ok(Change::Unchanged);
            }
        };
        // Exclusive: no party message is in flight while a revision may end
        // a session, and none starts after it did.
        let change = {
            let _no_sends = self.gate.write().await;
            record(&self.store, &dispute, now)?
        };
        if change != Change::Unchanged {
            tracing::info!(dispute_id = %dispute.dispute_id, status = %dispute.status, ?change, "dispute event");
        }
        match &change {
            Change::New => {
                if self.notify_new(&dispute, now).await? {
                    self.new_dispute_notified(&dispute.dispute_id);
                }
            }
            Change::Taken => {
                let by_serbero = self.taken_by_serbero(&dispute.dispute_id)?;
                let text = text::taken(&dispute.dispute_id, by_serbero);
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
            Change::Resolved(status) => {
                if let Some(hook) = self.on_resolved.get() {
                    let by_parties = dispute_event::resolved_by_parties(status);
                    hook(&dispute.dispute_id, &status.to_string(), by_parties);
                }
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
                {
                    let store = lock(&self.store)?;
                    disputes::mark_notified(store.conn(), &dispute.dispute_id, now)?;
                }
                // Every first DM had failed: the dispute is notified only
                // now, so it only now becomes a candidate for mediation.
                if dispute.lifecycle == Lifecycle::New {
                    self.new_dispute_notified(&dispute.dispute_id);
                }
            }
        }
        if processed > 0 {
            tracing::info!(count = processed, "reminder tick");
        }
        Ok(processed)
    }

    fn new_dispute_notified(&self, dispute_id: &str) {
        if let Some(hook) = self.on_new_dispute.get() {
            hook(dispute_id);
        }
    }

    /// Tells every solver about a new dispute; the dispute becomes
    /// `notified` once at least one DM was delivered. Returns whether one
    /// was.
    async fn notify_new(&self, dispute: &DisputeEvent, now: i64) -> Result<bool> {
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
        Ok(delivered > 0)
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
        DisputeStatus::InProgress => {
            // Serbero opens its session only after its own take succeeded, so
            // an in-progress revision newer than the session's opening is
            // someone else's take: only a `write` solver can take over from
            // Serbero. This holds even if Serbero's own revision was missed.
            let taken_over = sessions::live_for_dispute(conn, id)?
                .is_some_and(|s| dispute.revision_at > s.opened_at);
            if taken_over {
                end_live_session(
                    conn,
                    dispute,
                    SessionState::Superseded,
                    "session_superseded",
                    now,
                )?;
            }
            match existing.lifecycle {
                Lifecycle::New | Lifecycle::Notified => {
                    disputes::set_lifecycle(conn, id, Lifecycle::Taken, now)?;
                    Change::Taken
                }
                Lifecycle::Taken => Change::TakenAgain,
                Lifecycle::Resolved => Change::Updated {
                    from: existing.status,
                    to: dispute.status.clone(),
                },
            }
        }
        status if dispute_event::is_final(status) => {
            disputes::set_lifecycle(conn, id, Lifecycle::Resolved, now)?;
            end_live_session(conn, dispute, SessionState::Closed, "session_closed", now)?;
            let by = if dispute_event::resolved_by_parties(status) {
                "parties"
            } else {
                "solver"
            };
            append(
                conn,
                dispute,
                "resolved",
                // `resolved_at` is Mostro's revision time: after downtime,
                // it is when the dispute resolved, not when Serbero saw it.
                json!({
                    "status": status.to_string(),
                    "resolved_by": by,
                    "resolved_at": dispute.revision_at,
                }),
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

/// Ends the dispute's live session, if any, and records why.
fn end_live_session(
    conn: &rusqlite::Connection,
    dispute: &DisputeEvent,
    state: SessionState,
    kind: &str,
    now: i64,
) -> Result<()> {
    let Some(session) = sessions::live_for_dispute(conn, &dispute.dispute_id)? else {
        return Ok(());
    };
    sessions::set_state(conn, &session.session_id, state, now)?;
    events::append(
        conn,
        &events::NewEvent {
            dispute_id: &dispute.dispute_id,
            session_id: Some(&session.session_id),
            kind,
            payload: json!({ "status": dispute.status.to_string() }),
            now,
        },
    )?;
    tracing::info!(dispute_id = %dispute.dispute_id, session_id = %session.session_id, %state, "session ended");
    Ok(())
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
