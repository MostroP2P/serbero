//! Mid-session follow-up loop (T120 / FR-125–FR-131).
//!
//! [`advance_session_round`] is the orchestrator that the Phase 11
//! hook in `run_ingest_tick` (T121) calls once per session per
//! ingest cycle where a fresh inbound landed. It is the first — and
//! for now only — production call site of [`policy::evaluate`], the
//! missing trigger the 2026-04-21 audit identified.
//!
//! # Flow
//!
//! 1. Load session metadata (state, `round_count_last_evaluated`,
//!    `dispute_id`) and count the session's total fresh (non-stale)
//!    inbound rows. Short-circuit if the row is gone, the state is
//!    not `awaiting_response`, or the idempotency gate says every
//!    fresh inbound has already been classified (FR-127 — compares
//!    the live fresh-inbound count to `round_count_last_evaluated`,
//!    which counts evaluations, not completed rounds).
//! 2. Load the per-party chat material from the in-memory
//!    [`SessionKeyCache`]. Skip with a `debug!` if missing — the
//!    T052 restart-resume pass does not re-derive in production,
//!    so a freshly-restarted daemon may still have live sessions
//!    with no cache entry until a future slice reconstructs them.
//! 3. Load the dispute's `initiator_role` — needed to build the
//!    [`ClassificationRequest`].
//! 4. Load the transcript via [`transcript::load_transcript_for_session`]
//!    (FR-128; cap hardcoded at 40).
//! 5. Call [`ReasoningProvider::classify`]. On any error, bump the
//!    consecutive-failure counter (T118). On the third failure
//!    escalate the session with `ReasoningUnavailable` (FR-130)
//!    and return without further action.
//! 6. Hand the `ClassificationResponse` to [`policy::evaluate`] —
//!    the policy layer persists the rationale (audit store) and
//!    the session-scoped `classification_produced` event in its
//!    own transaction.
//! 7. Dispatch on the returned [`PolicyDecision`]:
//!    - `AskClarification(text)` → [`draft_and_send_followup_message`].
//!      The drafter commits the two outbound rows and the
//!      evaluator-marker advance in one transaction, then publishes
//!      the gift-wraps outside the transaction.
//!    - `Summarize { classification, confidence }` →
//!      [`deliver_summary`] owns the cooperative-summary progression
//!      (`awaiting_response → classified → summary_pending →
//!      summary_delivered`). The legal `summary_delivered → closed`
//!      transition is intentionally NOT taken here so the
//!      eligibility predicate keeps blocking re-mediation; it fires
//!      later from the `dispute_resolved` handler when Mostro
//!      closes the dispute. After `deliver_summary` returns `Ok`,
//!      we advance the marker in a separate, short-lived
//!      transaction because `deliver_summary` owns its own
//!      transaction scope.
//!    - `Escalate(trigger)` → [`escalation::recommend`] transitions
//!      the session to `escalation_recommended` and records the
//!      handoff. The marker is irrelevant after that — the session
//!      is leaving `awaiting_response` permanently.
//!
//! # Failure isolation
//!
//! Any error past the classify call calls
//! [`bump_consecutive_eval_failures`] and returns `Ok(())` so the
//! engine tick keeps running for other sessions. Three consecutive
//! failures escalate with `ReasoningUnavailable` (FR-130).
//!
//! The function never panics, never spawns a task, and holds the
//! async connection mutex only for the short stretches where a read
//! or a transaction is in flight — so other concurrent ingest-tick
//! work (e.g. per-session fetches done by the caller before invoking
//! us) keeps making progress.

use std::sync::Arc;

use nostr_sdk::prelude::{Client, Keys};
use rusqlite::params;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info, instrument, warn};

use crate::db;
use crate::error::Result;
use crate::models::dispute::InitiatorRole;
use crate::models::mediation::{EscalationTrigger, MediationSessionState};
use crate::models::reasoning::{ClassificationRequest, ReasoningContext};
use crate::models::{MediationConfig, SolverConfig};
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

use super::{
    deliver_summary, draft_and_send_followup_message, escalation, notify_solvers_escalation,
    policy, self_resolution, transcript, SessionKeyCache,
};
use crate::chat::outbound;

/// Hard cap on transcript rows passed to the classifier (FR-128).
/// Guards against runaway token costs on a session that accumulates
/// an unbounded number of messages. Kept hardcoded for this
/// increment; `spec.md` §"Non-Goals (Phase 11)" promises config
/// promotion to a later slice.
const TRANSCRIPT_CAP: usize = 40;

/// Number of consecutive failed evaluations that trigger an
/// automatic escalation with `ReasoningUnavailable` (FR-130).
const CONSECUTIVE_FAILURE_ESCALATION_THRESHOLD: i64 = 3;

