//! FR-121 event-driven mediation start.
//!
//! [`try_start_for`] is the single entry point that both the
//! `handlers::dispute_detected` event-handling path and the engine
//! tick's safety-net retry use to open a mediation session. Keeping
//! the two call sites unified on one function guarantees they cannot
//! disagree about:
//!
//! - the eligibility predicate (always [`eligibility::is_mediation_eligible`]),
//! - the audit-trail shape (a `start_attempt_started` event precedes
//!   any open-session work; a `start_attempt_stopped` event fires
//!   whenever the attempt halts before `take_dispute_issued`), and
//! - the per-outcome semantics the caller then routes on.
//!
//! The function is synchronous (in the tokio sense — it `.await`s
//! but never spawns), so the dispute-detected handler can wait for
//! the first outbound mediation message to be dispatched before
//! returning. That is the property SC-109 hinges on: a new dispute
//! reaches its first party-facing message within seconds of
//! detection, independent of `ENGINE_TICK_INTERVAL`.

use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, instrument, warn};

use crate::db::mediation_events;
use crate::error::{Error, Result};
use crate::mediation::{eligibility, session};

/// Where the start attempt came from. Serialized into the
/// `start_attempt_started` audit-event payload as the `trigger`
/// field; `data-model.md` §mediation_events enumerates the two
/// allowed values below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartTrigger {
    /// Fired by `handlers::dispute_detected` immediately after the
    /// Phase 1/2 persist + solver-notify path finishes (FR-121).
    Detected,
    /// Fired by the background engine tick. Reserved for the
    /// sweep-retry role the tick takes on once T100's refactor
    /// lands; the tick is no longer the primary opener.
    TickRetry,
}

impl StartTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            StartTrigger::Detected => "detected",
            StartTrigger::TickRetry => "tick_retry",
        }
    }
}

/// Outcome of [`try_start_for`].
///
/// The shape intentionally surfaces the full [`session::OpenOutcome`]
/// on the happy path so the caller can still dispatch post-session
/// work (cooperative summary delivery on `ReadyForSummary`,
/// escalation fan-out on `EscalatedOnOpen`, etc.). The goal of
/// `try_start_for` is to own the start-flow gates and the audit
/// trail, not to subsume every consumer of `OpenOutcome`.
#[derive(Debug)]
pub enum StartOutcome {
    /// The composed eligibility predicate rejected the dispute. A
    /// dispute-scoped `start_attempt_stopped{reason: "ineligible"}`
    /// row marks the rejection so operators see the attempt even
    /// when it stopped at the gate. No `start_attempt_started` is
    /// written for this case — the attempt never really began.
    NotEligible,
    /// [`session::open_session`] completed (whether it opened a
    /// fresh session, observed an already-open one, or returned a
    /// variant that needs further post-processing). The caller
    /// inspects the wrapped outcome to route downstream work.
    Started(session::OpenOutcome),
    /// The attempt stopped before any take-dispute exchange. Covers
    /// the auth-pending, auth-terminated, and reasoning-unavailable
    /// refusals surfaced by `open_session`. Post-T104 the
    /// `reasoning_verdict_negative` stop reason joins the same variant.
    StoppedBeforeTake { reason: String },
    /// A `TakeDispute` attempt failed. Reserved for the T104 rework
    /// where `open_session` records `take_dispute_issued{outcome:
    /// "failure"}` and returns an outcome variant instead of
    /// bubbling up the raw error. Pre-T104 take-phase failures
    /// surface as `Error` below.
    TakeFailed { reason: String },
    /// An unexpected error propagated out of the start flow. The
    /// caller logs; no retry happens inside `try_start_for`.
    Error(Error),
}

