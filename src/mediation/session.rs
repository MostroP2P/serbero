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
//! 0. Gate: is the reasoning provider reachable? (`health_check`)
//!    If not, refuse deterministically — no relay I/O, no DB row
//!    (FR-102 / SC-105).
//! 1. Gate: is another session already open for this dispute?
//! 2. Take-dispute exchange via `chat::dispute_chat_flow::run_take_flow`.
//! 3. Insert the `mediation_sessions` row + `session_opened` audit
//!    event atomically so downstream writes have a valid FK target.
//! 4. Call [`super::policy::initial_classification`] — the *only*
//!    place the reasoning provider is invoked on the opening path.
//!    Policy persists the rationale in the controlled audit store
//!    and emits `classification_produced` for this `session_id`.
//! 5. Dispatch on the returned [`super::policy::PolicyDecision`]:
//!    - `AskClarification(text)` → call
//!      [`super::draft_and_send_initial_message`], which persists
//!      the outbound rows, publishes the gift-wraps, and records
//!      `outbound_sent` only after each successful publish.
//!    - `Summarize` / `Escalate(_)` → transition the session to
//!      `escalation_recommended` (the cooperative-summary and
//!      trigger-specific-escalation paths land with US3 / US4).
//!
//! Every session open therefore goes through the same audit path
//! the engine drives on subsequent ticks, so the
//! `reasoning_rationales` + `mediation_events` rows line up with
//! the `mediation_sessions.policy_hash` pin (SC-103).

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use rusqlite::params;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use super::policy::{self, PolicyDecision};
use crate::chat::dispute_chat_flow;
use crate::chat::inbound::InboundEnvelope;
use crate::db;
use crate::error::{Error, Result};
use crate::models::dispute::InitiatorRole;
use crate::models::mediation::TranscriptParty;
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
    /// The reasoning provider's `health_check` failed; we refuse
    /// to open a session and leave Phase 1/2 behavior untouched
    /// (SC-105). The `reason` is the provider-reported error text
    /// for operator-facing logs; no rows are written to the
    /// mediation tables and no chat events are emitted.
    RefusedReasoningUnavailable { reason: String },
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
    /// Provider identifier persisted alongside the rationale row
    /// (e.g. `"openai"`). The adapter itself does not expose this,
    /// so the caller (daemon / tests) passes it in. Required for
    /// the SC-103 audit provenance: the rationale row carries
    /// `(provider, model, prompt_bundle_id, policy_hash)`.
    pub provider_name: &'a str,
    /// Model identifier (e.g. `"gpt-4o-mini"`). Same provenance
    /// rationale as `provider_name`.
    pub model_name: &'a str,
}

