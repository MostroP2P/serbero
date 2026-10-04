//! T5.2: against a simulated Mostro node on a local relay, Serbero takes an
//! eligible dispute, opens its session, greets both parties in the default
//! language, and tells the solvers; an ineligible dispute is never taken,
//! and a failed take leaves plain notification as it was.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mostro_core::chat::unwrap_chat_message;
use mostro_core::dispute::SolverDisputeInfo;
use mostro_core::error::CantDoReason;
use mostro_core::message::{Action, Message, Payload};
use mostro_core::transport::WrapOptions;
use mostro_core::transport::{unwrap_message_nip44, wrap_message_nip44};
use nostr_sdk::prelude::*;
use serbero::catalog::{Amount, Catalogs};
use serbero::chat::channels::Chats;
use serbero::config::Permission;
use serbero::error::Result as SerberoResult;
use serbero::mediation::eligibility::Ineligible;
use serbero::mediation::{MediationSettings, Mediator, NodeSupport, Opening, ReadyJudge};
use serbero::mostro::chat::ChannelKeys;
use serbero::nostr::dm::DmSender;
use serbero::notifier::Solver;
use serbero::store::disputes::{self, Initiator, Lifecycle, NewDispute};
use serbero::store::sessions::{self, SessionState};
use serbero::store::{Store, events};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(10);

/// Solver DMs, kept instead of sent. While `failing` is set, every send
/// fails, as when no relay accepts it.
#[derive(Clone, Default)]
struct Outbox {
    sent: Arc<Mutex<Vec<(PublicKey, String)>>>,
    failing: Arc<std::sync::atomic::AtomicBool>,
    /// While set, every send waits forever, as on a relay that never answers.
    hanging: Arc<std::sync::atomic::AtomicBool>,
}

impl DmSender for Outbox {
    async fn send_dm(
        &self,
        to: PublicKey,
        _dispute_id: Option<uuid::Uuid>,
        text: &str,
    ) -> SerberoResult<()> {
        if self.hanging.load(std::sync::atomic::Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(serbero::error::Error::Nostr("rate-limited".into()));
        }
        self.sent.lock().unwrap().push((to, text.to_owned()));
        Ok(())
    }
}

