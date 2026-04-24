//! Issue #39 — validate PPQ.ai compatibility with the
//! `openai-compatible` adapter.
//!
//! PPQ.ai (`https://api.ppq.ai`) is an OpenAI-compatible aggregator
//! that exposes `/chat/completions` directly at the root (NOT under
//! `/v1/`) and accepts the standard `Authorization: Bearer <key>`
//! header. Operators should configure it as:
//!
//! ```toml
//! [reasoning]
//! provider    = "openai-compatible"
//! api_base    = "https://api.ppq.ai"
//! api_key_env = "PPQ_API_KEY"
//! model       = "ppq/autoclaw"
//! ```
//!
//! The issue expects a live validation checklist; this file covers
//! the same ground deterministically via `httpmock`, pinning the
//! exact wire-format expectations of the PPQ.ai endpoint so any
//! future change to the adapter that would break PPQ.ai
//! compatibility gets caught by CI.
//!
//! Scope-match: the tests never invent PPQ.ai-specific behavior —
//! they only assert that the existing OpenAI-compatible adapter
//! handles the response shape and error paths documented at
//! <https://ppq.ai/api-docs>.

use std::sync::Arc;

use httpmock::prelude::*;
use httpmock::Method::POST;
use serbero::models::dispute::InitiatorRole;
use serbero::models::mediation::ClassificationLabel;
use serbero::models::reasoning::{
    ClassificationRequest, ReasoningContext, ReasoningError, SuggestedAction, SummaryRequest,
};
use serbero::models::ReasoningConfig;
use serbero::prompts::PromptBundle;
use serbero::reasoning::openai::OpenAiProvider;
use serbero::reasoning::{build_provider, ReasoningProvider};

fn fixture_bundle() -> Arc<PromptBundle> {
    Arc::new(PromptBundle {
        id: "phase3-ppqai-test".into(),
        policy_hash: "ppq-hash".into(),
        system: "SYSTEM_MARKER: respond with JSON only".into(),
        classification: "CLASSIFICATION_MARKER: policy text".into(),
        escalation: "ESCALATION_MARKER: escalation rules".into(),
        mediation_style: "STYLE_MARKER: neutral tone".into(),
        message_templates: "TEMPLATE_MARKER: templates here".into(),
    })
}

fn classification_request() -> ClassificationRequest {
    ClassificationRequest {
        session_id: "s-ppq-1".into(),
        dispute_id: "d-ppq-1".into(),
        initiator_role: InitiatorRole::Buyer,
        prompt_bundle: fixture_bundle(),
        transcript: vec![],
        context: ReasoningContext {
            round_count: 0,
            last_classification: None,
            last_confidence: None,
        },
    }
}

fn summary_request() -> SummaryRequest {
    SummaryRequest {
        session_id: "s-ppq-1".into(),
        dispute_id: "d-ppq-1".into(),
        prompt_bundle: fixture_bundle(),
        transcript: vec![],
        classification: ClassificationLabel::CoordinationFailureResolvable,
        confidence: 0.9,
    }
}

/// PPQ.ai-style config. The key detail is that `api_base` is the
/// bare host (no `/v1`), so the adapter appends `/chat/completions`
/// to produce `{host}/chat/completions` — PPQ.ai's documented path.
fn ppqai_cfg(api_base: String) -> ReasoningConfig {
    ReasoningConfig {
        provider: "openai-compatible".into(),
        api_base,
        api_key: "ppq-test-key".into(),
        model: "ppq/autoclaw".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    }
}

/// Validates the URL shape: PPQ.ai publishes
/// `POST https://api.ppq.ai/chat/completions` (no `/v1`). The
/// adapter's `chat_completions_url` = `{api_base}/chat/completions`
/// produces that exact path when `api_base = "https://api.ppq.ai"`.
/// The mock's path matcher would refuse any other URL — so a green
/// `health_check` is proof that the adapter targets the right path.
#[tokio::test]
async fn health_check_against_ppqai_root_chat_completions_path() {
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .header_exists("authorization");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"chatcmpl-ppq-health",
                        "object":"chat.completion",
                        "choices":[{"index":0,"message":{"role":"assistant","content":"pong"}}]
                    }"#,
                );
        })
        .await;

    let cfg = ppqai_cfg(mock.base_url());
    let provider = build_provider(&cfg).expect("openai-compatible must build for PPQ.ai config");
    provider
        .health_check()
        .await
        .expect("health_check against a mocked PPQ.ai endpoint must succeed");
    hit.assert_hits_async(1).await;
}

/// JSON-mode classification is the most failure-prone interop bit
/// called out by the issue. PPQ.ai's OpenAI-compatible layer forwards
/// `response_format: {type: "json_object"}` to the upstream model,
/// which returns a JSON string in `choices[0].message.content`. This
/// test mimics that exact response and asserts the adapter parses it
/// into a valid `ClassificationResponse`.
#[tokio::test]
async fn classify_parses_ppqai_json_mode_response() {
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .header_exists("authorization")
                // The adapter must request JSON mode; PPQ.ai relies
                // on the upstream model to honor it. A test that
                // didn't check this would not notice a silent
                // response_format regression.
                .body_contains("\"response_format\"")
                .body_contains("\"json_object\"")
                .body_contains("ppq/autoclaw");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"chatcmpl-ppq-classify",
                        "object":"chat.completion",
                        "model":"ppq/autoclaw",
                        "choices":[{
                            "index":0,
                            "message":{
                                "role":"assistant",
                                "content":"{\"classification\":\"coordination_failure_resolvable\",\"confidence\":0.87,\"suggested_action\":\"summarize\",\"rationale\":\"parties aligned on payment timing\",\"flags\":[\"low_info\"]}"
                            },
                            "finish_reason":"stop"
                        }]
                    }"#,
                );
        })
        .await;

    let cfg = ppqai_cfg(mock.base_url());
    let provider = OpenAiProvider::new(&cfg).unwrap();
    let resp = provider
        .classify(classification_request())
        .await
        .expect("PPQ.ai JSON-mode classify must parse");
    assert_eq!(
        resp.classification,
        ClassificationLabel::CoordinationFailureResolvable
    );
    assert!((resp.confidence - 0.87).abs() < f64::EPSILON);
    assert_eq!(resp.suggested_action, SuggestedAction::Summarize);
    hit.assert_hits_async(1).await;
}

