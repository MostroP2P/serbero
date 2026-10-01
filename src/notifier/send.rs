//! Delivery of solver notifications, with every attempt recorded.

use std::sync::Mutex;

use nostr_sdk::prelude::PublicKey;
use serde_json::json;

use crate::config::Permission;
use crate::error::{Error, Result};
use crate::nostr::dm::DmSender;
use crate::store::{Store, events};

/// Separates `Dispute <id>` from the subject on a solver message's first
/// line, a contract for clients (`docs/messages.md` §3).
const HEADER_SEPARATOR: &str = " · ";

/// A configured solver with a parsed pubkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Solver {
    pub pubkey: PublicKey,
    pub permission: Permission,
}

/// Sends `text` to every solver, tagged with `dispute_id` when it is a
/// UUID (Mostro dispute ids always are), and records each attempt as a
/// `notification_sent` or `notification_failed` event. Returns how many
/// DMs were delivered. Delivery failures are recorded, not returned: one
/// unreachable solver must not stop the others. If recording an attempt
/// fails, every solver is still tried and the first recording error is
/// returned afterwards.
pub async fn notify_solvers<S: DmSender>(
    store: &Mutex<Store>,
    sender: &S,
    solvers: &[Solver],
    dispute_id: &str,
    notification: &str,
    text: &str,
    now: i64,
) -> Result<usize> {
    if solvers.is_empty() {
        tracing::warn!(
            dispute_id,
            notification,
            "no solvers configured; nothing sent"
        );
        return Ok(0);
    }
    let dispute_uuid = uuid::Uuid::parse_str(dispute_id).ok();
    let mut delivered = 0;
    let mut record_error = None;
    for solver in solvers {
        let result = sender.send_dm(solver.pubkey, dispute_uuid, text).await;
        let (kind, payload) = match &result {
            Ok(()) => (
                "notification_sent",
                json!({ "solver": solver.pubkey.to_hex(), "notification": notification }),
            ),
            Err(e) => (
                "notification_failed",
                json!({
                    "solver": solver.pubkey.to_hex(),
                    "notification": notification,
                    "error": e.to_string(),
                }),
            ),
        };
        match &result {
            Ok(()) => {
                delivered += 1;
                tracing::info!(dispute_id, notification, solver = %solver.pubkey, "notification sent");
            }
            Err(e) => {
                tracing::warn!(dispute_id, notification, solver = %solver.pubkey, error = %e, "notification failed");
            }
        }
        // A failed audit write must not stop the remaining solvers from
        // being notified; the first such error is returned at the end.
        if let Err(e) = record_attempt(store, dispute_id, kind, payload, now) {
            tracing::error!(dispute_id, notification, error = %e, "cannot record notification attempt");
            record_error.get_or_insert(e);
        }
    }
    match record_error {
        Some(e) => Err(e),
        None => Ok(delivered),
    }
}

/// Sends each observer the first line of a solver message, `Dispute <id> ·
/// <subject>`, and nothing after it: the rest may quote the parties
/// (`docs/messages.md` §3). An observer gets each subject at most once per
/// dispute, so callers may call again whenever they retry the solvers.
/// Attempts are recorded as `observer_notified` or `observer_failed`, never
/// as solver notifications. Failures are logged, not returned: an observer
/// must never hold up a solver.
pub async fn notify_observers<S: DmSender>(
    store: &Mutex<Store>,
    sender: &S,
    observers: &[PublicKey],
    dispute_id: &str,
    notification: &str,
    text: &str,
    now: i64,
) {
    let header = text.lines().next().unwrap_or_default().trim();
    let subject = header
        .split_once(HEADER_SEPARATOR)
        .map_or(header, |(_, subject)| subject);
    let dispute_uuid = uuid::Uuid::parse_str(dispute_id).ok();
    for observer in observers {
        let observer_hex = observer.to_hex();
        let notified = store
            .lock()
            .map_err(|_| Error::Schema("store lock poisoned".into()))
            .and_then(|store| {
                events::observer_notified(store.conn(), dispute_id, &observer_hex, subject)
            });
        match notified {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                // Without the record a send could repeat; skip until it works.
                tracing::warn!(dispute_id, notification, observer = %observer, error = %e, "cannot check observer delivery");
                continue;
            }
        }
        let result = sender.send_dm(*observer, dispute_uuid, header).await;
        let (kind, payload) = match &result {
            Ok(()) => {
                tracing::info!(dispute_id, notification, observer = %observer, "observer notified");
                (
                    "observer_notified",
                    json!({ "observer": observer_hex, "notification": notification, "subject": subject }),
                )
            }
            Err(e) => {
                tracing::warn!(dispute_id, notification, observer = %observer, error = %e, "observer notification failed");
                (
                    "observer_failed",
                    json!({
                        "observer": observer_hex,
                        "notification": notification,
                        "subject": subject,
                        "error": e.to_string(),
                    }),
                )
            }
        };
        if let Err(e) = record_attempt(store, dispute_id, kind, payload, now) {
            tracing::error!(dispute_id, notification, error = %e, "cannot record observer attempt");
        }
    }
}