impl Outbox {
    /// Texts sent to solvers, in order: observers are left out, so the
    /// existing assertions read what solvers got.
    fn texts(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(to, _)| *to != observer())
            .map(|(_, text)| text.clone())
            .collect()
    }

    fn texts_to(&self, recipient: &PublicKey) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(to, _)| to == recipient)
            .map(|(_, text)| text.clone())
            .collect()
    }

    fn hang(&self, hanging: bool) {
        self.hanging
            .store(hanging, std::sync::atomic::Ordering::SeqCst);
    }

    fn fail(&self, failing: bool) {
        self.failing
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Opening never calls the judge; this one fails if it is called.
struct NeverJudge;

impl serbero::judge::Judge for NeverJudge {
    fn id(&self) -> &str {
        "test/never"
    }

    fn capabilities(&self) -> serbero::judge::Capabilities {
        serbero::judge::Capabilities {
            question_kinds: vec![],
            max_choice_options: 0,
            max_score_levels: 0,
            max_context_tokens: None,
        }
    }

    fn evaluate<'a>(
        &'a self,
        _state: &'a serde_json::Value,
        _questions: &'a serbero::judge::QuestionSet,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<serbero::judge::Judged, serbero::judge::JudgeError>,
    > {
        panic!("opening must not call the judge")
    }

    fn health_check(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<(), serbero::judge::JudgeError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Accept,
    Refuse,
    AcceptWithoutKeys,
}

struct FakeNode {
    keys: Keys,
    buyer: Keys,
    seller: Keys,
    takes: Arc<AtomicUsize>,
}

fn tagged(kind: u16, tags: &[&[&str]], signer: &Keys) -> Event {
    let tags = tags.iter().map(|t| Tag::parse(t.iter().copied()).unwrap());
    EventBuilder::new(Kind::Custom(kind), "")
        .tags(tags)
        .finalize(signer)
        .unwrap()
}

/// A protocol v2 node with one order, answering `admin-take-dispute`
/// according to `mode` and counting the requests it gets.
async fn start_node(url: &str, mode: Mode) -> FakeNode {
    let node = FakeNode {
        keys: Keys::generate(),
        buyer: Keys::generate(),
        seller: Keys::generate(),
        takes: Arc::new(AtomicUsize::new(0)),
    };
    let order_id = Uuid::new_v4();
    let client = serbero::nostr::connect(&[url.to_owned()], WAIT)
        .await
        .unwrap();
    let me = node.keys.public_key().to_hex();
    let order = order_id.to_string();
    client
        .send_event(&tagged(
            38385,
            &[&["d", &me], &["protocol_version", "2"], &["pow", "0"]],
            &node.keys,
        ))
        .await
        .unwrap();
    client
        .send_event(&tagged(
            38383,
            &[
                &["d", &order],
                &["f", "ARS"],
                &["published_at", "1700"],
                &["s", "in-progress"],
            ],
            &node.keys,
        ))
        .await
        .unwrap();
    let mut notifications = client.notifications();
    client
        .subscribe(
            Filter::new()
                .kind(Kind::PrivateDirectMessage)
                .pubkey(node.keys.public_key()),
        )
        .await
        .unwrap();
    let (keys, takes) = (node.keys.clone(), Arc::clone(&node.takes));
    let (buyer, seller) = (node.buyer.public_key(), node.seller.public_key());
    tokio::spawn(async move {
        while let Some(n) = notifications.next().await {
            let ClientNotification::Event { event, .. } = n else {
                continue;
            };
            let Ok(Some(msg)) = unwrap_message_nip44(&event, &keys) else {
                continue;
            };
            let kind = msg.message.get_inner_message_kind();
            if kind.action != Action::AdminTakeDispute {
                continue;
            }
            takes.fetch_add(1, Ordering::SeqCst);
            let dispute = kind.id.unwrap();
            let info = |with_keys: bool| SolverDisputeInfo {
                id: order_id,
                buyer_pubkey: with_keys.then(|| buyer.to_hex()),
                seller_pubkey: with_keys.then(|| seller.to_hex()),
                fiat_amount: 50_000,
                payment_method: "Mercado Pago".into(),
                ..Default::default()
            };
            let took = |info| {
                Message::new_dispute(
                    Some(dispute),
                    kind.request_id,
                    None,
                    Action::AdminTookDispute,
                    Some(Payload::Dispute(dispute, Some(info))),
                )
            };
            let reply = match mode {
                Mode::Accept => took(info(true)),
                Mode::AcceptWithoutKeys => took(info(false)),
                Mode::Refuse => Message::cant_do(
                    Some(dispute),
                    kind.request_id,
                    Some(Payload::CantDo(Some(CantDoReason::NotAllowedByStatus))),
                ),
            };
            let event =
                wrap_message_nip44(&reply, &keys, &keys, msg.identity, WrapOptions::default())
                    .unwrap();
            client.send_event(&event).await.unwrap();
        }
    });
    node
}

/// A store with one dispute in `lifecycle`.
fn store_with_dispute(dispute_id: &str, lifecycle: Lifecycle) -> Arc<Mutex<Store>> {
    let store = Store::open_in_memory().unwrap();
    disputes::insert_if_new(
        store.conn(),
        &NewDispute {
            dispute_id,
            initiator: Initiator::Buyer,
            status: "initiated",
            status_at: 1,
            now: 1,
        },
    )
    .unwrap();
    if lifecycle != Lifecycle::New {
        disputes::set_lifecycle(store.conn(), dispute_id, lifecycle, 2).unwrap();
    }
    Arc::new(Mutex::new(store))
}

struct Harness {
    mediator: Mediator<Outbox>,
    serbero: Keys,
    outbox: Outbox,
    url: String,
}

async fn harness(
    url: &str,
    node: &FakeNode,
    store: &Arc<Mutex<Store>>,
    enabled: bool,
    ready: bool,
) -> Harness {
    let serbero = Keys::generate();
    let client = serbero::nostr::connect(&[url.to_owned()], WAIT)
        .await
        .unwrap();
    let chats = Arc::new(Chats::new(
        client.clone(),
        serbero.clone(),
        Arc::clone(store),
        2000,
        Default::default(),
    ));
    let outbox = Outbox::default();
    let mediator = Mediator {
        client,
        keys: serbero.clone(),
        mostro: node.keys.public_key(),
        store: Arc::clone(store),
        gate: Arc::default(),
        chats,
        catalogs: Catalogs::embedded().unwrap(),
        settings: MediationSettings {
            enabled,
            default_language: "en".into(),
            languages: vec!["en".into(), "es".into(), "pt".into()],
            max_rounds: 3,
            max_message_chars: 2000,
            max_messages_per_turn: 10,
            response_timeout: Duration::from_secs(1800),
            self_resolution_timeout: Duration::from_secs(7200),
        },
        sender: outbox.clone(),
        solvers: vec![Solver {
            pubkey: Keys::generate().public_key(),
            permission: Permission::Write,
        }],
        observers: vec![],
        observer_wake: Default::default(),
        own_takes: Arc::default(),
        judge: Default::default(),
        finishing: Default::default(),
    };
    if ready {
        mediator.set_ready(ReadyJudge {
            judge: Arc::new(NeverJudge),
            thresholds: serde_json::from_value(serde_json::json!({
                "guide": 0.9, "fact": 0.8, "human_request": 0.8,
                "fraud": 0.6, "conflict": 0.75, "outside_scope": 0.8
            }))
            .unwrap(),
            turn: serbero::judge::questions::TurnQuestions::new(&[]),
        });
    }
    Harness {
        mediator,
        serbero,
        outbox,
        url: url.to_owned(),
    }
}

/// The observer some tests add, e.g. mostro-watchdog relaying to a team
/// chat.
fn observer() -> PublicKey {
    PublicKey::from_hex("e493dbf1c10d80f3581e4904930b1404cc6c13900ee0758474fa94abe8c4cd13").unwrap()
}

/// Listens as a party on its channel with Serbero and returns the first
/// message Serbero sends there.
async fn listen_as(
    party: &Keys,
    serbero: &PublicKey,
    url: &str,
) -> tokio::task::JoinHandle<String> {
    let client = serbero::nostr::connect(&[url.to_owned()], WAIT)
        .await
        .unwrap();
    let side = ChannelKeys::derive(party, serbero).unwrap();
    let mut inbox = client.notifications();
    client
        .subscribe(
            Filter::new()
                .kind(Kind::PrivateDirectMessage)
                .author(side.author_pubkey()),
        )
        .await
        .unwrap();
    let serbero = *serbero;
    tokio::spawn(async move {
        let event = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(ClientNotification::Event { event, .. }) = inbox.next().await {
                    return event;
                }
            }
        })
        .await
        .unwrap();
        let _keep = client;
        unwrap_chat_message(
            side.conv(),
            &side.author_pubkey(),
            &[serbero],
            &event,
            Timestamp::now(),
        )
        .unwrap()
        .content
    })
}

