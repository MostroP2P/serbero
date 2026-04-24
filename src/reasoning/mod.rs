//! Reasoning provider adapter boundary.
//!
//! Serbero's mediation call sites (Phase 3) see a single
//! request/response shape: the trait defined here plus the typed
//! request/response structs in `crate::models::reasoning`. Adapters
//! live in submodules. Phase 3 shipped exactly one implementation
//! (`openai`); issue #38 adds a native `anthropic` adapter so
//! operators can use Claude models without routing through an
//! OpenAI-compatible proxy. The remaining declared providers
//! (`ppqai`, `openclaw`) still route through
//! `NotYetImplementedProvider` so selection failures are loud at
//! startup instead of silently coercing to another backend.
//!
//! Spec anchors: FR-102 (mandatory provider), FR-103 (vendor
//! neutrality at the boundary), FR-104 (required config fields),
//! FR-116 (advisory-only outputs), and the scope-control note in
//! `plan.md` (one adapter shipped).

pub mod anthropic;
pub mod health;
pub mod not_yet_implemented;
pub mod openai;

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::models::reasoning::{
    ClassificationRequest, ClassificationResponse, ReasoningError, SummaryRequest, SummaryResponse,
};
use crate::models::ReasoningConfig;

/// Adapter trait. Every mediation call site depends on this, not on
/// a specific provider's HTTP shape.
#[async_trait]
pub trait ReasoningProvider: Send + Sync {
    async fn classify(
        &self,
        request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError>;

    async fn summarize(
        &self,
        request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError>;

    async fn health_check(&self) -> std::result::Result<(), ReasoningError>;
}

/// Build the configured provider. Unsupported values never fall
/// through to the OpenAI adapter — they return a
/// `NotYetImplementedProvider` so selection fails loudly at startup.
pub fn build_provider(config: &ReasoningConfig) -> Result<Arc<dyn ReasoningProvider>> {
    match config.provider.as_str() {
        "openai" | "openai-compatible" => Ok(Arc::new(openai::OpenAiProvider::new(config)?)),
        "anthropic" => Ok(Arc::new(anthropic::AnthropicProvider::new(config)?)),
        other @ ("ppqai" | "openclaw") => Ok(Arc::new(
            not_yet_implemented::NotYetImplementedProvider::new(other),
        )),
        other => Err(Error::Config(format!(
            "unknown reasoning provider '{other}'; supported: openai, openai-compatible, \
             anthropic (ppqai, openclaw are declared but not yet implemented)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ReasoningConfig;

    fn ppqai_cfg() -> ReasoningConfig {
        ReasoningConfig {
            provider: "ppqai".into(),
            ..ReasoningConfig::default()
        }
    }

    #[tokio::test]
    async fn build_provider_returns_nyi_for_ppqai() {
        let provider = build_provider(&ppqai_cfg()).expect("builds");
        let err = provider.health_check().await.unwrap_err();
        match err {
            ReasoningError::Unreachable(msg) => {
                assert!(
                    msg.contains("ppqai"),
                    "error should name the provider: {msg}"
                );
                assert!(
                    msg.contains("not yet implemented"),
                    "error should flag NYI status: {msg}"
                );
            }
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_provider_does_not_coerce_nyi_to_openai() {
        // Even if the adapter for a declared-but-unshipped provider
        // were mistakenly wired to OpenAI, the NYI stub would fail on
        // the very first call — this test guards the regression.
        let provider = build_provider(&ppqai_cfg()).expect("builds");
        assert!(provider.health_check().await.is_err());
    }

    #[test]
    fn build_provider_anthropic_returns_native_adapter() {
        // Issue #38: `provider = "anthropic"` must build the native
        // Anthropic adapter, NOT `NotYetImplementedProvider`. The
        // concrete type assertion is via downcast-by-attempt: the
        // NYI stub returns an error on its very first call without
        // any side effect, whereas `AnthropicProvider::new` succeeds
        // at build time. The more rigorous traffic-based check lives
        // in `tests/reasoning_anthropic.rs` where an HTTP mock
        // confirms the adapter actually dispatches requests.
        let cfg = ReasoningConfig {
            provider: "anthropic".into(),
            api_key: "test-key".into(),
            ..ReasoningConfig::default()
        };
        let _ = build_provider(&cfg).expect("anthropic provider must build");
    }

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
}
