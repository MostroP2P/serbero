#![allow(clippy::unwrap_used)] // test helpers

use std::collections::BTreeMap;

use super::*;
use crate::judge::questions::{Language, TurnQuestions};
use crate::judge::{Answer, Question, check_answers};

/// Distinct values, so a fact read against the wrong threshold fails.
const THRESHOLDS: Thresholds = Thresholds {
    guide: 0.9,
    fact: 0.8,
    human_request: 0.85,
    fraud: 0.6,
    conflict: 0.75,
    outside_scope: 0.7,
    validated_languages: Vec::new(),
};

const LANGUAGES: [Language<'static>; 3] = [
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

/// Just below a threshold.
const EPSILON: f64 = 0.01;

fn enabled() -> Vec<String> {
    LANGUAGES.iter().map(|l| l.code.to_owned()).collect()
}

/// The option that carries the remaining probability when a test sets
/// others: the answer that states nothing.
fn neutral(id: &str) -> &'static str {
    match id {
        "buyer_payment" | "seller_receipt" => "not_stated",
        "dispute_topic" => "not_yet_clear",
        _ if id.ends_with("_message_kind") => "answers",
        _ if id.ends_with("_language") => "unknown",
        _ => panic!("no neutral option for {id}"),
    }
}

/// Answers to every question of a turn that state nothing: each choice on
/// its neutral option, each noul at 0.
struct Turn {
    questions: crate::judge::QuestionSet,
    answers: Answers,
}

impl Turn {
    fn new(with_new_messages: &[Party], guiding: bool) -> Self {
        let questions = TurnQuestions::new(&LANGUAGES).for_turn(with_new_messages, guiding);
        let answers = questions
            .questions
            .iter()
            .map(|(id, question)| (id.clone(), neutral_answer(id, question)))
            .collect();
        Self { questions, answers }
    }

    fn full() -> Self {
        Self::new(&[Party::Buyer, Party::Seller], true)
    }

    /// Sets some options of a choice; the neutral option takes the rest.
    fn choice(mut self, id: &str, set: &[(&str, f64)]) -> Self {
        let Some(Answer::Choice { probabilities }) = self.answers.get_mut(id) else {
            panic!("{id} is not an asked choice");
        };
        probabilities.values_mut().for_each(|p| *p = 0.0);
        for (option, p) in set {
            assert!(
                probabilities.contains_key(*option),
                "{id} has no option {option}"
            );
            probabilities.insert((*option).to_owned(), *p);
        }
        let rest = 1.0 - set.iter().map(|(_, p)| p).sum::<f64>();
        *probabilities.get_mut(neutral(id)).unwrap() += rest;
        self
    }

    fn noul(mut self, id: &str, p_yes: f64) -> Self {
        assert!(self.answers.contains_key(id), "{id} was not asked");
        self.answers.insert(id.to_owned(), Answer::Noul { p_yes });
        self
    }

    fn facts(&self) -> Facts {
        check_answers(&self.questions, &self.answers).unwrap();
        from_answers(&self.answers, &THRESHOLDS, &enabled())
    }
}

fn neutral_answer(id: &str, question: &Question) -> Answer {
    match question {
        Question::Noul { .. } => Answer::Noul { p_yes: 0.0 },
        Question::Choice { options, .. } => Answer::Choice {
            probabilities: options
                .iter()
                .map(|(o, _)| (o.clone(), if o == neutral(id) { 1.0 } else { 0.0 }))
                .collect::<BTreeMap<_, _>>(),
        },
        Question::Score { .. } => panic!("no score in a turn"),
    }
}

