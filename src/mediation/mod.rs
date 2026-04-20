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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use rusqlite::params;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::chat::dispute_chat_flow::{self, DisputeChatMaterial};
use crate::chat::inbound::{self, PartyChatMaterial};
use crate::chat::outbound;
use crate::db;
use crate::db::mediation_events::MediationEventKind;
use crate::error::{Error, Result};
use crate::models::dispute::InitiatorRole;
use crate::models::mediation::{
    ClassificationLabel, EscalationTrigger, MediationSessionState, TranscriptParty,
};
use crate::models::reasoning::TranscriptEntry;
use crate::models::{MediationConfig, NotificationStatus, NotificationType, SolverConfig};
use crate::nostr::notifier::send_gift_wrap_notification;
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

/// Process-local cache of per-session [`DisputeChatMaterial`].
///
/// The ECDH shared-key secret is not persisted (see
/// `chat::dispute_chat_flow` key-lifecycle doc), so the engine keeps
/// the material in memory for as long as the session is live.
/// `run_engine` owns one `Arc<…>` and clones it into both the
/// session-open path (which inserts on success) and the ingest
/// tick (which reads).
pub type SessionKeyCache = Arc<AsyncMutex<HashMap<String, DisputeChatMaterial>>>;

/// Fetch budget used by [`run_ingest_tick`] for each party's relay
/// query. Kept short so a single slow fetch cannot stall the tick
/// for every other live session on the same cycle.
const INGEST_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Engine tick cadence (US1). Hardcoded to 30 seconds per tasks.md
/// T040 — configurable knob is US2+ scope.
const ENGINE_TICK_INTERVAL: Duration = Duration::from_secs(30);

/// Open a mediation session for one dispute. Thin wrapper over
/// `session::open_session` that fills in the timeouts the engine
/// uses today; kept as a separate entry point so the daemon and
/// tests do not have to know about the inner param shape.
///
/// `provider_name` and `model_name` are threaded through to the
/// audit store (`reasoning_rationales` rows) — the adapter trait
/// itself does not expose them, so the caller (daemon config or
/// integration test) supplies them explicitly.
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
    provider_name: &str,
    model_name: &str,
    auth_handle: &auth_retry::AuthRetryHandle,
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
        provider_name,
        model_name,
        auth_handle,
        // This wrapper is the integration-test entry point; no
        // ingest tick runs alongside, so no cache to register the
        // material in.
        session_key_cache: None,
        // No solver fan-out from this wrapper either — the test
        // harness manages its own recipient set.
        solvers: &[],
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
/// - Persists both outbound rows + the idempotent session-state
///   sync in a single DB transaction (transactional outbox: a
///   crash between commit and publish leaves the rows in place so a
///   later retry can republish, and the unique index on
///   `(session_id, inner_event_id)` keeps the table single-copy).
/// - Publishes each wrap with bounded retry. The
///   `outbound_sent` audit event is emitted AFTER its publish
///   succeeds — a failed publish therefore does not produce a
///   false "sent" entry in `mediation_events`. If a publish fails
///   we surface the error and let the engine decide to escalate on
///   a later tick.
/// - Bumps the session state to `awaiting_response` if it is not
///   already there. The invariant is that [`session::open_session`]
///   inserts the session directly at `awaiting_response`, so the
///   transition here is almost always a no-op — but keeping the
///   write makes the helper safe against callers whose flow inserted
///   the session at `opening` or `follow_up_pending` first.
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

    // SC-107: addresses shared pubkey, not primary — `buyer_shared_keys`
    // / `seller_shared_keys` are the ECDH-derived per-trade keys
    // surfaced via the Mostro key-material adapter; the parties'
    // primary pubkeys never appear as recipients on outbound mediation
    // wraps.
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
    let buyer_inner_id_hex = buyer_wrap.inner_event_id.to_hex();
    let seller_inner_id_hex = seller_wrap.inner_event_id.to_hex();
    let now = current_ts_secs()?;

    // One transaction for both outbound rows + the session-state
    // sync. Audit events (outbound_sent) are deferred to post-publish
    // so the audit log only claims "sent" when the relay actually
    // accepted the wrap.
    {
        let mut guard = conn.lock().await;
        let tx = guard.transaction()?;
        db::mediation::insert_outbound_message(
            &tx,
            &db::mediation::NewOutboundMessage {
                session_id,
                party: TranscriptParty::Buyer,
                shared_pubkey: &buyer_shared_pubkey_hex,
                inner_event_id: &buyer_inner_id_hex,
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
                inner_event_id: &seller_inner_id_hex,
                inner_event_created_at: seller_wrap.inner_created_at,
                outer_event_id: Some(&seller_wrap.outer.id.to_hex()),
                content: &seller_content,
                prompt_bundle_id: &prompt_bundle.id,
                policy_hash: &prompt_bundle.policy_hash,
                persisted_at: now,
            },
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

    // Publish first, THEN audit. A failed publish bubbles up; the
    // `mediation_messages` row is already persisted so a later
    // reconciliation pass can re-publish without duplicating the
    // table row (unique index on `(session_id, inner_event_id)`).
    session::publish_with_bounded_retry(client, &buyer_wrap.outer, "buyer").await?;
    record_outbound_sent_audit(
        conn,
        session_id,
        &buyer_shared_pubkey_hex,
        &buyer_inner_id_hex,
        prompt_bundle,
    )
    .await?;

    session::publish_with_bounded_retry(client, &seller_wrap.outer, "seller").await?;
    record_outbound_sent_audit(
        conn,
        session_id,
        &seller_shared_pubkey_hex,
        &seller_inner_id_hex,
        prompt_bundle,
    )
    .await?;

    info!(
        session_id = %session_id,
        prompt_bundle_id = %prompt_bundle.id,
        policy_hash = %prompt_bundle.policy_hash,
        "initial clarifying message dispatched to both parties"
    );
    Ok(())
}

/// Record one `outbound_sent` audit row in its own short-lived
/// transaction. Separate from the main outbound-persist tx because
/// the row should only land once the relay has accepted the wrap.
async fn record_outbound_sent_audit(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    shared_pubkey_hex: &str,
    inner_event_id_hex: &str,
    prompt_bundle: &Arc<PromptBundle>,
) -> Result<()> {
    let now = current_ts_secs()?;
    let guard = conn.lock().await;
    db::mediation_events::record_outbound_sent(
        &guard,
        session_id,
        shared_pubkey_hex,
        inner_event_id_hex,
        Some(&prompt_bundle.id),
        Some(&prompt_bundle.policy_hash),
        now,
    )?;
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
#[allow(clippy::too_many_arguments)]
pub async fn run_engine(
    conn: Arc<AsyncMutex<rusqlite::Connection>>,
    client: Client,
    serbero_keys: Keys,
    mostro_pubkey: PublicKey,
    reasoning: Arc<dyn ReasoningProvider>,
    prompt_bundle: Arc<PromptBundle>,
    provider_name: String,
    model_name: String,
    auth_handle: auth_retry::AuthRetryHandle,
    solvers: Vec<SolverConfig>,
    mediation_cfg: MediationConfig,
) {
    info!(
        tick_seconds = ENGINE_TICK_INTERVAL.as_secs(),
        provider = %provider_name,
        model = %model_name,
        "mediation engine loop starting"
    );

    // Process-local session-key cache. Populated on session-open
    // success by `session::open_session` and (best-effort) by the
    // T052 startup-resume pass below.
    let session_key_cache: SessionKeyCache = Arc::new(AsyncMutex::new(HashMap::new()));

    // T052 — restart-resume. On engine startup, walk every
    // non-terminal session and attempt to rebuild the in-memory
    // chat material. Any DB failure here is logged and ignored so
    // the engine still starts its tick loop.
    if let Err(e) = startup_resume_pass(&conn, &prompt_bundle, &session_key_cache).await {
        error!(error = %e, "mediation engine startup-resume pass failed");
    }

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
            &provider_name,
            &model_name,
            &auth_handle,
            &session_key_cache,
            &solvers,
        )
        .await
        {
            // run_engine_tick returns Err only on infrastructure
            // failures (DB lock poisoning, query builder errors) —
            // per-dispute failures are swallowed inside the tick.
            error!(error = %e, "mediation engine tick failed");
        }
        // Ingest tick follows the session-open tick. Both `run_engine_tick`'s
        // `session::open_session` commit (which inserts the
        // `mediation_sessions` row before returning Opened) and the
        // cache registration inside `open_session` happen BEFORE we
        // reach this line, so a session opened in the current cycle
        // is visible to `list_live_sessions` AND has its
        // `DisputeChatMaterial` in the cache on this same tick. In
        // practice the party has not had time to publish a reply yet,
        // so `fetch_inbound` for that freshly-opened session is a
        // cheap no-op — but the session is not hidden for an extra
        // 30 s cycle.
        if let Err(e) = run_ingest_tick(
            &conn,
            &client,
            &session_key_cache,
            &prompt_bundle,
            &mediation_cfg,
            &solvers,
        )
        .await
        {
            error!(error = %e, "mediation ingest tick failed");
        }
    }
}

