//! Mediation engine.
//!
//! US1 wiring:
//! - [`open_dispute_session`]: thin wrapper over
//!   [`session::open_session`] the integration tests drive directly.
//! - [`draft_and_send_initial_message`]: standalone drafter the
//!   engine loop (or US2+ follow-up paths) can call once a
//!   [`policy::PolicyDecision::AskClarification`] is in hand.
//!   Distinct from the inline drafting inside [`session::open_session`]
//!   on purpose: the session-open path already owns a single
//!   transaction that persists session + outbound atomically, whereas
//!   this helper is the entry point for follow-up / async flows
//!   where the session row already exists.
//! - [`run_engine`]: periodic background task the daemon spawns on
//!   startup (see [`crate::daemon`]). Scans Phase 2 `notified`
//!   disputes without an open mediation session and calls
//!   `session::open_session` for each. No per-error panic: the loop
//!   logs and continues so Phase 1/2 detection is never disturbed
//!   (SC-105).

pub mod auth_retry;
pub mod escalation;
pub mod policy;
pub mod router;
pub mod session;
pub mod summarizer;

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use rusqlite::params;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::chat::outbound;
use crate::db;
use crate::error::{Error, Result};
use crate::models::dispute::InitiatorRole;
use crate::models::mediation::TranscriptParty;
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

/// Engine tick cadence (US1). Hardcoded to 30 seconds per tasks.md
/// T040 — configurable knob is US2+ scope.
const ENGINE_TICK_INTERVAL: Duration = Duration::from_secs(30);

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

