#![allow(clippy::unwrap_used)] // test helpers

use serde_json::Value;

use super::*;
use crate::catalog::{Catalogs, embedded_codes};
use crate::judge::{Capabilities, QuestionKind};

/// Every released question set with the hash of its rendered questions for
/// the default languages. When a question changes, bump
/// `QUESTION_SET_VERSION`, add a line here, and re-run the golden set
/// (`docs/evaluation.md`). Never edit an existing line.
const RELEASED: &[(&str, &str)] = &[("qs-1", "40e09af5")];

const JUDGMENTS_MD: &str = include_str!("../../../docs/judgments.md");

fn catalogs() -> Catalogs {
    Catalogs::embedded().unwrap()
}

/// The default `[mediation].languages`: every embedded catalog.
fn default_questions() -> TurnQuestions {
    let catalogs = catalogs();
    let languages: Vec<Language<'_>> = embedded_codes()
        .into_iter()
        .map(|code| Language {
            code,
            name: &catalogs.get(code).unwrap().name,
        })
        .collect();
    TurnQuestions::new(&languages)
}

/// The JSON blocks of one section of `docs/judgments.md`, from its heading
/// to the next heading.
fn doc_blocks(heading: &str) -> Vec<Value> {
    let start = JUDGMENTS_MD.find(&format!("\n{heading}\n")).unwrap() + heading.len() + 2;
    let rest = &JUDGMENTS_MD[start..];
    let section = &rest[..rest.find("\n#").unwrap_or(rest.len())];
    section
        .split("```json\n")
        .skip(1)
        .map(|block| serde_json::from_str(&block[..block.find("```").unwrap()]).unwrap())
        .collect()
}

/// A per-party block of the doc with `<party>` filled in, keys and text.
fn for_party(block: &Value, party: &str) -> Value {
    serde_json::from_str(&block.to_string().replace("<party>", party)).unwrap()
}

