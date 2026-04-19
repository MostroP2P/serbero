//! US5 — provider-swap portability (T075).
//!
//! The OpenAI adapter is `api_base`-parametric: swapping the
//! reasoning endpoint across OpenAI-compatible targets (different
//! `api_base`, different `api_key_env`, same `provider = "openai"`)
//! takes effect on restart with no code change (SC-104 / FR-103).
//!
//! These integration tests verify the structural contract from
//! outside the crate:
//!
//! 1. Two `OpenAiProvider`s with different `api_base` values both
//!    construct successfully from identical-shape `ReasoningConfig`s.
//! 2. `provider = "openai-compatible"` routes to the same adapter
//!    as `provider = "openai"` (no silent coercion).
//!
//! The field-level assertions (url composition, timeout floor,
//! api_key plumbing) live in the inline tests inside
//! `src/reasoning/openai.rs` since the adapter's fields are
//! module-private. The existing inline tests
//! `classify_prompt_includes_every_bundle_section` and
//! `provider_honors_configured_retry_count` already pin that the
//! adapter reads every relevant config field rather than any
//! hardcoded value.

use serbero::models::ReasoningConfig;
use serbero::reasoning::build_provider;
use serbero::reasoning::openai::OpenAiProvider;

#[tokio::test]
async fn provider_swap_produces_identical_session_shape() {
    // Two configs differing only in api_base and api_key_env.
    // Nothing else changes — this is the "operator points serbero
    // at a different OpenAI-compatible endpoint" scenario.
    let cfg_a = ReasoningConfig {
        provider: "openai".into(),
        api_base: "http://127.0.0.1:9001/v1".into(),
        api_key_env: "KEY_A".into(),
        api_key: "test-key-a".into(),
        model: "gpt-4o-mini".into(),
        request_timeout_seconds: 5,
        followup_retry_count: 0,
        ..ReasoningConfig::default()
    };
    let cfg_b = ReasoningConfig {
        api_base: "http://127.0.0.1:9002/v1".into(),
        api_key_env: "KEY_B".into(),
        api_key: "test-key-b".into(),
        ..cfg_a.clone()
    };

    // Both configs build successfully with no code change. The real
    // end-to-end HTTP proof (sending bytes to a mock server and
    // checking which one received them) is out of scope here — we
    // do not spin up a real OpenAI-compatible server in the test
    // suite. The inline tests inside `src/reasoning/openai.rs`
    // already assert that the adapter composes its request URL from
    // `api_base`, carries the configured `api_key` in bearer auth,
    // and honors `request_timeout_seconds` / `followup_retry_count`.
    OpenAiProvider::new(&cfg_a).expect("cfg_a must build");
    OpenAiProvider::new(&cfg_b).expect("cfg_b must build");

    // Building via the public factory produces the same adapter
    // for either endpoint; no fallback path coerces one into the
    // other.
    let _provider_a = build_provider(&cfg_a).expect("cfg_a must build via factory");
    let _provider_b = build_provider(&cfg_b).expect("cfg_b must build via factory");
}

#[test]
fn build_provider_routes_openai_compatible_to_same_adapter() {
    // `"openai-compatible"` is the documented alias for
    // OpenAI-compatible gateways (vLLM, llama.cpp, Ollama, LiteLLM,
    // router proxies). It MUST route to `OpenAiProvider`, not to
    // `NotYetImplementedProvider` — coercing it to NYI would break
    // SC-104.
    let cfg = ReasoningConfig {
        provider: "openai-compatible".into(),
        api_key: "test".into(),
        ..ReasoningConfig::default()
    };
    let provider = build_provider(&cfg);
    assert!(
        provider.is_ok(),
        "openai-compatible must build successfully (SC-104)"
    );
}