/// T052 startup-resume pass. Walks every non-terminal session and
/// attempts to repopulate the in-memory chat-material cache.
///
/// Three outcomes per session:
/// 1. [`dispute_chat_flow::load_chat_keys_for_session`] returns
///    `Ok(material)` — insert into the cache and carry on. This is
///    the future-extension happy path; US2 always lands on (2) or (3).
/// 2. `Err` + session's `policy_hash` equals the currently-loaded
///    bundle's `policy_hash` — the session stays alive at its
///    current state. The ingest tick will skip it gracefully (no
///    cache entry → `debug!` skip per T051) until a future slice
///    re-runs the take-flow. Emit one `info!` per session.
/// 3. `Err` + `policy_hash` mismatch — the pinned bundle is gone.
///    Transition the session to `escalation_recommended` with
///    trigger `policy_bundle_missing` and record a
///    `mediation_events` row so the operator can investigate.
///    Emit one `error!` per session.
async fn startup_resume_pass(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    prompt_bundle: &Arc<PromptBundle>,
    session_key_cache: &SessionKeyCache,
) -> Result<()> {
    let sessions = {
        let guard = conn.lock().await;
        db::mediation::list_live_sessions(&guard)?
    };
    if sessions.is_empty() {
        debug!("startup resume: no live sessions");
        return Ok(());
    }
    info!(
        count = sessions.len(),
        "startup resume: attempting to repopulate session-key cache"
    );

    for s in sessions {
        let (bsp, ssp) = match (
            s.buyer_shared_pubkey.as_deref(),
            s.seller_shared_pubkey.as_deref(),
        ) {
            (Some(b), Some(se)) => (b, se),
            _ => {
                warn!(
                    session_id = %s.session_id,
                    "startup resume: session missing shared pubkey columns; skipping"
                );
                continue;
            }
        };
        match dispute_chat_flow::load_chat_keys_for_session(bsp, ssp) {
            Ok(material) => {
                let mut guard = session_key_cache.lock().await;
                guard.insert(s.session_id.clone(), material);
                info!(
                    session_id = %s.session_id,
                    "startup resume: session material restored into cache"
                );
            }
            Err(e) => {
                if s.policy_hash == prompt_bundle.policy_hash {
                    info!(
                        session_id = %s.session_id,
                        policy_hash = %s.policy_hash,
                        error = %e,
                        "startup resume: key material unavailable but pinned bundle matches; \
                         session stays alive (ingest tick will skip until re-derivation)"
                    );
                } else {
                    handle_policy_bundle_missing(
                        conn,
                        &s.session_id,
                        &s.policy_hash,
                        &prompt_bundle.policy_hash,
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}

async fn handle_policy_bundle_missing(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    pinned_hash: &str,
    loaded_hash: &str,
) {
    let now = match current_ts_secs() {
        Ok(t) => t,
        Err(e) => {
            error!(error = %e, "startup resume: refusing to escalate with invalid clock");
            return;
        }
    };
    let payload = json!({
        "trigger": "policy_bundle_missing",
        "session_id": session_id,
        "pinned_hash": pinned_hash,
        "loaded_hash": loaded_hash,
    })
    .to_string();

    // Wrap the state flip + audit insert in a single transaction so
    // the two writes cannot get out of sync. If either fails the
    // whole thing rolls back and the session stays at its previous
    // state — a retry on a subsequent startup / tick will see the
    // same mismatch and retry atomically.
    let mut guard = conn.lock().await;
    let tx = match guard.transaction() {
        Ok(tx) => tx,
        Err(e) => {
            error!(
                session_id = %session_id,
                error = %e,
                "startup resume: failed to open escalation transaction"
            );
            return;
        }
    };
    if let Err(e) = db::mediation::set_session_state(
        &tx,
        session_id,
        MediationSessionState::EscalationRecommended,
        now,
    ) {
        error!(
            session_id = %session_id,
            error = %e,
            "startup resume: set_session_state failed (transaction will roll back)"
        );
        return;
    }
    if let Err(e) = db::mediation_events::record_event(
        &tx,
        MediationEventKind::EscalationRecommended,
        Some(session_id),
        &payload,
        None,
        None,
        Some(pinned_hash),
        now,
    ) {
        error!(
            session_id = %session_id,
            error = %e,
            "startup resume: record_event failed (transaction will roll back)"
        );
        return;
    }
    if let Err(e) = tx.commit() {
        error!(
            session_id = %session_id,
            error = %e,
            "startup resume: escalation transaction commit failed"
        );
        return;
    }
    error!(
        session_id = %session_id,
        pinned_hash = %pinned_hash,
        loaded_hash = %loaded_hash,
        "startup resume: pinned prompt bundle missing; session escalated (policy_bundle_missing)"
    );
}

#[allow(clippy::too_many_arguments)]
async fn run_engine_tick(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    serbero_keys: &Keys,
    mostro_pubkey: &PublicKey,
    reasoning: &dyn ReasoningProvider,
    prompt_bundle: &Arc<PromptBundle>,
    provider_name: &str,
    model_name: &str,
    auth_handle: &auth_retry::AuthRetryHandle,
    session_key_cache: &SessionKeyCache,
    solvers: &[SolverConfig],
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
            provider_name,
            model_name,
            auth_handle,
            session_key_cache: Some(session_key_cache),
            solvers,
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
            Ok(session::OpenOutcome::ReadyForSummary {
                session_id,
                classification,
                confidence,
            }) => {
                info!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    classification = %classification,
                    confidence,
                    "engine: session opened in cooperative-summary mode; delivering summary"
                );
                // Transcript is empty on the opening call — that is
                // the documented US3 shape when the classifier
                // returns `Summarize` on an empty history (see PR
                // description: cooperative summary on open-time has
                // no prior transcript).
                if let Err(e) = deliver_summary(
                    conn,
                    client,
                    serbero_keys,
                    &session_id,
                    dispute_id,
                    classification,
                    confidence,
                    Vec::new(),
                    prompt_bundle,
                    reasoning,
                    solvers,
                    provider_name,
                    model_name,
                )
                .await
                {
                    error!(
                        session_id = %session_id,
                        error = %e,
                        "engine: deliver_summary failed; session left mid-pipeline"
                    );
                }
            }
            Ok(session::OpenOutcome::AlreadyOpen { session_id }) => {
                debug!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    "engine tick: dispute already has an open mediation session"
                );
            }
            Ok(session::OpenOutcome::EscalatedOnOpen {
                session_id,
                trigger,
            }) => {
                warn!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    trigger = %trigger,
                    "session escalated on open"
                );
                match escalation::recommend(escalation::RecommendParams {
                    conn,
                    session_id: &session_id,
                    trigger,
                    evidence_refs: Vec::new(),
                    rationale_refs: Vec::new(),
                    prompt_bundle_id: &prompt_bundle.id,
                    policy_hash: &prompt_bundle.policy_hash,
                })
                .await
                {
                    Ok(()) => {
                        notify_solvers_escalation(
                            conn,
                            client,
                            solvers,
                            dispute_id,
                            &session_id,
                            trigger,
                        )
                        .await;
                    }
                    Err(e) => {
                        error!(
                            session_id = %session_id,
                            error = %e,
                            "engine: escalation::recommend failed for EscalatedOnOpen"
                        );
                    }
                }
            }
            Ok(session::OpenOutcome::RefusedReasoningUnavailable { reason }) => {
                warn!(
                    dispute_id = %dispute_id,
                    reason = %reason,
                    "engine tick: reasoning provider unavailable; skipping (SC-105)"
                );
            }
            Ok(session::OpenOutcome::RefusedAuthPending { reason }) => {
                warn!(
                    dispute_id = %dispute_id,
                    reason = %reason,
                    "engine tick: auth pending; skipping dispute (SC-105)"
                );
            }
            Ok(session::OpenOutcome::RefusedAuthTerminated { reason }) => {
                error!(
                    dispute_id = %dispute_id,
                    reason = %reason,
                    "engine tick: auth terminated; skipping dispute (SC-105)"
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

/// Deliver a cooperative summary for a just-opened session (T060).
///
/// State machine: `classified → summary_pending → summary_delivered
/// → closed`. Each transition is written with
/// [`db::mediation::set_session_state`] (which `debug_assert!`s
/// legality in debug builds).
///
/// Failure handling:
/// - `summarizer::summarize` returning `Error::PolicyViolation(_)` →
///   flip the session to `escalation_recommended` and record an
///   `EscalationRecommended` audit row carrying trigger
///   `authority_boundary_attempt`. Return `Ok(())` so the engine
///   loop continues — the escalation *is* the intended outcome.
/// - Any other error → escalate with trigger `reasoning_unavailable`.
///   Same semantics: `Ok(())` return to keep the engine running.
/// - A failure to flip state to `escalation_recommended` itself is
///   logged at `error!` and bubbles up as an `Err` so the tick
///   surfaces the DB-level problem.
///
/// Recipient routing goes through [`router::resolve_recipients`] so
/// the rule stays in one place. Per-recipient send failures do NOT
/// abort the tick: the `notifications` row is written with status
/// `Failed` and delivery continues for the other recipients.
#[instrument(
    skip_all,
    fields(session_id = %session_id, dispute_id = %dispute_id)
)]
#[allow(clippy::too_many_arguments)]
pub async fn deliver_summary(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    // TODO(US4): `serbero_keys` is unused on the summary path —
    // `send_gift_wrap_notification` signs via the `Client`'s
    // internal signer (which is already keyed by the same secret).
    // US4's escalation-handoff path is expected to build a
    // structured handoff package signed directly with these keys
    // (outside the nostr_sdk client scope), so the parameter is
    // retained to keep the engine-side call site stable when US4
    // lands. Drop the `_` prefix then.
    _serbero_keys: &Keys,
    session_id: &str,
    dispute_id: &str,
    classification: ClassificationLabel,
    confidence: f64,
    transcript: Vec<TranscriptEntry>,
    prompt_bundle: &Arc<PromptBundle>,
    reasoning: &dyn ReasoningProvider,
    solvers: &[SolverConfig],
    provider_name: &str,
    model_name: &str,
) -> Result<()> {
    // (1) Transition `classified → summary_pending`.
    transition_session(
        conn,
        session_id,
        MediationSessionState::SummaryPending,
        current_ts_secs()?,
    )
    .await?;

    // (2) Call the summarizer. Two short-circuit error paths map to
    //     escalation; everything else returns `Ok(())` to keep the
    //     engine running.
    let summary = match summarizer::summarize(summarizer::SummarizeParams {
        conn,
        session_id,
        dispute_id,
        classification,
        confidence,
        transcript,
        prompt_bundle,
        reasoning,
        provider_name,
        model_name,
    })
    .await
    {
        Ok(s) => s,
        Err(Error::PolicyViolation(msg)) => {
            warn!(
                session_id = %session_id,
                reason = %msg,
                "deliver_summary: authority-boundary attempt in summary; escalating"
            );
            escalate_from_summary_path(
                conn,
                session_id,
                prompt_bundle,
                EscalationTrigger::AuthorityBoundaryAttempt,
                &msg,
            )
            .await?;
            return Ok(());
        }
        Err(e) => {
            warn!(
                session_id = %session_id,
                error = %e,
                "deliver_summary: summarizer failed; escalating as reasoning_unavailable"
            );
            escalate_from_summary_path(
                conn,
                session_id,
                prompt_bundle,
                EscalationTrigger::ReasoningUnavailable,
                &e.to_string(),
            )
            .await?;
            return Ok(());
        }
    };

    // (3) Read `disputes.assigned_solver` fresh — the value can have
    //     changed since the session was opened (a human solver may
    //     have taken the dispute mid-mediation).
    //
    //     Three distinct outcomes we must NOT conflate:
    //     - `Ok(Some(pk))`: a solver is explicitly assigned → try targeted.
    //     - `Ok(None)`: the dispute row exists but `assigned_solver`
    //       column is NULL → broadcast.
    //     - `Err(QueryReturnedNoRows)`: the dispute row itself is
    //       missing. This is a real bug (the session row's FK on
    //       `dispute_id` should prevent it), but if it ever happens
    //       we surface an error rather than silently broadcasting.
    //     - Any other `Err`: DB failure → surface; the caller will
    //       log and retry on a later tick.
    let assigned_solver: Option<String> = {
        let guard = conn.lock().await;
        match guard.query_row(
            "SELECT assigned_solver FROM disputes WHERE dispute_id = ?1",
            params![dispute_id],
            |r| r.get::<_, Option<String>>(0),
        ) {
            Ok(opt) => opt,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(Error::InvalidEvent(format!(
                    "deliver_summary: dispute row missing for dispute_id={dispute_id}; \
                     refusing to broadcast without a valid parent row"
                )));
            }
            Err(e) => {
                return Err(Error::Db(e));
            }
        }
    };

    // (4) Route.
    let recipients = router::resolve_recipients(solvers, assigned_solver.as_deref());
    let recipient_list: Vec<String> = match recipients {
        router::Recipients::Targeted(pk) => vec![pk],
        router::Recipients::Broadcast(v) => v,
    };
    if recipient_list.is_empty() {
        // No configured recipients → the summary cannot be
        // delivered. The summarizer + `mediation_summaries` row
        // already landed (so the rationale is preserved), but
        // the session MUST NOT be marked `summary_delivered` —
        // that would be a lie in the audit log. Leaving it at
        // `summary_pending` forever is also wrong: a human
        // operator would have to notice and escalate manually.
        // Instead, escalate automatically with a dedicated
        // trigger so the operator alert path handles it the same
        // way it handles every other "needs human attention"
        // outcome (US4).
        warn!(
            session_id = %session_id,
            "deliver_summary: no solver recipients configured; escalating (notification_failed)"
        );
        escalate_from_summary_path(
            conn,
            session_id,
            prompt_bundle,
            EscalationTrigger::NotificationFailed,
            "no solver recipients configured",
        )
        .await?;
        return Ok(());
    }

    // (5) Per-recipient send + notification row + tracing.
    //
    // The DM body concatenates `summary_text` and
    // `suggested_next_step` so the solver sees both the narrative
    // recap and the actionable recommendation in a single wrap.
    // Separator is a blank line — renders cleanly in most Nostr
    // clients and keeps the two fields visually distinct.
    let dm_body = format!(
        "{}\n\nSuggested next step: {}",
        summary.summary_text, summary.suggested_next_step
    );
    let mut any_sent = false;
    for pk_hex in &recipient_list {
        // SC-107: addresses solver pubkey, not party pubkey — the
        // recipients here come from the configured `[solvers]` list (or
        // the `disputes.assigned_solver` row) and are operator pubkeys,
        // never party primary or party shared pubkeys.
        let sent_at = current_ts_secs()?;
        let (status, error_message) = match PublicKey::parse(pk_hex) {
            Ok(pk) => match send_gift_wrap_notification(client, &pk, &dm_body).await {
                Ok(()) => {
                    info!(
                        session_id = %session_id,
                        solver_pubkey = %pk_hex,
                        rationale_id = %summary.rationale_id,
                        "solver_summary_delivered"
                    );
                    any_sent = true;
                    (NotificationStatus::Sent, None)
                }
                Err(e) => {
                    warn!(
                        session_id = %session_id,
                        solver_pubkey = %pk_hex,
                        error = %e,
                        "deliver_summary: notifier send failed; recording Failed notification row"
                    );
                    (NotificationStatus::Failed, Some(e.to_string()))
                }
            },
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    solver_pubkey = %pk_hex,
                    error = %e,
                    "deliver_summary: recipient pubkey parse failed; recording Failed notification row"
                );
                (
                    NotificationStatus::Failed,
                    Some(format!("invalid pubkey: {e}")),
                )
            }
        };
        let guard = conn.lock().await;
        db::notifications::record_notification_logged(
            &guard,
            dispute_id,
            pk_hex,
            sent_at,
            status,
            error_message.as_deref(),
            NotificationType::MediationSummary,
        );
    }

    // (6) `summary_pending → summary_delivered → closed`, only if
    //     at least one recipient accepted the DM. Otherwise the
    //     session is escalated the same way as the no-recipients
    //     branch above — a persisted-but-undelivered summary
    //     needs human attention, not an indefinite
    //     `summary_pending` state.
    if !any_sent {
        warn!(
            session_id = %session_id,
            recipients = recipient_list.len(),
            "deliver_summary: all recipient sends failed; escalating (notification_failed)"
        );
        escalate_from_summary_path(
            conn,
            session_id,
            prompt_bundle,
            EscalationTrigger::NotificationFailed,
            "all recipient sends failed",
        )
        .await?;
        return Ok(());
    }
    let now = current_ts_secs()?;
    transition_session(
        conn,
        session_id,
        MediationSessionState::SummaryDelivered,
        now,
    )
    .await?;
    transition_session(conn, session_id, MediationSessionState::Closed, now).await?;

    Ok(())
}

