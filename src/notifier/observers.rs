//! Observer notices (`docs/messages.md` §3): the first line of a mediation
//! update, for services such as mostro-watchdog that relay it to a team
//! chat. Mediation only queues a notice, in the transaction that records
//! what it reports; one task delivers the queue. An observer therefore never
//! delays a solver, a party or a retry marker, two tasks never send the same
//! notice, and a failed delivery is retried with backoff.

use std::sync::Mutex;
use std::time::Duration;

use nostr_sdk::prelude::PublicKey;
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::nostr::dm::DmSender;
use crate::store::{Store, events};

const PENDING: &str = "observer_pending";
const NOTIFIED: &str = "observer_notified";
const FAILED: &str = "observer_failed";

/// A notice still undelivered a day after it was queued is dropped.
const RETRY_WINDOW_SECS: i64 = 24 * 3600;

/// The wait after a failed attempt: a minute, doubling up to an hour.
const RETRY_BASE_SECS: i64 = 60;
const RETRY_MAX_SECS: i64 = 3600;

/// What an observer receives: a solver message's first line, built from the
/// dispute id and the subject alone, so no other line can follow it.
pub fn header(dispute_id: &str, subject: &str) -> String {
    format!("Dispute {dispute_id} · {subject}")
}

/// Queues `subject` about `dispute_id` for every observer that does not
/// have it queued yet; run it inside the transaction that records what the
/// subject reports. Returns whether anything was queued, so the caller can
/// wake the delivery task.
pub fn queue(
    conn: &Connection,
    observers: &[PublicKey],
    dispute_id: &str,
    subject: &str,
    now: i64,
) -> Result<bool> {
    if observers.is_empty() {
        return Ok(false);
    }
    // The id goes into the DM's only line, so it must not start another
    // one; Mostro's ids are UUIDs.
    if !is_one_token(dispute_id) {
        tracing::warn!("dispute id is not a single token; observers not told");
        return Ok(false);
    }
    let mut queued = false;
    for observer in observers {
        let observer = observer.to_hex();
        if events::observer_queued(conn, dispute_id, &observer, subject)? {
            continue;
        }
        events::append(
            conn,
            &events::NewEvent {
                dispute_id,
                session_id: None,
                kind: PENDING,
                payload: json!({ "observer": observer, "subject": subject }),
                now,
            },
        )?;
        queued = true;
    }
    Ok(queued)
}

/// Sends every due notice to the observers still configured, oldest first,
/// each send cut after `timeout`, and records the outcome. Returns how many
/// were delivered.
pub async fn deliver_due<S: DmSender>(
    store: &Mutex<Store>,
    sender: &S,
    observers: &[PublicKey],
    timeout: Duration,
    now: i64,
) -> Result<usize> {
    let notices = {
        let store = lock(store)?;
        events::pending_observer_notices(store.conn(), now - RETRY_WINDOW_SECS)?
    };
    let mut delivered = 0;
    for notice in notices {
        // Notices for an observer no longer configured stay undelivered.
        let Some(observer) = observers.iter().find(|o| o.to_hex() == notice.observer) else {
            continue;
        };
        if !due(notice.failures, notice.last_failure, now) {
            continue;
        }
        let text = header(&notice.dispute_id, &notice.subject);
        let dispute_uuid = Uuid::parse_str(&notice.dispute_id).ok();
        let result = tokio::time::timeout(timeout, sender.send_dm(*observer, dispute_uuid, &text))
            .await
            .unwrap_or_else(|_| {
                Err(Error::Nostr(format!(
                    "no relay answered within {timeout:?}"
                )))
            });
        let dispute_id = notice.dispute_id.as_str();
        let (kind, payload) = match &result {
            Ok(()) => {
                delivered += 1;
                tracing::info!(dispute_id, observer = %observer, "observer notified");
                (
                    NOTIFIED,
                    json!({ "observer": notice.observer, "subject": notice.subject }),
                )
            }
            Err(e) => {
                tracing::warn!(dispute_id, observer = %observer, error = %e, "observer notice failed; retried later");
                (
                    FAILED,
                    json!({
                        "observer": notice.observer,
                        "subject": notice.subject,
                        "error": e.to_string(),
                    }),
                )
            }
        };
        let store = lock(store)?;
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
    }
    Ok(delivered)
}

fn lock(store: &Mutex<Store>) -> Result<std::sync::MutexGuard<'_, Store>> {
    store
        .lock()
        .map_err(|_| Error::Schema("store lock poisoned".into()))
}

