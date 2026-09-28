//! T5.3: session scripts over a local relay. Party messages reach the turn
//! task through the chat channels, a burst is judged once after the quiet
//! period, the policy's templates reach the party in its language, and
//! every turn is recorded as an evaluation.
//!
//! The judge is scripted: it answers from fixed rules and counts its calls,
//! so the scripts are deterministic and never call a live API.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use serbero::policy::HandoffReason;
use serbero::store::disputes::{self, Initiator, NewDispute};
use serbero::store::messages::{self, Direction, NewMessage};
use serbero::store::sessions::{self, NewSession, SessionState};
use serbero::store::{Store, evaluations, events};

const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(400);

/// Solver DMs, kept instead of sent. While `failing` is set, every send
/// fails, as when no relay accepts it.
#[derive(Clone, Default)]
struct Outbox {
    sent: Arc<Mutex<Vec<String>>>,
    failing: Arc<AtomicBool>,
    /// Sends of texts containing this fail.
    fail_on: Arc<Mutex<Option<&'static str>>>,
    /// Every send hangs this long first, as on a relay that never answers.
    stall: Arc<Mutex<Duration>>,
}

impl DmSender for Outbox {
    async fn send_dm(&self, _to: PublicKey, text: &str) -> SerberoResult<()> {
        let stall = *self.stall.lock().unwrap();
        tokio::time::sleep(stall).await;
        let matches = self
            .fail_on
            .lock()
            .unwrap()
            .is_some_and(|part| text.contains(part));
        if self.failing.load(Ordering::SeqCst) || matches {
            return Err(serbero::error::Error::Nostr("relay rejected".into()));
        }
        self.sent.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

impl Outbox {
    fn texts(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    fn fail(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    fn fail_on(&self, part: Option<&'static str>) {
        *self.fail_on.lock().unwrap() = part;
    }
}

/// Answers every question with nothing stated, except the options `picks`
/// sets (question id → (option or yes, probability)).
struct ScriptedJudge {
    picks: BTreeMap<&'static str, (&'static str, f64)>,
    calls: AtomicUsize,
    /// How long each request takes.
    delay: Duration,
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
        let answers: Answers = questions
            .questions
            .iter()
            .map(|(id, q)| (id.clone(), self.answer(id, q)))
            .collect();
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
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
        "fraud": 0.6, "conflict": 0.75, "outside_scope": 0.8
    }))
    .unwrap()
}

struct Script {
    store: Arc<Mutex<Store>>,
    serbero: Keys,
    buyer: Keys,
    seller: Keys,
    second_buyer: Option<Keys>,
    url: String,
    judge: Arc<ScriptedJudge>,
    outbox: Outbox,
    mediator: Arc<Mediator<Outbox>>,
    languages: [Language<'static>; 3],
    _relay: MockRelay,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// A live session between Serbero and two parties, with the chat channels,
/// the turn task, and the scripted judge running. Both openers were sent in
/// English and are unanswered.
async fn script(picks: &[(&'static str, (&'static str, f64))]) -> Script {
    script_with(picks, Options::default()).await
}

/// Variations of a script.
#[derive(Default)]
struct Options {
    judge_delay: Duration,
    /// The judge is not ready when the script starts.
    not_ready: bool,
    /// A buyer message stored before the turn task starts, as after a
    /// restart.
    pending: Option<&'static str>,
    /// No solver is configured.
    no_solvers: bool,
    /// The session was handed off before the pending message, and a notice
    /// was stored after it, as when the process stopped mid-handoff.
    handed_off: bool,
    /// A second live session, `s2` of dispute `d2`.
    second_session: bool,
}

/// An active session with both openers sent and unanswered.
fn seed_session(store: &Store, session_id: &str, dispute_id: &str, buyer: &Keys, seller: &Keys) {
    disputes::insert_if_new(
        store.conn(),
        &NewDispute {
            dispute_id,
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
            session_id,
            dispute_id,
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
    sessions::set_state(store.conn(), session_id, SessionState::Active, 1).unwrap();
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
                session_id,
                direction: Direction::Out,
                party,
                template_id: Some(template),
                lang: Some("en"),
                content: "…",
                attachments: 0,
                inner_event_id: &format!("{id}-{session_id}"),
                created_at: 1,
            },
        )
        .unwrap();
    }
}

