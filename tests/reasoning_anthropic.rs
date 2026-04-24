//! Integration tests for the Anthropic (`provider = "anthropic"`)
//! reasoning adapter — issue #38.
//!
//! The tests here exercise the adapter end-to-end through a local
//! `httpmock` server. Mock responses use the real Anthropic Messages
//! API shape (a `content: [{"type":"text","text":"..."}]` array with
//! a top-level `system` in the request) so the adapter's wire-format
//! mapping is actually validated, not just its builder plumbing.

use std::sync::Arc;

use httpmock::prelude::*;
use httpmock::Method::POST;
use serbero::error::Error;
use serbero::models::dispute::InitiatorRole;
use serbero::models::mediation::ClassificationLabel;
use serbero::models::reasoning::{
    ClassificationRequest, ReasoningContext, ReasoningError, SuggestedAction, SummaryRequest,
};
use serbero::models::ReasoningConfig;
use serbero::prompts::PromptBundle;
use serbero::reasoning::anthropic::AnthropicProvider;
use serbero::reasoning::{build_provider, ReasoningProvider};

/// Minimal test bundle. The adapter copies `system` into the
/// Anthropic `system` field and embeds every other section in the
/// user prompt; the markers let us assert either in follow-up tests.
fn fixture_bundle() -> Arc<PromptBundle> {
    Arc::new(PromptBundle {
        id: "phase3-anthropic-test".into(),
        policy_hash: "abc123".into(),
        system: "SYSTEM_MARKER: respond with JSON only".into(),
        classification: "CLASSIFICATION_MARKER: policy text".into(),
        escalation: "ESCALATION_MARKER: escalation rules".into(),
        mediation_style: "STYLE_MARKER: neutral tone".into(),
        message_templates: "TEMPLATE_MARKER: templates here".into(),
    })
}

fn classification_request() -> ClassificationRequest {
    ClassificationRequest {
        session_id: "s-anthropic-1".into(),
        dispute_id: "d-anthropic-1".into(),
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
        session_id: "s-anthropic-1".into(),
        dispute_id: "d-anthropic-1".into(),
        prompt_bundle: fixture_bundle(),
        transcript: vec![],
        classification: ClassificationLabel::CoordinationFailureResolvable,
        confidence: 0.9,
    }
}

/// Issue #38 acceptance test 1: `build_provider` with
/// `provider = "anthropic"` must return the native Anthropic adapter,
/// NOT `NotYetImplementedProvider`. We prove this by traffic
/// observation — only the real adapter dials an HTTP endpoint.
#[tokio::test]
async fn build_provider_routes_anthropic_to_native_adapter() {
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"msg_test",
                        "type":"message",
                        "role":"assistant",
                        "content":[{"type":"text","text":"pong"}]
                    }"#,
                );
        })
        .await;

    let cfg = ReasoningConfig {
        provider: "anthropic".into(),
        api_base: mock.base_url(),
        api_key: "test-key".into(),
        model: "claude-3-5-sonnet-20241022".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    };
    let provider = build_provider(&cfg).expect("anthropic provider must build");
    provider
        .health_check()
        .await
        .expect("anthropic health_check against mock must succeed");

    hit.assert_hits_async(1).await;
}