fn event_kinds(store: &Arc<Mutex<Store>>, dispute_id: &str) -> Vec<String> {
    events::list_for_dispute(store.lock().unwrap().conn(), dispute_id)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn an_eligible_dispute_is_taken_and_both_parties_are_greeted() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::Accept).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let h = harness(&url, &node, &store, true, true).await;
    let buyer_inbox = listen_as(&node.buyer, &h.serbero.public_key(), &h.url).await;
    let seller_inbox = listen_as(&node.seller, &h.serbero.public_key(), &h.url).await;

    let started = serbero::daemon::now();
    let opening = h.mediator.consider(&dispute_id, 1_000).await;

    let Opening::Opened { session_id } = opening else {
        panic!("not opened: {opening:?}");
    };
    let session = sessions::get(store.lock().unwrap().conn(), &session_id)
        .unwrap()
        .unwrap();
    assert!(
        session.opened_at >= started,
        "stamped when inserted, after the take, not when first considered"
    );
    assert_eq!(session.state, SessionState::Active);
    assert_eq!(session.buyer_trade_pubkey, node.buyer.public_key().to_hex());
    assert_eq!(
        session.seller_trade_pubkey,
        node.seller.public_key().to_hex()
    );
    assert_eq!(session.fiat_amount.as_deref(), Some("50000"));
    assert_eq!(session.fiat_code.as_deref(), Some("ARS"));
    assert_eq!(session.payment_method.as_deref(), Some("Mercado Pago"));
    assert_eq!(session.order_published_at, Some(1700));

    let en = Catalogs::embedded().unwrap();
    let en = en.get("en").unwrap();
    let amount = Some(Amount {
        value: "50000",
        currency: "ARS",
    });
    assert_eq!(
        buyer_inbox.await.unwrap(),
        en.render_opening("ask_buyer_sent", amount).unwrap()
    );
    assert_eq!(
        seller_inbox.await.unwrap(),
        en.render_opening("ask_seller_received", amount).unwrap()
    );

    assert_eq!(
        h.outbox.texts(),
        [serbero::solver::mediation_started(&dispute_id)]
    );
    assert!(event_kinds(&store, &dispute_id).contains(&"mediation_opened".to_owned()));
    assert!(
        h.mediator.own_takes.lock().unwrap().is_empty(),
        "the take is no longer in flight"
    );
}

