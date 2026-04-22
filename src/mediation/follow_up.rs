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
//!      summary_delivered → closed`). After `deliver_summary`
//!      returns `Ok`, we advance the marker in a separate,
//!      short-lived transaction because `deliver_summary` owns its
//!      own transaction scope.
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
use crate::models::SolverConfig;
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

use super::{
    deliver_summary, draft_and_send_followup_message, escalation, notify_solvers_escalation,
    policy, transcript, SessionKeyCache,
};

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
) -> Result<()> {
    // (1) Load session metadata + idempotency gate.
    let info = match load_session_info(conn, session_id).await? {
        Some(i) => i,
        None => {
            debug!("advance_session_round: session row not found; skipping");
            return Ok(());
        }
    };
    if !matches!(info.state, MediationSessionState::AwaitingResponse) {
        debug!(
            state = %info.state,
            "advance_session_round: session not in awaiting_response; skipping"
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

    // (4) Transcript.
    let transcript_entries = {
        let guard = conn.lock().await;
        transcript::load_transcript_for_session(&guard, session_id, TRANSCRIPT_CAP)?
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
    // `followup_number` is 1-based — the count of the evaluation
    // currently in flight, not a retrospective index. Since
    // `round_count_last_evaluated` is the count of fresh inbounds
    // already classified (see `db::mediation::count_fresh_inbounds`
    // docstring), the next-to-commit evaluation is number
    // `round_count_last_evaluated + 1`. Used by the policy layer's
    // early-mid-session low-confidence bypass.
    let followup_number = info.round_count_last_evaluated.max(0) as u32 + 1;
    let decision = match policy::evaluate(
        conn,
        session_id,
        prompt_bundle,
        provider_name,
        model_name,
        classification,
        followup_number,
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
    match decision {
        policy::PolicyDecision::AskClarification {
            buyer_text,
            seller_text,
        } => {
            let new_marker = total_fresh_inbounds;
            let round_number = round_number_for_followup(info.round_count_last_evaluated);
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
            // Mark the round evaluated. Even though the session is
            // now terminal (closed), keeping the marker current is
            // a cheap invariant — a future tick never mistakes an
            // evaluated round for an unevaluated one.
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
        policy::PolicyDecision::Escalate(trigger) => {
            if let Err(e) = escalation::recommend(escalation::RecommendParams {
                conn,
                session_id,
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

/// Pure helper — the drafter receives `round_number` for the body
/// prefix. The first follow-up is `N = 1` (the opening clarification
/// was round 0); the second follow-up is `N = 2`, etc.
///
/// Computed from `round_count_last_evaluated` + 1. Post-FR-127 fix,
/// that column counts "fresh inbounds already evaluated", not
/// completed rounds — so `round_number` now increments once per
/// evaluation regardless of whether both parties replied. That
/// matches the user-visible intent ("follow-up N") better than the
/// old min-rule did, since we only emit a follow-up when we actually
/// run an evaluation.
fn round_number_for_followup(round_count_last_evaluated: i64) -> u32 {
    round_count_last_evaluated.max(0) as u32 + 1
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
        session_id,
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
    async fn skips_when_round_count_already_evaluated() {
        let conn = seeded_db().await;
        // round_count == round_count_last_evaluated → FR-127 idempotency gate blocks.
        seed_session(&conn, "awaiting_response", 2, 2).await;
        let spy = SpyClassifier {
            calls: AtomicUsize::new(0),
        };
        run_once(&conn, &spy).await;
        assert_eq!(spy.calls.load(Ordering::SeqCst), 0);
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

    #[test]
    fn round_number_for_followup_is_one_based() {
        // marker = 0 → the first follow-up round is N = 1.
        assert_eq!(round_number_for_followup(0), 1);
        // marker = 1 → the second follow-up round is N = 2.
        assert_eq!(round_number_for_followup(1), 2);
        // Defensive: negative markers never happen in production
        // (the column is NOT NULL DEFAULT 0), but the helper
        // clamps anyway.
        assert_eq!(round_number_for_followup(-5), 1);
    }
}
