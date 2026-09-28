//! T5.3: session scripts over a local relay. Party messages reach the turn
//! task through the chat channels, a burst is judged once after the quiet
//! period, the policy's templates reach the party in its language, and
//! every turn is recorded as an evaluation.
//!
//! The judge is scripted: it answers from fixed rules and counts its calls,
//! so the scripts are deterministic and never call a live API.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use mostro_core::chat::{unwrap_chat_message, wrap_chat_message};
use nostr_sdk::prelude::*;
use serbero::catalog::{Amount, Catalogs};
use serbero::chat::channels::Chats;
use serbero::config::{Permission, Thresholds};
use serbero::error::Result as SerberoResult;
use serbero::judge::questions::{Language, TurnQuestions};
use serbero::judge::{
    Answer, Answers, Capabilities, Judge, JudgeError, Judged, Question, QuestionKind, QuestionSet,
};
use serbero::mediation::{MediationSettings, Mediator, ReadyJudge};
use serbero::mostro::chat::ChannelKeys;
use serbero::nostr::dm::DmSender;
use serbero::notifier::Solver;
use serbero::store::disputes::{self, Initiator, NewDispute};
use serbero::store::messages::{self, Direction, NewMessage};
use serbero::store::sessions::{self, NewSession, SessionState};
use serbero::store::{Store, evaluations};

const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(400);
const MAX_PER_TURN: u32 = 3;

/// Solver DMs, kept instead of sent.
#[derive(Clone, Default)]
struct Outbox(Arc<Mutex<Vec<String>>>);

impl DmSender for Outbox {
    async fn send_dm(&self, _to: PublicKey, text: &str) -> SerberoResult<()> {
        self.0.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

impl Outbox {
    fn texts(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// Answers every question with nothing stated, except the options `picks`
/// sets (question id → (option or yes, probability)).
struct ScriptedJudge {
    picks: BTreeMap<&'static str, (&'static str, f64)>,
    calls: AtomicUsize,
    /// Every request fails, as after the adapter's retries.
    down: std::sync::atomic::AtomicBool,
}

fn neutral(id: &str) -> &'static str {
    match id {
        "buyer_payment" | "seller_receipt" => "not_stated",
        "dispute_topic" => "not_yet_clear",
        _ if id.starts_with("quote_") => "none",
        _ if id.ends_with("_message_kind") => "answers",
        _ => "unknown",
    }
}

impl ScriptedJudge {
    fn answer(&self, id: &str, question: &Question) -> Answer {
        match question {
            Question::Noul { .. } => Answer::Noul {
                p_yes: self.picks.get(id).map_or(0.0, |(_, p)| *p),
            },
            Question::Choice { options, .. } => {
                let (pick, p) = self.picks.get(id).copied().unwrap_or((neutral(id), 1.0));
                let rest = if pick == neutral(id) { 0.0 } else { 1.0 - p };
                Answer::Choice {
                    probabilities: options
                        .iter()
                        .map(|(o, _)| {
                            let value = if o == pick {
                                p
                            } else if o == neutral(id) {
                                rest
                            } else {
                                0.0
                            };
                            (o.clone(), value)
                        })
                        .collect(),
                }
            }
            Question::Score { levels, .. } => Answer::Score {
                probabilities: vec![1.0 / levels.len() as f64; levels.len()],
            },
        }
    }
}

impl Judge for ScriptedJudge {
    fn id(&self) -> &str {
        "test/scripted"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            question_kinds: vec![
                QuestionKind::Noul,
                QuestionKind::Choice,
                QuestionKind::Score,
            ],
            max_choice_options: 255,
            max_score_levels: 10,
            max_context_tokens: None,
        }
    }

    fn evaluate<'a>(
        &'a self,
        _state: &'a serde_json::Value,
        questions: &'a QuestionSet,
    ) -> BoxFuture<'a, Result<Judged, JudgeError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.down.load(Ordering::SeqCst) {
            return Box::pin(async { Err(JudgeError::Unavailable("overloaded".into())) });
        }
        let answers: Answers = questions
            .questions
            .iter()
            .map(|(id, q)| (id.clone(), self.answer(id, q)))
            .collect();
        Box::pin(async move {
            Ok(Judged {
                answers,
                input_tokens: Some(1000),
            })
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), JudgeError>> {
        Box::pin(async { Ok(()) })
    }
}