/// How a threshold row sets its probability.
#[derive(Clone, Copy)]
enum Input {
    Choice(&'static str, &'static str),
    Noul(&'static str),
}

struct Row {
    fact: &'static str,
    input: Input,
    threshold: f64,
    read: fn(&Facts) -> bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            fact: "buyer_sent",
            input: Input::Choice("buyer_payment", "says_sent"),
            threshold: THRESHOLDS.fact,
            read: |f| f.buyer_sent,
        },
        Row {
            fact: "buyer_not_sent",
            input: Input::Choice("buyer_payment", "says_not_sent"),
            threshold: THRESHOLDS.fact,
            read: |f| f.buyer_not_sent,
        },
        Row {
            fact: "buyer_not_sent_for_guide",
            input: Input::Choice("buyer_payment", "says_not_sent"),
            threshold: THRESHOLDS.guide,
            read: |f| f.buyer_not_sent_for_guide,
        },
        Row {
            fact: "buyer_has_details",
            input: Input::Noul("buyer_details"),
            threshold: THRESHOLDS.fact,
            read: |f| f.buyer_has_details,
        },
        Row {
            fact: "seller_received",
            input: Input::Choice("seller_receipt", "says_received"),
            threshold: THRESHOLDS.fact,
            read: |f| f.seller_received,
        },
        Row {
            fact: "seller_received_for_guide",
            input: Input::Choice("seller_receipt", "says_received"),
            threshold: THRESHOLDS.guide,
            read: |f| f.seller_received_for_guide,
        },
        Row {
            fact: "seller_not_received",
            input: Input::Choice("seller_receipt", "says_not_received"),
            threshold: THRESHOLDS.fact,
            read: |f| f.seller_not_received,
        },
        Row {
            fact: "seller_has_checked",
            input: Input::Noul("seller_checked"),
            threshold: THRESHOLDS.fact,
            read: |f| f.seller_has_checked,
        },
        Row {
            fact: "conflict",
            input: Input::Noul("claims_conflict"),
            threshold: THRESHOLDS.conflict,
            read: |f| f.conflict,
        },
        Row {
            fact: "fraud",
            input: Input::Noul("fraud_signal"),
            threshold: THRESHOLDS.fraud,
            read: |f| f.fraud,
        },
        Row {
            fact: "outside_scope (payment with a problem)",
            input: Input::Choice("seller_receipt", "says_received_with_problem"),
            threshold: THRESHOLDS.outside_scope,
            read: |f| f.outside_scope,
        },
        Row {
            fact: "outside_scope (topic)",
            input: Input::Choice("dispute_topic", "wrong_amount"),
            threshold: THRESHOLDS.outside_scope,
            read: |f| f.outside_scope,
        },
        Row {
            fact: "wants_human(buyer)",
            input: Input::Noul("buyer_wants_human"),
            threshold: THRESHOLDS.human_request,
            read: |f| f.buyer.as_ref().unwrap().wants_human,
        },
        Row {
            fact: "wants_human(seller)",
            input: Input::Noul("seller_wants_human"),
            threshold: THRESHOLDS.human_request,
            read: |f| f.seller.as_ref().unwrap().wants_human,
        },
        Row {
            fact: "rejects_path(buyer)",
            input: Input::Noul("buyer_rejects_path"),
            threshold: THRESHOLDS.conflict,
            read: |f| f.buyer.as_ref().unwrap().rejects_path,
        },
        Row {
            fact: "rejects_path(seller)",
            input: Input::Noul("seller_rejects_path"),
            threshold: THRESHOLDS.conflict,
            read: |f| f.seller.as_ref().unwrap().rejects_path,
        },
    ]
}

#[test]
fn each_fact_is_known_at_and_above_its_threshold_only() {
    for row in rows() {
        for (p, expected) in [
            (row.threshold - EPSILON, false),
            (row.threshold, true),
            (row.threshold + EPSILON, true),
        ] {
            let turn = match row.input {
                Input::Choice(id, option) => Turn::full().choice(id, &[(option, p)]),
                Input::Noul(id) => Turn::full().noul(id, p),
            };

            assert_eq!(
                (row.read)(&turn.facts()),
                expected,
                "{} at P = {p}",
                row.fact
            );
        }
    }
}

#[test]
fn nothing_stated_leaves_every_fact_unknown() {
    let facts = Turn::full().facts();

    assert!(facts.buyer_payment_unknown());
    assert!(facts.seller_receipt_unknown());
    assert!(!facts.buyer_sent && !facts.buyer_not_sent && !facts.buyer_has_details);
    assert!(!facts.seller_received && !facts.seller_not_received && !facts.seller_has_checked);
    assert!(!facts.conflict && !facts.fraud && !facts.outside_scope);
}

#[test]
fn a_payment_claim_is_no_longer_unknown_once_known() {
    let sent = Turn::full()
        .choice("buyer_payment", &[("says_sent", 0.8)])
        .facts();
    let not_sent = Turn::full()
        .choice("buyer_payment", &[("says_not_sent", 0.8)])
        .facts();
    let below = Turn::full()
        .choice("buyer_payment", &[("says_sent", 0.79)])
        .facts();

    assert!(!sent.buyer_payment_unknown());
    assert!(!not_sent.buyer_payment_unknown());
    assert!(below.buyer_payment_unknown());
}

#[test]
fn a_receipt_claim_is_no_longer_unknown_once_known() {
    let received = Turn::full()
        .choice("seller_receipt", &[("says_received", 0.8)])
        .facts();
    let not_received = Turn::full()
        .choice("seller_receipt", &[("says_not_received", 0.8)])
        .facts();

    assert!(!received.seller_receipt_unknown());
    assert!(!not_received.seller_receipt_unknown());
}

#[test]
fn a_seller_objecting_to_a_payment_is_outside_scope_not_unknown() {
    let facts = Turn::full()
        .choice("seller_receipt", &[("says_received_with_problem", 0.95)])
        .facts();

    assert!(facts.outside_scope);
    assert!(!facts.seller_received);
    assert!(
        !facts.seller_receipt_unknown(),
        "the receipt question is not asked again"
    );
}