/// T072 — deliver the "needs human judgment" gift-wrap DM to the
/// configured solver(s) after a session has been escalated.
///
/// Must be called AFTER [`escalation::recommend`] returns `Ok(())`
/// — that way the DB-side state flip + audit rows are durable
/// before we tell a human about the handoff. Per-recipient send
/// failures are recorded as `NotificationStatus::Failed`
/// notification rows; the function never returns `Err` because a
/// single flaky relay must not abort the surrounding engine tick.
///
/// Recipient resolution goes through
/// [`router::resolve_recipients`] so the routing rule stays in one
/// place for both summary and escalation paths.
pub(crate) async fn notify_solvers_escalation(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    solvers: &[SolverConfig],
    dispute_id: &str,
    session_id: &str,
    trigger: EscalationTrigger,
) {
    // Separate three distinct outcomes of the assigned-solver lookup.
    // The old `.ok().flatten()` collapsed DB errors and missing rows
    // into `None`, which then silently broadcast to every configured
    // solver — wrong: a DB error must not change routing, and a
    // missing dispute row is a FK-invariant bug we refuse to paper
    // over. This mirrors the pattern already used in `deliver_summary`.
    let assigned_solver: Option<String> = {
        let guard = conn.lock().await;
        match guard.query_row(
            "SELECT assigned_solver FROM disputes WHERE dispute_id = ?1",
            rusqlite::params![dispute_id],
            |r| r.get::<_, Option<String>>(0),
        ) {
            Ok(opt) => opt,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                warn!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    trigger = %trigger,
                    "notify_solvers_escalation: dispute row missing; \
                     refusing to broadcast without a valid parent row"
                );
                return;
            }
            Err(e) => {
                warn!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    trigger = %trigger,
                    error = %e,
                    "notify_solvers_escalation: assigned_solver lookup failed; \
                     skipping notification to avoid unsafe broadcast"
                );
                return;
            }
        }
    };
    let recipients = router::resolve_recipients(solvers, assigned_solver.as_deref());
    let recipient_list: Vec<String> = match recipients {
        router::Recipients::Targeted(pk) => vec![pk],
        router::Recipients::Broadcast(v) => v,
    };
    if recipient_list.is_empty() {
        warn!(
            dispute_id = %dispute_id,
            session_id = %session_id,
            trigger = %trigger,
            "notify_solvers_escalation: no solver recipients configured"
        );
        return;
    }

    // Operator-facing body. Kept compact so it renders cleanly in
    // typical Nostr clients; the full handoff package lives in the
    // `handoff_prepared` mediation_events row for Phase 4.
    let dm_body = format!(
        "Mediation session {session_id} (dispute {dispute_id}) escalated — \
         trigger: {trigger}. Needs human judgment."
    );

    // SC-107: addresses solver pubkey, not party pubkey — recipients
    // resolve from `[solvers]` config (broadcast) or from
    // `disputes.assigned_solver` (targeted); party primary / shared
    // pubkeys are never used as escalation recipients.
    for pk_hex in &recipient_list {
        // Surface the clock-before-UNIX-EPOCH guard instead of silently
        // recording `sent_at = 0`. We still best-effort the fallback
        // so one bad clock does not block every notification in the
        // loop, but the warn! makes the issue visible.
        let sent_at = match current_ts_secs() {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    solver_pubkey = %pk_hex,
                    error = %e,
                    "notify_solvers_escalation: clock guard returned Err; \
                     recording sent_at = 0 as a best-effort marker"
                );
                0
            }
        };
        let (status, error_message) = match PublicKey::parse(pk_hex) {
            Ok(pk) => match send_gift_wrap_notification(client, &pk, &dm_body).await {
                Ok(()) => {
                    info!(
                        session_id = %session_id,
                        solver_pubkey = %pk_hex,
                        trigger = %trigger,
                        "solver_escalation_notified"
                    );
                    (NotificationStatus::Sent, None)
                }
                Err(e) => {
                    warn!(
                        session_id = %session_id,
                        solver_pubkey = %pk_hex,
                        error = %e,
                        "notify_solvers_escalation: notifier send failed; recording Failed row"
                    );
                    (NotificationStatus::Failed, Some(e.to_string()))
                }
            },
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    solver_pubkey = %pk_hex,
                    error = %e,
                    "notify_solvers_escalation: recipient pubkey parse failed"
                );
                (
                    NotificationStatus::Failed,
                    Some(format!("invalid pubkey: {e}")),
                )
            }
        };
        let guard = conn.lock().await;
        db::notifications::record_notification_logged(
            &guard,
            dispute_id,
            pk_hex,
            sent_at,
            status,
            error_message.as_deref(),
            NotificationType::MediationEscalationRecommended,
        );
    }
}

