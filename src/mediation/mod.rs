//! Mediation engine.
//!
//! US1 slice: only exposes `open_dispute_session` as the single
//! invocation point the integration tests drive. A full background
//! loop (periodic scan of Phase 2 for mediation-eligible disputes,
//! per-tick reasoning calls, inbound ingest) is US2+ and is
//! deliberately not wired here — the daemon does NOT spawn a
//! running engine task in this slice.

pub mod auth_retry;
pub mod escalation;
pub mod policy;
pub mod router;
pub mod session;
pub mod summarizer;

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::error::Result;
use crate::models::dispute::InitiatorRole;
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

/// Open a mediation session for one dispute. Thin wrapper over
/// `session::open_session` that fills in the timeouts the engine
/// uses today; kept as a separate entry point so the daemon and
/// tests do not have to know about the inner param shape.
#[allow(clippy::too_many_arguments)]
pub async fn open_dispute_session(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    serbero_keys: &Keys,
    mostro_pubkey: &PublicKey,
    reasoning: &dyn ReasoningProvider,
    prompt_bundle: &Arc<PromptBundle>,
    dispute_id: &str,
    initiator_role: InitiatorRole,
    dispute_uuid: Uuid,
) -> Result<session::OpenOutcome> {
    session::open_session(session::OpenSessionParams {
        conn,
        client,
        serbero_keys,
        mostro_pubkey,
        reasoning,
        prompt_bundle,
        dispute_id,
        initiator_role,
        dispute_uuid,
        take_flow_timeout: Duration::from_secs(15),
        take_flow_poll_interval: Duration::from_millis(250),
    })
    .await
}
