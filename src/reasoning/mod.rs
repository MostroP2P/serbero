//! Reasoning provider adapter boundary.
//!
//! Serbero's mediation call sites (Phase 3) see a single
//! request/response shape: the trait defined here plus the typed
//! request/response structs in `crate::models::reasoning`. Adapters
//! live in submodules. Phase 3 ships exactly one implementation
//! (`openai`); all other providers route through
//! `NotYetImplementedProvider` so selection failures are loud at
//! startup instead of silently coercing to OpenAI.
//!
//! Spec anchors: FR-102 (mandatory provider), FR-103 (vendor
//! neutrality at the boundary), FR-104 (required config fields),
//! FR-116 (advisory-only outputs), and the scope-control note in
//! `plan.md` (one adapter shipped).

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
        other @ ("anthropic" | "ppqai" | "openclaw") => Ok(Arc::new(
            not_yet_implemented::NotYetImplementedProvider::new(other),
        )),
        other => Err(Error::Config(format!(
            "unknown reasoning provider '{other}'; supported: openai, openai-compatible \
             (anthropic, ppqai, openclaw are declared but not yet implemented in Phase 3)"
        ))),
    }
}