/// US6 (T092) — informational "dispute resolved externally" DM to the
/// configured solver(s).
///
/// Mirrors the shape of [`notify_solvers_escalation`] so the routing,
/// clock guard, and per-recipient failure handling stay consistent,
/// but the DM is a RESOLUTION REPORT — not an escalation. The dispute
/// is already resolved via Mostro; this is a "for your records"
/// notification so the solver knows the session closed cleanly and no
/// further mediation action is needed. Per FR-120 the body contains
/// no rationale text.
pub(crate) async fn notify_solvers_resolution_report(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    solvers: &[SolverConfig],
    dispute_id: &str,
    session_id: &str,
    resolution_status: &str,
) {
    // Same three-way split as `notify_solvers_escalation`: distinguish
    // `Ok(Some)` / `Ok(None)` / `Err(QueryReturnedNoRows)` / `Err(other)`
    // so a DB error never silently broadcasts and a missing-row bug
    // fails loudly instead of papering over a FK violation.
    let assigned_solver: Option<String> = {
        let guard = conn.lock().await;
        match guard.query_row(
            "SELECT assigned_solver FROM disputes WHERE dispute_id = ?1",
            rusqlite::params![dispute_id],
            |r| r.get::<_, Option<String>>(0),
        ) {
            Ok(opt) => opt,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                warn!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    resolution_status = %resolution_status,
                    "notify_solvers_resolution_report: dispute row missing; \
                     refusing to broadcast without a valid parent row"
                );
                return;
            }
            Err(e) => {
                warn!(
                    dispute_id = %dispute_id,
                    session_id = %session_id,
                    resolution_status = %resolution_status,
                    error = %e,
                    "notify_solvers_resolution_report: assigned_solver lookup failed; \
                     skipping notification to avoid unsafe broadcast"
                );
                return;
            }
        }
    };
    let recipients = router::resolve_recipients(solvers, assigned_solver.as_deref());
    let recipient_list: Vec<String> = match recipients {
        router::Recipients::Targeted(pk) => vec![pk],
        router::Recipients::Broadcast(v) => v,
    };
    if recipient_list.is_empty() {
        warn!(
            dispute_id = %dispute_id,
            session_id = %session_id,
            resolution_status = %resolution_status,
            "notify_solvers_resolution_report: no solver recipients configured"
        );
        return;
    }

    let dm_body = format!(
        "Mediation session {session_id} (dispute {dispute_id}) closed — \
         the dispute was resolved externally ({resolution_status}). \
         No further mediation action needed."
    );

    for pk_hex in &recipient_list {
        let sent_at = match current_ts_secs() {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    solver_pubkey = %pk_hex,
                    error = %e,
                    "notify_solvers_resolution_report: clock guard returned Err; \
                     recording sent_at = 0 as a best-effort marker"
                );
                0
            }
        };
        let (status, error_message) = match PublicKey::parse(pk_hex) {
            Ok(pk) => match send_gift_wrap_notification(client, &pk, &dm_body).await {
                Ok(()) => {
                    info!(
                        session_id = %session_id,
                        solver_pubkey = %pk_hex,
                        resolution_status = %resolution_status,
                        "solver_resolution_report_sent"
                    );
                    (NotificationStatus::Sent, None)
                }
                Err(e) => {
                    warn!(
                        session_id = %session_id,
                        solver_pubkey = %pk_hex,
                        error = %e,
                        "notify_solvers_resolution_report: notifier send failed; recording Failed row"
                    );
                    (NotificationStatus::Failed, Some(e.to_string()))
                }
            },
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    solver_pubkey = %pk_hex,
                    error = %e,
                    "notify_solvers_resolution_report: recipient pubkey parse failed"
                );
                (
                    NotificationStatus::Failed,
                    Some(format!("invalid pubkey: {e}")),
                )
            }
        };
        let guard = conn.lock().await;
        db::notifications::record_notification_logged(
            &guard,
            dispute_id,
            pk_hex,
            sent_at,
            status,
            error_message.as_deref(),
            NotificationType::MediationResolutionReport,
        );
    }
}