#[instrument(skip_all, fields(dispute_id = %params.dispute_id))]
pub async fn open_session(params: OpenSessionParams<'_>) -> Result<OpenOutcome> {
    // (0) Fast-path reasoning-provider reachability gate (T044 /
    //     FR-102 / SC-105). A cheap `health_check` call runs *before*
    //     any relay I/O or DB work so an unreachable provider never
    //     causes the mediation path to publish chat events or write
    //     `mediation_sessions` rows. Phase 1/2 detection and solver
    //     notification continue regardless — `open_session` simply
    //     returns without side effects.
    if let Err(e) = params.reasoning.health_check().await {
        warn!(
            error = %e,
            "refusing to open mediation session: reasoning provider health check failed"
        );
        return Ok(OpenOutcome::RefusedReasoningUnavailable {
            reason: e.to_string(),
        });
    }

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

    // (2) Take-dispute exchange. This is the expensive step; if it
    //     fails we haven't written anything to the DB yet, so a
    //     caller retry is safe.
    let material = dispute_chat_flow::run_take_flow(dispute_chat_flow::TakeFlowParams {
        client: params.client,
        serbero_keys: params.serbero_keys,
        mostro_pubkey: params.mostro_pubkey,
        dispute_id: params.dispute_uuid,
        timeout: params.take_flow_timeout,
        poll_interval: params.take_flow_poll_interval,
    })
    .await?;

    // (3) Insert session row + `session_opened` audit atomically.
    //     Done BEFORE the reasoning call so the rationale /
    //     classification_produced rows the policy layer writes in
    //     step (4) have a valid FK target on `session_id`. The
    //     step-1 gate is re-checked under the same connection to
    //     close the check-then-act race.
    let session_id = Uuid::new_v4().to_string();
    let now = current_ts_secs()?;
    {
        let mut conn = params.conn.lock().await;
        if let Some((sid, _state)) =
            db::mediation::latest_open_session_for(&conn, params.dispute_id)?
        {
            info!(
                session_id = %sid,
                "mediation session opened concurrently; aborting this attempt"
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
        db::mediation_events::record_session_opened(
            &tx,
            &session_id,
            &params.prompt_bundle.id,
            &params.prompt_bundle.policy_hash,
            now,
        )?;
        tx.commit()?;
    }

    // (4) Policy-validated classification. Persists the rationale
    //     and `classification_produced` tied to the session row we
    //     just committed. Never sees raw model output without
    //     validation.
    let decision = policy::initial_classification(
        params.conn,
        &session_id,
        params.dispute_id,
        params.initiator_role,
        params.prompt_bundle,
        params.reasoning,
        params.provider_name,
        params.model_name,
    )
    .await?;

    // (5) Dispatch on the policy decision.
    match decision {
        PolicyDecision::AskClarification(text) => {
            super::draft_and_send_initial_message(
                params.conn,
                params.client,
                params.serbero_keys,
                &session_id,
                &material.buyer_shared_keys,
                &material.seller_shared_keys,
                params.prompt_bundle,
                &text,
            )
            .await?;
            info!(
                session_id = %session_id,
                prompt_bundle_id = %params.prompt_bundle.id,
                policy_hash = %params.prompt_bundle.policy_hash,
                "mediation session opened; first clarifying message dispatched to both parties"
            );
            Ok(OpenOutcome::Opened { session_id })
        }
        PolicyDecision::Summarize | PolicyDecision::Escalate(_) => {
            // Non-AskClarification on the opening call means the
            // policy layer has already decided this dispute does
            // not belong in guided mediation (or cannot be handled
            // without a transcript). Transition to
            // `escalation_recommended` so the engine's eligibility
            // query does not re-pick this dispute on the next tick.
            // Cooperative-summary handling on a non-empty transcript
            // and the per-trigger escalation audit rows land with
            // US3 / US4 respectively.
            let escalation_now = current_ts_secs()?;
            {
                let guard = params.conn.lock().await;
                guard.execute(
                    "UPDATE mediation_sessions
                     SET state = 'escalation_recommended',
                         last_transition_at = ?1
                     WHERE session_id = ?2",
                    params![escalation_now, &session_id],
                )?;
            }
            debug!(
                session_id = %session_id,
                ?decision,
                "policy decision on opening call is not AskClarification; \
                 session marked escalation_recommended for a later phase"
            );
            Ok(OpenOutcome::DeferredToLaterPhase)
        }
    }
}

/// Outcome of a single-envelope ingest attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// The envelope was new and has been persisted. `round_count_after`
    /// reflects the recomputed round counter.
    Fresh { round_count_after: i64 },
    /// The envelope's inner event id was already in
    /// `mediation_messages` for this session. No rows written, no
    /// session-state change.
    Duplicate,
    /// The envelope was persisted with `stale = 1` because its inner
    /// `created_at` predated the party's last-seen marker. Last-seen
    /// is NOT updated and `round_count` is unchanged (stale rows do
    /// not count toward round boundaries).
    Stale,
}

/// Persist one inbound envelope against the named session.
///
/// Transactional boundary:
/// - Look up the per-party last-seen marker.
/// - Decide `stale` (inner ts <= last-seen).
/// - `INSERT OR IGNORE` the row. On duplicate the transaction
///   commits cleanly as a no-op — idempotency without exception
///   gymnastics.
/// - On a fresh, non-stale insert: update the party's last-seen
///   marker and recompute `round_count` from the transcript.
///
/// This function does NOT transition session state; `awaiting_response`
/// -> `classified` / further transitions belong to the policy layer
/// (US3 / US4). It also does NOT publish anything on the relay —
/// that's the outbound side of the transport.
pub async fn ingest_inbound(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
    envelope: &InboundEnvelope,
) -> Result<IngestOutcome> {
    // Serbero never appears as an inbound author; reject the
    // enum-widening mistake up front rather than writing a malformed
    // row.
    if matches!(envelope.party, TranscriptParty::Serbero) {
        return Err(Error::InvalidEvent(
            "ingest_inbound refused: envelope.party = Serbero".into(),
        ));
    }

    let now = current_ts_secs()?;
    let mut conn = conn.lock().await;

    // Read the per-party last-seen marker that the stale-check
    // depends on.
    let (buyer_last, seller_last) = db::mediation::get_last_seen(&conn, session_id)?;
    let last_seen_for_party = match envelope.party {
        TranscriptParty::Buyer => buyer_last,
        TranscriptParty::Seller => seller_last,
        TranscriptParty::Serbero => unreachable!("guarded above"),
    };
    // Strict less-than. Equal-timestamp messages are NOT stale:
    // the party may legitimately send two distinct messages in the
    // same second, and each carries its own inner_event_id. True
    // replays (identical inner_event_id) are caught downstream by
    // `INSERT OR IGNORE`, which returns `Duplicate`; using `<=`
    // here would instead mark the second same-second message as
    // stale and silently drop it from the round counter.
    let is_stale = last_seen_for_party
        .map(|prev| envelope.inner_created_at < prev)
        .unwrap_or(false);

    let tx = conn.transaction()?;

    let inserted = db::mediation::insert_inbound_message(
        &tx,
        &db::mediation::NewInboundMessage {
            session_id,
            party: envelope.party,
            shared_pubkey: &envelope.shared_pubkey,
            inner_event_id: &envelope.inner_event_id,
            inner_event_created_at: envelope.inner_created_at,
            outer_event_id: Some(&envelope.outer_event_id),
            content: &envelope.content,
            persisted_at: now,
            stale: is_stale,
        },
    )?;

    if !inserted {
        // Unique-index dedup kicked in. Commit the no-op transaction
        // so any reads in the next tick see a consistent state.
        tx.commit()?;
        debug!(
            session_id = %session_id,
            party = %envelope.party,
            inner_event_id = %envelope.inner_event_id,
            "inbound replay ignored (already persisted)"
        );
        return Ok(IngestOutcome::Duplicate);
    }

    if is_stale {
        tx.commit()?;
        debug!(
            session_id = %session_id,
            party = %envelope.party,
            inner_event_id = %envelope.inner_event_id,
            inner_created_at = envelope.inner_created_at,
            "inbound persisted as stale; last-seen and round_count unchanged"
        );
        return Ok(IngestOutcome::Stale);
    }

    db::mediation::update_last_seen_inner_ts(
        &tx,
        session_id,
        envelope.party,
        envelope.inner_created_at,
    )?;
    let round_count_after = db::mediation::recompute_round_count(&tx, session_id)?;
    tx.commit()?;

    info!(
        session_id = %session_id,
        party = %envelope.party,
        inner_event_id = %envelope.inner_event_id,
        inner_created_at = envelope.inner_created_at,
        round_count_after = round_count_after,
        "inbound ingested"
    );
    Ok(IngestOutcome::Fresh { round_count_after })
}

/// Publish one outer gift-wrap with a tiny bounded retry. No generic
/// retry framework — three attempts, exponential-ish backoff capped
/// at a few hundred milliseconds, aligned with the plan's "plain
/// bounded loops" discipline.
///
/// Failure here with the DB rows already committed leaves a
/// known-published-incomplete session; reconciliation on top of that
/// is US2 scope (durable outbox or restart-replay of unwrapped wraps).
pub(crate) async fn publish_with_bounded_retry(
    client: &Client,
    outer: &Event,
    label: &str,
) -> Result<()> {
    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err: Option<String> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match client.send_event(outer).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = Some(e.to_string());
                if attempt < MAX_ATTEMPTS {
                    let backoff_ms = 100u64 * (1u64 << (attempt - 1));
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }
    Err(Error::ChatTransport(format!(
        "publish {label} gift-wrap failed after {MAX_ATTEMPTS} attempts: {}",
        last_err.unwrap_or_default()
    )))
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
