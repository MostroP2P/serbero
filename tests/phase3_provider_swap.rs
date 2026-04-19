//! US5 — provider-swap portability (T075).
//!
//! The OpenAI adapter is `api_base`-parametric: swapping the
//! reasoning endpoint across OpenAI-compatible targets (different
//! `api_base`, different `api_key_env`, same `provider = "openai"`)
//! takes effect on restart with no code change (SC-104 / FR-103).
//!
//! Why this file uses `httpmock`: the previous revision asserted
//! only that `OpenAiProvider::new` / `build_provider` returned `Ok`,
//! which passes identically for the OpenAI adapter AND for
//! `NotYetImplementedProvider` — a false positive for "the OpenAI
//! adapter is wired". The tests here stand up two lightweight HTTP
//! mock servers on distinct ports and verify that the provider
//! actually dispatches requests to the configured `api_base`. Only
//! an adapter that reads `api_base` at call time can satisfy both
//! mocks; a hardcoded-host adapter or the NYI stub never would.

use httpmock::prelude::*;
use httpmock::Method::POST;
use serbero::models::ReasoningConfig;
use serbero::reasoning::build_provider;
use serbero::reasoning::openai::OpenAiProvider;

/// Canned OpenAI-shaped response so `health_check`'s parse path
/// succeeds. Content is irrelevant — we only assert on the mock's
/// hit count, not the returned body.
const CANNED_OPENAI_RESPONSE: &str = r#"{
    "choices": [
        { "message": { "content": "pong" } }
    ]
}"#;

#[tokio::test]
async fn provider_swap_produces_identical_session_shape() {
    // Two mocks on distinct ports — the "different OpenAI-compatible
    // endpoint" scenario. Each mock is isolated and records its own
    // hit count.
    let mock_a = MockServer::start_async().await;
    let mock_b = MockServer::start_async().await;

    let hit_a = mock_a
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(CANNED_OPENAI_RESPONSE);
        })
        .await;
    let hit_b = mock_b
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(CANNED_OPENAI_RESPONSE);
        })
        .await;

    // Identical-shape configs differing only in api_base + the env
    // var name that would source the key.
    let cfg_a = ReasoningConfig {
        provider: "openai".into(),
        api_base: format!("{}/v1", mock_a.base_url()),
        api_key_env: "KEY_A".into(),
        api_key: "test-key-a".into(),
        model: "gpt-4o-mini".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    };
    let cfg_b = ReasoningConfig {
        api_base: format!("{}/v1", mock_b.base_url()),
        api_key_env: "KEY_B".into(),
        api_key: "test-key-b".into(),
        ..cfg_a.clone()
    };

    let provider_a = OpenAiProvider::new(&cfg_a).expect("cfg_a must build");
    let provider_b = OpenAiProvider::new(&cfg_b).expect("cfg_b must build");

    // Drive both adapters. `health_check` is the cheapest real HTTP
    // call — it sends a minimal chat completion request to
    // `{api_base}/chat/completions`. We do not need a full
    // cooperative mediation fixture to prove the portability
    // invariant: if the adapter reads `api_base` at call time, each
    // mock receives exactly one request. If the adapter were
    // hardcoded to a different host (or were NYI and never sent
    // HTTP), `hit_a` and `hit_b` would both remain zero.
    use serbero::reasoning::ReasoningProvider;
    provider_a
        .health_check()
        .await
        .expect("health_check against mock_a must succeed");
    provider_b
        .health_check()
        .await
        .expect("health_check against mock_b must succeed");

    hit_a.assert_hits_async(1).await;
    hit_b.assert_hits_async(1).await;

    // Cross-check: the adapter only ever hits its OWN configured
    // base. Swapping cfg_a's api_base for cfg_b's at runtime would
    // violate the portability contract — demonstrated negatively
    // here by confirming neither mock received the other's traffic.
    // (`assert_hits_async(1)` above is exact, so this is a sanity
    // reread rather than an additional assertion.)
    assert_eq!(
        hit_a.hits_async().await,
        1,
        "mock_a must see exactly one request on this swap"
    );
    assert_eq!(
        hit_b.hits_async().await,
        1,
        "mock_b must see exactly one request on this swap"
    );
}

#[tokio::test]
async fn build_provider_routes_openai_compatible_to_same_adapter() {
    // `"openai-compatible"` is the documented alias for
    // OpenAI-compatible gateways (vLLM, llama.cpp, Ollama, LiteLLM,
    // router proxies). It MUST route to `OpenAiProvider`, not to
    // `NotYetImplementedProvider` — coercing it to NYI would break
    // SC-104.
    //
    // We prove the routing by observing side effects: only the
    // OpenAI adapter sends HTTP. The NYI stub surfaces
    // "not yet implemented" without any network traffic at all.
    let mock = MockServer::start_async().await;
    let hit = mock
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(CANNED_OPENAI_RESPONSE);
        })
        .await;

    let cfg = ReasoningConfig {
        provider: "openai-compatible".into(),
        api_base: format!("{}/v1", mock.base_url()),
        api_key: "test".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    };
    let provider = build_provider(&cfg).expect("openai-compatible must build");

    // The NYI stub would return `ReasoningError::Unreachable(...)`
    // WITHOUT touching the mock. The OpenAI adapter actually dials
    // the configured URL.
    provider
        .health_check()
        .await
        .expect("openai-compatible must route to the OpenAI adapter and reach the mock");

    hit.assert_hits_async(1).await;
}