async fn transition_session(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    new_state: MediationSessionState,
    at: i64,
) -> Result<()> {
    let guard = conn.lock().await;
    db::mediation::set_session_state(&guard, session_id, new_state, at)
}

async fn escalate_from_summary_path(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    prompt_bundle: &Arc<PromptBundle>,
    trigger: EscalationTrigger,
    reason: &str,
) -> Result<()> {
    let now = current_ts_secs()?;
    let payload = json!({
        "trigger": trigger.to_string(),
        "session_id": session_id,
        "reason": reason,
    })
    .to_string();
    let mut guard = conn.lock().await;
    let tx = guard.transaction()?;
    db::mediation::set_session_state(
        &tx,
        session_id,
        MediationSessionState::EscalationRecommended,
        now,
    )?;
    db::mediation_events::record_event(
        &tx,
        MediationEventKind::EscalationRecommended,
        Some(session_id),
        &payload,
        None,
        Some(&prompt_bundle.id),
        Some(&prompt_bundle.policy_hash),
        now,
    )?;
    tx.commit()?;
    Ok(())
}

struct Eligible {
    dispute_id: String,
    initiator_role: InitiatorRole,
}

/// Disputes in `lifecycle_state = 'notified'` that are eligible for
/// a fresh mediation open:
///
/// - No existing session is in a live (non-terminal) state.
///   "Live" here means anything other than `closed` — a dispute
///   that already has an `opening` / `awaiting_response` / …
///   session is being handled right now and MUST NOT be restarted.
/// - No existing session is in `escalation_recommended`. Once a
///   dispute has been handed off to a human solver the engine must
///   not silently pull it back into mediation — the handoff is
///   terminal. The separate `NOT EXISTS` makes this invariant
///   explicit and resistant to future state-set tweaks.
///
/// Ordering is ascending by `event_timestamp` so the oldest
/// disputes get worked first.
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
                 AND s.state NOT IN ('closed')
           )
           AND NOT EXISTS (
               SELECT 1 FROM mediation_sessions s
               WHERE s.dispute_id = d.dispute_id
                 AND s.state = 'escalation_recommended'
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

/// Seconds since the UNIX epoch. Shared by `session.rs`,
/// `summarizer.rs`, and the deliver-summary / escalation paths in
/// this module so there is a single source of truth for the
/// "system clock is before UNIX_EPOCH" error tag. Returns
/// `Error::ChatTransport` on a pre-epoch clock because the downstream
/// callers are all on the chat / transport path; the tag is load-
/// bearing for the existing log + error-filter pipeline.
pub(crate) fn current_ts_secs() -> Result<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| Error::ChatTransport(format!("system clock is before UNIX_EPOCH: {e}")))
}

