#![allow(clippy::unwrap_used)] // test helpers

use std::time::{Duration, Instant};

use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::judge::NoulCriteria;

const KEY: &str = "test-key";

type ErrorCheck = fn(&JudgeError) -> bool;

fn config(server: &MockServer) -> JudgeConfig {
    JudgeConfig {
        api_base: format!("{}/", server.uri()),
        timeout: Duration::from_millis(500),
        ..JudgeConfig::default()
    }
}

fn fast_retry(max_retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
    }
}

async fn judge(server: &MockServer, max_retries: u32) -> TypeSafeJudge {
    TypeSafeJudge::new(&config(server), KEY)
        .unwrap()
        .with_retry(fast_retry(max_retries))
}

fn questions() -> QuestionSet {
    QuestionSet {
        version: "qs-test".into(),
        questions: [
            (
                "wants_human".to_owned(),
                Question::Noul {
                    instructions: json!("Does the buyer ask for a human?"),
                    criteria: Some(NoulCriteria {
                        yes: json!("Asks for a person"),
                        no: json!("Does not"),
                    }),
                },
            ),
            (
                "payment".to_owned(),
                Question::Choice {
                    instructions: json!("What does the buyer say?"),
                    options: vec![
                        ("says_sent".into(), Some(json!("Claims to have paid"))),
                        ("unclear".into(), None),
                    ],
                },
            ),
            (
                "evidence".to_owned(),
                Question::Score {
                    instructions: json!("How strong is the evidence?"),
                    levels: vec![json!("None"), json!("Some"), json!("Strong")],
                },
            ),
        ]
        .into(),
    }
}

fn state() -> Value {
    json!({ "transcript": [{ "id": "m1", "from": "buyer", "text": "I paid" }] })
}

fn answers_body() -> Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "wants_human": { "type": "noul", "noul": 0.12 },
            "payment": {
                "type": "choice",
                "choice": "says_sent",
                "probabilities": { "says_sent": 0.9, "unclear": 0.1 },
                "confidence": 0.8
            },
            "evidence": {
                "type": "score",
                "score": 1.1,
                "legend": { "0": "None", "1": "Some", "2": "Strong" },
                "probabilities": { "0": 0.1, "1": 0.7, "2": 0.2 },
                "confidence": 0.55
            }
        },
        "usage": { "input_tokens": 300, "output_tokens": 30 }
    })
}

fn expected_request() -> Value {
    json!({
        "state": state(),
        "model": "jev-1.13.0",
        "questions": {
            "wants_human": {
                "type": "noul",
                "instructions": "Does the buyer ask for a human?",
                "criteria": { "true": "Asks for a person", "false": "Does not" }
            },
            "payment": {
                "type": "choice",
                "instructions": "What does the buyer say?",
                "criteria": { "says_sent": "Claims to have paid", "unclear": null }
            },
            "evidence": {
                "type": "score",
                "instructions": "How strong is the evidence?",
                "criteria": ["None", "Some", "Strong"]
            }
        }
    })
}

async fn respond(server: &MockServer, template: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(template)
        .mount(server)
        .await;
}

#[tokio::test]
async fn translates_questions_and_answers_of_every_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_json(expected_request()))
        .respond_with(ResponseTemplate::new(200).set_body_json(answers_body()))
        .expect(1)
        .mount(&server)
        .await;
    let judge = judge(&server, 0).await;

    let answers = judge.evaluate(&state(), &questions()).await.unwrap();

    assert_eq!(judge.id(), "typesafe/jev-1.13.0");
    assert_eq!(answers["wants_human"], Answer::Noul { p_yes: 0.12 });
    assert_eq!(answers["payment"].winner(), Some("says_sent"));
    assert_eq!(
        answers["evidence"],
        Answer::Score {
            probabilities: vec![0.1, 0.7, 0.2]
        }
    );
}

#[tokio::test]
async fn status_codes_map_onto_provider_neutral_errors() {
    let cases: [(u16, ErrorCheck); 5] = [
        (401, |e| matches!(e, JudgeError::Unauthorized(_))),
        (403, |e| matches!(e, JudgeError::Unauthorized(_))),
        (422, |e| matches!(e, JudgeError::InvalidRequest(_))),
        (429, |e| matches!(e, JudgeError::Unavailable(_))),
        (529, |e| matches!(e, JudgeError::Unavailable(_))),
    ];
    for (status, expected) in cases {
        let server = MockServer::start().await;
        respond(
            &server,
            ResponseTemplate::new(status).set_body_string("{\"detail\":\"nope\"}"),
        )
        .await;

        let err = judge(&server, 0)
            .await
            .evaluate(&state(), &questions())
            .await
            .unwrap_err();

        assert!(expected(&err), "{status} gave {err:?}");
        assert!(err.to_string().contains(&format!("HTTP {status}")), "{err}");
    }
}

#[tokio::test]
async fn retryable_failures_are_retried_until_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(529))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    respond(
        &server,
        ResponseTemplate::new(200).set_body_json(answers_body()),
    )
    .await;

    let answers = judge(&server, 3)
        .await
        .evaluate(&state(), &questions())
        .await
        .unwrap();

    assert_eq!(answers.len(), 3);
}

#[tokio::test]
async fn retries_stop_after_max_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(429))
        .expect(3)
        .mount(&server)
        .await;

    let err = judge(&server, 2)
        .await
        .evaluate(&state(), &questions())
        .await
        .unwrap_err();

    assert!(matches!(err, JudgeError::Unavailable(_)));
}