/// Draft and send the initial clarifying message to both parties.
///
/// Meant to be called once the policy layer has produced a
/// [`policy::PolicyDecision::AskClarification`]. Duplicates part of
/// [`session::open_session`]'s inline logic on purpose: the engine's
/// async follow-up path wants an entry point that starts from an
/// already-persisted session (by `session_id` + pre-derived shared
/// keys), not one that also runs the take-flow.
///
/// Contract:
/// - Builds per-party gift-wraps with role prefixes so the inner
///   event ids cannot collide on identical content.
/// - Persists both outbound rows + two `outbound_sent` audit events
///   in a single DB transaction; a crash between commit and publish
///   leaves the DB in a retriable state (the unique index on
///   `(session_id, inner_event_id)` makes a later retry idempotent).
/// - Bumps the session state to `awaiting_response` if it is not
///   already there. The invariant is that [`session::open_session`]
///   inserts the session directly at `awaiting_response`, so the
///   transition here is almost always a no-op — but keeping the
///   write makes the helper safe against callers whose flow inserted
///   the session at `opening` or `follow_up_pending` first.
/// - Publishes the two wraps with bounded retry AFTER the DB commit.
#[instrument(skip_all, fields(session_id = %session_id))]
#[allow(clippy::too_many_arguments)]
pub async fn draft_and_send_initial_message(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    serbero_keys: &Keys,
    session_id: &str,
    buyer_shared_keys: &Keys,
    seller_shared_keys: &Keys,
    prompt_bundle: &Arc<PromptBundle>,
    clarification_text: &str,
) -> Result<()> {
    let buyer_content = format!("Buyer: {}", clarification_text);
    let seller_content = format!("Seller: {}", clarification_text);

    let buyer_wrap = outbound::build_wrap(
        serbero_keys,
        &buyer_shared_keys.public_key(),
        &buyer_content,
    )
    .await?;
    let seller_wrap = outbound::build_wrap(
        serbero_keys,
        &seller_shared_keys.public_key(),
        &seller_content,
    )
    .await?;

    if buyer_wrap.inner_event_id == seller_wrap.inner_event_id {
        return Err(Error::ChatTransport(
            "inner event ids collided across parties; refusing to persist \
             rows that would violate the dedup invariant"
                .into(),
        ));
    }

    let buyer_shared_pubkey_hex = buyer_shared_keys.public_key().to_hex();
    let seller_shared_pubkey_hex = seller_shared_keys.public_key().to_hex();
    let now = current_ts_secs()?;

    // One transaction for both outbound rows + both audit events +
    // the (idempotent) session-state sync. Matches the outbox shape
    // used by `session::open_session`: DB commit first, then publish.
    {
        let mut guard = conn.lock().await;
        let tx = guard.transaction()?;
        db::mediation::insert_outbound_message(
            &tx,
            &db::mediation::NewOutboundMessage {
                session_id,
                party: TranscriptParty::Buyer,
                shared_pubkey: &buyer_shared_pubkey_hex,
                inner_event_id: &buyer_wrap.inner_event_id.to_hex(),
                inner_event_created_at: buyer_wrap.inner_created_at,
                outer_event_id: Some(&buyer_wrap.outer.id.to_hex()),
                content: &buyer_content,
                prompt_bundle_id: &prompt_bundle.id,
                policy_hash: &prompt_bundle.policy_hash,
                persisted_at: now,
            },
        )?;
        db::mediation::insert_outbound_message(
            &tx,
            &db::mediation::NewOutboundMessage {
                session_id,
                party: TranscriptParty::Seller,
                shared_pubkey: &seller_shared_pubkey_hex,
                inner_event_id: &seller_wrap.inner_event_id.to_hex(),
                inner_event_created_at: seller_wrap.inner_created_at,
                outer_event_id: Some(&seller_wrap.outer.id.to_hex()),
                content: &seller_content,
                prompt_bundle_id: &prompt_bundle.id,
                policy_hash: &prompt_bundle.policy_hash,
                persisted_at: now,
            },
        )?;
        db::mediation_events::record_outbound_sent(
            &tx,
            session_id,
            &buyer_shared_pubkey_hex,
            &buyer_wrap.inner_event_id.to_hex(),
            Some(&prompt_bundle.id),
            Some(&prompt_bundle.policy_hash),
            now,
        )?;
        db::mediation_events::record_outbound_sent(
            &tx,
            session_id,
            &seller_shared_pubkey_hex,
            &seller_wrap.inner_event_id.to_hex(),
            Some(&prompt_bundle.id),
            Some(&prompt_bundle.policy_hash),
            now,
        )?;
        // Set-if-not-already — unconditional UPDATE with equality in
        // the WHERE keeps this a safe no-op when the session is
        // already in `awaiting_response` (the common case after
        // `open_session`).
        tx.execute(
            "UPDATE mediation_sessions
             SET state = 'awaiting_response', last_transition_at = ?1
             WHERE session_id = ?2 AND state != 'awaiting_response'",
            params![now, session_id],
        )?;
        tx.commit()?;
    }

    session::publish_with_bounded_retry(client, &buyer_wrap.outer, "buyer").await?;
    session::publish_with_bounded_retry(client, &seller_wrap.outer, "seller").await?;

    info!(
        session_id = %session_id,
        prompt_bundle_id = %prompt_bundle.id,
        policy_hash = %prompt_bundle.policy_hash,
        "initial clarifying message dispatched to both parties"
    );
    Ok(())
}

/// Engine background task (T040).
///
/// Every [`ENGINE_TICK_INTERVAL`] seconds, scan Phase 2 disputes in
/// `lifecycle_state = 'notified'` that do not already carry an open
/// mediation session, and call [`session::open_session`] for each.
/// Each tick also yields to the tokio scheduler so a slow tick never
/// starves other tasks on the same runtime.
///
/// Resilience discipline:
/// - The loop NEVER panics: every error path logs and continues with
///   the next dispute.
/// - The tick interval is hardcoded for US1 (configurable is US2+).
/// - The engine owns no cached reasoning-health state for US1 — the
///   per-call gate inside `open_session` (T044) is the only check.
/// - Shutdown is not handled here: the daemon wraps the returned
///   future in a `tokio::select!` with its shutdown signal and
///   `abort()`s on shutdown. Keeping the function simple (no
///   shutdown channel parameter) mirrors the shape `renotif_handle`
///   uses today.
pub async fn run_engine(
    conn: Arc<AsyncMutex<rusqlite::Connection>>,
    client: Client,
    serbero_keys: Keys,
    mostro_pubkey: PublicKey,
    reasoning: Arc<dyn ReasoningProvider>,
    prompt_bundle: Arc<PromptBundle>,
) {
    info!(
        tick_seconds = ENGINE_TICK_INTERVAL.as_secs(),
        "mediation engine loop starting"
    );
    let mut ticker = tokio::time::interval(ENGINE_TICK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate first tick so we align to the cadence
    // rather than hammering the DB once on boot.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        if let Err(e) = run_engine_tick(
            &conn,
            &client,
            &serbero_keys,
            &mostro_pubkey,
            reasoning.as_ref(),
            &prompt_bundle,
        )
        .await
        {
            // run_engine_tick returns Err only on infrastructure
            // failures (DB lock poisoning, query builder errors) —
            // per-dispute failures are swallowed inside the tick.
            error!(error = %e, "mediation engine tick failed");
        }
    }
}