/// Summaries are plain text (no `response_format`). The parser reads
/// `SUGGESTED_NEXT_STEP:` and `RATIONALE:` markers out of
/// `choices[0].message.content`, so the only PPQ.ai-specific
/// invariant here is that the response wraps the text in the
/// standard OpenAI `choices` envelope.
#[tokio::test]
async fn summarize_parses_ppqai_response() {
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"chatcmpl-ppq-summary",
                        "object":"chat.completion",
                        "model":"ppq/autoclaw",
                        "choices":[{
                            "index":0,
                            "message":{
                                "role":"assistant",
                                "content":"Buyer confirmed receipt, seller confirmed funds released.\nSUGGESTED_NEXT_STEP: close the dispute in favor of buyer.\nRATIONALE: both parties aligned on the timeline."
                            },
                            "finish_reason":"stop"
                        }]
                    }"#,
                );
        })
        .await;

    let cfg = ppqai_cfg(mock.base_url());
    let provider = OpenAiProvider::new(&cfg).unwrap();
    let resp = provider
        .summarize(summary_request())
        .await
        .expect("PPQ.ai summarize must parse");
    assert!(resp.summary_text.starts_with("Buyer confirmed"));
    assert!(resp.suggested_next_step.contains("close the dispute"));
    assert!(resp.rationale.0.contains("aligned"));
    hit.assert_hits_async(1).await;
}

/// PPQ.ai returns 401 for invalid keys. The adapter must surface
/// this as `ReasoningError::Unreachable` with the HTTP status in
/// the message — a non-retryable status, so there is no retry storm.
#[tokio::test]
async fn invalid_key_maps_to_unreachable() {
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(401)
                .header("content-type", "application/json")
                .body(r#"{"error":{"message":"Invalid API key","type":"auth_error"}}"#);
        })
        .await;

    // Raise `followup_retry_count` to prove the adapter DOES NOT
    // retry on 401 (permanent client error). The mock must see
    // exactly one hit despite a budget of 3.
    let mut cfg = ppqai_cfg(mock.base_url());
    cfg.followup_retry_count = 3;

    let provider = OpenAiProvider::new(&cfg).unwrap();
    let err = provider.health_check().await.unwrap_err();
    match err {
        ReasoningError::Unreachable(msg) => {
            assert!(
                msg.contains("401"),
                "error must carry the HTTP status: {msg}"
            );
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
    hit.assert_hits_async(1).await;
}

/// PPQ.ai returns 429 when the account exceeds its quota. 429 IS
/// retryable per the adapter's status table, so with a retry budget
/// of 2 the mock should see 3 attempts (initial + 2 retries) before
/// the caller sees the final error.
#[tokio::test]
async fn rate_limit_retries_and_eventually_surfaces_unreachable() {
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(429)
                .header("content-type", "application/json")
                .header("retry-after", "7")
                .body(r#"{"error":{"message":"Rate limit exceeded","type":"rate_limit_error"}}"#);
        })
        .await;

    let mut cfg = ppqai_cfg(mock.base_url());
    cfg.followup_retry_count = 2;

    let provider = OpenAiProvider::new(&cfg).unwrap();
    let err = provider.health_check().await.unwrap_err();
    assert!(
        matches!(err, ReasoningError::Unreachable(_)),
        "429 after retries must surface as Unreachable, got {err:?}"
    );
    // 1 initial + 2 retries = 3 total attempts. Proves the adapter
    // treats 429 as retryable and honors the configured budget
    // against a PPQ.ai-shaped response.
    hit.assert_hits_async(3).await;
}

/// Requesting a model the PPQ.ai aggregator does not route to
/// surfaces as a 404 with an OpenAI-style error body. 404 is a
/// permanent client error and MUST NOT be retried.
#[tokio::test]
async fn model_not_found_maps_to_unreachable_without_retry() {
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(404).header("content-type", "application/json").body(
                r#"{"error":{"message":"Model 'ppq/does-not-exist' not found","type":"not_found"}}"#,
            );
        })
        .await;

    let mut cfg = ppqai_cfg(mock.base_url());
    cfg.model = "ppq/does-not-exist".into();
    cfg.followup_retry_count = 3;

    let provider = OpenAiProvider::new(&cfg).unwrap();
    let err = provider.health_check().await.unwrap_err();
    match err {
        ReasoningError::Unreachable(msg) => {
            assert!(
                msg.contains("404"),
                "error must carry the HTTP status: {msg}"
            );
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
    // Exactly one request despite retries=3 — proves 404 is treated
    // as non-retryable and the adapter fails fast.
    hit.assert_hits_async(1).await;
}