fn is_one_token(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether a notice with `failures` failed attempts, the last at
/// `last_failure`, may be tried again at `now`.
fn due(failures: u32, last_failure: Option<i64>, now: i64) -> bool {
    let doublings = failures.saturating_sub(1).min(6);
    let wait = (RETRY_BASE_SECS << doublings).min(RETRY_MAX_SECS);
    last_failure.is_none_or(|at| now - at >= wait)
}

#[cfg(test)]
mod tests {
    use nostr_sdk::prelude::Keys;

    use super::*;
    use crate::notifier::send::testing::FakeSender;

    const DAY: i64 = 24 * 3600;

    fn observer() -> PublicKey {
        Keys::generate().public_key()
    }

    fn dispute() -> String {
        Uuid::new_v4().to_string()
    }

    fn store() -> Mutex<Store> {
        Mutex::new(Store::open_in_memory().unwrap())
    }

    fn queue_in(
        store: &Mutex<Store>,
        observers: &[PublicKey],
        id: &str,
        subject: &str,
        now: i64,
    ) -> bool {
        queue(store.lock().unwrap().conn(), observers, id, subject, now).unwrap()
    }

    fn kinds(store: &Mutex<Store>, id: &str) -> Vec<String> {
        events::list_for_dispute(store.lock().unwrap().conn(), id)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    /// Never answers, as a relay that accepts the connection and goes
    /// silent.
    struct SilentSender;

    impl DmSender for SilentSender {
        async fn send_dm(&self, _: PublicKey, _: Option<Uuid>, _: &str) -> Result<()> {
            std::future::pending().await
        }
    }

    #[test]
    fn the_header_is_the_first_line_of_the_solver_message() {
        assert_eq!(
            header("d1", "handed off: conflicting_claims"),
            "Dispute d1 · handed off: conflicting_claims"
        );
    }

    #[test]
    fn a_subject_is_queued_once_per_observer_and_dispute() {
        let store = store();
        let observers = [observer(), observer()];
        let id = dispute();

        assert!(queue_in(&store, &observers, &id, "mediating", 10));
        assert!(
            !queue_in(&store, &observers, &id, "mediating", 20),
            "already queued"
        );
        assert!(queue_in(
            &store,
            &observers,
            &id,
            "handed off: round_limit",
            30
        ));

        assert_eq!(
            kinds(&store, &id),
            ["observer_pending"; 4],
            "two subjects for two observers"
        );
    }

    #[test]
    fn nothing_is_queued_without_observers_or_for_an_id_that_would_add_a_line() {
        let store = store();

        assert!(!queue_in(&store, &[], &dispute(), "mediating", 10));
        // The id goes into the first line of a DM; Mostro's are UUIDs.
        assert!(!queue_in(
            &store,
            &[observer()],
            "d1\nBuyer: hi",
            "mediating",
            10
        ));
        assert!(kinds(&store, "d1\nBuyer: hi").is_empty());
    }

    #[tokio::test]
    async fn a_queued_notice_is_delivered_once_with_its_dispute_id() {
        let store = store();
        let sender = FakeSender::default();
        let observers = [observer(), observer()];
        let id = dispute();
        queue_in(
            &store,
            &observers,
            &id,
            "handed off: conflicting_claims",
            10,
        );

        let delivered = deliver_due(&store, &sender, &observers, Duration::from_secs(5), 11)
            .await
            .unwrap();
        let again = deliver_due(&store, &sender, &observers, Duration::from_secs(5), 12)
            .await
            .unwrap();

        assert_eq!((delivered, again), (2, 0));
        let header = format!("Dispute {id} · handed off: conflicting_claims");
        let uuid = Uuid::parse_str(&id).ok();
        assert_eq!(
            *sender.sent.lock().unwrap(),
            observers
                .iter()
                .map(|o| (*o, uuid, header.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_failed_notice_is_retried_after_its_backoff() {
        let store = store();
        let sender = FakeSender {
            fail_first: 1.into(),
            ..Default::default()
        };
        let observers = [observer()];
        let id = dispute();
        queue_in(&store, &observers, &id, "mediating", 0);
        let deliver = |now| deliver_due(&store, &sender, &observers, Duration::from_secs(5), now);

        let first = deliver(10).await.unwrap();
        let too_soon = deliver(10 + RETRY_BASE_SECS - 1).await.unwrap();
        let retried = deliver(10 + RETRY_BASE_SECS).await.unwrap();

        assert_eq!((first, too_soon, retried), (0, 0, 1));
        assert_eq!(
            kinds(&store, &id),
            ["observer_pending", "observer_failed", "observer_notified"]
        );
    }

    #[tokio::test]
    async fn a_silent_relay_costs_one_timeout_and_is_recorded_as_a_failure() {
        let store = store();
        let observers = [observer()];
        let id = dispute();
        queue_in(&store, &observers, &id, "mediating", 0);

        let started = std::time::Instant::now();
        let delivered = deliver_due(
            &store,
            &SilentSender,
            &observers,
            Duration::from_millis(50),
            1,
        )
        .await
        .unwrap();

        assert_eq!(delivered, 0);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(kinds(&store, &id), ["observer_pending", "observer_failed"]);
    }

    #[tokio::test]
    async fn a_notice_older_than_a_day_or_for_a_removed_observer_is_not_sent() {
        let store = store();
        let sender = FakeSender::default();
        let (kept, removed) = (observer(), observer());
        let (old, fresh) = (dispute(), dispute());
        queue_in(&store, &[kept], &old, "mediating", 0);
        queue_in(&store, &[kept, removed], &fresh, "mediating", DAY);

        let delivered = deliver_due(&store, &sender, &[kept], Duration::from_secs(5), DAY + 1)
            .await
            .unwrap();

        assert_eq!(delivered, 1);
        assert_eq!(
            sender.texts(),
            [format!("Dispute {fresh} · mediating")],
            "only the fresh notice, only to the observer still configured"
        );
    }

    #[test]
    fn retries_back_off_from_a_minute_to_an_hour() {
        assert!(due(0, None, 0));
        assert!(!due(1, Some(100), 100 + 59));
        assert!(due(1, Some(100), 100 + 60));
        assert!(!due(2, Some(100), 100 + 119));
        assert!(due(2, Some(100), 100 + 120));
        assert!(!due(30, Some(100), 100 + 3599));
        assert!(due(30, Some(100), 100 + 3600));
    }
}
