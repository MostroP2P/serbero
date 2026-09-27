//! Staging: Serbero against a real mostrod, using an Ortsom regtest stack.
//!
//! Ignored by default. Run it with `scripts/staging-ortsom.sh`, which reads
//! the stack's identities from Ortsom's local files. The test:
//!
//! 1. registers a fresh Serbero identity on the daemon as a `read` solver;
//! 2. starts Serbero's dispute detection against the stack's relay;
//! 3. runs Ortsom's `dispute_by_buyer`, which opens a real dispute;
//! 4. checks that Serbero notifies the solver, takes the dispute, reads the
//!    order facts, and writes to the buyer;
//! 5. lets Ortsom's teardown hand the dispute to its `write` solver, which
//!    takes it over and cancels it, and checks that Serbero supersedes its
//!    session and records the resolution.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mostro_core::message::{Action, Message, Payload};
use mostro_core::nip59::WrapOptions;
use mostro_core::transport::wrap_message_nip44;
use nostr_sdk::prelude::*;
use serbero::chat::{Outbound, send_to_party};
use serbero::config::Permission;
use serbero::daemon;
use serbero::mostro::{order, take};
use serbero::nostr::dm::RelayDmSender;
use serbero::notifier::{Notifier, Solver};
use serbero::store::disputes::{self, Lifecycle};
use serbero::store::sessions::{self, NewSession, Party, SessionState};
use serbero::store::{Store, events};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(10);

fn env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is not set; use scripts/staging-ortsom.sh"))
}