/// Drive one mid-session round for one session.
///
/// The function is infallible from the caller's perspective: every
/// error path is absorbed locally (log + bump failure counter + for
/// the extreme case, escalate). Returning `Result` exists only so
/// the engine-tick caller can use `?` if we ever decide a
/// specific class of failure (e.g. DB lock poisoning) is too
/// serious to swallow. Today the implementation only returns `Err`
/// when the connection itself is broken — per-session logic
/// failures never bubble up.
#[instrument(skip_all, fields(session_id = %session_id))]
#[allow(clippy::too_many_arguments)]
pub async fn advance_session_round(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    serbero_keys: &Keys,
    reasoning: &dyn ReasoningProvider,
    prompt_bundle: &Arc<PromptBundle>,
    session_id: &str,
    session_key_cache: &SessionKeyCache,
    solvers: &[SolverConfig],
    provider_name: &str,
    model_name: &str,
    mediation_cfg: &MediationConfig,
) -> Result<()> {
    // (1) Load session metadata + idempotency gate.
    let info = match load_session_info(conn, session_id).await? {
        Some(i) => i,
        None => {
            debug!("advance_session_round: session row not found; skipping");
            return Ok(());
        }
    };
    // State gate: normally the session must be in `awaiting_response`.
    // Feature 005 carve-out: a session in `summary_delivered` that
    // received the cooperative invitation is still re-classifiable so
    // the human-assistance opt-in path (FR-008) can fire when a party
    // reply arrives after the summary. The carve-out is scoped by the
    // `self_resolution_offered` audit row — a legacy
    // `summary_delivered` session without that row is still treated
    // as terminal.
    let is_post_invitation_summary_delivered =
        matches!(info.state, MediationSessionState::SummaryDelivered) && {
            let guard = conn.lock().await;
            db::mediation_events::session_has_self_resolution_offered(&guard, session_id)?
        };
    if !matches!(info.state, MediationSessionState::AwaitingResponse)
        && !is_post_invitation_summary_delivered
    {
        debug!(
            state = %info.state,
            "advance_session_round: session not in awaiting_response (and not a post-invitation summary_delivered); skipping"
        );
        return Ok(());
    }
    // Gate on total fresh inbounds, not `round_count`.
    //
    // `round_count` only increments when BOTH parties have replied
    // (it's `min(buyer_fresh, seller_fresh)`), so a single-party
    // reply — the common mid-session case — would never cross a
    // `round_count`-based gate. FR-127 asks us to re-evaluate after
    // ANY fresh inbound, so we compare the total fresh-inbound count
    // against `round_count_last_evaluated` (reinterpreted as "count
    // of fresh inbounds already classified"; see the helper's
    // docstring in `db::mediation`).
    let total_fresh_inbounds = {
        let guard = conn.lock().await;
        db::mediation::count_fresh_inbounds(&guard, session_id)?
    };
    if total_fresh_inbounds <= info.round_count_last_evaluated {
        debug!(
            total_fresh_inbounds,
            round_count_last_evaluated = info.round_count_last_evaluated,
            "advance_session_round: no new fresh inbounds since last evaluation; skipping"
        );
        return Ok(());
    }

    // (2) Per-party chat material from the in-memory cache.
    //     Absent material usually means this session was opened
    //     before the current process started and T052's
    //     restart-resume pass could not re-derive — a known
    //     limitation documented alongside that pass. Skip with a
    //     debug! so the tick moves on; on a later restart that
    //     does re-derive (or when a new session opens), the loop
    //     will be reachable again.
    let material = {
        let cache = session_key_cache.lock().await;
        cache.get(session_id).cloned()
    };
    let Some(material) = material else {
        debug!("advance_session_round: no chat material in cache (post-restart?); skipping");
        return Ok(());
    };

    // (3) initiator_role from the dispute row. The session row
    //     doesn't carry it — only `dispute_id` — so we do one more
    //     short read. The disputes table is the source of truth.
    let initiator_role = match load_initiator_role(conn, &info.dispute_id).await? {
        Some(r) => r,
        None => {
            warn!(
                dispute_id = %info.dispute_id,
                "advance_session_round: dispute row vanished; skipping"
            );
            return Ok(());
        }
    };

    // (4) Transcript + cooperative-invitation flag (FR-008).
    //     The flag drives the conditional `human_requested`
    //     instruction block on the classifier prompt — only sessions
    //     that have already received the cooperative invitation pay
    //     the prompt-token cost of asking the model to detect
    //     human-assistance requests.
    let (transcript_entries, prior_self_resolution_offered) = {
        let guard = conn.lock().await;
        let entries = transcript::load_transcript_for_session(&guard, session_id, TRANSCRIPT_CAP)?;
        let flag = db::mediation_events::session_has_self_resolution_offered(&guard, session_id)?;
        (entries, flag)
    };

    // (5) Classify. On failure, bump + (maybe) escalate.
    let classification_req = ClassificationRequest {
        session_id: session_id.to_string(),
        dispute_id: info.dispute_id.clone(),
        initiator_role,
        prompt_bundle: Arc::clone(prompt_bundle),
        transcript: transcript_entries.clone(),
        context: ReasoningContext {
            round_count: info.round_count.max(0) as u32,
            // `current_classification` / `current_confidence` are
            // schema-defined but never written today; treat as
            // absent. A future slice can plumb them.
            last_classification: None,
            last_confidence: None,
            session_has_self_resolution_offered: prior_self_resolution_offered,
        },
    };
    let classification = match reasoning.classify(classification_req).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "advance_session_round: reasoning.classify failed");
            handle_reasoning_failure(
                conn,
                client,
                session_id,
                &info.dispute_id,
                solvers,
                prompt_bundle,
            )
            .await;
            return Ok(());
        }
    };

    // (6) Policy layer — persists the rationale + the
    //     `classification_produced` audit row in its own tx.
    // `followup_number` is 1-based — the ordinal of the mid-session
    // evaluation currently in flight. We derive it from the live
    // count of `classification_produced` events (the initial
    // classification counts as 1, so the first mid-session
    // evaluation sees a count of 1 → followup_number = 1). That
    // matches the [`PolicyRound::MidSession`] bypass-window test
    // semantics exactly. We deliberately do NOT reuse
    // `round_count_last_evaluated`: that column now counts fresh
    // inbounds (since the 2026-04-21 fix), so a tick that ingests
    // two replies at once would jump the ordinal by 2 and mislabel
    // the visible "Round N" prefix.
    let followup_number = {
        let guard = conn.lock().await;
        db::mediation::count_classification_events(&guard, session_id)?
    };
    let decision = match policy::evaluate(
        conn,
        session_id,
        prompt_bundle,
        provider_name,
        model_name,
        classification.clone(),
        followup_number,
        mediation_cfg,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "advance_session_round: policy::evaluate failed");
            handle_reasoning_failure(
                conn,
                client,
                session_id,
                &info.dispute_id,
                solvers,
                prompt_bundle,
            )
            .await;
            return Ok(());
        }
    };

    // (7) Dispatch.
    //
    // Feature 005 carve-out: when the session is in
    // `summary_delivered` (re-entered for the post-invitation
    // re-classification path), only an `Escalate` decision is
    // actionable. Any other decision would attempt to walk an
    // illegal transition (e.g. `summary_delivered → classified`).
    // The classification_produced audit row is already durable from
    // `policy::evaluate`, so a "wait silently" no-op is the right
    // outcome for a non-escalating reply.
    if is_post_invitation_summary_delivered
        && !matches!(decision, policy::PolicyDecision::Escalate(_))
    {
        // Advance the evaluator marker before returning. Otherwise
        // FR-127's idempotency gate at the top of the next tick
        // would still see `total_fresh_inbounds > round_count_last_evaluated`
        // and re-classify the same reply on every cycle — burning
        // reasoning-provider budget on a session that's already
        // settled into "wait silently for a possible human-assistance
        // request".
        let mut guard = conn.lock().await;
        let tx = guard.transaction()?;
        db::mediation::advance_evaluator_marker(&tx, session_id, total_fresh_inbounds)?;
        tx.commit()?;
        debug!(
            state = %info.state,
            ?decision,
            round_count_marked = total_fresh_inbounds,
            "advance_session_round: post-invitation reply did not request human; staying in summary_delivered (marker advanced)"
        );
        return Ok(());
    }
    match decision {
        policy::PolicyDecision::AskClarification {
            buyer_text,
            seller_text,
        } => {
            let new_marker = total_fresh_inbounds;
            // Same ordinal we used to gate the bypass window —
            // the "Round N" body label should match "this is the
            // N-th mid-session evaluation we've run", not the
            // number of fresh inbounds classified (see the block
            // above for why).
            let round_number = followup_number;
            if let Err(e) = draft_and_send_followup_message(
                conn,
                client,
                serbero_keys,
                session_id,
                round_number,
                new_marker,
                &material.buyer_shared_keys,
                &material.seller_shared_keys,
                prompt_bundle,
                &buyer_text,
                &seller_text,
            )
            .await
            {
                warn!(
                    error = %e,
                    "advance_session_round: follow-up drafter failed; rows may be committed without publish"
                );
                handle_reasoning_failure(
                    conn,
                    client,
                    session_id,
                    &info.dispute_id,
                    solvers,
                    prompt_bundle,
                )
                .await;
                return Ok(());
            }
            info!(
                round = round_number,
                round_count_marked = new_marker,
                "advance_session_round: AskClarification dispatched"
            );
        }
        policy::PolicyDecision::Summarize {
            classification,
            confidence,
        } => {
            // `deliver_summary` begins with a `classified →
            // summary_pending` transition, so we must pre-flip the
            // session from `awaiting_response` to `classified`
            // first. This creates a brief window (the summarizer
            // call + routing, typically a few seconds) where the
            // session is in `classified` without `round_count_last_evaluated`
            // having been advanced. If the daemon crashes inside
            // that window, the next ingest tick skips this session
            // (state gate rejects `classified`). This is a
            // documented Phase 11 limitation — see spec.md
            // §"Non-Goals (Phase 11)" regarding crash recovery
            // during mid-session dispatch.
            {
                let guard = conn.lock().await;
                db::mediation::set_session_state(
                    &guard,
                    session_id,
                    MediationSessionState::Classified,
                    super::current_ts_secs()?,
                )?;
            }
            if let Err(e) = deliver_summary(
                conn,
                client,
                serbero_keys,
                session_id,
                &info.dispute_id,
                classification,
                confidence,
                transcript_entries,
                prompt_bundle,
                reasoning,
                solvers,
                provider_name,
                model_name,
            )
            .await
            {
                warn!(error = %e, "advance_session_round: deliver_summary failed");
                handle_reasoning_failure(
                    conn,
                    client,
                    session_id,
                    &info.dispute_id,
                    solvers,
                    prompt_bundle,
                )
                .await;
                return Ok(());
            }
            // Mark the round evaluated. The session has just landed
            // in `summary_delivered` (the legal `summary_delivered →
            // closed` transition is deferred to the
            // `dispute_resolved` handler so the eligibility predicate
            // keeps blocking re-mediation), but keeping the marker
            // current is a cheap invariant either way — a future
            // tick never mistakes an evaluated round for an
            // unevaluated one.
            let new_marker = total_fresh_inbounds;
            let mut guard = conn.lock().await;
            let tx = guard.transaction()?;
            db::mediation::advance_evaluator_marker(&tx, session_id, new_marker)?;
            tx.commit()?;
            info!(
                round_count_marked = new_marker,
                "advance_session_round: Summarize dispatched"
            );
        }
        policy::PolicyDecision::SuggestSelfResolutionWithSummary { confidence } => {
            if classification.seller_confirmed_fiat_receipt != Some(true) {
                // FR-015 / spec.md:140-146 — buyer-only fiat claim
                // without seller-side receipt corroboration is NOT
                // enough to fire the cooperative invitation. We must
                // also avoid walking the session to a terminal
                // `summary_delivered` here, because the spec promises
                // that "if the seller later confirms the fiat
                // arrived, the invitation becomes eligible on that
                // later round". Leave the session in
                // `awaiting_response`, advance the evaluator marker
                // so this same fresh-inbound count doesn't keep
                // re-classifying, and let the next round re-enter
                // cleanly.
                let new_marker = total_fresh_inbounds;
                let mut guard = conn.lock().await;
                let tx = guard.transaction()?;
                db::mediation::advance_evaluator_marker(&tx, session_id, new_marker)?;
                tx.commit()?;
                info!(
                    confidence,
                    seller_confirmed_fiat_receipt = ?classification.seller_confirmed_fiat_receipt,
                    round_count_marked = new_marker,
                    "advance_session_round: self-resolution invitation suppressed (no seller-side fiat-receipt corroboration); session stays awaiting_response for the next round"
                );
                return Ok(());
            }
            // Feature 005 dispatch: cooperative self-resolution
            // invitation. Order of operations matches the contract
            // in `specs/005-cooperative-self-resolution/contracts/audit-events.md`:
            //
            //  1. Resolve per-party language codes from the
            //     classifier's structured response.
            //  2. Render each party's invitation from the static
            //     bundle templates.
            //  3. Open a transaction: write the
            //     `self_resolution_offered` audit row + insert two
            //     `mediation_messages` rows (audience-tagged).
            //  4. Commit, then publish the gift-wraps OUTSIDE the
            //     transaction (matches the existing initial /
            //     follow-up drafter pattern — failure to publish
            //     leaves the rows committed as a historical record).
            //  5. Pre-flip `awaiting_response → classified` and call
            //     `deliver_summary` so the solver still receives the
            //     existing `mediation_summary` notification.
            let buyer_lang = classification.buyer_language.as_deref();
            let seller_lang = classification.seller_language.as_deref();
            // Pull the rationale id of the producing
            // classification — `policy::evaluate` already wrote it
            // before returning the decision.
            let rationale_id = {
                let guard = conn.lock().await;
                latest_classification_rationale_id(&guard, session_id)?
            };
            let dispatch_outcome = draft_and_send_self_resolution_invitation(
                conn,
                client,
                serbero_keys,
                session_id,
                confidence,
                buyer_lang,
                seller_lang,
                &material.buyer_shared_keys,
                &material.seller_shared_keys,
                prompt_bundle,
                rationale_id.as_deref(),
            )
            .await;
            let invitation_committed = match dispatch_outcome {
                Ok(committed) => committed,
                Err(e) => {
                    warn!(
                        error = %e,
                        "advance_session_round: self-resolution invitation drafter failed"
                    );
                    handle_reasoning_failure(
                        conn,
                        client,
                        session_id,
                        &info.dispute_id,
                        solvers,
                        prompt_bundle,
                    )
                    .await;
                    return Ok(());
                }
            };
            if !invitation_committed {
                // Defensive duplicate-detection path. The in-TX
                // re-check inside `draft_and_send_self_resolution_invitation`
                // saw a prior `self_resolution_offered` row for this
                // session — another path won the race and has
                // already (or will shortly) drive the
                // pre-flip + `deliver_summary`. Skip those steps
                // here so we don't double-summarize. Advance the
                // evaluator marker in a short transaction so this
                // tick doesn't keep re-classifying the same fresh
                // inbound forever.
                let new_marker = total_fresh_inbounds;
                let mut guard = conn.lock().await;
                let tx = guard.transaction()?;
                db::mediation::advance_evaluator_marker(&tx, session_id, new_marker)?;
                tx.commit()?;
                info!(
                    confidence,
                    round_count_marked = new_marker,
                    "advance_session_round: SuggestSelfResolutionWithSummary skipped (duplicate race)"
                );
                return Ok(());
            }
            // Pre-flip awaiting_response → classified so
            // `deliver_summary`'s `classified → summary_pending` is
            // a legal transition. Same pattern as the legacy
            // Summarize arm above.
            {
                let guard = conn.lock().await;
                db::mediation::set_session_state(
                    &guard,
                    session_id,
                    MediationSessionState::Classified,
                    super::current_ts_secs()?,
                )?;
            }
            if let Err(e) = deliver_summary(
                conn,
                client,
                serbero_keys,
                session_id,
                &info.dispute_id,
                crate::models::mediation::ClassificationLabel::CoordinationFailureResolvable,
                confidence,
                transcript_entries,
                prompt_bundle,
                reasoning,
                solvers,
                provider_name,
                model_name,
            )
            .await
            {
                warn!(
                    error = %e,
                    "advance_session_round: deliver_summary after self-resolution invitation failed"
                );
                // Revert the pre-flip so the session is retryable
                // on the next ingest tick. Without this, the
                // session sits in `classified` forever — the gate
                // at the top of `advance_session_round` only
                // accepts `awaiting_response` or
                // post-invitation `summary_delivered`. The state
                // machine permits `classified → awaiting_response`
                // as a recovery edge (see `models::mediation`).
                // A failure to revert is logged loudly but not
                // bubbled — `handle_reasoning_failure` still runs
                // so the consecutive-failure counter advances and
                // can eventually escalate.
                {
                    let now = match super::current_ts_secs() {
                        Ok(t) => t,
                        Err(ts_err) => {
                            warn!(
                                error = %ts_err,
                                "advance_session_round: clock unavailable; cannot revert state to awaiting_response"
                            );
                            handle_reasoning_failure(
                                conn,
                                client,
                                session_id,
                                &info.dispute_id,
                                solvers,
                                prompt_bundle,
                            )
                            .await;
                            return Ok(());
                        }
                    };
                    let guard = conn.lock().await;
                    if let Err(rev_err) = db::mediation::set_session_state(
                        &guard,
                        session_id,
                        MediationSessionState::AwaitingResponse,
                        now,
                    ) {
                        warn!(
                            error = %rev_err,
                            "advance_session_round: failed to revert classified → awaiting_response after deliver_summary failure"
                        );
                    }
                }
                handle_reasoning_failure(
                    conn,
                    client,
                    session_id,
                    &info.dispute_id,
                    solvers,
                    prompt_bundle,
                )
                .await;
                return Ok(());
            }
            let new_marker = total_fresh_inbounds;
            let mut guard = conn.lock().await;
            let tx = guard.transaction()?;
            db::mediation::advance_evaluator_marker(&tx, session_id, new_marker)?;
            tx.commit()?;
            info!(
                confidence,
                round_count_marked = new_marker,
                "advance_session_round: SuggestSelfResolutionWithSummary dispatched"
            );
        }
        policy::PolicyDecision::Escalate(trigger) => {
            if let Err(e) = escalation::recommend(escalation::RecommendParams {
                conn,
                session_id: Some(session_id),
                dispute_id: &info.dispute_id,
                trigger,
                evidence_refs: Vec::new(),
                rationale_refs: Vec::new(),
                prompt_bundle_id: &prompt_bundle.id,
                policy_hash: &prompt_bundle.policy_hash,
            })
            .await
            {
                warn!(
                    error = %e,
                    trigger = %trigger,
                    "advance_session_round: escalation::recommend failed"
                );
                handle_reasoning_failure(
                    conn,
                    client,
                    session_id,
                    &info.dispute_id,
                    solvers,
                    prompt_bundle,
                )
                .await;
                return Ok(());
            }
            notify_solvers_escalation(conn, client, solvers, &info.dispute_id, session_id, trigger)
                .await;
            info!(
                trigger = %trigger,
                "advance_session_round: Escalate dispatched"
            );
        }
    }

    Ok(())
}

