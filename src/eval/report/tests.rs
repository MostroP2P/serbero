#![allow(clippy::unwrap_used)] // test helpers

use serde_json::{Value, json};

use super::*;
use crate::judge::Answer;
use crate::judge::providers::recorded::RecordedJudge;
use crate::judge::questions::Language;

const THRESHOLDS: Thresholds = Thresholds {
    guide: 0.9,
    fact: 0.8,
    human_request: 0.8,
    fraud: 0.6,
    conflict: 0.75,
    outside_scope: 0.8,
    validated_languages: Vec::new(),
};

fn turn() -> TurnQuestions {
    TurnQuestions::new(&[
        Language {
            code: "en",
            name: "English",
        },
        Language {
            code: "es",
            name: "Spanish",
        },
    ])
}

fn case(id: &str, text: &str, expect: Value) -> Case {
    serde_json::from_value(json!({
        "id": id,
        "lang": "es",
        "source": "synthetic",
        "state": {
            "transcript": [{ "id": "m1", "from": "buyer", "text": text }],
            "latest": { "buyer": ["m1"], "seller": [] }
        },
        "expect": expect
    }))
    .unwrap()
}

/// Answers for every question the case asks: `payment` for buyer_payment,
/// nothing else stated.
fn answers_for(case: &Case, payment: &str) -> crate::judge::Answers {
    case.questions(&turn())
        .questions
        .iter()
        .map(|(id, question)| {
            let answer = match question {
                crate::judge::Question::Noul { .. } => Answer::Noul { p_yes: 0.02 },
                crate::judge::Question::Choice { options, .. } => {
                    let pick = match id.as_str() {
                        "buyer_payment" => payment,
                        "buyer_language" => "es",
                        _ => options.last().map(|(o, _)| o.as_str()).unwrap(),
                    };
                    Answer::Choice {
                        probabilities: options
                            .iter()
                            .map(|(o, _)| (o.clone(), if o == pick { 1.0 } else { 0.0 }))
                            .collect(),
                    }
                }
                crate::judge::Question::Score { .. } => panic!("no score in a turn"),
            };
            (id.clone(), answer)
        })
        .collect()
}

fn recorded(cases: &[(Case, &str)]) -> RecordedJudge {
    RecordedJudge::new(Recording {
        judge: "typesafe/jev-1.13.0".into(),
        question_set: turn().id().into(),
        cases: cases
            .iter()
            .map(|(case, payment)| RecordedCase {
                case_id: case.id.clone(),
                state: case.state.clone(),
                answers: answers_for(case, payment),
            })
            .collect(),
    })
    .unwrap()
}

fn two_cases() -> Vec<(Case, &'static str)> {
    vec![
        (
            case(
                "a",
                "ya pagué",
                json!({ "buyer_payment": "says_sent", "buyer_language": "es" }),
            ),
            "says_sent",
        ),
        (
            case(
                "b",
                "aún no",
                json!({ "buyer_payment": "says_not_sent", "buyer_wants_human": false }),
            ),
            "says_sent",
        ),
    ]
}

#[tokio::test]
async fn a_run_keeps_case_order_and_answers() {
    let cases = two_cases();
    let judge = recorded(&cases);

    let runs = run(
        &judge,
        &turn(),
        cases.iter().map(|(c, _)| c.clone()).collect(),
    )
    .await
    .unwrap();

    let ids: Vec<&str> = runs.iter().map(|r| r.case.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(
        runs[1].judged.answers["buyer_payment"].winner(),
        Some("says_sent")
    );
}

#[tokio::test]
async fn the_recording_replays_the_same_answers() {
    let cases = two_cases();
    let runs = run(
        &recorded(&cases),
        &turn(),
        cases.iter().map(|(c, _)| c.clone()).collect(),
    )
    .await
    .unwrap();

    let replay = RecordedJudge::new(recording("typesafe/jev-1.13.0", turn().id(), &runs)).unwrap();
    let again = run(
        &replay,
        &turn(),
        cases.iter().map(|(c, _)| c.clone()).collect(),
    )
    .await
    .unwrap();

    for (first, second) in runs.iter().zip(&again) {
        assert_eq!(first.judged.answers, second.judged.answers);
    }
}

#[tokio::test]
async fn the_report_scores_targets_and_lists_misses() {
    let cases = two_cases();
    let runs = run(
        &recorded(&cases),
        &turn(),
        cases.iter().map(|(c, _)| c.clone()).collect(),
    )
    .await
    .unwrap();
    let turn = turn();
    let header = Header {
        judge_id: "typesafe/jev-1.13.0",
        question_set: turn.id(),
        lang: "es",
        thresholds: &THRESHOLDS,
        thresholds_source: "defaults",
    };

    let report = render(&header, &runs);

    assert!(
        report.starts_with("# Evaluation: es · typesafe/jev-1.13.0\n"),
        "{report}"
    );
    assert!(report.contains("2 cases, 4 labelled answers"), "{report}");
    assert!(
        report.contains("| `<party>_language` | accuracy | 1.000 | 1 | ≥ 0.95 | yes |"),
        "{report}"
    );
    assert!(
        report.contains("| `buyer_payment` | accuracy above 0.8 | 0.500 | 2 | ≥ 0.95 | **no** |"),
        "{report}"
    );
    assert!(
        report.contains("| `fraud_signal` | recall of `true` at 0.6 | — | 0 | ≥ 0.85 | **no** |"),
        "{report}"
    );
    assert!(report.contains("## Misses (1)"), "{report}");
    assert!(
        report.contains("| `b` | `buyer_payment` | `says_not_sent` | `says_sent` (1.00) | 0.00 |"),
        "{report}"
    );
    assert!(report.contains("Input tokens: not reported."), "{report}");
}