#[tokio::test]
async fn a_dispute_is_never_mediated_twice() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::Accept).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let h = harness(&url, &node, &store, true, true).await;

    assert!(matches!(
        h.mediator.consider(&dispute_id, 1_000).await,
        Opening::Opened { .. }
    ));
    let again = h.mediator.consider(&dispute_id, 1_001).await;

    assert_eq!(again, Opening::Ineligible(Ineligible::HadSession));
    assert_eq!(node.takes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_ineligible_dispute_is_never_taken() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::Accept).await;

    for (enabled, ready, lifecycle, reason) in [
        (false, true, Lifecycle::Notified, Ineligible::Disabled),
        (true, false, Lifecycle::Notified, Ineligible::JudgeNotReady),
        (
            true,
            true,
            Lifecycle::New,
            Ineligible::NotNotified(Lifecycle::New),
        ),
        (
            true,
            true,
            Lifecycle::Taken,
            Ineligible::NotNotified(Lifecycle::Taken),
        ),
    ] {
        let dispute_id = Uuid::new_v4().to_string();
        let store = store_with_dispute(&dispute_id, lifecycle);
        let h = harness(&url, &node, &store, enabled, ready).await;

        let opening = h.mediator.consider(&dispute_id, 1_000).await;

        assert_eq!(opening, Opening::Ineligible(reason));
        assert!(h.outbox.texts().is_empty());
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        node.takes.load(Ordering::SeqCst),
        0,
        "no take request was sent"
    );
}

#[tokio::test]
async fn a_refused_take_leaves_notification_as_it_was() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::Refuse).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let h = harness(&url, &node, &store, true, true).await;

    let opening = h.mediator.consider(&dispute_id, 1_000).await;

    assert!(
        matches!(opening, Opening::NotTaken(ref r) if r.contains("take failed")),
        "{opening:?}"
    );
    assert!(!sessions::exists_for_dispute(store.lock().unwrap().conn(), &dispute_id).unwrap());
    assert!(
        h.outbox.texts().is_empty(),
        "solvers keep the plain notification"
    );
    assert!(event_kinds(&store, &dispute_id).contains(&"mediation_not_opened".to_owned()));
    let dispute = disputes::get(store.lock().unwrap().conn(), &dispute_id)
        .unwrap()
        .unwrap();
    assert_eq!(dispute.lifecycle, Lifecycle::Notified);
}

#[tokio::test]
async fn a_take_without_trade_keys_hands_the_dispute_to_the_solvers() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::AcceptWithoutKeys).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let h = harness(&url, &node, &store, true, true).await;

    let opening = h.mediator.consider(&dispute_id, 1_000).await;

    assert!(
        matches!(opening, Opening::HandedOff(ref r) if r.contains("trade keys")),
        "{opening:?}"
    );
    assert_eq!(
        h.outbox.texts(),
        [serbero::solver::opening_failed(&dispute_id)]
    );
    assert!(event_kinds(&store, &dispute_id).contains(&"mediation_failed".to_owned()));
}

#[tokio::test]
async fn an_opening_failure_no_solver_heard_of_is_retried_by_the_timer() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::AcceptWithoutKeys).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let h = harness(&url, &node, &store, true, true).await;
    h.outbox.fail(true);

    let opening = h.mediator.consider(&dispute_id, 1_000).await;

    assert!(matches!(opening, Opening::HandedOff(_)), "{opening:?}");
    assert!(h.outbox.texts().is_empty());
    h.outbox.fail(false);

    // However long it stayed undelivered, it is still retried.
    let month_later = 1_000 + 30 * 24 * 3600;
    h.mediator.tick(month_later).await.unwrap();
    h.mediator.tick(month_later + 100).await.unwrap();

    assert_eq!(
        h.outbox.texts(),
        [serbero::solver::opening_failed(&dispute_id)],
        "sent once, on the first tick after the failure"
    );
    let kinds = event_kinds(&store, &dispute_id);
    let position = |kind: &str| kinds.iter().position(|k| k == kind).unwrap();
    assert!(
        position("opening_notice_pending") < position("notification_failed"),
        "recorded before the first attempt: {kinds:?}"
    );
    assert!(kinds.contains(&"opening_notice_sent".to_owned()));
}

