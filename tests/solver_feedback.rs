//! T6.1: a solver's `wrong <question>` reply, sent as a Mostro DM through a
//! relay, is recorded against the evaluation behind the brief they got.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nostr_sdk::prelude::*;
use serbero::config::Permission;
use serbero::nostr::dm::solver_dm;
use serbero::notifier::Solver;
use serbero::store::disputes::{self, Initiator, NewDispute};
use serbero::store::sessions::{self, NewSession};
use serbero::store::{Store, evaluations, events};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(10);

/// Dispute `d1` with session `s1`, one turn evaluation that asked
/// `seller_receipt`, and a brief sent to `solver`. Returns the store and the
/// evaluation id.
fn briefed_store(solver: &Keys) -> (Arc<Mutex<Store>>, i64) {
    let store = Store::open_in_memory().unwrap();
    disputes::insert_if_new(
        store.conn(),
        &NewDispute {
            dispute_id: "d1",
            initiator: Initiator::Buyer,
            status: "in-progress",
            status_at: 1,
            now: 1,
        },
    )
    .unwrap();
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
            now: 1,
        },
    )
    .unwrap();
    let (answers, action) = (json!({ "seller_receipt": {} }), json!("wait"));
    let evaluation = evaluations::insert(
        store.conn(),
        &evaluations::NewEvaluation {
            session_id: "s1",
            question_set_version: "qs-1-x",
            judge_id: "test",
            last_message_id: 0,
            answers: &answers,
            action: &action,
            input_tokens: None,
            latency_ms: None,
            now: 2,
        },
    )
    .unwrap();
    events::append(
        store.conn(),
        &events::NewEvent {
            dispute_id: "d1",
            session_id: None,
            kind: "notification_sent",
            payload: json!({ "notification": "brief", "solver": solver.public_key().to_hex() }),
            now: 3,
        },
    )
    .unwrap();
    (Arc::new(Mutex::new(store)), evaluation)
}

async fn wait_for_feedback(store: &Mutex<Store>, within: Duration) -> events::Event {
    tokio::time::timeout(within, async {
        loop {
            let found = events::list_for_dispute(store.lock().unwrap().conn(), "d1")
                .unwrap()
                .into_iter()
                .find(|e| e.kind == "solver_feedback");
            if let Some(event) = found {
                return event;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap()
}

fn listen(
    client: Client,
    serbero: &Keys,
    solver: &Keys,
    store: &Arc<Mutex<Store>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(serbero::daemon::solver_replies(
        client,
        serbero.clone(),
        Arc::clone(store),
        vec![Solver {
            pubkey: solver.public_key(),
            permission: Permission::Write,
        }],
        Default::default(),
    ))
}

#[tokio::test]
async fn a_solver_reply_is_recorded_against_the_briefed_evaluation() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let (serbero, solver) = (Keys::generate(), Keys::generate());
    let (store, evaluation) = briefed_store(&solver);
    let client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let task = listen(client, &serbero, &solver, &store);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let solver_client = serbero::nostr::connect(&[url], WAIT).await.unwrap();
    solver_client
        .send_event(
            &solver_dm(&solver, serbero.public_key(), None, "wrong seller_receipt").unwrap(),
        )
        .await
        .unwrap();

    let recorded = wait_for_feedback(&store, WAIT).await;
    task.abort();

    assert_eq!(recorded.payload["evaluation_id"], evaluation);
    assert_eq!(recorded.payload["question"], "seller_receipt");
    assert_eq!(recorded.session_id.as_deref(), Some("s1"));
}

#[tokio::test]
async fn a_reply_sent_while_serbero_was_offline_is_recorded_once_at_startup() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let (serbero, solver) = (Keys::generate(), Keys::generate());
    let (store, evaluation) = briefed_store(&solver);
    let solver_client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    solver_client
        .send_event(
            &solver_dm(&solver, serbero.public_key(), None, "wrong seller_receipt").unwrap(),
        )
        .await
        .unwrap();
    // Serbero starts later than the reply, in a later second.
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let first = listen(client.clone(), &serbero, &solver, &store);
    let recorded = wait_for_feedback(&store, WAIT).await;
    first.abort();
    // A restart replays the same DM.
    let second = listen(client, &serbero, &solver, &store);
    tokio::time::sleep(Duration::from_millis(500)).await;
    second.abort();

    assert_eq!(recorded.payload["evaluation_id"], evaluation);
    let feedback = events::list_for_dispute(store.lock().unwrap().conn(), "d1")
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "solver_feedback")
        .count();
    assert_eq!(feedback, 1);
}

/// AGENTS.md relays rule 7: a silent relay never delays solver feedback
/// from a healthy one.
#[tokio::test]
async fn a_silent_relay_never_delays_solver_feedback() {
    let healthy = MockRelay::run().await.unwrap();
    let silent = MockRelay::run_with_opts(LocalRelayTestOptions {
        unresponsive_connection: Some(Duration::from_secs(60)),
        ..Default::default()
    })
    .await
    .unwrap();
    let healthy_url = healthy.url().await.to_string();
    let (serbero, solver) = (Keys::generate(), Keys::generate());
    let (store, evaluation) = briefed_store(&solver);
    let client = serbero::nostr::connect(
        &[healthy_url.clone(), silent.url().await.to_string()],
        Duration::from_millis(500),
    )
    .await
    .unwrap();
    let task = listen(client, &serbero, &solver, &store);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let solver_client = serbero::nostr::connect(&[healthy_url], WAIT).await.unwrap();
    solver_client
        .send_event(
            &solver_dm(&solver, serbero.public_key(), None, "wrong seller_receipt").unwrap(),
        )
        .await
        .unwrap();

    let recorded = wait_for_feedback(&store, Duration::from_secs(2)).await;
    task.abort();

    assert_eq!(recorded.payload["evaluation_id"], evaluation);
}