async fn wait_until<T>(limit: Duration, mut check: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(limit, async {
        loop {
            if let Some(value) = check() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("timed out")
}

async fn register_read_solver(client: &Client, daemon: &Keys, solver: PublicKey) {
    let message = Message::new_dispute(
        Some(Uuid::new_v4()),
        Some(Uuid::new_v4().as_u64_pair().0),
        None,
        Action::AdminAddSolver,
        Some(Payload::TextMessage(format!(
            "{}:read",
            solver.to_bech32().unwrap()
        ))),
    );
    // The daemon's own key is the admin: identity and trade key coincide.
    let event = wrap_message_nip44(
        &message,
        daemon,
        daemon,
        daemon.public_key(),
        WrapOptions::default(),
    )
    .unwrap();
    client.send_event(&event).await.unwrap();
}

#[tokio::test]
#[ignore = "needs an Ortsom regtest stack; run scripts/staging-ortsom.sh"]
async fn serbero_handles_a_real_dispute_end_to_end() {
    let relay = env("SERBERO_STAGING_RELAY");
    let mostro = PublicKey::parse(&env("SERBERO_STAGING_MOSTRO_PUBKEY")).unwrap();
    let daemon = Keys::parse(&env("SERBERO_STAGING_DAEMON_NSEC")).unwrap();
    let human_solver = Keys::parse(&env("SERBERO_STAGING_SOLVER_NSEC")).unwrap();
    assert_eq!(
        daemon.public_key(),
        mostro,
        "daemon key does not match the stack's pubkey"
    );

    let serbero = Keys::generate();
    let client = serbero::nostr::connect(&[relay], WAIT).await.unwrap();
    register_read_solver(&client, &daemon, serbero.public_key()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    println!(
        "registered Serbero {} as a read solver",
        serbero.public_key()
    );

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let solver = Solver {
        pubkey: human_solver.public_key(),
        permission: Permission::Write,
    };
    let sender = RelayDmSender::new(client.clone(), serbero.clone());
    let notifier = Arc::new(Notifier::new(
        Arc::clone(&store),
        sender,
        vec![solver],
        mostro,
    ));
    let (notifications, _background) = daemon::start(&client, &notifier, mostro, 900)
        .await
        .unwrap();
    let loop_notifier = Arc::clone(&notifier);
    tokio::spawn(async move {
        daemon::event_loop(notifications, &loop_notifier, std::future::pending()).await
    });
    let known: Vec<String> = {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let store = store.lock().unwrap();
        let mut stmt = store
            .conn()
            .prepare("SELECT dispute_id FROM disputes")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };

    // A real dispute, opened by Ortsom's buyer.
    let ortsom_dir = env("SERBERO_STAGING_ORTSOM_DIR");
    let ortsom = tokio::process::Command::new(env("SERBERO_STAGING_ORTSOM_BIN"))
        .args(["run", "dispute_by_buyer"])
        .current_dir(&ortsom_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let dispute_id = wait_until(Duration::from_secs(90), || {
        let store = store.lock().unwrap();
        let mut stmt = store
            .conn()
            .prepare("SELECT dispute_id FROM disputes WHERE lifecycle = 'notified'")
            .unwrap();
        let ids: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        ids.into_iter().find(|id| !known.contains(id))
    })
    .await;
    println!("detected and notified dispute {dispute_id}");

    let info = take::take_dispute(
        &client,
        &serbero,
        mostro,
        dispute_id.parse().unwrap(),
        0,
        WAIT,
    )
    .await
    .expect("Serbero could not take the dispute");
    let facts = order::fetch(&client, mostro, &info.id.to_string(), WAIT)
        .await
        .unwrap();
    println!(
        "took it: order {} · {} {:?} · published_at {:?}",
        info.id, info.fiat_amount, facts.fiat_code, facts.published_at
    );
    assert!(info.buyer_pubkey.is_some() && info.seller_pubkey.is_some());
    assert_eq!(facts.fiat_code.as_deref(), Some("ARS"));
    assert!(
        facts.published_at.is_some(),
        "mostrod #1000 publishes published_at on orders"
    );

    let (buyer, seller) = (
        info.buyer_pubkey.clone().unwrap(),
        info.seller_pubkey.clone().unwrap(),
    );
    let fiat_amount = info.fiat_amount.to_string();
    let now = Timestamp::now().as_secs() as i64;
    {
        let store = store.lock().unwrap();
        assert!(
            sessions::insert(
                store.conn(),
                &NewSession {
                    session_id: "staging",
                    dispute_id: &dispute_id,
                    buyer_trade_pubkey: &buyer,
                    seller_trade_pubkey: &seller,
                    fiat_amount: Some(&fiat_amount),
                    fiat_code: facts.fiat_code.as_deref(),
                    payment_method: Some(&info.payment_method),
                    order_published_at: facts.published_at,
                    now,
                },
            )
            .unwrap()
        );
        sessions::set_state(store.conn(), "staging", SessionState::Active, now).unwrap();
    }
    let session = sessions::get(store.lock().unwrap().conn(), "staging")
        .unwrap()
        .unwrap();
    send_to_party(
        &client,
        &notifier.outbound_gate(),
        &store,
        &serbero,
        &session,
        &Outbound {
            party: Party::Buyer,
            text: "Did you send the payment for this order?",
            template_id: Some("ask_buyer_sent"),
            lang: Some("en"),
        },
    )
    .await
    .expect("could not write to the buyer");
    println!("wrote to the buyer on the dispute chat");

    // Ortsom's teardown hands the dispute to its write solver.
    let output = ortsom.wait_with_output().await.unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && !report.contains("[FAIL]"),
        "the Ortsom scenario itself failed:\n{report}"
    );
    println!(
        "ortsom: {}",
        report
            .lines()
            .filter(|l| l.contains("PASS") || l.contains("FAIL"))
            .collect::<Vec<_>>()
            .join(" | ")
    );

    let (state, lifecycle) = wait_until(Duration::from_secs(240), || {
        let store = store.lock().unwrap();
        let session = sessions::get(store.conn(), "staging").unwrap().unwrap();
        let dispute = disputes::get(store.conn(), &dispute_id).unwrap().unwrap();
        (session.state.is_terminal() && dispute.lifecycle == Lifecycle::Resolved)
            .then_some((session.state, dispute.lifecycle))
    })
    .await;
    let kinds: Vec<String> = events::list_for_dispute(store.lock().unwrap().conn(), &dispute_id)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    println!("session {state} · dispute {lifecycle} · events {kinds:?}");
    assert_eq!(
        state,
        SessionState::Superseded,
        "the write solver took over from Serbero"
    );
    assert!(kinds.contains(&"notification_sent".to_owned()));
    assert!(kinds.contains(&"session_superseded".to_owned()));
    assert!(kinds.contains(&"resolved".to_owned()));
}
