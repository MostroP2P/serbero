//! Mediation session lifecycle (US1 slice).
//!
//! Ships only the session-open path for US1. Inbound ingest,
//! round-counter advance, timeout handling, and US4 escalation
//! triggers are deferred.
//!
//! The session-open flow follows a transactional-outbox shape —
//! persistence happens before the outbound publish, so a crash
//! between commit and publish leaves a resumable state rather than
//! relay events with no DB trace:
//!
//! 1. Gate: is another session already open for this dispute?
//! 2. Take-dispute exchange via `chat::dispute_chat_flow::run_take_flow`.
//! 3. Initial classification via the configured `ReasoningProvider`,
//!    honoring the `policy_hash` invariant — the full bundle flows
//!    to the model, not just its id + hash.
//! 4. Draft the first clarifying message per party from the
//!    reasoning provider's `SuggestedAction::AskClarification` text
//!    (fallback: refuse to open the session if the adapter returns
//!    a non-clarification action; those paths belong to US3/US4).
//! 5. Build the per-party gift-wraps with `chat::outbound::build_wrap`
//!    and persist everything in a single DB transaction: a
//!    `mediation_sessions` row at `awaiting_response` with the pinned
//!    bundle, plus two `mediation_messages` rows (direction
//!    `outbound`) keyed by the real inner-event ids. The step-1 gate
//!    is re-checked inside this lock scope to close the check-then-
//!    act race.
//! 6. Publish the two already-built gift-wraps to the relay. The
//!    unique `(session_id, inner_event_id)` index makes a later
//!    retry idempotent on partial-publish failure.

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use crate::chat::{dispute_chat_flow, outbound};
use crate::db;
use crate::error::{Error, Result};
use crate::models::dispute::InitiatorRole;
use crate::models::mediation::TranscriptParty;
use crate::models::reasoning::{ClassificationRequest, ReasoningContext, SuggestedAction};
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

/// Outcome of a session-open attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenOutcome {
    /// A new session was opened and its outbound messages dispatched.
    Opened { session_id: String },
    /// The dispute already has an open session; no-op.
    AlreadyOpen { session_id: String },
    /// The reasoning provider returned a non-clarification action
    /// (Summarize / Escalate) on the opening call. US3/US4 will
    /// handle these; for now the session is not opened and the
    /// caller is told to skip.
    DeferredToLaterPhase,
}

/// Parameters for [`open_session`]. Grouped to keep the signature
/// readable and to make the engine-wiring site compact.
pub struct OpenSessionParams<'a> {
    pub conn: &'a Arc<AsyncMutex<rusqlite::Connection>>,
    pub client: &'a Client,
    pub serbero_keys: &'a Keys,
    pub mostro_pubkey: &'a PublicKey,
    pub reasoning: &'a dyn ReasoningProvider,
    pub prompt_bundle: &'a Arc<PromptBundle>,
    pub dispute_id: &'a str,
    pub initiator_role: InitiatorRole,
    /// Parsed UUID form of `dispute_id`. Phase 1/2 stores dispute
    /// ids as TEXT, but the mostro-core take-flow needs a `Uuid`.
    pub dispute_uuid: Uuid,
    /// Wall-clock budget for the take-dispute DM exchange. Default
    /// mirrors Mostrix's `FETCH_EVENTS_TIMEOUT` (15s).
    pub take_flow_timeout: Duration,
    /// Poll cadence inside the take-flow while waiting for the
    /// `AdminTookDispute` response.
    pub take_flow_poll_interval: Duration,
}

