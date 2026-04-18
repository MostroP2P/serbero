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
//!    (FR-102 / SC-105). This is a fast-path check so the US1
//!    gating test can pin the behavior regardless of whether the
//!    take-flow would succeed.
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

use crate::chat::inbound::InboundEnvelope;
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
    //
    //     We do NOT cache the last health result here: US1 does not
    //     ship a running engine loop, and caching across calls would
    //     require background orchestration that belongs to T042 /
    //     T019. A per-call `health_check` matches the contract's
    //     "cheap" shape (small-tokens / models-list call for the real
    //     adapter, in-process for tests).
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
    // signer + same second would collide. Two approaches were
    // considered:
    //
    // - Keep identical visible content and diverge the inner
    //   `created_at` (either by a `custom_created_at` on the builder
    //   or a >=1s sleep between wraps). Cleaner dedup but loses the
    //   per-party context cue and either couples the messages to a
    //   synthetic timestamp or adds latency on the happy path.
    // - Prefix each party's message with its role label. The prefix
    //   is legitimate context for the reader (the party sees who
    //   Serbero is addressing, not just free-floating text) and
    //   guarantees distinct inner event ids regardless of clock or
    //   scheduler. Full per-party model drafting (distinct
    //   questions per party) is US3+ territory — this is the
    //   minimal US1-safe shape.
    //
    // We take the prefix approach; the explicit inner-id collision
    // check below remains as a belt-and-braces guard.
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
    // Audit: the `session_opened` event lands in the same
    // transaction as the session + outbound rows. A crash between
    // the commit and the relay publish therefore leaves a
    // consistent DB view — the session row and its audit trace
    // rise and fall together, and the reasoning-rationale /
    // classification-produced events (T038 scope) can later chain
    // off this `session_opened` row.
    db::mediation_events::record_session_opened(
        &tx,
        &session_id,
        &params.prompt_bundle.id,
        &params.prompt_bundle.policy_hash,
        now,
    )?;
    tx.commit()?;
    // Release the DB lock before doing network I/O.
    drop(conn);

    // (7) Publish the wraps.
    //
    // The outer events are NOT deterministic: `outbound::build_wrap`
    // generates a fresh ephemeral signing key per wrap, and we only
    // persist the outer event ids in hex — not the serialized bytes.
    // So if this process crashes between commit and a successful
    // publish, the `mediation_messages` rows exist but the exact
    // bytes needed to republish are lost. A durable outbox that
    // stores the serialized outer event (or replays via a refresh
    // of the wrap on restart) is US2 reconciliation territory.
    //
    // For this US1 slice we narrow the window with a small bounded
    // retry per send — enough to absorb transient relay errors
    // without a generic retry framework — and surface a ChatTransport
    // error to the caller if retries are exhausted. The unique
    // index on `(session_id, inner_event_id)` keeps the
    // mediation_messages rows single-copy regardless of how many
    // publish attempts the relay eventually saw.
    publish_with_bounded_retry(params.client, &buyer_wrap.outer, "buyer").await?;
    publish_with_bounded_retry(params.client, &seller_wrap.outer, "seller").await?;

    info!(
        session_id = %session_id,
        prompt_bundle_id = %params.prompt_bundle.id,
        policy_hash = %params.prompt_bundle.policy_hash,
        "mediation session opened; first clarifying message dispatched to both parties"
    );
    Ok(OpenOutcome::Opened { session_id })
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
async fn publish_with_bounded_retry(client: &Client, outer: &Event, label: &str) -> Result<()> {
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
