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
//! 0a. Gate: is Serbero currently authorized as a Mostro solver?
//!     (T043 / SC-105). Read-only check on the [`auth_retry`]
//!     handle; `Unauthorized` / `Terminated` short-circuit with a
//!     refusal variant, no DB row, no relay I/O.
//! 0b. Gate: is the reasoning provider reachable? (`health_check`)
//!     If not, refuse deterministically — no relay I/O, no DB row
//!     (FR-102 / SC-105).
//! 1. Gate: is another session already open for this dispute?
//! 2. **Reasoning verdict FIRST (FR-122).** Call
//!    [`super::policy::classify_for_start`] — the *only* place the
//!    reasoning provider is invoked on the opening path. The
//!    rationale lands dispute-scoped (`session_id = NULL`); the
//!    dispute-scoped `reasoning_verdict` event captures the decision
//!    for audit. An `Escalate` verdict short-circuits: no take, no
//!    session row, dispute-scoped handoff via
//!    [`super::escalation::recommend`] with `session_id = None`.
//! 3. Take-dispute exchange via `chat::dispute_chat_flow::run_take_flow`
//!    — gated on the positive verdict from step 2. A take failure
//!    lands `take_dispute_issued{outcome:"failure"}` dispute-scoped
//!    and returns without committing a session row.
//! 4. Insert the `mediation_sessions` row + `session_opened` audit
//!    event atomically so downstream writes have a valid FK target,
//!    then call
//!    [`super::policy::record_classification_for_session`] to
//!    rebind the rationale to the session and emit the
//!    session-scoped `classification_produced` event.
//! 5. Dispatch on the returned [`super::policy::PolicyDecision`]:
//!    - `AskClarification { buyer_text, seller_text }` → call
//!      [`super::draft_and_send_initial_message`] with the two
//!      party-specific texts, which persists the outbound rows,
//!      publishes the gift-wraps (each addressed to its intended
//!      recipient only — the buyer never sees the seller's text
//!      and vice versa), and records `outbound_sent` only after
//!      each successful publish. The session stays at
//!      `awaiting_response`.
//!    - `Summarize { classification, confidence }` → transition
//!      `awaiting_response → classified` and return
//!      [`OpenOutcome::ReadyForSummary`] so the engine can call
//!      `deliver_summary` on this same tick (US3 / T060). No
//!      outbound clarifying message is drafted on this path.
//!    - `Escalate(trigger)` → **handled before take-dispute**, per
//!      FR-122. `open_session` writes the reasoning verdict plus a
//!      dispute-scoped handoff via
//!      [`super::escalation::recommend`] with `session_id = None`
//!      and returns [`OpenOutcome::EscalatedBeforeTake`]. No
//!      `mediation_sessions` row is ever committed for an
//!      Escalate verdict.
//!
//! The FR-122 ordering means the `mediation_events` audit trail
//! for any session row shows a strict chronological sequence of
//! `start_attempt_started` → dispute-scoped `reasoning_verdict` →
//! `take_dispute_issued{outcome:"success"}` → `session_opened` →
//! `classification_produced` (session-scoped, via
//! [`super::policy::record_classification_for_session`]) → first
//! outbound. Operators auditing "was this take gated on a positive
//! reasoning verdict?" can walk that sequence by event id without
//! touching any other table.

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use super::policy::{self, PolicyDecision};
use super::SessionKeyCache;
use crate::chat::dispute_chat_flow::{self, DisputeChatMaterial};
use crate::chat::inbound::InboundEnvelope;
use crate::db;
use crate::error::{Error, Result};
use crate::models::dispute::InitiatorRole;
use crate::models::mediation::TranscriptParty;
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