#[tokio::test]
async fn an_opening_notice_survives_a_failed_audit_write() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::AcceptWithoutKeys).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let h = harness(&url, &node, &store, true, true).await;
    h.outbox.fail(true);
    // Recording the failed attempt fails too.
    store
        .lock()
        .unwrap()
        .conn()
        .execute_batch(
            "CREATE TRIGGER no_audit BEFORE INSERT ON events
             WHEN NEW.kind = 'notification_failed'
             BEGIN SELECT RAISE(ABORT, 'audit write failed'); END;",
        )
        .unwrap();

    let opening = h.mediator.consider(&dispute_id, 1_000).await;

    assert!(matches!(opening, Opening::HandedOff(_)), "{opening:?}");
    store
        .lock()
        .unwrap()
        .conn()
        .execute_batch("DROP TRIGGER no_audit;")
        .unwrap();
    h.outbox.fail(false);
    h.mediator.tick(1_100).await.unwrap();
    assert_eq!(
        h.outbox.texts(),
        [serbero::solver::opening_failed(&dispute_id)]
    );
}

#[tokio::test]
async fn an_opening_notice_survives_a_cancelled_send() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::AcceptWithoutKeys).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let h = harness(&url, &node, &store, true, true).await;
    let outbox = h.outbox.clone();
    outbox.hang(true);
    let mediator = Arc::new(h.mediator);

    // The process stops while the notice is on its way.
    let opening = tokio::spawn({
        let (mediator, dispute_id) = (Arc::clone(&mediator), dispute_id.clone());
        async move { mediator.consider(&dispute_id, 1_000).await }
    });
    tokio::time::timeout(WAIT, async {
        while !event_kinds(&store, &dispute_id).contains(&"opening_notice_pending".to_owned()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    opening.abort();
    outbox.hang(false);

    mediator.tick(1_100).await.unwrap();

    assert_eq!(
        outbox.texts(),
        [serbero::solver::opening_failed(&dispute_id)]
    );
}

#[tokio::test]
async fn a_dispute_that_arrived_before_the_judge_was_ready_is_reconsidered() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::Accept).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let h = harness(&url, &node, &store, true, false).await;
    assert_eq!(
        h.mediator.consider(&dispute_id, 1_000).await,
        Opening::Ineligible(Ineligible::JudgeNotReady)
    );
    h.mediator.set_ready(ReadyJudge {
        judge: Arc::new(NeverJudge),
        thresholds: serde_json::from_value(serde_json::json!({
            "guide": 0.9, "fact": 0.8, "human_request": 0.8,
            "fraud": 0.6, "conflict": 0.75, "outside_scope": 0.8
        }))
        .unwrap(),
        turn: serbero::judge::questions::TurnQuestions::new(&[]),
    });

    let opened = h.mediator.reconsider(0, 1_100).await;

    assert_eq!(opened, 1);
    assert!(sessions::exists_for_dispute(store.lock().unwrap().conn(), &dispute_id).unwrap());
}

/// AGENTS.md relays rule 7: opening talks to the node, but a silent relay
/// must never slow notification. The hook only spawns the opening.
#[tokio::test]
async fn a_silent_relay_never_delays_notification() {
    let silent = MockRelay::run_with_opts(LocalRelayTestOptions {
        unresponsive_connection: Some(Duration::from_secs(60)),
        ..Default::default()
    })
    .await
    .unwrap();
    let url = silent.url().await.to_string();
    let mostro = Keys::generate();
    let dispute_id = Uuid::new_v4().to_string();
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let node = FakeNode {
        keys: mostro.clone(),
        buyer: Keys::generate(),
        seller: Keys::generate(),
        takes: Arc::default(),
    };
    let h = harness(&url, &node, &store, true, true).await;
    let mediator = Arc::new(h.mediator);
    let notifier = serbero::notifier::Notifier::new(
        Arc::clone(&store),
        Outbox::default(),
        vec![Solver {
            pubkey: Keys::generate().public_key(),
            permission: Permission::Write,
        }],
        mostro.public_key(),
    );
    let hook = Arc::clone(&mediator);
    notifier.on_new_dispute(Box::new(move |id| {
        let (mediator, id) = (Arc::clone(&hook), id.to_owned());
        tokio::spawn(async move { mediator.consider(&id, 1_000).await });
    }));
    let event = EventBuilder::new(Kind::Custom(38386), "")
        .tags(
            [
                ["d", dispute_id.as_str()],
                ["s", "initiated"],
                ["initiator", "buyer"],
                ["y", "mostro"],
                ["z", "dispute"],
            ]
            .into_iter()
            .map(|t| Tag::parse(t).unwrap()),
        )
        .finalize(&mostro)
        .unwrap();

    let started = std::time::Instant::now();
    notifier.handle_event(&event, 1_000).await.unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    let dispute = disputes::get(store.lock().unwrap().conn(), &dispute_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        dispute.lifecycle,
        Lifecycle::Notified,
        "solvers were told at once"
    );
}