fn record_attempt(
    store: &Mutex<Store>,
    dispute_id: &str,
    kind: &str,
    payload: serde_json::Value,
    now: i64,
) -> Result<()> {
    let store = store
        .lock()
        .map_err(|_| Error::Schema("store lock poisoned".into()))?;
    events::append(
        store.conn(),
        &events::NewEvent {
            dispute_id,
            session_id: None,
            kind,
            payload,
            now,
        },
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use nostr_sdk::prelude::PublicKey;
    use uuid::Uuid;

    use crate::error::{Error, Result};
    use crate::nostr::dm::DmSender;

    /// Records every DM; fails for the pubkeys in `failing`.
    #[derive(Default)]
    pub struct FakeSender {
        pub sent: Mutex<Vec<(PublicKey, Option<Uuid>, String)>>,
        pub failing: HashSet<PublicKey>,
        /// The first this-many DMs fail, whoever they are for.
        pub fail_first: std::sync::atomic::AtomicUsize,
    }

    impl FakeSender {
        pub fn texts(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|(_, _, t)| t.clone())
                .collect()
        }

        pub fn dispute_ids(&self) -> Vec<Option<Uuid>> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|(_, id, _)| *id)
                .collect()
        }
    }

    impl DmSender for FakeSender {
        async fn send_dm(&self, to: PublicKey, dispute_id: Option<Uuid>, text: &str) -> Result<()> {
            use std::sync::atomic::Ordering;
            let early = self
                .fail_first
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if early || self.failing.contains(&to) {
                return Err(Error::Nostr("relay rejected".into()));
            }
            self.sent
                .lock()
                .unwrap()
                .push((to, dispute_id, text.to_owned()));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use nostr_sdk::prelude::Keys;

    use super::testing::FakeSender;
    use super::*;

    fn solver() -> Solver {
        Solver {
            pubkey: Keys::generate().public_key(),
            permission: Permission::Write,
        }
    }

    #[tokio::test]
    async fn every_solver_is_notified_and_recorded() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let sender = FakeSender::default();
        let solvers = [solver(), solver()];

        let delivered = notify_solvers(&store, &sender, &solvers, "d1", "new_dispute", "hi", 10)
            .await
            .unwrap();

        assert_eq!(delivered, 2);
        assert_eq!(sender.texts(), ["hi", "hi"]);
        let events = events::list_for_dispute(store.lock().unwrap().conn(), "d1").unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.kind == "notification_sent"));
        assert_eq!(events[0].payload["notification"], "new_dispute");
        assert_eq!(events[0].payload["solver"], solvers[0].pubkey.to_hex());
    }

    #[tokio::test]
    async fn every_dm_carries_the_dispute_id() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let sender = FakeSender::default();
        let dispute_id = uuid::Uuid::new_v4();

        notify_solvers(
            &store,
            &sender,
            &[solver(), solver()],
            &dispute_id.to_string(),
            "new_dispute",
            "hi",
            10,
        )
        .await
        .unwrap();

        assert_eq!(sender.dispute_ids(), [Some(dispute_id), Some(dispute_id)]);
    }

    #[tokio::test]
    async fn a_dispute_id_that_is_not_a_uuid_is_sent_without_one() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let sender = FakeSender::default();

        notify_solvers(&store, &sender, &[solver()], "d1", "new_dispute", "hi", 10)
            .await
            .unwrap();

        assert_eq!(sender.dispute_ids(), [None]);
        assert_eq!(sender.texts(), ["hi"]);
    }

    #[tokio::test]
    async fn one_failure_does_not_stop_the_others() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let solvers = [solver(), solver()];
        let sender = FakeSender {
            failing: [solvers[0].pubkey].into(),
            ..Default::default()
        };

        let delivered = notify_solvers(&store, &sender, &solvers, "d1", "new_dispute", "hi", 10)
            .await
            .unwrap();

        assert_eq!(delivered, 1);
        let events = events::list_for_dispute(store.lock().unwrap().conn(), "d1").unwrap();
        assert_eq!(events[0].kind, "notification_failed");
        assert_eq!(events[0].payload["error"], "nostr error: relay rejected");
        assert_eq!(events[1].kind, "notification_sent");
    }

    #[tokio::test]
    async fn audit_failure_does_not_skip_the_remaining_solvers() {
        let store = Store::open_in_memory().unwrap();
        store.conn().execute_batch("DROP TABLE events;").unwrap();
        let store = Mutex::new(store);
        let sender = FakeSender::default();
        let solvers = [solver(), solver(), solver()];

        let result = notify_solvers(&store, &sender, &solvers, "d1", "new_dispute", "hi", 10).await;

        assert!(result.is_err());
        assert_eq!(sender.texts().len(), 3, "every solver was still notified");
    }

    #[tokio::test]
    async fn no_solvers_sends_nothing() {
        let store = Mutex::new(Store::open_in_memory().unwrap());

        let delivered = notify_solvers(
            &store,
            &FakeSender::default(),
            &[],
            "d1",
            "new_dispute",
            "hi",
            10,
        )
        .await
        .unwrap();

        assert_eq!(delivered, 0);
    }

    fn observer() -> PublicKey {
        Keys::generate().public_key()
    }

    /// A brief whose later lines quote a party: observers must never see
    /// them.
    fn brief(dispute_id: &str) -> String {
        format!(
            "Dispute {dispute_id} · handed off: conflicting_claims\n\
             Buyer — says sent (0.96)\n  \"ya envié el pago desde mi cuenta\""
        )
    }

    fn observer_events(store: &Mutex<Store>, dispute_id: &str) -> Vec<events::Event> {
        events::list_for_dispute(store.lock().unwrap().conn(), dispute_id)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind.starts_with("observer_"))
            .collect()
    }

    #[tokio::test]
    async fn observers_get_only_the_first_line() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let sender = FakeSender::default();
        let observers = [observer(), observer()];
        let dispute_id = uuid::Uuid::new_v4();
        let id = dispute_id.to_string();

        notify_observers(&store, &sender, &observers, &id, "brief", &brief(&id), 10).await;

        let header = format!("Dispute {id} · handed off: conflicting_claims");
        let sent = sender.sent.lock().unwrap().clone();
        assert_eq!(
            sent,
            observers
                .iter()
                .map(|o| (*o, Some(dispute_id), header.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn an_observer_gets_each_header_once_per_dispute() {
        // A brief is retried until a solver receives it: the observer, who
        // already got its header, must not be pinged again.
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let sender = FakeSender::default();
        let observers = [observer()];

        for at in [10, 20] {
            notify_observers(&store, &sender, &observers, "d1", "brief", &brief("d1"), at).await;
        }
        let guided = "Dispute d1 · guidance sent: payment_arrived\nmore";
        notify_observers(&store, &sender, &observers, "d1", "brief", guided, 30).await;

        assert_eq!(
            sender.texts(),
            [
                "Dispute d1 · handed off: conflicting_claims",
                "Dispute d1 · guidance sent: payment_arrived",
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_observer_is_tried_again_on_the_next_call() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let sender = FakeSender {
            fail_first: 1.into(),
            ..Default::default()
        };
        let observers = [observer()];

        notify_observers(&store, &sender, &observers, "d1", "brief", &brief("d1"), 10).await;
        notify_observers(&store, &sender, &observers, "d1", "brief", &brief("d1"), 20).await;

        assert_eq!(sender.texts().len(), 1, "delivered on the second call");
        let kinds: Vec<String> = observer_events(&store, "d1")
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["observer_failed", "observer_notified"]);
    }

    #[tokio::test]
    async fn observer_deliveries_are_recorded_apart_from_solver_notifications() {
        // Solver retries and feedback read `notification_sent`; an observer
        // delivery must never count as one.
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let sender = FakeSender::default();
        let observers = [observer()];

        notify_observers(&store, &sender, &observers, "d1", "brief", &brief("d1"), 10).await;

        let all = events::list_for_dispute(store.lock().unwrap().conn(), "d1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "observer_notified");
        assert_eq!(all[0].payload["observer"], observers[0].to_hex());
        assert_eq!(all[0].payload["notification"], "brief");
        assert_eq!(all[0].payload["subject"], "handed off: conflicting_claims");
        assert!(
            !all[0].payload.to_string().contains("ya envié"),
            "no party text in the audit trail"
        );
    }

    #[tokio::test]
    async fn no_observers_sends_nothing() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let sender = FakeSender::default();

        notify_observers(&store, &sender, &[], "d1", "brief", &brief("d1"), 10).await;

        assert!(sender.texts().is_empty());
        assert!(observer_events(&store, "d1").is_empty());
    }
}