/// Issue #38 acceptance test 2: invalid keys surface as
/// `ReasoningError::Unreachable`. The mock simulates the real
/// Anthropic 401 body so the caller-facing error path is exercised.
#[tokio::test]
async fn health_check_surfaces_invalid_key_as_unreachable() {
    let mock = MockServer::start_async().await;
    let _hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(401)
                .header("content-type", "application/json")
                .body(
                    r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
                );
        })
        .await;

    let cfg = ReasoningConfig {
        provider: "anthropic".into(),
        api_base: mock.base_url(),
        api_key: "definitely-not-a-real-key".into(),
        model: "claude-3-5-sonnet-20241022".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    };
    let provider = AnthropicProvider::new(&cfg).unwrap();
    let err = provider.health_check().await.unwrap_err();
    match err {
        ReasoningError::Unreachable(msg) => {
            assert!(
                msg.contains("401") || msg.to_lowercase().contains("unauthorized"),
                "401 must be reported in the error message, got: {msg}"
            );
            assert!(
                msg.to_lowercase().contains("anthropic"),
                "error should name the provider, got: {msg}"
            );
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

/// Issue #38 acceptance test 3a: the adapter translates an Anthropic
/// Messages response into the correct `ClassificationResponse`.
#[tokio::test]
async fn classify_parses_mocked_anthropic_response() {
    let mock = MockServer::start_async().await;
    // The adapter is expected to send a POST to /v1/messages with
    // `system` at the top level (not a role in `messages`), an
    // `anthropic-version` header, and `x-api-key` auth. The mock
    // asserts those wire-format invariants while returning a
    // classification JSON in the `content[0].text` field.
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("anthropic-version", "2023-06-01")
                .header_exists("x-api-key")
                .body_contains("\"system\"")
                .body_contains("claude-3-5-sonnet-20241022");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"msg_test",
                        "type":"message",
                        "role":"assistant",
                        "content":[{"type":"text","text":"{\"classification\":\"coordination_failure_resolvable\",\"confidence\":0.88,\"suggested_action\":\"summarize\",\"rationale\":\"parties aligned on timing\",\"flags\":[\"low_info\"]}"}],
                        "model":"claude-3-5-sonnet-20241022",
                        "stop_reason":"end_turn"
                    }"#,
                );
        })
        .await;

    let cfg = ReasoningConfig {
        provider: "anthropic".into(),
        api_base: mock.base_url(),
        api_key: "test-key".into(),
        model: "claude-3-5-sonnet-20241022".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    };
    let provider = AnthropicProvider::new(&cfg).unwrap();
    let resp = provider
        .classify(classification_request())
        .await
        .expect("classify must succeed on a well-formed mock response");

    assert_eq!(
        resp.classification,
        ClassificationLabel::CoordinationFailureResolvable
    );
    assert!((resp.confidence - 0.88).abs() < f64::EPSILON);
    assert_eq!(resp.suggested_action, SuggestedAction::Summarize);
    assert_eq!(resp.rationale.0, "parties aligned on timing");

    hit.assert_hits_async(1).await;
}

/// Issue #38 acceptance test 3b: `summarize` maps the Anthropic
/// response's text block into the expected `SummaryResponse` shape.
#[tokio::test]
async fn summarize_parses_mocked_anthropic_response() {
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"msg_test",
                        "type":"message",
                        "role":"assistant",
                        "content":[{"type":"text","text":"Buyer confirmed receipt, seller confirmed funds released.\nSUGGESTED_NEXT_STEP: close the dispute in favor of buyer.\nRATIONALE: both parties aligned on the timeline."}],
                        "model":"claude-3-5-sonnet-20241022",
                        "stop_reason":"end_turn"
                    }"#,
                );
        })
        .await;

    let cfg = ReasoningConfig {
        provider: "anthropic".into(),
        api_base: mock.base_url(),
        api_key: "test-key".into(),
        model: "claude-3-5-sonnet-20241022".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    };
    let provider = AnthropicProvider::new(&cfg).unwrap();
    let resp = provider
        .summarize(summary_request())
        .await
        .expect("summarize must succeed on a well-formed mock response");

    assert!(resp.summary_text.starts_with("Buyer confirmed"));
    assert!(resp.suggested_next_step.contains("close the dispute"));
    assert!(resp.rationale.0.contains("aligned"));

    hit.assert_hits_async(1).await;
}

/// Malformed JSON inside the `content[0].text` block must surface as
/// `MalformedResponse` rather than a success. This guards the
/// "JSON mode without native JSON mode" strategy called out in the
/// issue — if the model ignored the system prompt and returned prose,
/// the adapter must NOT silently accept it.
#[tokio::test]
async fn classify_rejects_non_json_text_content() {
    let mock = MockServer::start_async().await;
    let _hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"msg_test",
                        "type":"message",
                        "role":"assistant",
                        "content":[{"type":"text","text":"I think this is a coordination failure."}]
                    }"#,
                );
        })
        .await;

    let cfg = ReasoningConfig {
        provider: "anthropic".into(),
        api_base: mock.base_url(),
        api_key: "k".into(),
        model: "claude-3-5-sonnet-20241022".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    };
    let provider = AnthropicProvider::new(&cfg).unwrap();
    let err = provider.classify(classification_request()).await.unwrap_err();
    assert!(
        matches!(err, ReasoningError::MalformedResponse(_)),
        "non-JSON prose must surface as MalformedResponse, got {err:?}"
    );
}

/// Regression for the routing table in `build_provider`: a fully
/// unknown provider name must still fail with `Error::Config`, NOT
/// silently coerce to anthropic or openai.
#[test]
fn build_provider_rejects_unknown_provider_name() {
    let cfg = ReasoningConfig {
        provider: "totally_made_up".into(),
        ..ReasoningConfig::default()
    };
    match build_provider(&cfg) {
        Err(Error::Config(msg)) => {
            assert!(
                msg.contains("totally_made_up"),
                "error should name the provider: {msg}"
            );
        }
        Err(other) => panic!("expected Error::Config, got {other}"),
        Ok(_) => panic!("unknown providers must not build"),
    }
}