#[instrument(skip_all, fields(dispute_id = %params.dispute_id))]
pub async fn open_session(params: OpenSessionParams<'_>) -> Result<OpenOutcome> {
    // (1) Gate: existing session?
    {
        let conn = params.conn.lock().await;
        if let Some((sid, _state)) =
            db::mediation::latest_open_session_for(&conn, params.dispute_id)?
        {
            info!(session_id = %sid, "mediation session already open; skipping");
            return Ok(OpenOutcome::AlreadyOpen { session_id: sid });
        }
    }

    // (2) Take-dispute exchange.
    let material = dispute_chat_flow::run_take_flow(dispute_chat_flow::TakeFlowParams {
        client: params.client,
        serbero_keys: params.serbero_keys,
        mostro_pubkey: params.mostro_pubkey,
        dispute_id: params.dispute_uuid,
        timeout: params.take_flow_timeout,
        poll_interval: params.take_flow_poll_interval,
    })
    .await?;

    // (3) Initial classification. Bundle flows into the request so
    //     the policy_hash invariant holds (SC-103).
    let session_id = Uuid::new_v4().to_string();
    let request = ClassificationRequest {
        session_id: session_id.clone(),
        dispute_id: params.dispute_id.to_string(),
        initiator_role: params.initiator_role,
        prompt_bundle: Arc::clone(params.prompt_bundle),
        transcript: Vec::new(),
        context: ReasoningContext {
            round_count: 0,
            last_classification: None,
            last_confidence: None,
        },
    };
    let classification = params
        .reasoning
        .classify(request)
        .await
        .map_err(|e| Error::ReasoningUnavailable(e.to_string()))?;

    // (4) Decide the action. US1 only opens a session when the
    //     adapter returns AskClarification. Summarize / Escalate are
    //     US3 / US4 territory; exit early without opening so those
    //     phases can take over later.
    let ask_text = match classification.suggested_action {
        SuggestedAction::AskClarification(text) if !text.trim().is_empty() => text,
        SuggestedAction::AskClarification(_) => {
            warn!("reasoning returned empty clarification text; deferring");
            return Ok(OpenOutcome::DeferredToLaterPhase);
        }
        SuggestedAction::Summarize | SuggestedAction::Escalate(_) => {
            debug!(
                action = ?classification.suggested_action,
                "reasoning suggested a non-clarification action on the opening call; \
                 leaving it to a later phase"
            );
            return Ok(OpenOutcome::DeferredToLaterPhase);
        }
    };

    // (5) Build outbound chat wraps (but do NOT publish yet).
    //
    // The inner event id is a content-hash: same content + same
    // signer + same second would collide. We address that by
    // prefixing each party's message with its role label; this is
    // also useful context for the party and keeps every
    // `mediation_messages` row uniquely identifiable even when two
    // clarification drafts land in the same second. Full per-party
    // model drafting (distinct questions per party) is US3+
    // territory — this is the minimal US1-safe shape.
    //
    // Construction order matters: we persist the session +
    // outbound rows FIRST, then publish the wraps. That is the
    // transactional-outbox shape: on commit failure nothing is on
    // the relay, on publish failure the unique index on
    // `(session_id, inner_event_id)` makes a later retry idempotent.
    let buyer_shared = &material.buyer_shared_keys;
    let seller_shared = &material.seller_shared_keys;
    let buyer_content = format!("Buyer: {}", ask_text);
    let seller_content = format!("Seller: {}", ask_text);
    let buyer_wrap = outbound::build_wrap(
        params.serbero_keys,
        &buyer_shared.public_key(),
        &buyer_content,
    )
    .await?;
    let seller_wrap = outbound::build_wrap(
        params.serbero_keys,
        &seller_shared.public_key(),
        &seller_content,
    )
    .await?;
    // Belt-and-braces guard: if the inner event ids ever collide
    // despite the prefix, refuse to persist (the DB unique index
    // would reject the second row anyway — we surface a clearer
    // error here).
    if buyer_wrap.inner_event_id == seller_wrap.inner_event_id {
        return Err(Error::ChatTransport(
            "inner event ids collided across parties; \
             refusing to write rows that would violate the dedup invariant"
                .into(),
        ));
    }

    // (6) Persist first. The gate from step (1) is re-checked under
    //     the same connection so that a concurrent open on the same
    //     dispute_id cannot slip through between step (1) and here.
    //     SQLite serialises on the single `AsyncMutex<Connection>`,
    //     so this re-check is atomic with the inserts that follow.
    let now = current_ts_secs()?;
    let mut conn = params.conn.lock().await;
    if let Some((sid, _state)) = db::mediation::latest_open_session_for(&conn, params.dispute_id)? {
        info!(
            session_id = %sid,
            "mediation session opened concurrently; aborting this attempt without publishing"
        );
        return Ok(OpenOutcome::AlreadyOpen { session_id: sid });
    }
    let tx = conn.transaction()?;
    db::mediation::insert_session(
        &tx,
        &db::mediation::NewMediationSession {
            session_id: &session_id,
            dispute_id: params.dispute_id,
            prompt_bundle_id: &params.prompt_bundle.id,
            policy_hash: &params.prompt_bundle.policy_hash,
            buyer_shared_pubkey: Some(&material.buyer_shared_pubkey()),
            seller_shared_pubkey: Some(&material.seller_shared_pubkey()),
            started_at: now,
        },
    )?;
    db::mediation::insert_outbound_message(
        &tx,
        &db::mediation::NewOutboundMessage {
            session_id: &session_id,
            party: TranscriptParty::Buyer,
            shared_pubkey: &material.buyer_shared_pubkey(),
            inner_event_id: &buyer_wrap.inner_event_id.to_hex(),
            inner_event_created_at: buyer_wrap.inner_created_at,
            outer_event_id: Some(&buyer_wrap.outer.id.to_hex()),
            content: &buyer_content,
            prompt_bundle_id: &params.prompt_bundle.id,
            policy_hash: &params.prompt_bundle.policy_hash,
            persisted_at: now,
        },
    )?;
    db::mediation::insert_outbound_message(
        &tx,
        &db::mediation::NewOutboundMessage {
            session_id: &session_id,
            party: TranscriptParty::Seller,
            shared_pubkey: &material.seller_shared_pubkey(),
            inner_event_id: &seller_wrap.inner_event_id.to_hex(),
            inner_event_created_at: seller_wrap.inner_created_at,
            outer_event_id: Some(&seller_wrap.outer.id.to_hex()),
            content: &seller_content,
            prompt_bundle_id: &params.prompt_bundle.id,
            policy_hash: &params.prompt_bundle.policy_hash,
            persisted_at: now,
        },
    )?;
    tx.commit()?;
    // Release the DB lock before doing network I/O.
    drop(conn);

    // (7) Publish the wraps. On failure the DB rows already exist;
    //     a later reconciliation pass (deferred to US2) can re-send
    //     them because the outer events are deterministic from the
    //     stored inner_event_id / outer_event_id. For this US1
    //     slice we surface a ChatTransport error and let the caller
    //     decide; the unique index on `(session_id, inner_event_id)`
    //     keeps retries from creating duplicate rows.
    params
        .client
        .send_event(&buyer_wrap.outer)
        .await
        .map_err(|e| Error::ChatTransport(format!("publish buyer gift-wrap failed: {e}")))?;
    params
        .client
        .send_event(&seller_wrap.outer)
        .await
        .map_err(|e| Error::ChatTransport(format!("publish seller gift-wrap failed: {e}")))?;

    info!(
        session_id = %session_id,
        prompt_bundle_id = %params.prompt_bundle.id,
        policy_hash = %params.prompt_bundle.policy_hash,
        "mediation session opened; first clarifying message dispatched to both parties"
    );
    Ok(OpenOutcome::Opened { session_id })
}

/// Surface clock-before-UNIX-EPOCH as a loud error rather than a
/// silent `0` timestamp. A zero would corrupt `started_at` /
/// `persisted_at` ordering across `mediation_sessions` and
/// `mediation_messages` rows without leaving any trace.
fn current_ts_secs() -> Result<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| Error::ChatTransport(format!("system clock is before UNIX_EPOCH: {e}")))
}