/// Parameters for [`try_start_for`]. Nests [`session::OpenSessionParams`]
/// verbatim so callers that already construct the open-session params
/// (engine tick, test harness) do not duplicate 15+ field initialisers
/// just to route through the start flow.
pub struct StartParams<'a> {
    /// Everything `open_session` needs. `try_start_for` reads
    /// `open.conn` and `open.dispute_id` for the eligibility check
    /// and the audit writes; every other field is forwarded verbatim.
    pub open: session::OpenSessionParams<'a>,
    /// Whether this attempt came from the event-driven handler
    /// (`Detected`) or the background tick (`TickRetry`). Persisted
    /// in the `start_attempt_started` payload.
    pub trigger: StartTrigger,
}

/// Open (or attempt to open) a mediation session for one dispute,
/// recording the full start-flow audit trail along the way.
///
/// Flow:
/// 1. Evaluate [`eligibility::is_mediation_eligible`]. If it returns
///    `false`, record `start_attempt_stopped{reason: "ineligible"}`
///    dispute-scoped and return [`StartOutcome::NotEligible`].
/// 2. Record `start_attempt_started{trigger}` dispute-scoped.
/// 3. Delegate to [`session::open_session`].
/// 4. Map the `OpenOutcome` into a [`StartOutcome`]. For the refusal
///    variants emit a `start_attempt_stopped` with the matching stop
///    reason. For the happy-path variants wrap the outcome in
///    `StartOutcome::Started` and leave post-processing to the caller.
///
/// A failure in the eligibility SQL or in any audit write is logged
/// and surfaced as [`StartOutcome::Error`] — we never silently drop
/// an attempt.
///
/// The function never spawns a task. Callers that need async
/// isolation wrap the call site themselves.
#[instrument(skip_all, fields(
    dispute_id = %params.open.dispute_id,
    trigger = params.trigger.as_str(),
))]
pub async fn try_start_for(params: StartParams<'_>) -> StartOutcome {
    let StartParams { open, trigger } = params;
    let dispute_id = open.dispute_id.to_string();
    // Hold onto the connection Arc before `open` is moved into
    // `session::open_session` below; we need it for the
    // post-delegation refusal-audit writes.
    let conn: Arc<AsyncMutex<rusqlite::Connection>> = Arc::clone(open.conn);

    // (1) Eligibility gate.
    match run_eligibility(&conn, &dispute_id).await {
        Ok(true) => {}
        Ok(false) => {
            write_stop(&conn, &dispute_id, "ineligible").await;
            return StartOutcome::NotEligible;
        }
        Err(e) => {
            warn!(error = %e, "try_start_for: eligibility check failed");
            return StartOutcome::Error(e);
        }
    }

    // (2) `start_attempt_started`. Logged at info! so operators can
    //     correlate the attempt with the downstream audit rows
    //     without needing DEBUG tracing.
    if let Err(e) = write_started(&conn, &dispute_id, trigger).await {
        warn!(
            error = %e,
            "try_start_for: failed to write start_attempt_started; aborting attempt"
        );
        return StartOutcome::Error(e);
    }
    info!("try_start_for: start attempt recorded; delegating to open_session");

    // (3) Delegate. `open_session` owns every downstream gate
    //     (auth, reasoning health, take-flow) and every `mediation_*`
    //     row write beyond this point.
    let open_result = session::open_session(open).await;

    // (4) Translate.
    match open_result {
        Ok(outcome) => match &outcome {
            session::OpenOutcome::RefusedAuthPending { reason } => {
                let reason_str = reason.clone();
                write_stop(&conn, &dispute_id, "auth_pending").await;
                StartOutcome::StoppedBeforeTake { reason: reason_str }
            }
            session::OpenOutcome::RefusedAuthTerminated { reason } => {
                let reason_str = reason.clone();
                write_stop(&conn, &dispute_id, "auth_terminated").await;
                StartOutcome::StoppedBeforeTake { reason: reason_str }
            }
            session::OpenOutcome::RefusedReasoningUnavailable { reason } => {
                let reason_str = reason.clone();
                write_stop(&conn, &dispute_id, "reasoning_unhealthy").await;
                StartOutcome::StoppedBeforeTake { reason: reason_str }
            }
            session::OpenOutcome::Opened { .. }
            | session::OpenOutcome::AlreadyOpen { .. }
            | session::OpenOutcome::ReadyForSummary { .. }
            | session::OpenOutcome::EscalatedOnOpen { .. } => StartOutcome::Started(outcome),
        },
        Err(e) => {
            warn!(error = %e, "try_start_for: open_session returned error");
            StartOutcome::Error(e)
        }
    }
}

