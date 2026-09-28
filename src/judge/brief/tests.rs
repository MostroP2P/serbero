#![allow(clippy::unwrap_used)] // test helpers

use serde_json::json;

use super::*;
use crate::judge::questions::canonical_json;
use crate::judge::{Answer, check_answers};

/// Every released brief set with the hash of its fixed text. When a brief
/// question changes, bump `QUESTION_SET_VERSION`, add a line here, and
/// re-run the brief evaluation. Never edit an existing line.
const RELEASED: &[(&str, &str)] = &[("qs-1", "b7bbcf8f")];

const JUDGMENTS_MD: &str = include_str!("../../../docs/judgments.md");

const THRESHOLDS: Thresholds = Thresholds {
    guide: 0.9,
    fact: 0.8,
    human_request: 0.85,
    fraud: 0.6,
    conflict: 0.75,
    outside_scope: 0.7,
    validated_languages: Vec::new(),
};

fn doc_block() -> Value {
    let heading = "\n## 5. Brief request\n";
    let start = JUDGMENTS_MD.find(heading).unwrap() + heading.len();
    let rest = &JUDGMENTS_MD[start..];
    let block = &rest[rest.find("```json\n").unwrap() + "```json\n".len()..];
    serde_json::from_str(&block[..block.find("```").unwrap()]).unwrap()
}

/// The transcript of the doc's example: the seller wrote m3 and m6, the
/// buyer m4 and m7; m1, m2 and m5 are Serbero's.
fn doc_state() -> Value {
    state(&[
        ("serbero", Some("buyer")),
        ("serbero", Some("seller")),
        ("seller", None),
        ("buyer", None),
        ("serbero", Some("seller")),
        ("seller", None),
        ("buyer", None),
    ])
}

fn state(messages: &[(&str, Option<&str>)]) -> Value {
    let transcript: Vec<Value> = messages
        .iter()
        .enumerate()
        .map(|(i, (from, to))| {
            let mut m = json!({ "id": format!("m{}", i + 1), "from": from, "text": "…" });
            if let Some(to) = to {
                m["to"] = json!(to);
            }
            m
        })
        .collect();
    json!({ "transcript": transcript })
}

fn options(set: &QuestionSet, id: &str) -> Vec<String> {
    match &set.questions[id] {
        crate::judge::Question::Choice { options, .. } => {
            options.iter().map(|(o, _)| o.clone()).collect()
        }
        _ => panic!("{id} is not a choice"),
    }
}

/// Answers that pick `none` everywhere, with a flat evidence reading.
fn answers_for(set: &QuestionSet) -> Answers {
    set.questions
        .iter()
        .map(|(id, question)| {
            let answer = match question {
                crate::judge::Question::Choice { options, .. } => Answer::Choice {
                    probabilities: options
                        .iter()
                        .map(|(o, _)| (o.clone(), if o == "none" { 1.0 } else { 0.0 }))
                        .collect(),
                },
                crate::judge::Question::Score { levels, .. } => Answer::Score {
                    probabilities: vec![1.0 / levels.len() as f64; levels.len()],
                },
                crate::judge::Question::Noul { .. } => panic!("no noul in a brief"),
            };
            (id.clone(), answer)
        })
        .collect()
}

fn pick(answers: &mut Answers, id: &str, option: &str, p: f64) {
    let Some(Answer::Choice { probabilities }) = answers.get_mut(id) else {
        panic!("{id} is not a choice");
    };
    probabilities.values_mut().for_each(|v| *v = 0.0);
    probabilities.insert(option.to_owned(), p);
    *probabilities.get_mut("none").unwrap() += 1.0 - p;
}

#[test]
fn the_brief_for_the_doc_example_matches_judgments_md() {
    let set = BriefQuestions::new().for_state(&doc_state());

    assert_eq!(canonical_json(&set.questions), doc_block());
}

#[test]
fn quote_options_are_only_the_right_partys_messages() {
    let set = BriefQuestions::new().for_state(&doc_state());

    assert_eq!(options(&set, "quote_buyer_payment"), ["m4", "m7", "none"]);
    assert_eq!(options(&set, "quote_buyer_details"), ["m4", "m7", "none"]);
    assert_eq!(options(&set, "quote_seller_receipt"), ["m3", "m6", "none"]);
    assert_eq!(
        options(&set, "quote_concern"),
        ["m3", "m4", "m6", "m7", "none"]
    );
}

#[test]
fn a_party_who_wrote_nothing_gets_no_quote_question() {
    let only_seller = state(&[("serbero", Some("seller")), ("seller", None)]);

    let set = BriefQuestions::new().for_state(&only_seller);

    assert!(!set.questions.contains_key("quote_buyer_payment"));
    assert!(!set.questions.contains_key("quote_buyer_details"));
    assert_eq!(options(&set, "quote_seller_receipt"), ["m2", "none"]);
    assert_eq!(options(&set, "quote_concern"), ["m2", "none"]);
    assert!(set.questions.contains_key("evidence_balance"));
}

#[test]
fn without_party_messages_only_the_evidence_reading_is_asked() {
    let silent = state(&[("serbero", Some("buyer")), ("serbero", Some("seller"))]);

    let set = BriefQuestions::new().for_state(&silent);

    let ids: Vec<&str> = set.questions.keys().map(String::as_str).collect();
    assert_eq!(ids, ["evidence_balance"]);
}

#[test]
fn every_brief_carries_the_brief_id() {
    let questions = BriefQuestions::new();

    let set = questions.for_state(&doc_state());

    assert_eq!(set.version, questions.id());
    assert!(questions.id().starts_with("qs-1-"));
}

