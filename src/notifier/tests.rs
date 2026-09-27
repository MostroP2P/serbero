use nostr_sdk::prelude::Keys;

use super::send::testing::FakeSender;
use super::testing::dispute_event;
use super::*;

fn notifier(mostro: &Keys) -> Notifier<FakeSender> {
    notifier_with(mostro, FakeSender::default(), vec![solver()])
}

fn notifier_with(mostro: &Keys, sender: FakeSender, solvers: Vec<Solver>) -> Notifier<FakeSender> {
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

    assert_eq!(change, Change::Taken);
    assert_eq!(lifecycle(&n, "d1"), Lifecycle::Taken);
}

#[tokio::test]
async fn taken_dispute_notifies_solvers_once() {
    let mostro = Keys::generate();
    let n = notifier(&mostro);
    n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
        .await
        .unwrap();
    let taken = dispute_event(&mostro, "d1", "in-progress", 200);

    n.handle_event(&taken, 1_001).await.unwrap();
    n.handle_event(&taken, 1_002).await.unwrap();

    let texts = n.sender.texts();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[1], "Dispute taken\ndispute: d1\ntaken by: a solver");
    assert_eq!(n.remind(900, 9_000).await.unwrap(), 0);
}

#[tokio::test]
async fn second_in_progress_revision_is_a_takeover() {
    let mostro = Keys::generate();
    let n = notifier(&mostro);
    n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
        .await
        .unwrap();
    n.handle_event(&dispute_event(&mostro, "d1", "in-progress", 200), 1_001)
        .await
        .unwrap();

    let change = n
        .handle_event(&dispute_event(&mostro, "d1", "in-progress", 300), 1_002)
        .await
        .unwrap();

    assert_eq!(change, Change::TakenAgain);
    assert_eq!(n.sender.texts().len(), 2);
}

#[tokio::test]
async fn every_final_status_resolves_the_dispute() {
    for (status, by) in [
        ("settled", "solver"),
        ("seller-refunded", "solver"),
        ("released", "parties"),
        ("cooperatively-canceled", "parties"),
    ] {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();
        n.handle_event(&dispute_event(&mostro, "d1", "in-progress", 200), 1_001)
            .await
            .unwrap();

        let change = n
            .handle_event(&dispute_event(&mostro, "d1", status, 300), 1_002)
            .await
            .unwrap();

        assert_eq!(
            change,
            Change::Resolved(status.parse().unwrap()),
            "{status}"
        );
        assert_eq!(lifecycle(&n, "d1"), Lifecycle::Resolved, "{status}");
        let store = n.store.lock().unwrap();
        let resolved = events::list_for_dispute(store.conn(), "d1")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "resolved")
            .unwrap();
        assert_eq!(resolved.payload["resolved_by"], by, "{status}");
    }
}

#[tokio::test]
async fn dispute_resolved_before_being_taken_is_resolved() {
    let mostro = Keys::generate();
    let n = notifier(&mostro);
    n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
        .await
        .unwrap();

    n.handle_event(&dispute_event(&mostro, "d1", "released", 200), 1_001)
        .await
        .unwrap();

    assert_eq!(lifecycle(&n, "d1"), Lifecycle::Resolved);
    assert_eq!(n.remind(900, 9_000).await.unwrap(), 0);
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

mod sessions_end {
    use super::*;
    use crate::store::sessions::{self, NewSession, SessionState};

    fn with_session(n: &Notifier<FakeSender>) {
        let store = n.store.lock().unwrap();
        sessions::insert(
            store.conn(),
            &NewSession {
                session_id: "s1",
                dispute_id: "d1",
                buyer_trade_pubkey: "b",
                seller_trade_pubkey: "s",
                fiat_amount: None,
                fiat_code: None,
                payment_method: None,
                order_published_at: None,
                now: 1_000,
            },
        )
        .unwrap();
    }

    fn state(n: &Notifier<FakeSender>) -> SessionState {
        let store = n.store.lock().unwrap();
        sessions::get(store.conn(), "s1").unwrap().unwrap().state
    }

    #[tokio::test]
    async fn serberos_own_take_does_not_end_the_session() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();
        with_session(&n);

        // The in-progress revision Mostro publishes for Serbero's own take.
        let change = n
            .handle_event(&dispute_event(&mostro, "d1", "in-progress", 200), 1_001)
            .await
            .unwrap();

        assert_eq!(change, Change::Taken);
        assert_eq!(state(&n), SessionState::Opening);
    }

    #[tokio::test]
    async fn a_takeover_supersedes_the_session() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();
        with_session(&n);
        n.handle_event(&dispute_event(&mostro, "d1", "in-progress", 200), 1_001)
            .await
            .unwrap();

        // The human's take comes after Serbero opened its session (1_000).
        let change = n
            .handle_event(&dispute_event(&mostro, "d1", "in-progress", 2_000), 2_001)
            .await
            .unwrap();

        assert_eq!(change, Change::TakenAgain);
        assert_eq!(state(&n), SessionState::Superseded);
        let store = n.store.lock().unwrap();
        let ended = events::list_for_dispute(store.conn(), "d1")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "session_superseded")
            .unwrap();
        assert_eq!(ended.session_id.as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn a_takeover_is_caught_even_if_serberos_own_take_was_missed() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();
        with_session(&n);

        // Only the human's later take reaches Serbero.
        n.handle_event(&dispute_event(&mostro, "d1", "in-progress", 2_000), 2_001)
            .await
            .unwrap();

        assert_eq!(state(&n), SessionState::Superseded);
    }

    #[tokio::test]
    async fn a_final_status_closes_the_session() {
        let mostro = Keys::generate();
        let n = notifier(&mostro);
        n.handle_event(&dispute_event(&mostro, "d1", "initiated", 100), 1_000)
            .await
            .unwrap();
        with_session(&n);
        n.handle_event(&dispute_event(&mostro, "d1", "in-progress", 200), 1_001)
            .await
            .unwrap();

        n.handle_event(&dispute_event(&mostro, "d1", "released", 300), 1_002)
            .await
            .unwrap();

        assert_eq!(state(&n), SessionState::Closed);
    }
}