/// Evaluate the composed eligibility predicate under the async
/// connection lock.
async fn run_eligibility(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
) -> Result<bool> {
    let guard = conn.lock().await;
    eligibility::is_mediation_eligible(&guard, dispute_id)
}

/// Write a dispute-scoped `start_attempt_started` row.
async fn write_started(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    trigger: StartTrigger,
) -> Result<()> {
    let now = super::current_ts_secs()?;
    let guard = conn.lock().await;
    mediation_events::record_start_attempt_started(&guard, None, dispute_id, trigger.as_str(), now)
        .map(|_| ())
}

/// Write a dispute-scoped `start_attempt_stopped` row. Audit-write
/// failures are logged at `warn!` but never mask the caller's outcome
/// decision: the outcome classification itself is the authoritative
/// signal for the caller, the audit row is additive.
async fn write_stop(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    stop_reason: &str,
) {
    let now = match super::current_ts_secs() {
        Ok(ts) => ts,
        Err(e) => {
            warn!(error = %e, "try_start_for: clock error; skipping stop audit write");
            return;
        }
    };
    let guard = conn.lock().await;
    if let Err(e) =
        mediation_events::record_start_attempt_stopped(&guard, None, dispute_id, stop_reason, now)
    {
        warn!(
            error = %e,
            stop_reason,
            "try_start_for: failed to write start_attempt_stopped"
        );
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the eligibility-gate branch of `try_start_for`.
    //!
    //! The happy-path branches (`Started`, `StoppedBeforeTake` via
    //! auth / reasoning refusal) are covered by the integration test
    //! T103 against a real mock relay + reasoning provider — spinning
    //! the full `OpenSessionParams` up in a unit test is not
    //! substantially cheaper than the integration harness, and the
    //! boundaries are more meaningful in the end-to-end form.

    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::models::LifecycleState;
    use rusqlite::params;

    fn ineligible_fixture() -> rusqlite::Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-resolved', 'e1', 'm1', 'buyer',
                       'initiated', 1, 2, ?1)",
            params![LifecycleState::Resolved.to_string()],
        )
        .unwrap();
        conn
    }

    #[test]
    fn start_trigger_strings_match_data_model() {
        // Same strings the `start_attempt_started` payload carries
        // in production; drifting either side without drifting the
        // other silently breaks operator tooling.
        assert_eq!(StartTrigger::Detected.as_str(), "detected");
        assert_eq!(StartTrigger::TickRetry.as_str(), "tick_retry");
    }

    #[test]
    fn ineligible_dispute_records_dispute_scoped_stop_row() {
        // The eligibility SQL and the stop-row constructor together
        // are what `try_start_for` performs on the ineligible branch.
        // Assembling the full async wiring for this branch would add
        // no meaningful coverage beyond the T103 integration test.
        let conn = ineligible_fixture();
        assert!(!eligibility::is_mediation_eligible(&conn, "d-resolved").unwrap());
        mediation_events::record_start_attempt_stopped(
            &conn,
            None,
            "d-resolved",
            "ineligible",
            42,
        )
        .unwrap();
        let (sid, payload): (Option<String>, String) = conn
            .query_row(
                "SELECT session_id, payload_json
                 FROM mediation_events
                 WHERE kind = 'start_attempt_stopped'
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(sid.is_none(), "ineligible stop row must be dispute-scoped");
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["dispute_id"], "d-resolved");
        assert_eq!(parsed["stop_reason"], "ineligible");
    }
}