#[test]
fn outside_scope_adds_up_the_topics_that_are_not_about_payment_confirmation() {
    let spread = Turn::full()
        .choice(
            "dispute_topic",
            &[("wrong_amount", 0.4), ("technical_problem", 0.3)],
        )
        .facts();
    let below = Turn::full()
        .choice(
            "dispute_topic",
            &[("wrong_account_or_method", 0.4), ("other", 0.29)],
        )
        .facts();
    let in_scope = Turn::full()
        .choice("dispute_topic", &[("payment_not_confirmed", 0.95)])
        .facts();
    let unresponsive = Turn::full()
        .choice("dispute_topic", &[("counterpart_unresponsive", 0.95)])
        .facts();

    assert!(spread.outside_scope, "0.4 + 0.3 reaches 0.7");
    assert!(!below.outside_scope, "0.4 + 0.29 is below 0.7");
    assert!(!in_scope.outside_scope);
    assert!(
        !unresponsive.outside_scope,
        "an unresponsive party is in scope"
    );
}

#[test]
fn each_topic_counts_toward_outside_scope_on_its_own() {
    for (topic, outside) in [
        ("wrong_amount", true),
        ("wrong_account_or_method", true),
        ("technical_problem", true),
        ("other", true),
        ("payment_not_confirmed", false),
        ("counterpart_unresponsive", false),
    ] {
        let facts = Turn::full()
            .choice("dispute_topic", &[(topic, THRESHOLDS.outside_scope)])
            .facts();

        assert_eq!(facts.outside_scope, outside, "{topic}");
    }
}

#[test]
fn two_options_of_one_choice_are_never_both_known() {
    // A distribution may sum to up to 1.01 and still pass `check_answers`;
    // with raw probabilities, two options at 0.504 would both reach a
    // threshold of 0.502.
    let thresholds = Thresholds {
        guide: 0.502,
        fact: 0.502,
        ..THRESHOLDS
    };
    let mut turn = Turn::full();
    let Some(Answer::Choice { probabilities }) = turn.answers.get_mut("seller_receipt") else {
        panic!("seller_receipt is a choice");
    };
    probabilities.values_mut().for_each(|p| *p = 0.0);
    probabilities.insert("says_received".into(), 0.504);
    probabilities.insert("says_not_received".into(), 0.504);
    check_answers(&turn.questions, &turn.answers).unwrap();

    let facts = from_answers(&turn.answers, &thresholds, &enabled());

    assert!(
        !(facts.seller_received && facts.seller_not_received),
        "received and not received are both known"
    );
    assert!(!facts.seller_received_for_guide);
}

#[test]
fn party_facts_exist_only_for_parties_who_wrote() {
    let facts = Turn::new(&[Party::Seller], false).facts();

    assert!(facts.party(Party::Buyer).is_none());
    assert!(facts.party(Party::Seller).is_some());
}

#[test]
fn rejects_path_is_false_when_not_guiding() {
    let facts = Turn::new(&[Party::Buyer], false).facts();

    assert!(!facts.buyer.unwrap().rejects_path);
}

#[test]
fn language_needs_an_enabled_winner_at_seven_tenths() {
    let at = Turn::full()
        .choice("buyer_language", &[("es", 0.7)])
        .facts();
    let below = Turn::full()
        .choice("buyer_language", &[("es", 0.69)])
        .facts();
    let other = Turn::full()
        .choice("buyer_language", &[("other", 0.95)])
        .facts();

    assert_eq!(at.buyer.unwrap().language.as_deref(), Some("es"));
    assert_eq!(
        below.buyer.unwrap().language,
        None,
        "keep the current language"
    );
    assert_eq!(other.buyer.unwrap().language, None);
}

#[test]
fn language_ignores_a_winner_that_is_not_enabled() {
    let turn = Turn::full().choice("seller_language", &[("pt", 0.95)]);
    turn.facts();

    let facts = from_answers(
        &turn.answers,
        &THRESHOLDS,
        &["en".to_owned(), "es".to_owned()],
    );

    assert_eq!(facts.seller.unwrap().language, None);
}

#[test]
fn message_kind_is_the_winner_when_confident_enough() {
    // Six options: confidence (6 · peak − 1) / 5 reaches 0.5 at peak 7/12.
    let confident = Turn::full()
        .choice("buyer_message_kind", &[("greeting", 0.6), ("other", 0.2)])
        .facts();
    let unsure = Turn::full()
        .choice("buyer_message_kind", &[("greeting", 0.55), ("other", 0.45)])
        .facts();

    assert_eq!(confident.buyer.unwrap().message_kind, MessageKind::Greeting);
    assert_eq!(
        unsure.buyer.unwrap().message_kind,
        MessageKind::Answers,
        "treated as answers"
    );
}

#[test]
fn every_message_kind_option_maps_to_its_kind() {
    for (option, kind) in [
        ("answers", MessageKind::Answers),
        ("greeting", MessageKind::Greeting),
        ("asks_language", MessageKind::AsksLanguage),
        ("not_understood", MessageKind::NotUnderstood),
        ("asks_next_step", MessageKind::AsksNextStep),
        ("other", MessageKind::Other),
    ] {
        let facts = Turn::full()
            .choice("seller_message_kind", &[(option, 1.0)])
            .facts();

        assert_eq!(facts.seller.unwrap().message_kind, kind, "{option}");
    }
}
