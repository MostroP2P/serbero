use tracing::{info, warn};

use super::ReasoningProvider;
use crate::models::reasoning::ReasoningError;

/// Run the startup health check against the configured provider.
///
/// Returns the `ReasoningError` on failure so the caller (typically
/// `daemon::run`) can log it and decide to leave Phase 3 disabled
/// for this run. Phase 1/2 behavior MUST remain unaffected
/// regardless of the result (SC-105).
pub async fn run_startup_health_check(
    provider: &dyn ReasoningProvider,
) -> std::result::Result<(), ReasoningError> {
    match provider.health_check().await {
        Ok(()) => {
            info!("reasoning provider health check ok");
            Ok(())
        }
        Err(e) => {
            warn!(error = %e, "reasoning provider health check failed; Phase 3 mediation will stay disabled this run");
            Err(e)
        }
    }
}