#[tokio::test]
async fn non_retryable_failures_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let err = judge(&server, 3)
        .await
        .evaluate(&state(), &questions())
        .await
        .unwrap_err();

    assert!(matches!(err, JudgeError::Unauthorized(_)));
}

#[tokio::test]
async fn retry_after_is_honored() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after-ms", "300"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    respond(
        &server,
        ResponseTemplate::new(200).set_body_json(answers_body()),
    )
    .await;
    let started = Instant::now();

    judge(&server, 1)
        .await
        .evaluate(&state(), &questions())
        .await
        .unwrap();

    assert!(started.elapsed() >= Duration::from_millis(300));
}

#[tokio::test]
async fn a_timeout_is_unavailable() {
    let server = MockServer::start().await;
    respond(
        &server,
        ResponseTemplate::new(200)
            .set_body_json(answers_body())
            .set_delay(Duration::from_secs(2)),
    )
    .await;

    let err = judge(&server, 0)
        .await
        .evaluate(&state(), &questions())
        .await
        .unwrap_err();

    assert!(
        matches!(&err, JudgeError::Unavailable(e) if e.starts_with("timeout")),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_network_failure_is_unavailable() {
    // Port 1 is reserved and nothing listens on it; a dropped mock server's
    // port could be reused by a test running in parallel.
    let config = JudgeConfig {
        api_base: "http://127.0.0.1:1".into(),
        timeout: Duration::from_millis(500),
        ..JudgeConfig::default()
    };

    let err = TypeSafeJudge::new(&config, KEY)
        .unwrap()
        .with_retry(fast_retry(0))
        .evaluate(&state(), &questions())
        .await
        .unwrap_err();

    assert!(matches!(err, JudgeError::Unavailable(_)), "{err:?}");
}

#[tokio::test]
async fn bodies_that_do_not_fit_are_malformed() {
    let mut missing_level = answers_body();
    missing_level["answers"]["evidence"]["probabilities"] = json!({ "0": 0.5, "2": 0.5 });
    let mut missing_answer = answers_body();
    missing_answer["answers"]
        .as_object_mut()
        .unwrap()
        .remove("payment");
    let mut wrong_option = answers_body();
    wrong_option["answers"]["payment"]["probabilities"] = json!({ "says_sent": 0.9, "x": 0.1 });
    let cases = [
        ResponseTemplate::new(200).set_body_string("not json"),
        ResponseTemplate::new(200).set_body_json(missing_level),
        ResponseTemplate::new(200).set_body_json(missing_answer),
        ResponseTemplate::new(200).set_body_json(wrong_option),
    ];
    for template in cases {
        let server = MockServer::start().await;
        respond(&server, template).await;

        let err = judge(&server, 3)
            .await
            .evaluate(&state(), &questions())
            .await
            .unwrap_err();

        assert!(matches!(err, JudgeError::Malformed(_)), "{err:?}");
    }
}

#[tokio::test]
async fn health_check_lists_models_with_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "models": [] })))
        .expect(1)
        .mount(&server)
        .await;

    assert_eq!(judge(&server, 0).await.health_check().await, Ok(()));
}

#[tokio::test]
async fn health_check_reports_a_bad_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let err = judge(&server, 0).await.health_check().await.unwrap_err();

    assert!(matches!(err, JudgeError::Unauthorized(_)));
}

#[tokio::test]
async fn capabilities_follow_jevs_documented_limits() {
    let server = MockServer::start().await;

    let caps = judge(&server, 0).await.capabilities();

    assert_eq!(caps.max_choice_options, 255);
    assert_eq!(caps.max_score_levels, 10);
    assert_eq!(caps.max_context_tokens, Some(64_000));
    assert!(caps.check(&questions()).is_empty());
}

#[test]
fn backoff_doubles_up_to_the_cap() {
    let policy = RetryPolicy::new(5);

    assert_eq!(policy.backoff(0), Duration::from_millis(500));
    assert_eq!(policy.backoff(1), Duration::from_secs(1));
    assert_eq!(policy.backoff(4), Duration::from_secs(5));
    assert_eq!(policy.backoff(40), Duration::from_secs(5));
}

#[test]
fn retry_after_headers_are_parsed_and_capped() {
    let headers = |pairs: &[(&'static str, &'static str)]| {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    };

    assert_eq!(
        retry_after(&headers(&[("retry-after", "2")])),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        retry_after(&headers(&[("retry-after-ms", "250"), ("retry-after", "9")])),
        Some(Duration::from_millis(250))
    );
    assert_eq!(
        retry_after(&headers(&[("retry-after", "3600")])),
        Some(MAX_RETRY_AFTER)
    );
    assert_eq!(retry_after(&headers(&[("retry-after", "soon")])), None);
    assert_eq!(retry_after(&HeaderMap::new()), None);
}

/// Proves TLS works with the `ring` provider against the real API; without
/// a key it must be refused, not fail to connect.
#[tokio::test]
#[ignore = "calls api.typesafe.ai"]
async fn the_real_api_refuses_a_missing_key() {
    let judge = TypeSafeJudge::new(&JudgeConfig::default(), "not-a-key")
        .unwrap()
        .with_retry(fast_retry(0));

    let err = judge.health_check().await.unwrap_err();

    assert!(matches!(err, JudgeError::Unauthorized(_)), "{err:?}");
}