fn only(set: &QuestionSet, ids: &[&str]) -> BTreeMap<String, Question> {
    ids.iter()
        .map(|id| ((*id).to_owned(), set.questions[*id].clone()))
        .collect()
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn case_facts_match_judgments_md() {
    let doc = doc_blocks("### 2.1 Case facts (whole transcript)");
    let questions = default_questions();

    assert_eq!(
        canonical_json(&only(questions.all(), &keys(&doc[0]))),
        doc[0]
    );
}

#[test]
fn per_party_and_guiding_questions_match_judgments_md() {
    let doc = doc_blocks("### 2.2 Per-party questions (latest messages)");
    let questions = default_questions();

    for party in ["buyer", "seller"] {
        for block in &doc {
            let expected = for_party(block, party);
            let actual = canonical_json(&only(questions.all(), &keys(&expected)));
            assert_eq!(actual, expected, "{party}");
        }
    }
}

#[test]
fn the_full_set_is_exactly_what_the_doc_lists() {
    let mut expected: Vec<String> = keys(&doc_blocks("### 2.1 Case facts (whole transcript)")[0])
        .into_iter()
        .map(str::to_owned)
        .collect();
    for block in doc_blocks("### 2.2 Per-party questions (latest messages)") {
        for party in ["buyer", "seller"] {
            expected.extend(
                keys(&for_party(&block, party))
                    .into_iter()
                    .map(str::to_owned),
            );
        }
    }
    expected.sort();

    let actual: Vec<String> = default_questions()
        .all()
        .questions
        .keys()
        .cloned()
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn changing_a_question_requires_a_version_bump() {
    let questions = default_questions();
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
        "the questions changed: bump QUESTION_SET_VERSION, add a line to RELEASED, \
         and re-run the golden set (docs/evaluation.md)"
    );
}

#[test]
fn released_versions_are_unique() {
    let mut versions: Vec<&str> = RELEASED.iter().map(|(v, _)| *v).collect();
    versions.sort_unstable();
    versions.dedup();

    assert_eq!(versions.len(), RELEASED.len());
}

#[test]
fn the_id_is_stable_and_depends_on_the_language_set() {
    let en = Language {
        code: "en",
        name: "English",
    };
    let es = Language {
        code: "es",
        name: "Spanish",
    };

    let a = TurnQuestions::new(&[en, es]);
    let b = TurnQuestions::new(&[en, es]);
    let fewer = TurnQuestions::new(&[en]);
    let reordered = TurnQuestions::new(&[es, en]);

    assert_eq!(a.id(), b.id());
    assert!(a.id().starts_with("qs-1-"));
    assert_eq!(a.id().len(), "qs-1-".len() + HASH_CHARS);
    assert_ne!(a.id(), fewer.id());
    // Options are rendered keyed by code, so the order in the config does
    // not change what the judge receives.
    assert_eq!(a.id(), reordered.id());
}

#[test]
fn language_options_are_the_enabled_languages_then_other_and_unknown() {
    let questions = TurnQuestions::new(&[
        Language {
            code: "es",
            name: "Spanish",
        },
        Language {
            code: "fr",
            name: "French",
        },
    ]);

    let Question::Choice { options, .. } = &questions.all().questions["seller_language"] else {
        panic!("seller_language is a choice");
    };
    let options: Vec<(&str, &str)> = options
        .iter()
        .map(|(name, description)| {
            let description = description.as_ref().unwrap().as_str().unwrap();
            (name.as_str(), description)
        })
        .collect();
    assert_eq!(
        options,
        [
            ("es", "Spanish"),
            ("fr", "French"),
            ("other", "Another language"),
            ("unknown", "The messages are too short or mixed to tell."),
        ]
    );
}

#[test]
fn a_party_without_new_messages_gets_no_per_party_questions() {
    let questions = default_questions();

    let turn = questions.for_turn(&[Party::Seller], false);

    let ids: Vec<&str> = turn.questions.keys().map(String::as_str).collect();
    assert!(ids.contains(&"seller_message_kind"));
    assert!(ids.contains(&"seller_language"));
    assert!(ids.contains(&"seller_wants_human"));
    for suffix in PER_PARTY.iter().chain([&GUIDING]) {
        assert!(
            !ids.contains(&format!("buyer_{suffix}").as_str()),
            "buyer_{suffix}"
        );
    }
    assert!(
        !ids.contains(&"seller_rejects_path"),
        "asked only while guiding"
    );
}

#[test]
fn without_new_messages_only_the_case_facts_are_asked() {
    let questions = default_questions();
    let doc = doc_blocks("### 2.1 Case facts (whole transcript)");

    let turn = questions.for_turn(&[], true);

    let ids: Vec<&str> = turn.questions.keys().map(String::as_str).collect();
    let mut facts = keys(&doc[0]);
    facts.sort_unstable();
    assert_eq!(ids, facts);
}

#[test]
fn guiding_adds_rejects_path_for_parties_with_new_messages() {
    let questions = default_questions();

    let turn = questions.for_turn(&[Party::Buyer], true);

    assert!(turn.questions.contains_key("buyer_rejects_path"));
    assert!(!turn.questions.contains_key("seller_rejects_path"));
}

#[test]
fn every_turn_carries_the_full_set_id() {
    let questions = default_questions();

    let turn = questions.for_turn(&[Party::Buyer, Party::Seller], true);

    assert_eq!(turn.version, questions.id());
    assert_eq!(turn.questions, questions.all().questions);
}

#[test]
fn no_placeholder_is_left_in_any_question() {
    let rendered = canonical_json(&default_questions().all().questions).to_string();

    assert!(!rendered.contains("<party>"));
}

#[test]
fn the_full_set_fits_a_provider_with_common_limits() {
    let capabilities = Capabilities {
        question_kinds: vec![
            QuestionKind::Noul,
            QuestionKind::Choice,
            QuestionKind::Score,
        ],
        max_choice_options: 10,
        max_score_levels: 10,
        max_context_tokens: None,
    };

    assert_eq!(
        capabilities.check(default_questions().all()),
        Vec::<String>::new()
    );
}