fn thresholds() -> Thresholds {
    serde_json::from_value(serde_json::json!({
        "guide": 0.9, "fact": 0.8, "human_request": 0.8,
        "fraud": 0.6, "conflict": 0.75, "outside_scope": 0.8,
        "validated_languages": ["en", "es"]
    }))
    .unwrap()
}

struct Script {
    store: Arc<Mutex<Store>>,
    serbero: Keys,
    buyer: Keys,
    seller: Keys,
    mostro: Keys,
    notifier: Arc<serbero::notifier::Notifier<Outbox>>,
    mediator: Arc<Mediator<Outbox>>,
    url: String,
    judge: Arc<ScriptedJudge>,
    outbox: Outbox,
    _relay: MockRelay,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// A live session between Serbero and two parties, with the chat channels,
/// the turn task, and the scripted judge running. Both openers were sent in
/// English and are unanswered.
async fn script(picks: &[(&'static str, (&'static str, f64))]) -> Script {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let (serbero, buyer, seller) = (Keys::generate(), Keys::generate(), Keys::generate());
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
    let (b, s) = (buyer.public_key().to_hex(), seller.public_key().to_hex());
    sessions::insert(
        store.conn(),
        &NewSession {
            session_id: "s1",
            dispute_id: "d1",
            buyer_trade_pubkey: &b,
            seller_trade_pubkey: &s,
            fiat_amount: Some("50000"),
            fiat_code: Some("ARS"),
            payment_method: None,
            order_published_at: None,
            now: 1,
        },
    )
    .unwrap();
    sessions::set_state(store.conn(), "s1", SessionState::Active, 1).unwrap();
    // Both openers, as `Mediator::open` sends them.
    for (party, template, id) in [
        (
            serbero::store::sessions::Party::Buyer,
            "ask_buyer_sent",
            "opener-buyer",
        ),
        (
            serbero::store::sessions::Party::Seller,
            "ask_seller_received",
            "opener-seller",
        ),
    ] {
        messages::insert_if_new(
            store.conn(),
            &NewMessage {
                session_id: "s1",
                direction: Direction::Out,
                party,
                template_id: Some(template),
                lang: Some("en"),
                content: "…",
                attachments: 0,
                inner_event_id: id,
                created_at: 1,
            },
        )
        .unwrap();
    }
    let store = Arc::new(Mutex::new(store));
    let client = serbero::nostr::connect(std::slice::from_ref(&url), WAIT)
        .await
        .unwrap();
    let chats = Arc::new(Chats::new(
        client.clone(),
        serbero.clone(),
        Arc::clone(&store),
        2000,
        Default::default(),
    ));
    let judge = Arc::new(ScriptedJudge {
        picks: picks.iter().copied().collect(),
        calls: AtomicUsize::new(0),
        down: false.into(),
    });
    let outbox = Outbox::default();
    let mostro = Keys::generate();
    let catalogs = Catalogs::embedded().unwrap();
    let languages = [
        Language {
            code: "en",
            name: "English",
        },
        Language {
            code: "es",
            name: "Spanish",
        },
        Language {
            code: "pt",
            name: "Portuguese",
        },
    ];
    let mediator = Arc::new(Mediator {
        client: client.clone(),
        keys: serbero.clone(),
        mostro: mostro.public_key(),
        store: Arc::clone(&store),
        gate: Arc::default(),
        chats: Arc::clone(&chats),
        catalogs,
        settings: MediationSettings {
            enabled: true,
            default_language: "en".into(),
            languages: vec!["en".into(), "es".into(), "pt".into()],
            max_rounds: 3,
            max_message_chars: 2000,
            max_messages_per_turn: MAX_PER_TURN,
            response_timeout: Duration::from_secs(1800),
            self_resolution_timeout: Duration::from_secs(7200),
        },
        sender: outbox.clone(),
        solvers: vec![Solver {
            pubkey: Keys::generate().public_key(),
            permission: Permission::Write,
        }],
        own_takes: Arc::default(),
        judge: Default::default(),
    });
    mediator.set_ready(ReadyJudge {
        judge: Arc::clone(&judge) as Arc<dyn Judge>,
        thresholds: thresholds(),
        turn: TurnQuestions::new(&languages),
    });

    let notifier = Arc::new(serbero::notifier::Notifier::new(
        Arc::clone(&store),
        outbox.clone(),
        vec![],
        mostro.public_key(),
    ));
    let closing = Arc::clone(&mediator);
    notifier.on_resolved(Box::new(move |dispute_id, status, by_parties| {
        let (mediator, dispute_id, status) = (
            Arc::clone(&closing),
            dispute_id.to_owned(),
            status.to_owned(),
        );
        tokio::spawn(async move {
            mediator
                .finish(&dispute_id, &status, by_parties, 10_000)
                .await
                .unwrap();
        });
    }));
    let notifications = client.notifications();
    let session = sessions::get(store.lock().unwrap().conn(), "s1")
        .unwrap()
        .unwrap();
    chats.open(&session).await.unwrap();
    let (forward, received) = tokio::sync::mpsc::unbounded_channel();
    chats.forward_to(forward);
    let chat_task = Arc::clone(&chats);
    let tasks = vec![
        tokio::spawn(async move { chat_task.run(notifications).await }),
        tokio::spawn(Arc::clone(&mediator).run_turns(received, QUIET)),
    ];
    Script {
        store,
        serbero,
        buyer,
        seller,
        mostro,
        notifier,
        mediator,
        url,
        judge,
        outbox,
        _relay: relay,
        _tasks: tasks,
    }
}