/// Latest `classification_produced` rationale id for a session.
/// Used by the cooperative-self-resolution dispatch arm to populate
/// the `self_resolution_offered` audit row's `rationale_id` column —
/// the policy layer wrote the row a moment earlier, so this lookup
/// always succeeds in practice. Returns `None` defensively (older
/// sessions / missing audit) so the caller can still proceed
/// without a rationale reference rather than panic.
fn latest_classification_rationale_id(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<String>> {
    let row = conn.query_row(
        "SELECT rationale_id FROM mediation_events
         WHERE session_id = ?1 AND kind = 'classification_produced' AND rationale_id IS NOT NULL
         ORDER BY occurred_at DESC, id DESC LIMIT 1",
        params![session_id],
        |r| r.get::<_, Option<String>>(0),
    );
    match row {
        Ok(opt) => Ok(opt),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(crate::error::Error::Db(e)),
    }
}

/// Feature 005 — write the cooperative-self-resolution invitation
/// gift-wraps + audit row. Patterned on
/// [`super::draft_and_send_followup_message`]: opens a single
/// transaction for both `mediation_messages` rows + the
/// `self_resolution_offered` audit row, commits, then publishes the
/// gift-wraps OUTSIDE the transaction. A relay-side publish failure
/// after commit leaves the rows in place as historical record —
/// matches the existing drafter discipline (FR-126 Non-Goals).
///
/// Returns:
/// - `Ok(true)` — invitation committed and published.
/// - `Ok(false)` — duplicate detected at write time; in-TX
///   re-check found a `self_resolution_offered` row for the
///   session (another path won the race), the transaction was
///   rolled back without writing, no gift-wraps were published.
///   The caller MUST skip subsequent dispatch steps
///   (state pre-flip, `deliver_summary`) since the other path
///   already drove them.
/// - `Err(_)` — genuine failure; failure-counter path applies.
#[allow(clippy::too_many_arguments)]
async fn draft_and_send_self_resolution_invitation(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    serbero_keys: &Keys,
    session_id: &str,
    confidence: f64,
    buyer_language: Option<&str>,
    seller_language: Option<&str>,
    buyer_shared_keys: &Keys,
    seller_shared_keys: &Keys,
    prompt_bundle: &Arc<PromptBundle>,
    rationale_id: Option<&str>,
) -> Result<bool> {
    use crate::models::mediation::TranscriptParty;

    // Resolve the EFFECTIVE language each party will actually
    // receive (raw classifier code when the bundle has a matching
    // section; bundle's `fallback_language` otherwise). The audit
    // row below records these resolved codes — not the raw
    // classifier output — so a forensic replay can reproduce the
    // exact bytes each party saw without having to re-run the
    // resolver.
    let buyer_effective_language = prompt_bundle
        .self_resolution
        .resolve_effective_language(buyer_language)
        .map(|s| s.to_string());
    let seller_effective_language = prompt_bundle
        .self_resolution
        .resolve_effective_language(seller_language)
        .map(|s| s.to_string());

    let buyer_msg = match self_resolution::render_for(
        buyer_language,
        &prompt_bundle.self_resolution,
    ) {
        Some(s) => s,
        None => {
            // Structurally invalid bundle (no requested-language
            // entry AND no fallback entry). The parser rejects this
            // at load time and `policy::evaluate` gates on
            // `templates_present`, so this is unreachable in normal
            // flow; we return `Ok(false)` rather than panic so the
            // dispatch caller skips the publishes + state walk
            // cleanly. Skipping is safer than emitting a diagnostic
            // operator-message into a party's chat.
            warn!(
                session_id = %session_id,
                "draft_and_send_self_resolution_invitation: bundle is missing fallback-language section; \
                 skipping cooperative invitation"
            );
            return Ok(false);
        }
    };
    let seller_msg = match self_resolution::render_for(
        seller_language,
        &prompt_bundle.self_resolution,
    ) {
        Some(s) => s,
        None => {
            warn!(
                session_id = %session_id,
                "draft_and_send_self_resolution_invitation: bundle is missing fallback-language section; \
                 skipping cooperative invitation"
            );
            return Ok(false);
        }
    };

    let buyer_wrap = outbound::build_wrap_with_audience(
        serbero_keys,
        &buyer_shared_keys.public_key(),
        &buyer_msg,
        Some("buyer"),
    )
    .await?;
    let seller_wrap = outbound::build_wrap_with_audience(
        serbero_keys,
        &seller_shared_keys.public_key(),
        &seller_msg,
        Some("seller"),
    )
    .await?;

    if buyer_wrap.inner_event_id == seller_wrap.inner_event_id {
        return Err(crate::error::Error::ChatTransport(
            "inner event ids collided across parties on cooperative invitation; refusing to \
             persist rows that would violate the dedup invariant"
                .into(),
        ));
    }

    let buyer_shared_pubkey_hex = buyer_shared_keys.public_key().to_hex();
    let seller_shared_pubkey_hex = seller_shared_keys.public_key().to_hex();
    let buyer_inner_id_hex = buyer_wrap.inner_event_id.to_hex();
    let seller_inner_id_hex = seller_wrap.inner_event_id.to_hex();
    let now = super::current_ts_secs()?;

    let committed = {
        let mut guard = conn.lock().await;
        let tx = guard.transaction()?;

        // Defensive in-TX re-check (belt-and-braces against the
        // TOCTOU window that exists on paper between the predicate
        // read in `policy::evaluate` and this write site). The
        // single-process engine architecture already serialises
        // these calls per session via the global `AsyncMutex` on
        // `Connection` plus the sequential per-session loop in
        // `run_ingest_tick`, but having the guard at the actual
        // write site means the invariant is visible AT the
        // critical section and the dispatch is robust to any
        // future architectural change. If the predicate is true
        // here, another path already wrote the row — drop the tx
        // (rolls back the two outbound rows we'd have inserted)
        // and let the caller skip the publish + summary steps.
        if db::mediation_events::session_has_self_resolution_offered(&tx, session_id)? {
            warn!(
                session_id = %session_id,
                "draft_and_send_self_resolution_invitation: prior `self_resolution_offered` \
                 row detected at write time; rolling back this dispatch's transaction and \
                 skipping outbound publishes"
            );
            // `tx` drops without commit → rollback. Explicit drop
            // makes the rollback visible to the reader.
            drop(tx);
            false
        } else {
            db::mediation::insert_outbound_message(
                &tx,
                &db::mediation::NewOutboundMessage {
                    session_id,
                    party: TranscriptParty::Buyer,
                    shared_pubkey: &buyer_shared_pubkey_hex,
                    inner_event_id: &buyer_inner_id_hex,
                    inner_event_created_at: buyer_wrap.inner_created_at,
                    outer_event_id: Some(&buyer_wrap.outer.id.to_hex()),
                    content: &buyer_msg,
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
                    content: &seller_msg,
                    prompt_bundle_id: &prompt_bundle.id,
                    policy_hash: &prompt_bundle.policy_hash,
                    persisted_at: now,
                },
            )?;
            // Self-resolution audit row. `rationale_id` is the producing
            // classification's content hash, embedded inside `payload_json`
            // per the contract (the dedicated `mediation_events.rationale_id`
            // column stays NULL on this kind). The `classification_confidence`
            // and the EFFECTIVE per-party language codes go into the
            // structured payload — i.e. the codes after fallback
            // resolution — so a forensic replay can reconstruct exactly
            // which template section each party received without having
            // to re-run the resolver.
            db::mediation_events::record_self_resolution_offered(
                &tx,
                session_id,
                rationale_id,
                confidence,
                buyer_effective_language.as_deref(),
                seller_effective_language.as_deref(),
                &prompt_bundle.id,
                &prompt_bundle.policy_hash,
                now,
            )?;
            tx.commit()?;
            true
        }
    };
    if !committed {
        return Ok(false);
    }

    // Operational tracing for SC-001 baseline (T029). We log both
    // the raw classifier output AND the effective resolved code so
    // operators can see at a glance when the bundle's fallback
    // kicked in (e.g. classifier says `de`, bundle has only en/es/pt
    // → effective resolves to `en`).
    let bid_for_log = prompt_bundle.id.clone();
    info!(
        event = "cooperative_case_detected",
        session_id = %session_id,
        confidence,
        prompt_bundle_id = %bid_for_log,
        buyer_language = buyer_language.unwrap_or("(none)"),
        seller_language = seller_language.unwrap_or("(none)"),
        buyer_effective_language = buyer_effective_language.as_deref().unwrap_or("(none)"),
        seller_effective_language = seller_effective_language.as_deref().unwrap_or("(none)"),
        occurred_at_unix = now,
        "cooperative_case_detected"
    );

    super::session::publish_with_bounded_retry(client, &buyer_wrap.outer, "buyer").await?;
    super::record_outbound_sent_audit(
        conn,
        session_id,
        &buyer_shared_pubkey_hex,
        &buyer_inner_id_hex,
        prompt_bundle,
    )
    .await?;
    super::session::publish_with_bounded_retry(client, &seller_wrap.outer, "seller").await?;
    super::record_outbound_sent_audit(
        conn,
        session_id,
        &seller_shared_pubkey_hex,
        &seller_inner_id_hex,
        prompt_bundle,
    )
    .await?;

    Ok(true)
}

/// One read of everything `advance_session_round` needs from the
/// session row. Batched into a single SELECT so the async mutex
/// lock is held for one query rather than four.
struct SessionEvalInfo {
    state: MediationSessionState,
    round_count: i64,
    round_count_last_evaluated: i64,
    dispute_id: String,
}

async fn load_session_info(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    session_id: &str,
) -> Result<Option<SessionEvalInfo>> {
    use std::str::FromStr;
    let guard = conn.lock().await;
    let row = guard.query_row(
        "SELECT state, round_count, round_count_last_evaluated, dispute_id
             FROM mediation_sessions
             WHERE session_id = ?1",
        params![session_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        },
    );
    match row {
        Ok((state_s, round_count, rcle, dispute_id)) => {
            let state = MediationSessionState::from_str(&state_s)?;
            Ok(Some(SessionEvalInfo {
                state,
                round_count,
                round_count_last_evaluated: rcle,
                dispute_id,
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn load_initiator_role(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
) -> Result<Option<InitiatorRole>> {
    use std::str::FromStr;
    let guard = conn.lock().await;
    let s: Option<String> = match guard.query_row(
        "SELECT initiator_role FROM disputes WHERE dispute_id = ?1",
        params![dispute_id],
        |r| r.get::<_, String>(0),
    ) {
        Ok(s) => Some(s),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    match s {
        Some(s) => Ok(Some(InitiatorRole::from_str(&s)?)),
        None => Ok(None),
    }
}

/// Bump `consecutive_eval_failures` and, if it crosses the
/// threshold, escalate the session with `ReasoningUnavailable`
/// (FR-130). Absorbs all errors with a `warn!`; never returns
/// failure to the caller.
///
/// Used by every failure path in `advance_session_round` — the
/// pre-dispatch failures (classify / policy::evaluate) and the
/// post-dispatch failures (drafter, deliver_summary,
/// escalation::recommend). The same threshold applies in both
/// classes so persistent failures of any kind eventually surface
/// to a human operator rather than looping silently. The trigger
/// string `reasoning_unavailable` is the closest fit in the
/// existing `EscalationTrigger` enum; a future refinement may
/// introduce a dedicated `DispatchFailed` trigger, but that is not
/// Phase 11 scope.
async fn handle_reasoning_failure(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    session_id: &str,
    dispute_id: &str,
    solvers: &[SolverConfig],
    prompt_bundle: &Arc<PromptBundle>,
) {
    let failures = {
        let guard = conn.lock().await;
        match db::mediation::bump_consecutive_eval_failures(&guard, session_id) {
            Ok(n) => n,
            Err(e) => {
                warn!(error = %e, "advance_session_round: failed to bump failure counter");
                return;
            }
        }
    };
    if failures < CONSECUTIVE_FAILURE_ESCALATION_THRESHOLD {
        warn!(
            failures,
            threshold = CONSECUTIVE_FAILURE_ESCALATION_THRESHOLD,
            "advance_session_round: will retry on next tick"
        );
        return;
    }
    warn!(
        failures,
        "advance_session_round: consecutive failure threshold reached; escalating"
    );
    if let Err(e) = escalation::recommend(escalation::RecommendParams {
        conn,
        session_id: Some(session_id),
        dispute_id,
        trigger: EscalationTrigger::ReasoningUnavailable,
        evidence_refs: Vec::new(),
        rationale_refs: Vec::new(),
        prompt_bundle_id: &prompt_bundle.id,
        policy_hash: &prompt_bundle.policy_hash,
    })
    .await
    {
        // The escalation helper already did its own logging. We
        // suppress the failure here because there is nothing useful
        // to retry at the tick layer — the session will keep failing
        // on every subsequent tick and a human operator needs to
        // intervene regardless.
        warn!(
            error = %e,
            "advance_session_round: escalation::recommend also failed after reasoning failures"
        );
        return;
    }
    notify_solvers_escalation(
        conn,
        client,
        solvers,
        dispute_id,
        session_id,
        EscalationTrigger::ReasoningUnavailable,
    )
    .await;
}

// Previous versions carried a `bump_failure_best_effort` helper
// that only incremented the counter on dispatch errors without
// escalating. Review feedback flagged that as a zombie-session
// risk: if the drafter's publish keeps failing or
// escalation::recommend itself keeps failing, the session would
// never surface to a human. All failure paths now go through
// `handle_reasoning_failure` so the FR-130 threshold applies
// uniformly.

#[cfg(test)]
mod tests {
    //! The orchestrator is intentionally integration-test heavy —
    //! the end-to-end behavior is verified in
    //! `tests/phase3_followup_round.rs` (T122),
    //! `tests/phase3_followup_summary.rs` (T123), and
    //! `tests/phase3_followup_reasoning_failure.rs` (T124), which
    //! stand up a real MockRelay + scripted reasoning provider +
    //! real session key material.
    //!
    //! The unit tests below cover the parts of the flow that
    //! don't require the full harness:
    //! - the idempotency gate (no reasoning call when marker
    //!   already at current round),
    //! - the state-machine gate (skip when state is not
    //!   `awaiting_response`),
    //! - the missing-cache-material skip,
    //! - the round-number helper.

    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::mediation::auth_retry::AuthRetryHandle;
    use crate::models::mediation::TranscriptParty;
    use crate::models::reasoning::{
        ClassificationResponse, ReasoningError, SummaryRequest, SummaryResponse,
    };
    use crate::prompts::PromptBundle;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_bundle() -> Arc<PromptBundle> {
        Arc::new(PromptBundle {
            id: "phase3-default".into(),
            policy_hash: "hash-test".into(),
            system: String::new(),
            classification: String::new(),
            escalation: String::new(),
            mediation_style: String::new(),
            message_templates: String::new(),
            self_resolution: crate::mediation::self_resolution::SelfResolutionTemplates::default(),
        })
    }

    /// Reasoning provider that counts `classify` calls. Any
    /// non-zero count from one of the gate tests below is a bug —
    /// the gate should skip before we hit classify.
    struct SpyClassifier {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ReasoningProvider for SpyClassifier {
        async fn classify(
            &self,
            _request: ClassificationRequest,
        ) -> std::result::Result<ClassificationResponse, ReasoningError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(ReasoningError::Unreachable("should not be called".into()))
        }
        async fn summarize(
            &self,
            _request: SummaryRequest,
        ) -> std::result::Result<SummaryResponse, ReasoningError> {
            panic!("summarize unused in follow_up tests")
        }
        async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
            Ok(())
        }
    }

    async fn seeded_db() -> Arc<AsyncMutex<rusqlite::Connection>> {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-t120', 'e-t120', 'm', 'buyer',
                       'initiated', 1, 2, 'notified')",
            [],
        )
        .unwrap();
        Arc::new(AsyncMutex::new(conn))
    }

    /// Insert a session row pre-populated with the state/marker/
    /// round_count the test wants to exercise. Shared pubkeys are
    /// intentionally None — the cache-missing gate runs before we
    /// need them, and the non-cache-miss path uses an empty cache
    /// to still trip the same gate.
    async fn seed_session(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        state: &str,
        round_count: i64,
        round_count_last_evaluated: i64,
    ) {
        let guard = conn.lock().await;
        guard
            .execute(
                "INSERT INTO mediation_sessions (
                    session_id, dispute_id, state, round_count,
                    round_count_last_evaluated, consecutive_eval_failures,
                    prompt_bundle_id, policy_hash,
                    started_at, last_transition_at
                 ) VALUES ('sess-t120', 'd-t120', ?1, ?2, ?3, 0,
                           'phase3-default', 'hash-test',
                           100, 100)",
                params![state, round_count, round_count_last_evaluated],
            )
            .unwrap();
    }

    /// Insert one fresh (non-stale) inbound message row for the
    /// `sess-t120` session. Used by gate tests that need
    /// `count_fresh_inbounds` to return a specific count. The
    /// `inner_event_created_at` disambiguates rows so the unique
    /// index `uq_mediation_messages_inner_event` doesn't reject a
    /// second call with the same party.
    async fn seed_fresh_inbound(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        party: TranscriptParty,
        inner_event_created_at: i64,
    ) {
        let guard = conn.lock().await;
        let party_s = match party {
            TranscriptParty::Buyer => "buyer",
            TranscriptParty::Seller => "seller",
            TranscriptParty::Serbero => {
                panic!("Serbero is outbound-only; not valid for inbound seed")
            }
        };
        guard
            .execute(
                "INSERT INTO mediation_messages (
                    session_id, direction, party, shared_pubkey,
                    inner_event_id, inner_event_created_at,
                    outer_event_id, content,
                    prompt_bundle_id, policy_hash,
                    persisted_at, stale
                 ) VALUES ('sess-t120', 'inbound', ?1, 'sp-test',
                           ?2, ?3, NULL, 'hello',
                           'phase3-default', 'hash-test',
                           200, 0)",
                params![
                    party_s,
                    format!("inner-{}", inner_event_created_at),
                    inner_event_created_at
                ],
            )
            .unwrap();
    }

    async fn run_once(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        reasoning: &dyn ReasoningProvider,
    ) {
        // We don't need a live Client / key material for the gate
        // tests — those gates short-circuit before any of those
        // are used. A dummy client connected to no relays is
        // sufficient for the function to run to its early returns.
        let serbero_keys = Keys::generate();
        let client = Client::new(serbero_keys.clone());
        let bundle = test_bundle();
        let cache: SessionKeyCache = Arc::new(AsyncMutex::new(HashMap::new()));
        let _auth = AuthRetryHandle::new_authorized();
        advance_session_round(
            conn,
            &client,
            &serbero_keys,
            reasoning,
            &bundle,
            "sess-t120",
            &cache,
            &[],
            "mock-provider",
            "mock-model",
            &MediationConfig::default(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn skips_when_session_row_is_absent() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let conn = Arc::new(AsyncMutex::new(conn));
        // Explicitly no session row for `sess-t120`.
        let spy = SpyClassifier {
            calls: AtomicUsize::new(0),
        };
        run_once(&conn, &spy).await;
        assert_eq!(spy.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn skips_when_state_is_not_awaiting_response() {
        let conn = seeded_db().await;
        seed_session(&conn, "escalation_recommended", 3, 2).await;
        let spy = SpyClassifier {
            calls: AtomicUsize::new(0),
        };
        run_once(&conn, &spy).await;
        assert_eq!(
            spy.calls.load(Ordering::SeqCst),
            0,
            "state gate must block classify when session is not awaiting_response"
        );
    }

    #[tokio::test]
    async fn skips_when_fresh_inbounds_already_evaluated() {
        let conn = seeded_db().await;
        // FR-127 idempotency gate: count_fresh_inbounds ==
        // round_count_last_evaluated → skip. Seed two fresh inbound
        // rows (one per party) and mark both as already evaluated.
        seed_session(&conn, "awaiting_response", 1, 2).await;
        seed_fresh_inbound(&conn, TranscriptParty::Buyer, 10).await;
        seed_fresh_inbound(&conn, TranscriptParty::Seller, 20).await;
        let spy = SpyClassifier {
            calls: AtomicUsize::new(0),
        };
        run_once(&conn, &spy).await;
        assert_eq!(
            spy.calls.load(Ordering::SeqCst),
            0,
            "gate must block when total fresh inbounds <= round_count_last_evaluated"
        );
    }

    #[tokio::test]
    async fn skips_when_cache_material_missing() {
        // State + round gates pass; the missing-cache gate is the
        // one that blocks. This mirrors the post-restart case.
        let conn = seeded_db().await;
        seed_session(&conn, "awaiting_response", 3, 2).await;
        let spy = SpyClassifier {
            calls: AtomicUsize::new(0),
        };
        run_once(&conn, &spy).await;
        assert_eq!(
            spy.calls.load(Ordering::SeqCst),
            0,
            "missing-cache gate must block classify when material is absent"
        );
    }
}
