use async_trait::async_trait;

use super::ReasoningProvider;
use crate::models::reasoning::{
    ClassificationRequest, ClassificationResponse, ReasoningError, SummaryRequest, SummaryResponse,
};

/// Adapter stub for providers declared at the boundary but not yet
/// implemented in Phase 3 (`anthropic`, `ppqai`, `openclaw`).
///
/// Selecting one of these in `config.toml` surfaces an actionable
/// error at the first call site (health check, classify, or
/// summarize) instead of silently coercing to OpenAI.
pub struct NotYetImplementedProvider {
    provider_name: String,
}

impl NotYetImplementedProvider {
    pub fn new(provider_name: &str) -> Self {
        Self {
            provider_name: provider_name.to_string(),
        }
    }

    fn err<T>(&self) -> std::result::Result<T, ReasoningError> {
        Err(ReasoningError::Unreachable(format!(
            "reasoning provider '{name}' is declared at the Phase 3 boundary but not yet \
             implemented; currently shipped providers: openai, openai-compatible. \
             Landing {name} adapter support is tracked as future work beyond Phase 3.",
            name = self.provider_name
        )))
    }
}

#[async_trait]
impl ReasoningProvider for NotYetImplementedProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        self.err()
    }

    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        self.err()
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        self.err()
    }
}