/// Outcome of a session-open attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenOutcome {
    /// A new session was opened and its outbound messages dispatched.
    Opened { session_id: String },
    /// The dispute already has an open session; no-op.
    AlreadyOpen { session_id: String },
    /// The initial classification returned `Summarize` — the session
    /// is open at state `classified` and the engine should call the
    /// summarizer immediately. No outbound clarifying message was
    /// drafted (the cooperative path skips that round). The
    /// classification label + confidence flow with the outcome so
    /// the engine can build the `SummaryRequest` without re-reading
    /// the classification_produced audit event.
    ReadyForSummary {
        session_id: String,
        classification: crate::models::mediation::ClassificationLabel,
        confidence: f64,
    },
    /// `open_session` ran `classify_for_start` and the policy layer
    /// returned `Escalate(trigger)` BEFORE any take-dispute step
    /// (FR-122). No `mediation_sessions` row was ever inserted —
    /// the dispute-scoped handoff has already been written by
    /// `open_session` (via
    /// [`super::escalation::recommend`] with `session_id = None`),
    /// and the caller only needs to fan out the solver
    /// notification (a `MediationEscalationRecommended` DM with
    /// `session_id = None`).
    ///
    /// The trigger is surfaced here so the engine's solver-DM
    /// payload captures exactly which US4 trigger fired, and
    /// `dispute_id` is surfaced because downstream consumers
    /// (router, notifier) need it without having to re-hit the DB.
    EscalatedBeforeTake {
        dispute_id: String,
        trigger: crate::models::mediation::EscalationTrigger,
    },
    /// The reasoning provider's `health_check` failed; we refuse
    /// to open a session and leave Phase 1/2 behavior untouched
    /// (SC-105). The `reason` is the provider-reported error text
    /// for operator-facing logs; no rows are written to the
    /// mediation tables and no chat events are emitted.
    RefusedReasoningUnavailable { reason: String },
    /// Serbero's solver authorization is being revalidated (the
    /// bounded retry loop from `auth_retry` is running). Phase 1/2
    /// is unaffected (SC-105) — this arm only refuses the Phase 3
    /// mediation path.
    RefusedAuthPending { reason: String },
    /// The auth-retry loop terminated without recovering
    /// authorization. Terminal for this daemon run; Phase 1/2
    /// continues normally (SC-105).
    RefusedAuthTerminated { reason: String },
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
    /// Read-only handle to the auth-retry state machine. The gate
    /// in step (0a) reads `current_state()` and refuses session
    /// opens while `Unauthorized` or `Terminated`. Phase 1/2
    /// detection and solver notification are NEVER affected by the
    /// auth state (SC-105).
    pub auth_handle: &'a super::auth_retry::AuthRetryHandle,
    /// Optional in-memory cache that maps `session_id` to the
    /// [`DisputeChatMaterial`] derived by the take-flow. When
    /// `Some`, the session-open path registers the freshly-derived
    /// material keyed by the new `session_id` on a successful
    /// `OpenOutcome::Opened` return — that is how the engine's
    /// ingest tick (T051) gets the shared-key secrets needed to
    /// decrypt inbound gift-wraps.
    ///
    /// Tests / callers that do not run an ingest tick can pass
    /// `None`; the session-open path behaves identically otherwise.
    pub session_key_cache: Option<&'a SessionKeyCache>,
    /// Configured solver pubkeys. Threaded in so the
    /// authorization-lost branch (T071 / T072) can dispatch the
    /// `MediationEscalationRecommended` gift-wrap DM to the
    /// appropriate solver(s) after `escalation::recommend` commits.
    /// Callers that do not require solver notification can pass
    /// `&[]` — `router::resolve_recipients` returns an empty
    /// broadcast and `notify_solvers_escalation` short-circuits.
    pub solvers: &'a [crate::models::SolverConfig],
}

