use tracing::info;

use super::ReasoningProvider;
use crate::models::reasoning::ReasoningError;

/// Run the startup health check against the configured provider.
///
/// Returns the `ReasoningError` on failure so the caller (typically
/// `daemon::run`) can log it with provider / model / api-base context.
/// This function deliberately does NOT log the failure itself to avoid
/// double-logging — the caller owns the operator-facing message.
/// Phase 1/2 behavior MUST remain unaffected regardless of the
/// result (SC-105).
pub async fn run_startup_health_check(
    provider: &dyn ReasoningProvider,
) -> std::result::Result<(), ReasoningError> {
    let out = provider.health_check().await;
    if out.is_ok() {
        info!("reasoning provider health check ok");
    }
    out
}