async fn script_with(picks: &[(&'static str, (&'static str, f64))], options: Options) -> Script {
    let relay = MockRelay::run().await.unwrap();
    let url = relay.url().await.to_string();
    let (serbero, buyer, seller) = (Keys::generate(), Keys::generate(), Keys::generate());
    let store = Store::open_in_memory().unwrap();
    seed_session(&store, "s1", "d1", &buyer, &seller);
    let second_buyer = options.second_session.then(|| {
        let (buyer, seller) = (Keys::generate(), Keys::generate());
        seed_session(&store, "s2", "d2", &buyer, &seller);
        buyer
    });
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
        delay: options.judge_delay,
    });
    let outbox = Outbox::default();
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
        mostro: Keys::generate().public_key(),
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
        },
        sender: outbox.clone(),
        solvers: if options.no_solvers {
            vec![]
        } else {
            vec![Solver {
                pubkey: Keys::generate().public_key(),
                permission: Permission::Write,
            }]
        },
        own_takes: Arc::default(),
        judge: Default::default(),
    });
    if !options.not_ready {
        mediator.set_ready(ReadyJudge {
            judge: Arc::clone(&judge) as Arc<dyn Judge>,
            thresholds: thresholds(),
            turn: TurnQuestions::new(&languages),
        });
    }
    if options.handed_off {
        let store = store.lock().unwrap();
        sessions::hand_off(store.conn(), "s1", "human_requested", 1).unwrap();
        let last = messages::list_for_session(store.conn(), "s1")
            .unwrap()
            .iter()
            .map(|m| m.id)
            .max()
            .unwrap();
        events::append(
            store.conn(),
            &events::NewEvent {
                dispute_id: "d1",
                session_id: Some("s1"),
                kind: "handoff",
                payload: serde_json::json!({ "reason": "human_requested", "last_message_id": last }),
                now: 1,
            },
        )
        .unwrap();
    }
    if let Some(text) = options.pending {
        messages::insert_if_new(
            store.lock().unwrap().conn(),
            &NewMessage {
                session_id: "s1",
                direction: Direction::In,
                party: serbero::store::sessions::Party::Buyer,
                template_id: None,
                lang: None,
                content: text,
                attachments: 0,
                inner_event_id: "stored-before-restart",
                created_at: 2,
            },
        )
        .unwrap();
    }
    if options.handed_off {
        messages::insert_if_new(
            store.lock().unwrap().conn(),
            &NewMessage {
                session_id: "s1",
                direction: Direction::Out,
                party: serbero::store::sessions::Party::Buyer,
                template_id: Some("handoff_notice"),
                lang: Some("en"),
                content: "…",
                attachments: 0,
                inner_event_id: "notice-after-it",
                created_at: 3,
            },
        )
        .unwrap();
    }

    let notifications = client.notifications();
    let session = sessions::get(store.lock().unwrap().conn(), "s1")
        .unwrap()
        .unwrap();
    chats.open(&session).await.unwrap();
    if second_buyer.is_some() {
        let second = sessions::get(store.lock().unwrap().conn(), "s2")
            .unwrap()
            .unwrap();
        chats.open(&second).await.unwrap();
    }
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
        second_buyer,
        url,
        judge,
        outbox,
        mediator,
        languages,
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
        self.next_within(WAIT).await.unwrap()
    }

    /// The next message Serbero sends on this channel within `wait`.
    async fn next_within(&mut self, wait: Duration) -> Option<String> {
        tokio::time::timeout(wait, async {
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
        .ok()
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
    let newest = messages::list_for_session(store.conn(), "s1")
        .unwrap()
        .iter()
        .filter(|m| m.direction == Direction::In)
        .map(|m| m.id)
        .max()
        .unwrap();
    assert_eq!(
        evaluations[0].last_message_id, newest,
        "the cutoff covers every judged row"
    );
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
    assert_eq!(open_briefs(&script), 0, "the brief was delivered");
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

/// Briefs recorded as pending and not yet delivered.
fn open_briefs(script: &Script) -> usize {
    let history = events::list_for_dispute(script.store.lock().unwrap().conn(), "d1").unwrap();
    history
        .iter()
        .filter(|e| e.kind == "brief_pending")
        .filter(|e| {
            !history
                .iter()
                .any(|s| s.kind == "brief_sent" && s.payload["pending"] == e.id)
        })
        .count()
}

fn event_kinds(script: &Script) -> Vec<String> {
    events::list_for_dispute(script.store.lock().unwrap().conn(), "d1")
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn a_brief_no_solver_received_is_recorded_and_the_session_is_handed_off_once() {
    let script = script(&[("buyer_wants_human", ("yes", 0.95))]).await;
    let mut buyer = buyer_side(&script).await;
    script.outbox.fail(true);

    buyer.say("quiero hablar con una persona").await;

    assert_eq!(buyer.next_from_serbero().await, en("handoff_notice"));
    assert!(script.outbox.texts().is_empty());
    assert!(open_briefs(&script) == 1, "{:?}", event_kinds(&script));
    let session = sessions::get(script.store.lock().unwrap().conn(), "s1")
        .unwrap()
        .unwrap();
    assert_eq!(session.state, SessionState::HandedOff);

    script.outbox.fail(false);
    let claimed = script
        .mediator
        .hand_off(&session, HandoffReason::FraudSignal, None, 2)
        .await
        .unwrap();

    assert!(!claimed);
    assert!(script.outbox.texts().is_empty(), "no second brief");
    let handoffs = event_kinds(&script)
        .iter()
        .filter(|k| *k == "handoff")
        .count();
    assert_eq!(handoffs, 1);
}

#[tokio::test]
async fn an_update_no_solver_received_is_sent_with_the_next_one() {
    let script = script(&[("buyer_wants_human", ("yes", 0.95))]).await;
    let mut buyer = buyer_side(&script).await;
    buyer.say("quiero hablar con una persona").await;
    buyer.next_from_serbero().await;
    let briefed = script.outbox.texts().len();
    script.outbox.fail(true);

    buyer.say("hola?").await;
    tokio::time::sleep(QUIET + Duration::from_millis(600)).await;

    assert!(!event_kinds(&script).contains(&"update_sent".to_owned()));
    script.outbox.fail(false);
    buyer.say("sigue alguien?").await;
    let update = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(text) = script.outbox.texts().get(briefed).cloned() {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();

    assert!(
        update.starts_with("Dispute d1 · new messages since handoff (2)\n"),
        "{update}"
    );
    assert!(update.contains("buyer: hola?"), "{update}");
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
async fn a_brief_whose_transcript_failed_is_still_pending() {
    let script = script(&[("buyer_wants_human", ("yes", 0.95))]).await;
    let mut buyer = buyer_side(&script).await;
    script.outbox.fail_on(Some("· transcript"));

    buyer.say("quiero hablar con una persona").await;

    assert_eq!(buyer.next_from_serbero().await, en("handoff_notice"));
    assert!(
        open_briefs(&script) == 1,
        "the solver got the brief but not the conversation"
    );
}

#[tokio::test]
async fn a_handoff_with_no_solver_configured_is_pending() {
    let script = script_with(
        &[("buyer_wants_human", ("yes", 0.95))],
        Options {
            no_solvers: true,
            ..Options::default()
        },
    )
    .await;
    let mut buyer = buyer_side(&script).await;

    buyer.say("quiero hablar con una persona").await;

    assert_eq!(buyer.next_from_serbero().await, en("handoff_notice"));
    assert!(open_briefs(&script) == 1);
}

#[tokio::test]
async fn an_unforwarded_message_is_forwarded_after_a_restart_even_behind_a_notice() {
    let script = script_with(
        &[],
        Options {
            pending: Some("hola? sigue alguien?"),
            handed_off: true,
            ..Options::default()
        },
    )
    .await;

    let update = wait_for_text(
        &script.outbox,
        "Dispute d1 · new messages since handoff (1)\n",
    )
    .await;

    assert!(update.ends_with("buyer: hola? sigue alguien?"), "{update}");
}

#[tokio::test]
async fn the_brief_evaluation_covers_only_the_judged_state() {
    let script = script_with(
        &[("buyer_wants_human", ("yes", 0.95))],
        Options {
            judge_delay: Duration::from_millis(800),
            ..Options::default()
        },
    )
    .await;
    let buyer = buyer_side(&script).await;
    buyer.say("quiero hablar con una persona").await;
    // The turn is judged, then the brief request starts.
    tokio::time::timeout(WAIT, async {
        while script.judge.calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    buyer.say("hola?").await;
    wait_for_text(&script.outbox, "Dispute d1 · handed off").await;

    let store = script.store.lock().unwrap();
    let evaluations = evaluations::list_for_session(store.conn(), "s1").unwrap();
    let turn_cutoff = evaluations[0].last_message_id;
    assert_eq!(
        evaluations[1].last_message_id, turn_cutoff,
        "the brief was asked on the turn's state"
    );
    let newest = messages::list_for_session(store.conn(), "s1")
        .unwrap()
        .iter()
        .filter(|m| m.content == "hola?")
        .map(|m| m.id)
        .max()
        .unwrap();
    assert!(newest > turn_cutoff);
}

/// AGENTS.md relays rule 7: a handoff whose solver DMs hang on a silent
/// relay never holds back another session's turn.
#[tokio::test]
async fn a_handoff_stuck_on_a_silent_relay_never_delays_another_session() {
    let script = script_with(
        &[
            // Only asked about the party who wrote: s1's seller, s2's buyer.
            ("seller_wants_human", ("yes", 0.95)),
            ("buyer_message_kind", ("greeting", 1.0)),
        ],
        Options {
            second_session: true,
            ..Options::default()
        },
    )
    .await;
    *script.outbox.stall.lock().unwrap() = Duration::from_secs(30);
    let seller = party_side(&script, &script.seller).await;
    let second = script.second_buyer.clone().unwrap();
    let mut other = party_side(&script, &second).await;

    seller.say("quiero hablar con una persona").await;
    tokio::time::sleep(QUIET * 3).await;
    let started = std::time::Instant::now();
    other.say("hola").await;
    let reply = other.next_within(Duration::from_secs(5)).await;

    assert!(reply.is_some(), "the other session was held back");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_message_sent_while_judging_is_answered_together() {
    let script = script_with(
        &[("buyer_message_kind", ("greeting", 1.0))],
        Options {
            judge_delay: Duration::from_millis(800),
            ..Options::default()
        },
    )
    .await;
    let mut buyer = buyer_side(&script).await;

    buyer.say("hola").await;
    tokio::time::sleep(QUIET + Duration::from_millis(300)).await;
    buyer.say("ya pagué").await;
    let reply = buyer.next_from_serbero().await;

    assert_eq!(reply, en("ask_buyer_sent_simple"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        script.judge.calls.load(Ordering::SeqCst),
        2,
        "the stale turn was not acted on"
    );
    let store = script.store.lock().unwrap();
    assert_eq!(
        evaluations::list_for_session(store.conn(), "s1")
            .unwrap()
            .len(),
        1
    );
    let replies = messages::list_for_session(store.conn(), "s1")
        .unwrap()
        .into_iter()
        .filter(|m| m.direction == Direction::Out && m.inner_event_id.len() == 64)
        .count();
    assert_eq!(replies, 1, "one answer for both messages");
}

#[tokio::test]
async fn a_message_stored_before_a_restart_gets_its_turn() {
    let script = script_with(
        &[("buyer_message_kind", ("greeting", 1.0))],
        Options {
            pending: Some("hola?"),
            ..Options::default()
        },
    )
    .await;
    let mut buyer = buyer_side(&script).await;

    assert_eq!(buyer.next_from_serbero().await, en("ask_buyer_sent_simple"));
}

#[tokio::test]
async fn a_turn_waits_for_the_judge_to_be_ready() {
    let script = script_with(
        &[("buyer_message_kind", ("greeting", 1.0))],
        Options {
            not_ready: true,
            ..Options::default()
        },
    )
    .await;
    let mut buyer = buyer_side(&script).await;
    buyer.say("hola").await;
    tokio::time::sleep(QUIET * 2).await;
    assert_eq!(script.judge.calls.load(Ordering::SeqCst), 0);

    script.mediator.set_ready(ReadyJudge {
        judge: Arc::clone(&script.judge) as Arc<dyn Judge>,
        thresholds: thresholds(),
        turn: TurnQuestions::new(&script.languages),
    });

    assert_eq!(buyer.next_from_serbero().await, en("ask_buyer_sent_simple"));
}