/// `docs/spec.md` §5.1: the node's protocol is checked at startup.
#[tokio::test]
async fn the_node_protocol_is_checked_before_any_dispute() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let publisher = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let node_with = |version: Option<&'static str>| {
        let publisher = publisher.clone();
        async move {
            let node = FakeNode {
                keys: Keys::generate(),
                buyer: Keys::generate(),
                seller: Keys::generate(),
                takes: Arc::default(),
            };
            if let Some(version) = version {
                let me = node.keys.public_key().to_hex();
                publisher
                    .send_event(&tagged(
                        38385,
                        &[&["d", &me], &["protocol_version", version], &["pow", "0"]],
                        &node.keys,
                    ))
                    .await
                    .unwrap();
            }
            node
        }
    };

    for (version, expected) in [
        (Some("2"), "v2"),
        (Some("1"), "unsupported"),
        (None, "unknown"),
    ] {
        let node = node_with(version).await;
        let h = harness(&url, &node, &store, true, true).await;

        let support = match h.mediator.check_node().await {
            NodeSupport::V2 => "v2",
            NodeSupport::Unsupported => "unsupported",
            NodeSupport::Unknown(_) => "unknown",
        };

        assert_eq!(support, expected, "{version:?}");
    }
}

#[tokio::test]
async fn observers_hear_that_mediation_started_in_one_line() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::Accept).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let mut h = harness(&url, &node, &store, true, true).await;
    h.mediator.observers = vec![observer()];

    let opening = h.mediator.consider(&dispute_id, 1_000).await;

    assert!(matches!(opening, Opening::Opened { .. }), "{opening:?}");
    assert_eq!(
        h.outbox.texts(),
        [serbero::solver::mediation_started(&dispute_id)],
        "solvers still get the full message"
    );
    assert!(
        h.outbox.texts_to(&observer()).is_empty(),
        "only queued: the opening never waits for an observer"
    );
    h.mediator.deliver_observer_notices(1_001).await.unwrap();
    assert_eq!(
        h.outbox.texts_to(&observer()),
        [format!("Dispute {dispute_id} · mediating")]
    );
}

#[tokio::test]
async fn an_opening_failure_reaches_an_observer_once_across_retries() {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let node = start_node(&url, Mode::AcceptWithoutKeys).await;
    let dispute_id = Uuid::new_v4().to_string();
    let store = store_with_dispute(&dispute_id, Lifecycle::Notified);
    let mut h = harness(&url, &node, &store, true, true).await;
    h.mediator.observers = vec![observer()];
    h.outbox.fail(true);

    let opening = h.mediator.consider(&dispute_id, 1_000).await;
    h.mediator.deliver_observer_notices(1_001).await.unwrap();
    h.outbox.fail(false);
    // The solvers' notice is retried by the timer; the observer's by its
    // own backoff, a minute after the failure.
    h.mediator.tick(1_030).await.unwrap();
    h.mediator.deliver_observer_notices(1_030).await.unwrap();
    assert!(h.outbox.texts_to(&observer()).is_empty(), "backing off");
    for now in [1_061, 1_200, 5_000] {
        h.mediator.deliver_observer_notices(now).await.unwrap();
    }

    assert!(matches!(opening, Opening::HandedOff(_)), "{opening:?}");
    assert_eq!(
        h.outbox.texts_to(&observer()),
        [format!("Dispute {dispute_id} · mediation could not start")],
        "missed while relays failed, delivered on the retry, never twice"
    );
}