/// T051 — ingest tick.
///
/// Walk every live session, reconstruct per-party
/// [`PartyChatMaterial`] from the in-memory cache, fetch inbound
/// gift-wraps for both parties, and ingest the envelopes. Sessions
/// whose material is missing from the cache (e.g. because T052's
/// restart-resume could not rebuild them, which is the US2 common
/// case) are skipped at `debug!` — they stay alive and will be
/// picked up as soon as a future slice re-derives the keys.
///
/// Per-session failures are logged at `warn!` and the tick continues
/// with the next session so one slow / misbehaving relay cannot
/// starve the rest. The function only returns `Err` on infrastructure
/// failures (DB lock poisoning, query builder errors).
#[instrument(
    skip_all,
    fields(
        sessions_checked = tracing::field::Empty,
        envelopes_fetched = tracing::field::Empty,
        rows_ingested = tracing::field::Empty,
        rows_duplicate = tracing::field::Empty,
        rows_stale = tracing::field::Empty,
    )
)]
#[allow(clippy::too_many_arguments)]
async fn run_ingest_tick(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    session_key_cache: &SessionKeyCache,
    prompt_bundle: &Arc<PromptBundle>,
    mediation_cfg: &MediationConfig,
    solvers: &[SolverConfig],
) -> Result<()> {
    debug!("ingest tick starting");

    let sessions = {
        let guard = conn.lock().await;
        db::mediation::list_live_sessions(&guard)?
    };

    let mut sessions_checked: u64 = 0;
    let mut envelopes_fetched: u64 = 0;
    let mut rows_ingested: u64 = 0;
    let mut rows_duplicate: u64 = 0;
    let mut rows_stale: u64 = 0;

    // Fan out the relay fetches. Each spawned task owns its own
    // clone of the client + the session's chat material, and
    // returns `(session_id, Result<Vec<InboundEnvelope>>)` so the
    // DB side of the tick can stay single-threaded (the shared
    // `AsyncMutex<Connection>` serialises ingest anyway). One slow
    // or misbehaving relay therefore cannot stall the rest of the
    // tick — fetches run concurrently and results are drained as
    // they arrive.
    let mut fetchers: tokio::task::JoinSet<IngestFetchResult> = tokio::task::JoinSet::new();

    for s in sessions {
        sessions_checked += 1;

        // Pull the material out of the cache by clone so we do not
        // hold the cache lock across the relay fetch + DB ingest.
        let material = {
            let guard = session_key_cache.lock().await;
            guard.get(&s.session_id).cloned()
        };
        let Some(material) = material else {
            debug!(
                session_id = %s.session_id,
                "ingest tick: no in-memory chat material; skipping session (restart-resume pending)"
            );
            continue;
        };

        // Sanity check: the cache entry must still match the DB row's
        // advertised shared pubkeys. If a future bug flips them out of
        // sync we want a loud `warn!` rather than silently decrypting
        // with stale keys.
        if let (Some(bsp), Some(ssp)) = (
            s.buyer_shared_pubkey.as_deref(),
            s.seller_shared_pubkey.as_deref(),
        ) {
            if bsp != material.buyer_shared_pubkey() || ssp != material.seller_shared_pubkey() {
                warn!(
                    session_id = %s.session_id,
                    "ingest tick: cached chat material does not match session row's \
                     shared pubkeys; skipping"
                );
                continue;
            }
        }

        // Hoist the `PublicKey::parse` calls out of the array
        // literal: if either trade pubkey is malformed we want a
        // single early `continue` rather than two nested match
        // arms inside a struct-initialiser expression, and the
        // `continue` must skip the whole session (not just skip
        // one party).
        let buyer_pk = match PublicKey::parse(&material.buyer_pubkey) {
            Ok(pk) => pk,
            Err(e) => {
                warn!(
                    session_id = %s.session_id,
                    error = %e,
                    "ingest tick: invalid buyer trade pubkey in cache; skipping session"
                );
                continue;
            }
        };
        let seller_pk = match PublicKey::parse(&material.seller_pubkey) {
            Ok(pk) => pk,
            Err(e) => {
                warn!(
                    session_id = %s.session_id,
                    error = %e,
                    "ingest tick: invalid seller trade pubkey in cache; skipping session"
                );
                continue;
            }
        };

        let client = client.clone();
        let session_id = s.session_id.clone();
        fetchers.spawn(async move {
            let parties = [
                PartyChatMaterial {
                    party: TranscriptParty::Buyer,
                    shared_keys: &material.buyer_shared_keys,
                    expected_author: buyer_pk,
                },
                PartyChatMaterial {
                    party: TranscriptParty::Seller,
                    shared_keys: &material.seller_shared_keys,
                    expected_author: seller_pk,
                },
            ];
            let result = inbound::fetch_inbound(&client, &parties, INGEST_FETCH_TIMEOUT).await;
            (session_id, result)
        });
    }

    // Drain the fetch results. Ingest runs against the shared DB
    // lock so it is naturally single-writer; the concurrency win
    // is purely in the fetch phase, which is I/O-bound.
    while let Some(res) = fetchers.join_next().await {
        let (session_id, fetch_result) = match res {
            Ok(pair) => pair,
            Err(e) => {
                warn!(
                    error = %e,
                    "ingest tick: a fetch task panicked or was cancelled; continuing"
                );
                continue;
            }
        };
        let envelopes = match fetch_result {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    error = %e,
                    "ingest tick: fetch_inbound failed; continuing with next session"
                );
                continue;
            }
        };
        envelopes_fetched += envelopes.len() as u64;

        'envelope_loop: for env in &envelopes {
            match session::ingest_inbound(conn, &session_id, env).await {
                Ok(session::IngestOutcome::Fresh { round_count_after }) => {
                    rows_ingested += 1;
                    // T068 — after each Fresh ingest, check whether
                    // the session has hit the configured round cap.
                    // If so, escalate with `RoundLimit` and STOP
                    // processing more envelopes for this session on
                    // this tick — once the session is at
                    // `escalation_recommended`, further inbound rows
                    // would just add noise to an escalated transcript.
                    let rc_after: u32 = round_count_after.max(0) as u32;
                    if session::check_round_limit(rc_after, mediation_cfg.max_rounds) {
                        warn!(
                            session_id = %session_id,
                            round_count = rc_after,
                            max_rounds = mediation_cfg.max_rounds,
                            "round_limit_escalation"
                        );
                        match escalation::recommend(escalation::RecommendParams {
                            conn,
                            session_id: &session_id,
                            trigger: EscalationTrigger::RoundLimit,
                            evidence_refs: vec![env.inner_event_id.clone()],
                            rationale_refs: Vec::new(),
                            prompt_bundle_id: &prompt_bundle.id,
                            policy_hash: &prompt_bundle.policy_hash,
                        })
                        .await
                        {
                            Ok(()) => {
                                // Look up dispute_id for the solver
                                // notification. The JoinSet fan-out
                                // only carried session_id through, so
                                // the cheap SQL hop is the simplest
                                // place to resolve it.
                                let dispute_id: Option<String> = {
                                    let g = conn.lock().await;
                                    g.query_row(
                                        "SELECT dispute_id FROM mediation_sessions \
                                         WHERE session_id = ?1",
                                        rusqlite::params![session_id],
                                        |r| r.get::<_, String>(0),
                                    )
                                    .ok()
                                };
                                if let Some(did) = dispute_id {
                                    notify_solvers_escalation(
                                        conn,
                                        client,
                                        solvers,
                                        &did,
                                        &session_id,
                                        EscalationTrigger::RoundLimit,
                                    )
                                    .await;
                                }
                            }
                            Err(e) => {
                                // Typically the session was already
                                // escalated by a concurrent path
                                // (e.g., the timeout sweep). Log and
                                // still break so we stop writing
                                // more inbound rows against an
                                // escalated session on this tick.
                                warn!(
                                    session_id = %session_id,
                                    error = %e,
                                    "ingest tick: round_limit escalation failed; \
                                     breaking out of envelope loop for this session"
                                );
                            }
                        }
                        break 'envelope_loop;
                    }
                }
                Ok(session::IngestOutcome::Duplicate) => rows_duplicate += 1,
                Ok(session::IngestOutcome::Stale) => rows_stale += 1,
                Err(e) => {
                    warn!(
                        session_id = %session_id,
                        error = %e,
                        inner_event_id = %env.inner_event_id,
                        "ingest tick: ingest_inbound failed for envelope"
                    );
                }
            }
        }
    }

    let span = tracing::Span::current();
    span.record("sessions_checked", sessions_checked);
    span.record("envelopes_fetched", envelopes_fetched);
    span.record("rows_ingested", rows_ingested);
    span.record("rows_duplicate", rows_duplicate);
    span.record("rows_stale", rows_stale);

    debug!(
        sessions_checked,
        envelopes_fetched, rows_ingested, rows_duplicate, rows_stale, "ingest tick finished"
    );

    // T069 — party-response timeout sweep. Runs AFTER the ingest
    // JoinSet drains so last-seen timestamps reflect any fresh
    // envelope written this tick.
    if let Err(e) =
        check_party_unresponsive_timeout(conn, client, solvers, prompt_bundle, mediation_cfg).await
    {
        warn!(error = %e, "ingest tick: party-unresponsive timeout sweep failed");
    }

    Ok(())
}