#[test]
fn the_id_does_not_depend_on_the_transcript() {
    let a = BriefQuestions::new().for_state(&doc_state());
    let b = BriefQuestions::new().for_state(&state(&[("buyer", None)]));

    assert_eq!(a.version, b.version);
}

#[test]
fn changing_a_brief_question_requires_a_version_bump() {
    let questions = BriefQuestions::new();
    let hash = questions
        .id()
        .strip_prefix(&format!("{QUESTION_SET_VERSION}-"))
        .unwrap();
    let released = RELEASED
        .iter()
        .find(|(version, _)| *version == QUESTION_SET_VERSION)
        .unwrap_or_else(|| panic!("add ({QUESTION_SET_VERSION:?}, {hash:?}) to RELEASED"));

    assert_eq!(
        hash, released.1,
        "a brief question changed: bump QUESTION_SET_VERSION, add a line to RELEASED, \
         and re-run the brief evaluation"
    );
}

#[test]
fn a_quote_is_kept_at_the_fact_threshold_and_not_below() {
    let state = doc_state();
    let set = BriefQuestions::new().for_state(&state);

    for (p, expected) in [(0.79, None), (0.8, Some("m7")), (0.95, Some("m7"))] {
        let mut answers = answers_for(&set);
        pick(&mut answers, "quote_buyer_payment", "m7", p);
        check_answers(&set, &answers).unwrap();

        let brief = from_answers(&state, &answers, &THRESHOLDS);

        assert_eq!(brief.quote_buyer_payment.as_deref(), expected, "P = {p}");
    }
}

#[test]
fn none_is_never_a_quote() {
    let state = doc_state();
    let set = BriefQuestions::new().for_state(&state);
    let answers = answers_for(&set);

    let brief = from_answers(&state, &answers, &THRESHOLDS);

    assert_eq!(brief.quote_buyer_payment, None);
    assert_eq!(brief.quote_buyer_details, None);
    assert_eq!(brief.quote_seller_receipt, None);
    assert_eq!(brief.quote_concern, None);
}

#[test]
fn each_quote_reads_its_own_question() {
    let state = doc_state();
    let set = BriefQuestions::new().for_state(&state);
    let mut answers = answers_for(&set);
    pick(&mut answers, "quote_buyer_payment", "m4", 0.9);
    pick(&mut answers, "quote_buyer_details", "m7", 0.9);
    pick(&mut answers, "quote_seller_receipt", "m6", 0.9);
    pick(&mut answers, "quote_concern", "m3", 0.9);

    let brief = from_answers(&state, &answers, &THRESHOLDS);

    assert_eq!(brief.quote_buyer_payment.as_deref(), Some("m4"));
    assert_eq!(brief.quote_buyer_details.as_deref(), Some("m7"));
    assert_eq!(brief.quote_seller_receipt.as_deref(), Some("m6"));
    assert_eq!(brief.quote_concern.as_deref(), Some("m3"));
}

#[test]
fn a_quote_from_the_wrong_party_is_dropped() {
    // Answers that did not come from this state's question set: the buyer
    // question names a seller message.
    let state = doc_state();
    let set = BriefQuestions::new().for_state(&state);
    let mut answers = answers_for(&set);
    answers.insert(
        "quote_buyer_payment".into(),
        Answer::Choice {
            probabilities: [("m3".to_owned(), 1.0), ("none".to_owned(), 0.0)].into(),
        },
    );

    let brief = from_answers(&state, &answers, &THRESHOLDS);

    assert_eq!(brief.quote_buyer_payment, None);
}

#[test]
fn a_serbero_message_is_never_a_concern_quote() {
    let state = doc_state();
    let set = BriefQuestions::new().for_state(&state);
    let mut answers = answers_for(&set);
    answers.insert(
        "quote_concern".into(),
        Answer::Choice {
            probabilities: [("m5".to_owned(), 1.0), ("none".to_owned(), 0.0)].into(),
        },
    );

    let brief = from_answers(&state, &answers, &THRESHOLDS);

    assert_eq!(brief.quote_concern, None);
}

#[test]
fn evidence_balance_keeps_its_value_and_distribution() {
    let state = doc_state();
    let set = BriefQuestions::new().for_state(&state);
    let mut answers = answers_for(&set);
    answers.insert(
        "evidence_balance".into(),
        Answer::Score {
            probabilities: vec![0.0, 0.0, 0.0, 0.5, 0.5],
        },
    );

    let brief = from_answers(&state, &answers, &THRESHOLDS);

    let balance = brief.evidence_balance.unwrap();
    assert!((balance.value - 0.875).abs() < 1e-9, "{}", balance.value);
    assert_eq!(balance.distribution, [0.0, 0.0, 0.0, 0.5, 0.5]);
}

#[test]
fn the_brief_fits_a_provider_with_common_limits() {
    let capabilities = crate::judge::Capabilities {
        question_kinds: vec![
            crate::judge::QuestionKind::Choice,
            crate::judge::QuestionKind::Score,
        ],
        max_choice_options: 255,
        max_score_levels: 10,
        max_context_tokens: None,
    };

    let set = BriefQuestions::new().for_state(&doc_state());

    assert_eq!(capabilities.check(&set), Vec::<String>::new());
}

#[test]
fn a_long_transcript_offers_only_the_most_recent_messages() {
    let many: Vec<(&str, Option<&str>)> = (0..300).map(|_| ("buyer", None)).collect();
    let state = state(&many);

    let set = BriefQuestions::new().for_state(&state);

    let offered = options(&set, "quote_concern");
    assert_eq!(
        offered.len(),
        MAX_QUOTE_OPTIONS + 1,
        "the most recent messages and none"
    );
    assert_eq!(
        offered[offered.len() - 2],
        "m300",
        "the newest message is offered"
    );
    assert!(!offered.contains(&"m1".to_owned()));
}