/// A client for the buyer's side of its channel with Serbero.
struct PartySide {
    client: Client,
    keys: Keys,
    side: ChannelKeys,
    serbero: PublicKey,
    inbox: std::pin::Pin<Box<dyn futures_util::Stream<Item = ClientNotification> + Send>>,
}

async fn buyer_side(script: &Script) -> PartySide {
    party_side(script, &script.buyer).await
}

async fn seller_side(script: &Script) -> PartySide {
    party_side(script, &script.seller).await
}

async fn party_side(script: &Script, party: &Keys) -> PartySide {
    let client = serbero::nostr::connect(std::slice::from_ref(&script.url), WAIT)
        .await
        .unwrap();
    let side = ChannelKeys::derive(party, &script.serbero.public_key()).unwrap();
    let inbox = Box::pin(client.notifications());
    client
        .subscribe(
            Filter::new()
                .kind(Kind::PrivateDirectMessage)
                .author(side.author_pubkey()),
        )
        .await
        .unwrap();
    PartySide {
        client,
        keys: party.clone(),
        side,
        serbero: script.serbero.public_key(),
        inbox,
    }
}

impl PartySide {
    async fn say(&self, text: &str) {
        let event = wrap_chat_message(&self.keys, self.side.conv(), self.side.sign(), text)
            .await
            .unwrap();
        self.client.send_event(&event).await.unwrap();
    }

    /// The next message Serbero sends on this channel.
    async fn next_from_serbero(&mut self) -> String {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(ClientNotification::Event { event, .. }) = self.inbox.next().await
                    && let Ok(message) = unwrap_chat_message(
                        self.side.conv(),
                        &self.side.author_pubkey(),
                        &[self.serbero],
                        &event,
                        Timestamp::now(),
                    )
                    && message.sender == self.serbero
                {
                    return message.content;
                }
            }
        })
        .await
        .unwrap()
    }
}

fn en(template: &str) -> String {
    let catalogs = Catalogs::embedded().unwrap();
    catalogs
        .get("en")
        .unwrap()
        .render(
            template,
            Some(Amount {
                value: "50000",
                currency: "ARS",
            }),
        )
        .unwrap()
}

#[tokio::test]
async fn a_burst_of_messages_is_judged_once_and_answered_once() {
    let script = script(&[("buyer_message_kind", ("greeting", 1.0))]).await;
    let mut buyer = buyer_side(&script).await;

    buyer.say("hola").await;
    buyer.say("buenas").await;
    buyer.say("??").await;
    let reply = buyer.next_from_serbero().await;

    assert_eq!(
        reply,
        en("ask_buyer_sent_simple"),
        "a greeting after the question gets its simple form"
    );
    tokio::time::sleep(QUIET * 2).await;
    assert_eq!(
        script.judge.calls.load(Ordering::SeqCst),
        1,
        "one turn for the burst"
    );
    let store = script.store.lock().unwrap();
    let evaluations = evaluations::list_for_session(store.conn(), "s1").unwrap();
    assert_eq!(evaluations.len(), 1);
    assert_eq!(evaluations[0].judge_id, "test/scripted");
    assert_eq!(evaluations[0].input_tokens, Some(1000));
    assert_eq!(
        evaluations[0].action,
        serde_json::json!({ "ask": { "buyer": ["ask_buyer_sent_simple"], "seller": [] } }),
        "the seller still owes an answer and gets nothing"
    );
    assert_eq!(
        sessions::get(store.conn(), "s1").unwrap().unwrap().rounds,
        1
    );
}