/// T069 — for every live mediation session, check whether the
/// per-party response deadline has been exceeded. If so, escalate via
/// [`escalation::recommend`] with [`EscalationTrigger::PartyUnresponsive`].
///
/// Deadline rule (from spec §FR-111):
/// ```text
/// reference_ts = max(buyer_last_seen_inner_ts,
///                    seller_last_seen_inner_ts,
///                    started_at)
/// deadline     = reference_ts + party_response_timeout_seconds
/// ```
/// If neither party has ever responded (both `last_seen` are NULL)
/// `started_at` is the reference. Sessions already in a terminal
/// state or at `escalation_recommended` are skipped — the row does
/// not appear in `list_live_sessions` so the SELECT below naturally
/// excludes them, but the guard is belt-and-braces.
///
/// Exposed `pub` (not `pub(crate)`) so the US4 integration tests in
/// `tests/phase3_escalation_triggers.rs` can drive the production
/// sweep directly rather than duplicating the deadline math — that
/// way a regression in the deadline rule shows up as a test
/// failure, not a test that happens to compute the same wrong
/// answer the code does.
pub async fn check_party_unresponsive_timeout(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    solvers: &[SolverConfig],
    prompt_bundle: &Arc<PromptBundle>,
    mediation_cfg: &MediationConfig,
) -> Result<()> {
    // Timeout disabled → do not scan. A zero timeout combined with
    // the `deadline = reference + timeout` rule below would escalate
    // every live session on the first tick (since `started_at` is
    // always in the past), so we treat 0 as the documented
    // "timeout disabled" sentinel instead of a 0-second deadline.
    if mediation_cfg.party_response_timeout_seconds == 0 {
        debug!("party-response timeout sweep disabled (timeout = 0)");
        return Ok(());
    }

    let now = current_ts_secs()?;
    let timeout = mediation_cfg.party_response_timeout_seconds as i64;

    #[derive(Debug)]
    struct Candidate {
        session_id: String,
        dispute_id: String,
        state: MediationSessionState,
        started_at: i64,
        buyer_last: Option<i64>,
        seller_last: Option<i64>,
    }

    let candidates: Vec<Candidate> = {
        use std::str::FromStr;
        let guard = conn.lock().await;
        let mut stmt = guard.prepare(
            "SELECT session_id, dispute_id, state, started_at,
                    buyer_last_seen_inner_ts, seller_last_seen_inner_ts
             FROM mediation_sessions
             WHERE state NOT IN (
                 'closed',
                 'summary_delivered',
                 'escalation_recommended',
                 'superseded_by_human'
             )",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (session_id, dispute_id, state_s, started_at, buyer_last, seller_last) = row?;
            let state = match MediationSessionState::from_str(&state_s) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        session_id = %session_id,
                        state = %state_s,
                        error = %e,
                        "timeout sweep: skipping session with unparseable state"
                    );
                    continue;
                }
            };
            out.push(Candidate {
                session_id,
                dispute_id,
                state,
                started_at,
                buyer_last,
                seller_last,
            });
        }
        out
    };

    for c in candidates {
        if c.state.is_terminal() || c.state == MediationSessionState::EscalationRecommended {
            continue;
        }
        let reference = [Some(c.started_at), c.buyer_last, c.seller_last]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(c.started_at);
        let deadline = reference.saturating_add(timeout);
        if now <= deadline {
            continue;
        }
        warn!(
            session_id = %c.session_id,
            reference_ts = reference,
            deadline,
            now,
            "party_unresponsive_escalation"
        );
        match escalation::recommend(escalation::RecommendParams {
            conn,
            session_id: &c.session_id,
            trigger: EscalationTrigger::PartyUnresponsive,
            evidence_refs: Vec::new(),
            rationale_refs: Vec::new(),
            prompt_bundle_id: &prompt_bundle.id,
            policy_hash: &prompt_bundle.policy_hash,
        })
        .await
        {
            Ok(()) => {
                notify_solvers_escalation(
                    conn,
                    client,
                    solvers,
                    &c.dispute_id,
                    &c.session_id,
                    EscalationTrigger::PartyUnresponsive,
                )
                .await;
            }
            Err(e) => {
                error!(
                    session_id = %c.session_id,
                    error = %e,
                    "timeout sweep: escalation::recommend failed"
                );
            }
        }
    }

    Ok(())
}

/// Per-session result emitted by the ingest tick's fetch fan-out.
/// Named so the `JoinSet` type parameter stays readable.
type IngestFetchResult = (String, Result<Vec<inbound::InboundEnvelope>>);