async fn run_engine_tick(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    serbero_keys: &Keys,
    mostro_pubkey: &PublicKey,
    reasoning: &dyn ReasoningProvider,
    prompt_bundle: &Arc<PromptBundle>,
) -> Result<()> {
    let eligible = list_eligible_disputes(conn).await?;
    if eligible.is_empty() {
        debug!("engine tick: no eligible disputes");
        return Ok(());
    }
    debug!(count = eligible.len(), "engine tick: eligible disputes");

    for eligible in eligible {
        let Eligible {
            dispute_id,
            initiator_role,
        } = &eligible;
        let dispute_uuid = match Uuid::parse_str(dispute_id) {
            Ok(u) => u,
            Err(e) => {
                warn!(
                    dispute_id = %dispute_id,
                    error = %e,
                    "engine tick: skipping dispute with non-UUID id"
                );
                continue;
            }
        };
        match session::open_session(session::OpenSessionParams {
            conn,
            client,
            serbero_keys,
            mostro_pubkey,
            reasoning,
            prompt_bundle,
            dispute_id,
            initiator_role: *initiator_role,
            dispute_uuid,
            take_flow_timeout: Duration::from_secs(15),
            take_flow_poll_interval: Duration::from_millis(250),
        })
        .await
        {
            Ok(session::OpenOutcome::Opened { session_id }) => {
                info!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    "engine opened new mediation session"
                );
            }
            Ok(session::OpenOutcome::AlreadyOpen { session_id }) => {
                debug!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    "engine tick: dispute already has an open mediation session"
                );
            }
            Ok(session::OpenOutcome::DeferredToLaterPhase) => {
                debug!(
                    dispute_id = %dispute_id,
                    "engine tick: dispute deferred (non-AskClarification suggestion)"
                );
            }
            Ok(session::OpenOutcome::RefusedReasoningUnavailable { reason }) => {
                warn!(
                    dispute_id = %dispute_id,
                    reason = %reason,
                    "engine tick: reasoning provider unavailable; skipping (SC-105)"
                );
            }
            Err(e) => {
                error!(
                    dispute_id = %dispute_id,
                    error = %e,
                    "engine tick: open_session failed; continuing with next dispute"
                );
            }
        }
    }
    Ok(())
}

struct Eligible {
    dispute_id: String,
    initiator_role: InitiatorRole,
}

/// Disputes in `lifecycle_state = 'notified'` with no live mediation
/// session. "Live" excludes `closed` and `escalation_recommended`
/// so a previously handed-off dispute does not block a fresh
/// attempt. Ordering is ascending by `event_timestamp` so the
/// oldest disputes get worked first.
async fn list_eligible_disputes(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
) -> Result<Vec<Eligible>> {
    use std::str::FromStr;

    let guard = conn.lock().await;
    let mut stmt = guard.prepare(
        "SELECT dispute_id, initiator_role
         FROM disputes d
         WHERE d.lifecycle_state = 'notified'
           AND NOT EXISTS (
               SELECT 1 FROM mediation_sessions s
               WHERE s.dispute_id = d.dispute_id
                 AND s.state NOT IN ('closed', 'escalation_recommended')
           )
         ORDER BY d.event_timestamp ASC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (dispute_id, initiator_role_s) = row?;
        let initiator_role = match InitiatorRole::from_str(&initiator_role_s) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    dispute_id = %dispute_id,
                    role = %initiator_role_s,
                    error = %e,
                    "engine tick: skipping dispute with unrecognised initiator_role"
                );
                continue;
            }
        };
        out.push(Eligible {
            dispute_id,
            initiator_role,
        });
    }
    Ok(out)
}

fn current_ts_secs() -> Result<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| Error::ChatTransport(format!("system clock is before UNIX_EPOCH: {e}")))
}