#[instrument(skip_all, fields(dispute_id = %params.dispute_id))]
pub async fn open_session(params: OpenSessionParams<'_>) -> Result<OpenOutcome> {
    // (0a) Auth gate (T043 / SC-105). Serbero must be authorized as
    //      a Mostro solver before opening any mediation session.
    //      This is a read-only check on in-memory state — no DB
    //      writes, no relay I/O. Phase 1/2 detection and solver
    //      notification are NEVER affected: the early return here
    //      touches no tables, publishes no events, and never
    //      reaches `check_authorization` (which is US3's job; the
    //      retry task owns that call site).
    match params.auth_handle.current_state() {
        super::auth_retry::AuthState::Authorized => {}
        super::auth_retry::AuthState::Unauthorized => {
            let reason = "solver authorization pending (retry loop running)".to_string();
            warn!(reason = %reason, "refusing to open mediation session: auth pending");
            return Ok(OpenOutcome::RefusedAuthPending { reason });
        }
        super::auth_retry::AuthState::Terminated => {
            let reason = "solver authorization terminated without recovery".to_string();
            warn!(reason = %reason, "refusing to open mediation session: auth terminated");
            return Ok(OpenOutcome::RefusedAuthTerminated { reason });
        }
    }

    // (0b) Fast-path reasoning-provider reachability gate (T044 /
    //      FR-102 / SC-105). A cheap `health_check` call runs *before*
    //      any relay I/O or DB work so an unreachable provider never
    //      causes the mediation path to publish chat events or write
    //      `mediation_sessions` rows. Phase 1/2 detection and solver
    //      notification continue regardless — `open_session` simply
    //      returns without side effects.
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

    // (2) Reasoning verdict BEFORE any chat-transport step (FR-122).
    //     The classify_for_start helper persists the rationale
    //     dispute-scoped (session_id = NULL) and returns the policy
    //     decision plus a handle we'll pass to
    //     `record_classification_for_session` once a session row
    //     exists. A negative verdict means we must not issue
    //     `TakeDispute` and no mediation_sessions row is ever
    //     committed — the dispute-scoped handoff stands in for the
    //     session-scoped one.
    let classify_outcome = policy::classify_for_start(
        params.conn,
        params.dispute_id,
        params.initiator_role,
        params.prompt_bundle,
        params.reasoning,
        params.provider_name,
        params.model_name,
    )
    .await?;

    // (2a) Record the reasoning verdict dispute-scoped regardless
    //      of outcome. Operators auditing a take that never
    //      happened need to see exactly why reasoning said no.
    {
        let now = current_ts_secs()?;
        let payload = match &classify_outcome.decision {
            PolicyDecision::AskClarification { .. } => {
                serde_json::json!({
                    "dispute_id": params.dispute_id,
                    "decision": "ask_clarification",
                })
            }
            PolicyDecision::Summarize {
                classification,
                confidence,
            } => serde_json::json!({
                "dispute_id": params.dispute_id,
                "decision": "summarize",
                "classification": classification.to_string(),
                "confidence": confidence,
            }),
            PolicyDecision::Escalate(trigger) => serde_json::json!({
                "dispute_id": params.dispute_id,
                "decision": "escalate",
                "trigger": trigger.to_string(),
            }),
        };
        let guard = params.conn.lock().await;
        if let Err(e) = db::mediation_events::record_event(
            &guard,
            db::mediation_events::MediationEventKind::ReasoningVerdict,
            None,
            &payload.to_string(),
            None,
            Some(&params.prompt_bundle.id),
            Some(&params.prompt_bundle.policy_hash),
            now,
        ) {
            warn!(
                dispute_id = %params.dispute_id,
                error = %e,
                "failed to record dispute-scoped reasoning_verdict event"
            );
        }
    }

    // (2b) Escalate branch: dispute-scoped handoff, no take, no
    //      session row. FR-122 step 3.
    if let PolicyDecision::Escalate(trigger) = &classify_outcome.decision {
        let trigger = *trigger;
        // Record start_attempt_stopped for the audit trail — a
        // dashboard reader can then tell "never took because policy
        // said escalate" apart from "never took because reasoning
        // was unhealthy".
        {
            let now = current_ts_secs()?;
            let payload = serde_json::json!({
                "dispute_id": params.dispute_id,
                "reason": "policy_escalate",
                "trigger": trigger.to_string(),
            })
            .to_string();
            let guard = params.conn.lock().await;
            if let Err(e) = db::mediation_events::record_event(
                &guard,
                db::mediation_events::MediationEventKind::StartAttemptStopped,
                None,
                &payload,
                None,
                Some(&params.prompt_bundle.id),
                Some(&params.prompt_bundle.policy_hash),
                now,
            ) {
                warn!(
                    dispute_id = %params.dispute_id,
                    error = %e,
                    "failed to record dispute-scoped start_attempt_stopped event"
                );
            }
        }
        // Dispute-scoped handoff. `session_id = None` tells
        // `escalation::recommend` to skip the state flip and write
        // the escalation_recommended / handoff_prepared rows as
        // dispute-scoped events.
        let rationale_refs = classify_outcome
            .rationale_audit
            .as_ref()
            .map(|a| vec![a.rationale_id.clone()])
            .unwrap_or_default();
        if let Err(e) = super::escalation::recommend(super::escalation::RecommendParams {
            conn: params.conn,
            session_id: None,
            dispute_id: params.dispute_id,
            trigger,
            evidence_refs: Vec::new(),
            rationale_refs,
            prompt_bundle_id: &params.prompt_bundle.id,
            policy_hash: &params.prompt_bundle.policy_hash,
        })
        .await
        {
            warn!(
                dispute_id = %params.dispute_id,
                error = %e,
                "escalation::recommend (dispute-scoped) failed on opening-call escalate"
            );
            return Err(e);
        }
        info!(
            dispute_id = %params.dispute_id,
            trigger = %trigger,
            "opening-call escalate: dispute-scoped handoff recorded, no take"
        );
        return Ok(OpenOutcome::EscalatedBeforeTake {
            dispute_id: params.dispute_id.to_string(),
            trigger,
        });
    }

    // (3) Take-dispute exchange (positive verdict only). If it
    //     fails we haven't committed anything else, so a caller
    //     retry is safe. On take failure we record the outcome
    //     dispute-scoped so the audit trail captures the reason.
    let material = match dispute_chat_flow::run_take_flow(dispute_chat_flow::TakeFlowParams {
        client: params.client,
        serbero_keys: params.serbero_keys,
        mostro_pubkey: params.mostro_pubkey,
        dispute_id: params.dispute_uuid,
        timeout: params.take_flow_timeout,
        poll_interval: params.take_flow_poll_interval,
    })
    .await
    {
        Ok(m) => {
            // Take success — audit row precedes the session row so
            // the event-id ordering invariant holds
            // (start_attempt_started < reasoning_verdict <
            // take_dispute_issued{success} < session_opened).
            let now = current_ts_secs()?;
            let payload = serde_json::json!({
                "dispute_id": params.dispute_id,
                "outcome": "success",
            })
            .to_string();
            let guard = params.conn.lock().await;
            if let Err(e) = db::mediation_events::record_event(
                &guard,
                db::mediation_events::MediationEventKind::TakeDisputeIssued,
                None,
                &payload,
                None,
                Some(&params.prompt_bundle.id),
                Some(&params.prompt_bundle.policy_hash),
                now,
            ) {
                warn!(
                    dispute_id = %params.dispute_id,
                    error = %e,
                    "failed to record take_dispute_issued{{success}} event"
                );
            }
            m
        }
        Err(e) => {
            // Take failure — capture the reason dispute-scoped, no
            // session row committed, bubble the error up for the
            // caller to translate into StartOutcome::TakeFailed.
            let now = current_ts_secs()?;
            let payload = serde_json::json!({
                "dispute_id": params.dispute_id,
                "outcome": "failure",
                "reason": e.to_string(),
            })
            .to_string();
            let guard = params.conn.lock().await;
            if let Err(db_err) = db::mediation_events::record_event(
                &guard,
                db::mediation_events::MediationEventKind::TakeDisputeIssued,
                None,
                &payload,
                None,
                Some(&params.prompt_bundle.id),
                Some(&params.prompt_bundle.policy_hash),
                now,
            ) {
                warn!(
                    dispute_id = %params.dispute_id,
                    error = %db_err,
                    "failed to record take_dispute_issued{{failure}} event"
                );
            }
            return Err(e);
        }
    };

    // (4) Insert session row + `session_opened` audit atomically.
    //     Re-checks the step-1 gate under the same connection so a
    //     concurrent opener cannot insert two rows for the same
    //     dispute.
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

    // (5) Re-bind the rationale to the session row and emit the
    //     session-scoped `classification_produced` audit event.
    //     Unwrap: reasoning path without a rationale only exits via
    //     the Escalate branch above; reaching this point with
    //     `None` would be a policy-layer bug.
    let rationale_audit = classify_outcome
        .rationale_audit
        .expect("classify_for_start must return a rationale_audit on non-Escalate paths");
    policy::record_classification_for_session(
        params.conn,
        &session_id,
        &rationale_audit,
        params.prompt_bundle,
    )
    .await?;

    let decision = classify_outcome.decision;

    // (5) Dispatch on the policy decision.
    match decision {
        PolicyDecision::AskClarification {
            buyer_text,
            seller_text,
        } => {
            if let Err(e) = super::draft_and_send_initial_message(
                params.conn,
                params.client,
                params.serbero_keys,
                &session_id,
                &material.buyer_shared_keys,
                &material.seller_shared_keys,
                params.prompt_bundle,
                &buyer_text,
                &seller_text,
            )
            .await
            {
                // T071: if the outbound publish surfaced an
                // `AuthorizationLost` we MUST escalate the session
                // with the matching trigger AND signal the
                // auth-retry handle so it re-arms. Any other error
                // bubbles up as before.
                if let Error::AuthorizationLost(ref msg) = e {
                    handle_authorization_lost(
                        params.conn,
                        params.client,
                        params.solvers,
                        params.dispute_id,
                        &session_id,
                        params.auth_handle,
                        params.prompt_bundle,
                        msg,
                    )
                    .await;
                }
                return Err(e);
            }
            // Register the derived chat material with the engine's
            // in-memory cache so the ingest tick (T051) can decrypt
            // inbound gift-wraps on this session. The cache is a
            // process-local `HashMap` — the raw secrets never touch
            // disk (see `dispute_chat_flow` key-lifecycle doc). A
            // daemon restart therefore loses the material; T052
            // handles that by either re-deriving or escalating.
            if let Some(cache) = params.session_key_cache {
                register_session_material(cache, &session_id, material.clone()).await;
            }
            info!(
                session_id = %session_id,
                prompt_bundle_id = %params.prompt_bundle.id,
                policy_hash = %params.prompt_bundle.policy_hash,
                "mediation session opened; first clarifying message dispatched to both parties"
            );
            Ok(OpenOutcome::Opened { session_id })
        }
        PolicyDecision::Summarize {
            classification,
            confidence,
        } => {
            // Cooperative-summary path (US3 / T060). The session is
            // already persisted at `awaiting_response`; transition
            // it to `classified` so the engine's
            // `ClassifiedPending → SummaryPending` chain in
            // `deliver_summary` has a legal starting state.
            // We do NOT call `draft_and_send_initial_message` —
            // the cooperative path skips the clarifying-question
            // round entirely and moves straight to the summary.
            let now = current_ts_secs()?;
            {
                let guard = params.conn.lock().await;
                db::mediation::set_session_state(
                    &guard,
                    &session_id,
                    crate::models::mediation::MediationSessionState::Classified,
                    now,
                )?;
            }
            // Register the derived chat material in the cache even
            // on the cooperative path — US2 / future slices may
            // still want to observe party replies against this
            // session after the summary is delivered.
            if let Some(cache) = params.session_key_cache {
                register_session_material(cache, &session_id, material.clone()).await;
            }
            info!(
                session_id = %session_id,
                classification = %classification,
                confidence = confidence,
                "mediation session opened; ready for cooperative summary"
            );
            Ok(OpenOutcome::ReadyForSummary {
                session_id,
                classification,
                confidence,
            })
        }
        PolicyDecision::Escalate(_) => {
            // Unreachable: the FR-122 flow above handles the
            // Escalate verdict before the take step and returns
            // `EscalatedBeforeTake`. We must never insert a session
            // row for an Escalate verdict — if control reaches
            // here, a later refactor accidentally moved the
            // Escalate short-circuit and produced an orphan session.
            unreachable!(
                "Escalate must be handled in the FR-122 pre-take branch; \
                 reaching the post-insert dispatch is a refactor bug"
            );
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
            inner_event_id = %envelope.inner_event_id,
            "inbound_duplicate"
        );
        return Ok(IngestOutcome::Duplicate);
    }

    if is_stale {
        tx.commit()?;
        debug!(
            session_id = %session_id,
            inner_event_id = %envelope.inner_event_id,
            stale = true,
            "inbound_ingested_stale"
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
        "inbound_ingested"
    );
    Ok(IngestOutcome::Fresh { round_count_after })
}

async fn register_session_material(
    cache: &SessionKeyCache,
    session_id: &str,
    material: DisputeChatMaterial,
) {
    let mut guard = cache.lock().await;
    guard.insert(session_id.to_string(), material);
}

/// T071 / T072 / T074 — dispatch every mid-session
/// authorization-loss action through one call:
///
/// 1. `tracing::error!` with the underlying error message so
///    operator dashboards can see the loss immediately.
/// 2. [`super::auth_retry::AuthRetryHandle::signal_auth_lost`] —
///    flips the in-memory state so future `open_session` attempts
///    refuse with `RefusedAuthPending` (SC-105 keeps Phase 1/2
///    detection unaffected).
/// 3. [`super::escalation::recommend`] with
///    [`crate::models::mediation::EscalationTrigger::AuthorizationLost`]
///    — flips the session to `escalation_recommended` and assembles
///    the Phase 4 handoff package.
/// 4. [`super::notify_solvers_escalation`] — delivers the
///    `MediationEscalationRecommended` gift-wrap DM to the
///    configured solver(s). Only runs if (3) succeeded.
///
/// Extracted from the inline outbound-failure handler in
/// [`open_session`] so integration tests can exercise the "auth
/// lost mid-session" scenario without simulating the full outbound
/// pipeline, and so a regression that forgets any of (2)-(4)
/// surfaces as a single-call test failure.
#[allow(clippy::too_many_arguments)]
pub async fn handle_authorization_lost(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    solvers: &[crate::models::SolverConfig],
    dispute_id: &str,
    session_id: &str,
    auth_handle: &super::auth_retry::AuthRetryHandle,
    prompt_bundle: &Arc<PromptBundle>,
    error_msg: &str,
) {
    tracing::error!(
        session_id = %session_id,
        error = %error_msg,
        "authorization_lost"
    );
    auth_handle.signal_auth_lost();
    match super::escalation::recommend(super::escalation::RecommendParams {
        conn,
        session_id: Some(session_id),
        dispute_id,
        trigger: crate::models::mediation::EscalationTrigger::AuthorizationLost,
        evidence_refs: Vec::new(),
        rationale_refs: Vec::new(),
        prompt_bundle_id: &prompt_bundle.id,
        policy_hash: &prompt_bundle.policy_hash,
    })
    .await
    {
        Ok(()) => {
            super::notify_solvers_escalation(
                conn,
                client,
                solvers,
                dispute_id,
                session_id,
                crate::models::mediation::EscalationTrigger::AuthorizationLost,
            )
            .await;
        }
        Err(esc_err) => {
            warn!(
                session_id = %session_id,
                error = %esc_err,
                "handle_authorization_lost: escalation::recommend failed"
            );
        }
    }
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

// `current_ts_secs` moved to `super::current_ts_secs` so
// `session.rs`, `summarizer.rs`, and the deliver-summary path in
// `mediation/mod.rs` all share one implementation of the
// clock-before-UNIX-EPOCH guard.
use super::current_ts_secs;

/// Round-limit gate (T068). Returns `true` when the session has
/// reached or exceeded `max_rounds` without a cooperative
/// resolution — the engine should escalate with
/// `EscalationTrigger::RoundLimit`.
///
/// Pure: takes the round count and the configured cap directly so
/// the check has zero DB cost. The caller (engine) reads the
/// round count from the session row or keeps it in hand from the
/// latest `ingest_inbound(...) -> Fresh { round_count_after }`
/// return.
///
/// Evaluation sites per spec §FR-111:
/// - After every `ingest_inbound` that returns
///   `IngestOutcome::Fresh` — round just completed, check whether
///   the cap is hit.
/// - After every `policy::evaluate` that returns
///   `AskClarification` — another clarification round is about
///   to start, check whether we have budget for it.
pub fn check_round_limit(round_count: u32, max_rounds: u32) -> bool {
    round_count >= max_rounds
}

#[cfg(test)]
mod round_limit_tests {
    use super::check_round_limit;

    #[test]
    fn at_limit_returns_true() {
        assert!(check_round_limit(3, 3));
    }

    #[test]
    fn below_limit_returns_false() {
        assert!(!check_round_limit(2, 3));
    }

    #[test]
    fn above_limit_returns_true() {
        // Shouldn't normally happen (caller guards with this
        // function on every round-boundary), but if it does the
        // gate must still fire.
        assert!(check_round_limit(4, 3));
    }

    #[test]
    fn zero_max_always_true() {
        // Degenerate config: max_rounds = 0 means "don't even try
        // the first clarifying round". `0 >= 0` fires immediately.
        assert!(check_round_limit(0, 0));
    }
}

#[cfg(test)]
mod tests {
    //! Auth-gate coverage for [`open_session`] (T043).
    //!
    //! These tests pin that `RefusedAuthPending` and
    //! `RefusedAuthTerminated` short-circuit **before** any DB
    //! write or relay I/O. All other fields of `OpenSessionParams`
    //! are dummies because the gate returns on the first line; any
    //! regression that lets execution past the gate would either
    //! panic on the mock reasoning provider or return a distinct
    //! error, which is itself detectable.
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::mediation::auth_retry::{AuthRetryHandle, AuthState};
    use crate::models::reasoning::{
        ClassificationRequest, ClassificationResponse, ReasoningError, SummaryRequest,
        SummaryResponse,
    };
    use async_trait::async_trait;

    /// Reasoning stub that panics on use — guarantees the gate
    /// returned before the reasoning path was ever reached.
    struct PanicReasoning;

    #[async_trait]
    impl ReasoningProvider for PanicReasoning {
        async fn classify(
            &self,
            _request: ClassificationRequest,
        ) -> std::result::Result<ClassificationResponse, ReasoningError> {
            panic!("auth gate must refuse before classify is called");
        }
        async fn summarize(
            &self,
            _request: SummaryRequest,
        ) -> std::result::Result<SummaryResponse, ReasoningError> {
            panic!("auth gate must refuse before summarize is called");
        }
        async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
            panic!("auth gate must refuse before health_check is called");
        }
    }

    fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        // Seed a parent `disputes` row so any accidental foreign-key
        // write past the gate would surface as a distinct error
        // rather than a FK-constraint panic that could be mistaken
        // for the gate working.
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('dispute-auth-gate', 'e1', 'm1', 'buyer',
                       'initiated', 0, 0, 'notified')",
            [],
        )
        .unwrap();
        Arc::new(AsyncMutex::new(conn))
    }

    fn fresh_bundle() -> Arc<PromptBundle> {
        Arc::new(PromptBundle {
            id: "phase3-default".into(),
            policy_hash: "test-policy-hash".into(),
            system: "sys".into(),
            classification: "cls".into(),
            escalation: "esc".into(),
            mediation_style: "style".into(),
            message_templates: "tpl".into(),
        })
    }

    async fn run_gate_with(
        auth_handle: &AuthRetryHandle,
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    ) -> OpenOutcome {
        let serbero_keys = Keys::generate();
        let mostro_pk = Keys::generate().public_key();
        let client = Client::new(Keys::generate());
        let reasoning = PanicReasoning;
        let bundle = fresh_bundle();
        open_session(OpenSessionParams {
            conn,
            client: &client,
            serbero_keys: &serbero_keys,
            mostro_pubkey: &mostro_pk,
            reasoning: &reasoning,
            prompt_bundle: &bundle,
            dispute_id: "dispute-auth-gate",
            initiator_role: InitiatorRole::Buyer,
            dispute_uuid: Uuid::new_v4(),
            take_flow_timeout: Duration::from_secs(1),
            take_flow_poll_interval: Duration::from_millis(50),
            provider_name: "mock-provider",
            model_name: "mock-model",
            auth_handle,
            session_key_cache: None,
            solvers: &[],
        })
        .await
        .expect("auth-gate path must not return Err")
    }

    async fn mediation_row_counts(conn: &Arc<AsyncMutex<rusqlite::Connection>>) -> (i64, i64, i64) {
        let guard = conn.lock().await;
        let sessions: i64 = guard
            .query_row("SELECT COUNT(*) FROM mediation_sessions", [], |r| r.get(0))
            .unwrap();
        let messages: i64 = guard
            .query_row("SELECT COUNT(*) FROM mediation_messages", [], |r| r.get(0))
            .unwrap();
        let events: i64 = guard
            .query_row("SELECT COUNT(*) FROM mediation_events", [], |r| r.get(0))
            .unwrap();
        (sessions, messages, events)
    }

    #[tokio::test]
    async fn unauthorized_gate_refuses_with_pending_and_writes_nothing() {
        let conn = fresh_conn();
        let handle = AuthRetryHandle::with_state_for_testing(AuthState::Unauthorized);
        let outcome = run_gate_with(&handle, &conn).await;
        match outcome {
            OpenOutcome::RefusedAuthPending { reason } => {
                assert!(reason.contains("pending"), "unexpected reason: {reason}");
            }
            other => panic!("expected RefusedAuthPending, got {other:?}"),
        }
        let (sessions, messages, events) = mediation_row_counts(&conn).await;
        assert_eq!(sessions, 0, "no mediation_sessions row may be written");
        assert_eq!(messages, 0, "no mediation_messages row may be written");
        assert_eq!(events, 0, "no mediation_events row may be written");
    }

    #[tokio::test]
    async fn terminated_gate_refuses_with_terminated_and_writes_nothing() {
        let conn = fresh_conn();
        let handle = AuthRetryHandle::with_state_for_testing(AuthState::Terminated);
        let outcome = run_gate_with(&handle, &conn).await;
        match outcome {
            OpenOutcome::RefusedAuthTerminated { reason } => {
                assert!(reason.contains("terminated"), "unexpected reason: {reason}");
            }
            other => panic!("expected RefusedAuthTerminated, got {other:?}"),
        }
        let (sessions, messages, events) = mediation_row_counts(&conn).await;
        assert_eq!(sessions, 0);
        assert_eq!(messages, 0);
        assert_eq!(events, 0);
    }
}
