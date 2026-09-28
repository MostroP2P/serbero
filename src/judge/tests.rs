#![allow(clippy::unwrap_used)] // test helpers

use serde_json::json;

use super::*;

fn choice(pairs: &[(&str, f64)]) -> Answer {
    Answer::Choice {
        probabilities: pairs.iter().map(|(o, p)| ((*o).to_owned(), *p)).collect(),
    }
}

fn choice_question(options: &[&str]) -> Question {
    Question::Choice {
        instructions: json!("Which one?"),
        options: options.iter().map(|o| ((*o).to_owned(), None)).collect(),
    }
}

fn score_question(levels: usize) -> Question {
    Question::Score {
        instructions: json!("How much?"),
        levels: (0..levels).map(|i| json!(format!("level {i}"))).collect(),
    }
}

fn noul_question() -> Question {
    Question::Noul {
        instructions: json!("Is it so?"),
        criteria: None,
    }
}

fn set(questions: Vec<(&str, Question)>) -> QuestionSet {
    QuestionSet {
        version: "qs-test".into(),
        questions: questions
            .into_iter()
            .map(|(id, q)| (id.to_owned(), q))
            .collect(),
    }
}

fn caps() -> Capabilities {
    Capabilities {
        question_kinds: vec![
            QuestionKind::Noul,
            QuestionKind::Choice,
            QuestionKind::Score,
        ],
        max_choice_options: 4,
        max_score_levels: 3,
        max_context_tokens: None,
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn confidence_is_one_for_a_certain_answer() {
    let answer = choice(&[("a", 1.0), ("b", 0.0), ("c", 0.0)]);

    assert!(close(answer.confidence().unwrap(), 1.0));
}

#[test]
fn confidence_is_zero_for_a_flat_distribution() {
    let answer = choice(&[("a", 0.25), ("b", 0.25), ("c", 0.25), ("d", 0.25)]);

    assert!(close(answer.confidence().unwrap(), 0.0));
}

#[test]
fn confidence_follows_the_documented_formula() {
    // (3 · 0.9 − 1) / (3 − 1) = 0.85
    let answer = choice(&[("a", 0.9), ("b", 0.06), ("c", 0.04)]);

    assert!(close(answer.confidence().unwrap(), 0.85));
}

#[test]
fn confidence_depends_only_on_the_distribution() {
    let as_choice = choice(&[("a", 0.7), ("b", 0.2), ("c", 0.1)]);
    let as_score = Answer::Score {
        probabilities: vec![0.1, 0.2, 0.7],
    };

    assert!(close(
        as_choice.confidence().unwrap(),
        as_score.confidence().unwrap()
    ));
}

#[test]
fn a_noul_has_no_confidence() {
    assert_eq!(Answer::Noul { p_yes: 0.9 }.confidence(), None);
}

#[test]
fn winner_is_the_most_probable_option() {
    let answer = choice(&[("sent", 0.2), ("not_sent", 0.7), ("unclear", 0.1)]);

    assert_eq!(answer.winner(), Some("not_sent"));
    assert!(close(answer.probability("sent"), 0.2));
    assert!(close(answer.probability("missing"), 0.0));
}

#[test]
fn a_tie_goes_to_the_first_option_by_name() {
    let answer = choice(&[("b", 0.5), ("a", 0.5)]);

    assert_eq!(answer.winner(), Some("a"));
}

#[test]
fn score_value_is_the_weighted_level() {
    let answer = Answer::Score {
        probabilities: vec![0.0, 0.5, 0.5],
    };

    assert!(close(answer.score_value().unwrap(), 0.75));
    assert_eq!(Answer::Noul { p_yes: 0.5 }.score_value(), None);
}

#[test]
fn score_value_stays_in_range_for_a_tolerated_sum() {
    // Sums to 1.009: accepted by `check`, and still at most 1.
    let answer = Answer::Score {
        probabilities: vec![0.0, 0.009, 1.0],
    };

    assert!(answer.check(&score_question(3)).is_ok());
    let value = answer.score_value().unwrap();
    assert!(value <= 1.0, "{value}");
    assert!(close(value, (0.0045 + 1.0) / 1.009));
}

#[test]
fn a_matching_answer_passes_the_check() {
    let answers: Answers = [
        ("n".to_owned(), Answer::Noul { p_yes: 0.3 }),
        ("c".to_owned(), choice(&[("x", 0.6), ("y", 0.4)])),
        (
            "s".to_owned(),
            Answer::Score {
                probabilities: vec![0.2, 0.3, 0.5],
            },
        ),
    ]
    .into();
    let questions = set(vec![
        ("n", noul_question()),
        ("c", choice_question(&["y", "x"])),
        ("s", score_question(3)),
    ]);

    assert_eq!(check_answers(&questions, &answers), Ok(()));
}

#[test]
fn malformed_answers_are_rejected() {
    let q = choice_question(&["x", "y"]);
    let cases = [
        (Answer::Noul { p_yes: 0.5 }, "a Noul answer"),
        (choice(&[("x", 1.0)]), "do not match"),
        (choice(&[("x", 0.5), ("z", 0.5)]), "do not match"),
        (choice(&[("x", 0.5), ("y", 0.2)]), "sum to"),
        (choice(&[("x", 1.5), ("y", -0.5)]), "not in [0, 1]"),
    ];

    for (answer, expected) in cases {
        let err = answer.check(&q).unwrap_err();
        assert!(err.contains(expected), "{err} should contain {expected}");
    }
    assert!(
        Answer::Score {
            probabilities: vec![1.0]
        }
        .check(&score_question(2))
        .unwrap_err()
        .contains("1 probabilities for 2 levels")
    );
    assert!(Answer::Noul { p_yes: 1.2 }.check(&noul_question()).is_err());
}

#[test]
fn missing_or_extra_answers_are_malformed() {
    let questions = set(vec![("n", noul_question())]);
    let extra: Answers = [
        ("n".to_owned(), Answer::Noul { p_yes: 0.3 }),
        ("other".to_owned(), Answer::Noul { p_yes: 0.3 }),
    ]
    .into();

    assert!(matches!(
        check_answers(&questions, &Answers::new()),
        Err(JudgeError::Malformed(e)) if e.contains("no answer to n")
    ));
    assert!(matches!(
        check_answers(&questions, &extra),
        Err(JudgeError::Malformed(e)) if e.contains("unknown question other")
    ));
}

#[test]
fn a_supported_question_set_has_no_problems() {
    let questions = set(vec![
        ("n", noul_question()),
        ("c", choice_question(&["a", "b", "c", "d"])),
        ("s", score_question(3)),
    ]);

    assert!(caps().check(&questions).is_empty());
}

#[test]
fn capabilities_report_every_question_that_cannot_be_expressed() {
    let questions = set(vec![
        ("too_many", choice_question(&["a", "b", "c", "d", "e"])),
        ("too_few", choice_question(&["a"])),
        ("dupes", choice_question(&["a", "a"])),
        ("levels", score_question(4)),
    ]);

    let problems = caps().check(&questions);

    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("too_many: 5 options"))
    );
    assert!(problems.iter().any(|p| p.starts_with("too_few: 1 options")));
    assert!(
        problems
            .iter()
            .any(|p| p == "dupes: duplicate option names")
    );
    assert!(problems.iter().any(|p| p.starts_with("levels: 4 levels")));
}

#[test]
fn capabilities_reject_unsupported_question_kinds() {
    let nouls_only = Capabilities {
        question_kinds: vec![QuestionKind::Noul],
        ..caps()
    };
    let questions = set(vec![("s", score_question(2))]);

    assert_eq!(
        nouls_only.check(&questions),
        vec!["s: Score questions are not supported".to_owned()]
    );
}

#[test]
fn only_unavailable_is_retryable() {
    assert!(JudgeError::Unavailable("429".into()).is_retryable());
    assert!(!JudgeError::Unauthorized("401".into()).is_retryable());
    assert!(!JudgeError::InvalidRequest("422".into()).is_retryable());
    assert!(!JudgeError::Malformed("x".into()).is_retryable());
}