#[tokio::test]
async fn asking_for_spanish_switches_the_language_and_resends_the_question() {
    let script = script(&[
        ("buyer_message_kind", ("asks_language", 1.0)),
        ("buyer_language", ("es", 0.95)),
    ])
    .await;
    let mut buyer = buyer_side(&script).await;

    buyer.say("hablas español?").await;
    let reply = buyer.next_from_serbero().await;

    let catalogs = Catalogs::embedded().unwrap();
    let es = catalogs
        .get("es")
        .unwrap()
        .render(
            "ask_buyer_sent",
            Some(Amount {
                value: "50000",
                currency: "ARS",
            }),
        )
        .unwrap();
    assert_eq!(reply, es);
    let store = script.store.lock().unwrap();
    let session = sessions::get(store.conn(), "s1").unwrap().unwrap();
    assert_eq!(session.buyer_lang.as_deref(), Some("es"));
    assert_eq!(session.rounds, 0, "a language resend is not a round");
    let sent = messages::list_for_session(store.conn(), "s1").unwrap();
    let last = sent
        .iter()
        .rev()
        .find(|m| m.direction == Direction::Out)
        .unwrap();
    assert_eq!(
        (last.template_id.as_deref(), last.lang.as_deref()),
        (Some("ask_buyer_sent"), Some("es"))
    );
}

#[tokio::test]
async fn a_request_for_a_person_hands_off_and_later_messages_are_forwarded() {
    let script = script(&[("buyer_wants_human", ("yes", 0.95))]).await;
    let mut buyer = buyer_side(&script).await;

    buyer.say("quiero hablar con una persona").await;
    let notice = buyer.next_from_serbero().await;

    assert_eq!(notice, en("handoff_notice"));
    let texts = script.outbox.texts();
    assert!(
        texts[0].starts_with("Dispute d1 · handed off: human_requested\n"),
        "{}",
        texts[0]
    );
    assert!(texts[0].contains("human requested: yes"), "{}", texts[0]);
    assert!(
        texts[1].starts_with("Dispute d1 · transcript (3 messages, times UTC)\n"),
        "{}",
        texts[1]
    );
    assert!(
        texts[1].contains("buyer: quiero hablar con una persona"),
        "{}",
        texts[1]
    );
    {
        let store = script.store.lock().unwrap();
        let session = sessions::get(store.conn(), "s1").unwrap().unwrap();
        assert_eq!(session.state, SessionState::HandedOff);
        assert_eq!(session.handoff_reason.as_deref(), Some("human_requested"));
        let evaluations = evaluations::list_for_session(store.conn(), "s1").unwrap();
        assert_eq!(evaluations.len(), 2, "the turn and the brief");
        assert_ne!(
            evaluations[0].question_set_version, evaluations[1].question_set_version,
            "the brief has its own question set"
        );
    }
    let notices = || {
        messages::list_for_session(script.store.lock().unwrap().conn(), "s1")
            .unwrap()
            .into_iter()
            .filter(|m| m.template_id.as_deref() == Some("handoff_notice"))
            .count()
    };
    tokio::time::timeout(WAIT, async {
        while notices() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both parties are told");
    let judged = script.judge.calls.load(Ordering::SeqCst);

    buyer.say("hola? sigue alguien?").await;
    let update = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(text) = script.outbox.texts().get(2).cloned() {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();

    assert!(
        update.starts_with("Dispute d1 · new messages since handoff (1)\n"),
        "{update}"
    );
    assert!(update.ends_with("buyer: hola? sigue alguien?"), "{update}");
    assert_eq!(
        script.judge.calls.load(Ordering::SeqCst),
        judged,
        "no judging after a handoff"
    );
}

fn dispute_event(mostro: &Keys, status: &str) -> Event {
    let tags = [
        vec!["d", "d1"],
        vec!["s", status],
        vec!["initiator", "buyer"],
        vec!["y", "mostro"],
        vec!["z", "dispute"],
    ]
    .into_iter()
    .map(|t| Tag::parse(t).unwrap());
    EventBuilder::new(Kind::Custom(38386), "")
        .tags(tags)
        .custom_created_at(Timestamp::from_secs(9_000))
        .finalize(mostro)
        .unwrap()
}

async fn wait_for_text(outbox: &Outbox, prefix: &str) -> String {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(text) = outbox.texts().into_iter().find(|t| t.starts_with(prefix)) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no solver DM starting {prefix:?}"))
}

#[tokio::test]
async fn payment_arrived_is_guided_and_closed_when_the_seller_releases() {
    let script = script(&[("seller_receipt", ("says_received", 0.97))]).await;
    let mut buyer = buyer_side(&script).await;
    let mut seller = seller_side(&script).await;

    seller.say("sí, me llegó el pago").await;

    assert_eq!(
        seller.next_from_serbero().await,
        en("guide_arrived_seller"),
        "the seller, who releases, first"
    );
    assert_eq!(buyer.next_from_serbero().await, en("guide_arrived_buyer"));
    let brief = wait_for_text(
        &script.outbox,
        "Dispute d1 · guidance sent: payment_arrived\n",
    )
    .await;
    assert!(brief.contains("Seller — says received (0.97)"), "{brief}");
    assert_eq!(
        sessions::get(script.store.lock().unwrap().conn(), "s1")
            .unwrap()
            .unwrap()
            .state,
        SessionState::Guiding
    );

    script
        .notifier
        .handle_event(&dispute_event(&script.mostro, "released"), 9_001)
        .await
        .unwrap();

    assert_eq!(seller.next_from_serbero().await, en("resolved_thanks"));
    assert_eq!(buyer.next_from_serbero().await, en("resolved_thanks"));
    let report = wait_for_text(&script.outbox, "Dispute d1 resolved: released\n").await;
    assert!(report.contains("outcome: self_resolved"), "{report}");
    assert_eq!(
        sessions::get(script.store.lock().unwrap().conn(), "s1")
            .unwrap()
            .unwrap()
            .state,
        SessionState::Closed
    );
}

#[tokio::test]
async fn payment_not_sent_is_guided_and_closed_by_a_cooperative_cancel() {
    let script = script(&[("buyer_payment", ("says_not_sent", 0.97))]).await;
    let mut buyer = buyer_side(&script).await;
    let mut seller = seller_side(&script).await;

    buyer.say("no pagué, me confundí de orden").await;

    assert_eq!(buyer.next_from_serbero().await, en("guide_not_sent_buyer"));
    assert_eq!(
        seller.next_from_serbero().await,
        en("guide_not_sent_seller")
    );

    script
        .notifier
        .handle_event(
            &dispute_event(&script.mostro, "cooperatively-canceled"),
            9_001,
        )
        .await
        .unwrap();

    assert_eq!(buyer.next_from_serbero().await, en("resolved_thanks"));
    let report = wait_for_text(
        &script.outbox,
        "Dispute d1 resolved: cooperatively-canceled\n",
    )
    .await;
    assert!(report.contains("outcome: self_resolved"), "{report}");
}

#[tokio::test]
async fn a_buyers_claim_alone_never_guides() {
    let script = script(&[
        ("buyer_payment", ("says_sent", 0.99)),
        ("buyer_details", ("yes", 0.99)),
    ])
    .await;
    let mut buyer = buyer_side(&script).await;

    buyer
        .say("ya pagué a las 14:10 por mercado pago, ref 8841")
        .await;

    assert_eq!(buyer.next_from_serbero().await, en("thanks_waiting"));
    let store = script.store.lock().unwrap();
    assert_eq!(
        sessions::get(store.conn(), "s1").unwrap().unwrap().state,
        SessionState::Active
    );
    let sent = messages::list_for_session(store.conn(), "s1").unwrap();
    assert!(
        !sent.iter().any(|m| m
            .template_id
            .as_deref()
            .is_some_and(|t| t.starts_with("guide_"))),
        "no guidance from the buyer's word alone"
    );
}

#[tokio::test]
async fn a_handed_off_dispute_resolved_later_gets_no_thanks() {
    let script = script(&[("buyer_wants_human", ("yes", 0.95))]).await;
    let mut buyer = buyer_side(&script).await;
    buyer.say("quiero hablar con una persona").await;
    assert_eq!(buyer.next_from_serbero().await, en("handoff_notice"));

    script
        .notifier
        .handle_event(&dispute_event(&script.mostro, "released"), 9_001)
        .await
        .unwrap();

    let report = wait_for_text(&script.outbox, "Dispute d1 resolved: released\n").await;
    assert!(
        report.contains("outcome: handed_off (human_requested)"),
        "{report}"
    );
    let thanks = messages::list_for_session(script.store.lock().unwrap().conn(), "s1")
        .unwrap()
        .into_iter()
        .filter(|m| m.template_id.as_deref() == Some("resolved_thanks"))
        .count();
    assert_eq!(thanks, 0, "the parties already know a person took over");
}

#[tokio::test]
async fn silence_gets_a_reminder_and_then_hands_off_as_unresponsive() {
    let script = script(&[]).await;
    let mut buyer = buyer_side(&script).await;
    let mut seller = seller_side(&script).await;

    // The openers were sent at t = 1; the response timeout is 30 min.
    script.mediator.tick(1 + 1800).await.unwrap();

    assert_eq!(buyer.next_from_serbero().await, en("reminder"));
    assert_eq!(seller.next_from_serbero().await, en("reminder"));
    let reminded = serbero::daemon::now();
    script.mediator.tick(reminded + 1799).await.unwrap();
    assert!(script.outbox.texts().is_empty(), "not before the timeout");

    script.mediator.tick(reminded + 1801).await.unwrap();

    let brief = wait_for_text(&script.outbox, "Dispute d1 · handed off: unresponsive\n").await;
    assert!(
        brief.contains("Automated reading unavailable"),
        "no turn was judged: {brief}"
    );
    assert_eq!(buyer.next_from_serbero().await, en("handoff_notice"));
}

#[tokio::test]
async fn guidance_that_does_not_resolve_hands_off_as_stalled() {
    let script = script(&[("seller_receipt", ("says_received", 0.97))]).await;
    let mut seller = seller_side(&script).await;
    seller.say("sí, me llegó").await;
    assert_eq!(seller.next_from_serbero().await, en("guide_arrived_seller"));
    let guided = serbero::daemon::now();

    script.mediator.tick(guided + 7201).await.unwrap();

    let brief = wait_for_text(
        &script.outbox,
        "Dispute d1 · handed off: self_resolution_stalled\n",
    )
    .await;
    assert!(
        brief.contains("Seller — says received (0.97)"),
        "the last turn's reading: {brief}"
    );
}

#[tokio::test]
async fn flooding_twice_hands_off_without_judging_again() {
    let script = script(&[]).await;
    let mut buyer = buyer_side(&script).await;
    for text in ["1", "2", "3", "4"] {
        buyer.say(text).await;
    }
    assert_eq!(
        buyer.next_from_serbero().await,
        en("ask_buyer_sent_simple"),
        "the first strike is a warning only"
    );

    for text in ["5", "6", "7", "8"] {
        buyer.say(text).await;
    }

    wait_for_text(&script.outbox, "Dispute d1 · handed off: flood\n").await;
    assert_eq!(
        script.judge.calls.load(Ordering::SeqCst),
        1,
        "the flooding turn is not judged"
    );
}

#[tokio::test]
async fn a_judge_that_is_down_hands_off_with_the_transcript() {
    let script = script(&[]).await;
    script.judge.down.store(true, Ordering::SeqCst);
    let mut buyer = buyer_side(&script).await;

    buyer.say("ya pagué").await;

    assert_eq!(buyer.next_from_serbero().await, en("handoff_notice"));
    let brief = wait_for_text(
        &script.outbox,
        "Dispute d1 · handed off: judge_unavailable\n",
    )
    .await;
    assert!(brief.contains("Automated reading unavailable"), "{brief}");
    let transcript = wait_for_text(&script.outbox, "Dispute d1 · transcript").await;
    assert!(transcript.contains("buyer: ya pagué"), "{transcript}");
    let store = script.store.lock().unwrap();
    assert_eq!(
        sessions::get(store.conn(), "s1")
            .unwrap()
            .unwrap()
            .handoff_reason
            .as_deref(),
        Some("judge_unavailable")
    );
    assert!(
        evaluations::list_for_session(store.conn(), "s1")
            .unwrap()
            .is_empty(),
        "nothing was judged"
    );
}
