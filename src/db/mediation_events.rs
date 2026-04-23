//! Audit log for session-level Phase 3 events.
//!
//! Mirrors `data-model.md` §mediation_events. Every write goes
//! through [`record_event`] or one of the typed constructors so
//! call sites cannot misspell a `kind` value — the enum is the
//! single source of truth, and the SQL text form is derived from it.
//!
//! FR-120 discipline: event payloads are *small* structured JSON.
//! The controlled audit store for raw rationales is
//! [`crate::db::rationales`]; `mediation_events` references that
//! store by `rationale_id` (content hash), never by inlining the
//! rationale text. General application logs MUST NOT include the
//! `rationale_text` column either.

use rusqlite::{params, Connection, Transaction};
use serde_json::json;

use crate::error::Result;

/// Enumerated event kinds. Text form is the snake_case spelling
/// stored in `mediation_events.kind` (see `data-model.md` table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediationEventKind {
    // Phase 10 start-flow audit (FR-121 / FR-122 / FR-123). These
    // kinds may be written with `session_id = NULL` when the attempt
    // stopped before a session row was committed; the payload then
    // carries `dispute_id`.
    StartAttemptStarted,
    StartAttemptStopped,
    ReasoningVerdict,
    TakeDisputeIssued,
    SessionOpened,
    OutboundSent,
    InboundIngested,
    StateTransition,
    ClassificationProduced,
    SummaryGenerated,
    EscalationRecommended,
    HandoffPrepared,
    ReasoningCallFailed,
    AuthorizationLost,
    AuthRetryAttempt,
    AuthRetryTerminated,
    AuthRetryRecovered,
    SupersededByHuman,
    /// FR-124 — emitted when a dispute resolves externally while
    /// Serbero has collected mediation context for it. May be
    /// session-scoped (active/escalated/closed session existed) or
    /// dispute-scoped (only reasoning context existed, no session).
    ResolvedExternallyReported,
    SessionClosed,
    // --- Phase 4 escalation dispatcher audit kinds (FR-211 / FR-212 /
    // FR-213 / FR-214). Session_id scoping copies the upstream
    // `handoff_prepared` row: set if a session existed, NULL for
    // FR-122 dispute-scoped handoffs.
    /// FR-211 — written in the same transaction as the
    /// `escalation_dispatches` row whenever a dispatch attempt
    /// reached the send step (succeeded AT LEAST on one recipient OR
    /// failed on every recipient — the `status` payload field
    /// distinguishes the two).
    EscalationDispatched,
    /// FR-212 — written when the dispatcher skipped a handoff
    /// because the dispute had already resolved externally before
    /// the send step (FR-208 supersession gate). No
    /// `escalation_dispatches` row is written alongside.
    EscalationSuperseded,
    /// FR-213 — written when no write-permission solver is
    /// configured and `[escalation].fallback_to_all_solvers = false`.
    /// Paired with an ERROR-level log line. The handoff event
    /// deliberately stays unconsumed so a later config change can
    /// pick it up.
    EscalationDispatchUnroutable,
    /// FR-214 — written when the `handoff_prepared` payload cannot
    /// be used. Two sub-shapes distinguished by the payload's
    /// `reason` field: `deserialize_failed` (payload does not parse)
    /// and `orphan_dispute_reference` (payload parses but the
    /// dispute id has no row in `disputes`).
    EscalationDispatchParseFailed,
}

impl MediationEventKind {
    pub fn as_str(&self) -> &'static str {
        use MediationEventKind::*;
        match self {
            StartAttemptStarted => "start_attempt_started",
            StartAttemptStopped => "start_attempt_stopped",
            ReasoningVerdict => "reasoning_verdict",
            TakeDisputeIssued => "take_dispute_issued",
            SessionOpened => "session_opened",
            OutboundSent => "outbound_sent",
            InboundIngested => "inbound_ingested",
            StateTransition => "state_transition",
            ClassificationProduced => "classification_produced",
            SummaryGenerated => "summary_generated",
            EscalationRecommended => "escalation_recommended",
            HandoffPrepared => "handoff_prepared",
            ReasoningCallFailed => "reasoning_call_failed",
            AuthorizationLost => "authorization_lost",
            AuthRetryAttempt => "auth_retry_attempt",
            AuthRetryTerminated => "auth_retry_terminated",
            AuthRetryRecovered => "auth_retry_recovered",
            SupersededByHuman => "superseded_by_human",
            ResolvedExternallyReported => "resolved_externally_reported",
            SessionClosed => "session_closed",
            EscalationDispatched => "escalation_dispatched",
            EscalationSuperseded => "escalation_superseded",
            EscalationDispatchUnroutable => "escalation_dispatch_unroutable",
            EscalationDispatchParseFailed => "escalation_dispatch_parse_failed",
        }
    }
}

impl std::fmt::Display for MediationEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Low-level event insert. Returns the autoincremented row id.
///
/// Prefer a typed constructor (e.g. [`record_session_opened`]) when
/// one exists for the kind: it encodes the payload shape correctly
/// so two different call sites never emit `classification_produced`
/// events with diverging JSON keys.
#[allow(clippy::too_many_arguments)]
pub fn record_event(
    conn: &Connection,
    kind: MediationEventKind,
    session_id: Option<&str>,
    payload_json: &str,
    rationale_id: Option<&str>,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO mediation_events (
            session_id, kind, payload_json,
            rationale_id, prompt_bundle_id, policy_hash, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            session_id,
            kind.as_str(),
            payload_json,
            rationale_id,
            prompt_bundle_id,
            policy_hash,
            occurred_at,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Record a `session_opened` event. Emitted right after the
/// session + outbound rows commit in `mediation::session::open_session`,
/// so the audit log captures the prompt bundle actually pinned on
/// the session row.
pub fn record_session_opened(
    conn: &Connection,
    session_id: &str,
    prompt_bundle_id: &str,
    policy_hash: &str,
    occurred_at: i64,
) -> Result<i64> {
    // Empty payload: the `prompt_bundle_id` and `policy_hash`
    // columns already carry the provenance. Keeping the payload
    // empty matches the data-model's guidance to avoid
    // duplicating fields that are first-class columns.
    record_event(
        conn,
        MediationEventKind::SessionOpened,
        Some(session_id),
        "{}",
        None,
        Some(prompt_bundle_id),
        Some(policy_hash),
        occurred_at,
    )
}

/// Record an `outbound_sent` event. Intended to fire after a
/// successful relay publish of one gift-wrap. `shared_pubkey` and
/// `inner_event_id` together identify the addressed party + the
/// authoritative inner-event id used as the dedup key in
/// `mediation_messages`.
///
/// `prompt_bundle_id` / `policy_hash` pin the bundle that produced
/// the outbound draft. They are `Option` so daemon-level reconciliation
/// paths (e.g. restart-resume re-publishing a pre-existing outbound
/// row whose bundle may no longer be loaded) can pass `None`, but
/// fresh session-open / draft paths should always supply them — the
/// SC-103 invariant carries into the audit log, not just into the
/// `mediation_messages` row.
#[allow(clippy::too_many_arguments)]
pub fn record_outbound_sent(
    conn: &Connection,
    session_id: &str,
    shared_pubkey: &str,
    inner_event_id: &str,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "shared_pubkey": shared_pubkey,
        "inner_event_id": inner_event_id,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::OutboundSent,
        Some(session_id),
        &payload,
        None,
        prompt_bundle_id,
        policy_hash,
        occurred_at,
    )
}

/// Record a `classification_produced` event. `rationale_id`
/// references [`crate::db::rationales`]; the raw rationale text is
/// NEVER inlined into the payload, per FR-120.
///
/// `prompt_bundle_id` / `policy_hash` pin the bundle active at
/// classification time — load-bearing for SC-103 audit, matching
/// `record_session_opened`.
#[allow(clippy::too_many_arguments)]
pub fn record_classification_produced(
    conn: &Connection,
    session_id: &str,
    rationale_id: &str,
    classification: &str,
    confidence: f64,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "classification": classification,
        "confidence": confidence,
        "rationale_id": rationale_id,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::ClassificationProduced,
        Some(session_id),
        &payload,
        Some(rationale_id),
        prompt_bundle_id,
        policy_hash,
        occurred_at,
    )
}

// ---------------------------------------------------------------
// Phase 10 — dispute-scoped start-flow audit (FR-121 / FR-122 / FR-123)
//
// The constructors below all accept `session_id: Option<&str>`
// because they fire before (or instead of) a `mediation_sessions`
// row being committed. `session_id = None` routes a dispute-scoped
// row; `session_id = Some(..)` routes a session-scoped row once the
// session exists (e.g. after a successful take on the happy path).
// ---------------------------------------------------------------

/// `start_attempt_started` — the event-driven start path (FR-121)
/// has begun evaluating a dispute. Fires before any gate decision
/// or reasoning call. `trigger` is either `"detected"` (the
/// dispute-detection event-handling path) or `"tick_retry"` (the
/// background engine tick's safety-net retry).
pub fn record_start_attempt_started(
    conn: &Connection,
    session_id: Option<&str>,
    dispute_id: &str,
    trigger: &str,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "dispute_id": dispute_id,
        "trigger": trigger,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::StartAttemptStarted,
        session_id,
        &payload,
        None,
        None,
        None,
        occurred_at,
    )
}

/// `start_attempt_stopped` — an in-flight start attempt refused
/// before `take_dispute_issued` fired. `stop_reason` is one of the
/// enumerated strings from `data-model.md`: `"ineligible"`,
/// `"reasoning_unhealthy"`, `"reasoning_verdict_negative"`,
/// `"reasoning_provider_error"`, `"policy_escalate"`.
pub fn record_start_attempt_stopped(
    conn: &Connection,
    session_id: Option<&str>,
    dispute_id: &str,
    stop_reason: &str,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "dispute_id": dispute_id,
        "stop_reason": stop_reason,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::StartAttemptStopped,
        session_id,
        &payload,
        None,
        None,
        None,
        occurred_at,
    )
}

/// `reasoning_verdict` — the reasoning layer produced a verdict
/// during a start attempt. Precedes any `TakeDispute` for this
/// attempt (FR-122). `verdict` is `"mediation_eligible"` or
/// `"not_eligible"`. `rationale_id` references the audit store
/// (FR-120). The rationale may have been persisted with
/// `session_id = NULL` when this event fires; the session-scoped
/// `classification_produced` event (if any) is emitted separately
/// once the session row exists.
#[allow(clippy::too_many_arguments)]
pub fn record_reasoning_verdict(
    conn: &Connection,
    session_id: Option<&str>,
    dispute_id: &str,
    verdict: &str,
    classification: &str,
    confidence: f64,
    rationale_id: &str,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "dispute_id": dispute_id,
        "verdict": verdict,
        "classification": classification,
        "confidence": confidence,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::ReasoningVerdict,
        session_id,
        &payload,
        Some(rationale_id),
        prompt_bundle_id,
        policy_hash,
        occurred_at,
    )
}

/// `take_dispute_issued` — Serbero attempted `TakeDispute` against
/// Mostro for this dispute. `outcome` is `"success"` or `"failure"`.
/// On `success`, the session row is committed and a subsequent
/// `session_opened` event fires session-scoped. On `failure`, no
/// session row exists and this event is dispute-scoped; `reason`
/// carries the underlying error message.
pub fn record_take_dispute_issued(
    conn: &Connection,
    session_id: Option<&str>,
    dispute_id: &str,
    outcome: &str,
    reason: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = match reason {
        Some(r) => json!({
            "dispute_id": dispute_id,
            "outcome": outcome,
            "reason": r,
        }),
        None => json!({
            "dispute_id": dispute_id,
            "outcome": outcome,
        }),
    }
    .to_string();
    record_event(
        conn,
        MediationEventKind::TakeDisputeIssued,
        session_id,
        &payload,
        None,
        None,
        None,
        occurred_at,
    )
}

/// `resolved_externally_reported` — FR-124 final solver-facing
/// report was emitted after a Phase 1/2 lifecycle transition to a
/// resolved terminal state while Serbero had collected mediation
/// context. Fires at most once per dispute (idempotency is provided
/// by the outer `handlers::dispute_resolved` early-return guard on
/// already-resolved disputes). `session_id` is `Some(..)` when a
/// session row existed at report time, `None` for the
/// reasoning-verdict-only case.
#[allow(clippy::too_many_arguments)]
pub fn record_resolved_externally_reported(
    conn: &Connection,
    session_id: Option<&str>,
    dispute_id: &str,
    final_dispute_status: &str,
    outbound_party_messages_count: u8,
    had_classification: bool,
    had_escalation_recommendation: bool,
    notifier_route: &str,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "dispute_id": dispute_id,
        "final_dispute_status": final_dispute_status,
        "outbound_party_messages_count": outbound_party_messages_count,
        "had_classification": had_classification,
        "had_escalation_recommendation": had_escalation_recommendation,
        "notifier_route": notifier_route,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::ResolvedExternallyReported,
        session_id,
        &payload,
        None,
        prompt_bundle_id,
        policy_hash,
        occurred_at,
    )
}

/// FR-211 typed constructor for the Phase 4 dispatch audit event.
///
/// Payload shape matches
/// `specs/004-escalation-execution/contracts/audit-events.md`
/// §escalation_dispatched. Takes `&Transaction<'_>` (not
/// `&Connection`) because FR-211 requires this audit row to land
/// atomically with the matching `escalation_dispatches` row —
/// forcing the transaction at the type level makes the pairing
/// impossible to bypass by accident, and matches the signature of
/// `db::escalation_dispatches::insert_dispatch`.
///
/// The other three Phase 4 audit writers
/// (`record_escalation_superseded`,
/// `record_escalation_dispatch_unroutable`,
/// `record_escalation_dispatch_parse_failed`) stay on `&Connection`:
/// they do NOT pair with a dispatch row, so the atomicity
/// invariant does not apply to them.
#[allow(clippy::too_many_arguments)]
pub fn record_escalation_dispatched(
    tx: &Transaction<'_>,
    session_id: Option<&str>,
    dispatch_id: &str,
    dispute_id: &str,
    handoff_event_id: i64,
    target_solver: &str,
    status: &str,
    fallback_broadcast: bool,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "dispatch_id": dispatch_id,
        "dispute_id": dispute_id,
        "handoff_event_id": handoff_event_id,
        "target_solver": target_solver,
        "status": status,
        "fallback_broadcast": fallback_broadcast,
    })
    .to_string();
    // `Transaction` derefs to `Connection`, so `record_event` can
    // run the INSERT inside the caller's tx scope. The tx is
    // committed by the caller after both this audit row and the
    // paired `escalation_dispatches` row land — partial-commit
    // races are impossible.
    record_event(
        tx,
        MediationEventKind::EscalationDispatched,
        session_id,
        &payload,
        None,
        prompt_bundle_id,
        policy_hash,
        occurred_at,
    )
}

/// Lookup used by the FR-208 supersession gate to avoid emitting a
/// second `escalation_superseded` audit row for the same
/// `handoff_event_id`.
///
/// Deduplication lives at the gate (not at the scan) so FR-213 is
/// preserved: the upstream `handoff_prepared` row stays visible to
/// `list_pending_handoffs` on every cycle — a later lifecycle flip
/// that reopens the dispute (e.g. an operator corrective workflow)
/// must be able to pick the same handoff back up. The consumer
/// keeps seeing the handoff; the audit stops growing.
pub fn escalation_superseded_exists_for_handoff(
    conn: &Connection,
    handoff_event_id: i64,
) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM mediation_events
         WHERE kind = 'escalation_superseded'
           AND json_extract(payload_json, '$.handoff_event_id') = ?1",
        params![handoff_event_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// FR-212 typed constructor for supersession.
///
/// Fires when the dispatcher's FR-208 gate detected the dispute
/// had already resolved before the send step. No
/// `escalation_dispatches` row is written alongside — supersession
/// is a non-event from the dispatch-table's perspective.
#[allow(clippy::too_many_arguments)]
pub fn record_escalation_superseded(
    conn: &Connection,
    session_id: Option<&str>,
    dispute_id: &str,
    handoff_event_id: i64,
    reason: &str,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "dispute_id": dispute_id,
        "handoff_event_id": handoff_event_id,
        "reason": reason,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::EscalationSuperseded,
        session_id,
        &payload,
        None,
        prompt_bundle_id,
        policy_hash,
        occurred_at,
    )
}

/// FR-213 typed constructor for the unroutable dispatch case.
///
/// Fires when `[escalation].fallback_to_all_solvers = false` AND
/// no write-permission solver is configured. The handoff event
/// stays unconsumed — a later config change re-surfaces it on the
/// next dispatcher cycle.
#[allow(clippy::too_many_arguments)]
pub fn record_escalation_dispatch_unroutable(
    conn: &Connection,
    session_id: Option<&str>,
    dispute_id: &str,
    handoff_event_id: i64,
    configured_solver_count: usize,
    fallback_to_all_solvers: bool,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "dispute_id": dispute_id,
        "handoff_event_id": handoff_event_id,
        "configured_solver_count": configured_solver_count,
        "fallback_to_all_solvers": fallback_to_all_solvers,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::EscalationDispatchUnroutable,
        session_id,
        &payload,
        None,
        prompt_bundle_id,
        policy_hash,
        occurred_at,
    )
}

/// FR-214 typed constructor for the parse-failed dispatch case.
///
/// Two sub-shapes disambiguated by `reason`:
/// - `deserialize_failed` — `payload_json` did not parse into a
///   `HandoffPackage`.
/// - `orphan_dispute_reference` — payload parsed but `dispute_id`
///   has no row in `disputes` (theoretically impossible; defensive).
///
/// The event is considered consumed so the queue moves forward;
/// manual operator action is required to re-dispatch.
#[allow(clippy::too_many_arguments)]
pub fn record_escalation_dispatch_parse_failed(
    conn: &Connection,
    session_id: Option<&str>,
    dispute_id: &str,
    handoff_event_id: i64,
    reason: &str,
    detail: &str,
    prompt_bundle_id: Option<&str>,
    policy_hash: Option<&str>,
    occurred_at: i64,
) -> Result<i64> {
    let payload = json!({
        "dispute_id": dispute_id,
        "handoff_event_id": handoff_event_id,
        "reason": reason,
        "detail": detail,
    })
    .to_string();
    record_event(
        conn,
        MediationEventKind::EscalationDispatchParseFailed,
        session_id,
        &payload,
        None,
        prompt_bundle_id,
        policy_hash,
        occurred_at,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use rusqlite::params;

    fn fresh_with_session() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d1', 'e1', 'm1', 'buyer', 'initiated', 1, 2, 'new')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                started_at, last_transition_at
             ) VALUES ('sess-1', 'd1', 'awaiting_response', 0,
                       'phase3-default', 'pol-hash', 100, 100)",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn kind_as_str_matches_data_model_spellings() {
        // Explicit cross-check against the snake_case forms in
        // data-model.md §mediation_events. If anyone renames the
        // data-model column value, this table fails loudly.
        let expected = [
            (
                MediationEventKind::StartAttemptStarted,
                "start_attempt_started",
            ),
            (
                MediationEventKind::StartAttemptStopped,
                "start_attempt_stopped",
            ),
            (MediationEventKind::ReasoningVerdict, "reasoning_verdict"),
            (MediationEventKind::TakeDisputeIssued, "take_dispute_issued"),
            (MediationEventKind::SessionOpened, "session_opened"),
            (MediationEventKind::OutboundSent, "outbound_sent"),
            (MediationEventKind::InboundIngested, "inbound_ingested"),
            (MediationEventKind::StateTransition, "state_transition"),
            (
                MediationEventKind::ClassificationProduced,
                "classification_produced",
            ),
            (MediationEventKind::SummaryGenerated, "summary_generated"),
            (
                MediationEventKind::EscalationRecommended,
                "escalation_recommended",
            ),
            (MediationEventKind::HandoffPrepared, "handoff_prepared"),
            (
                MediationEventKind::ReasoningCallFailed,
                "reasoning_call_failed",
            ),
            (MediationEventKind::AuthorizationLost, "authorization_lost"),
            (MediationEventKind::AuthRetryAttempt, "auth_retry_attempt"),
            (
                MediationEventKind::AuthRetryTerminated,
                "auth_retry_terminated",
            ),
            (
                MediationEventKind::AuthRetryRecovered,
                "auth_retry_recovered",
            ),
            (MediationEventKind::SupersededByHuman, "superseded_by_human"),
            (
                MediationEventKind::ResolvedExternallyReported,
                "resolved_externally_reported",
            ),
            (MediationEventKind::SessionClosed, "session_closed"),
            (
                MediationEventKind::EscalationDispatched,
                "escalation_dispatched",
            ),
            (
                MediationEventKind::EscalationSuperseded,
                "escalation_superseded",
            ),
            (
                MediationEventKind::EscalationDispatchUnroutable,
                "escalation_dispatch_unroutable",
            ),
            (
                MediationEventKind::EscalationDispatchParseFailed,
                "escalation_dispatch_parse_failed",
            ),
        ];
        for (kind, want) in expected {
            assert_eq!(kind.as_str(), want, "kind {kind:?} string form drifted");
        }
    }

    #[test]
    fn session_opened_constructor_writes_expected_row() {
        let conn = fresh_with_session();
        let id = record_session_opened(&conn, "sess-1", "phase3-default", "pol-hash", 500).unwrap();
        assert!(id > 0);
        let (kind, sid, bid, ph): (String, String, String, String) = conn
            .query_row(
                "SELECT kind, session_id, prompt_bundle_id, policy_hash
                 FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "session_opened");
        assert_eq!(sid, "sess-1");
        assert_eq!(bid, "phase3-default");
        assert_eq!(ph, "pol-hash");
    }

    #[test]
    fn outbound_sent_constructor_encodes_payload_and_pin() {
        let conn = fresh_with_session();
        let id = record_outbound_sent(
            &conn,
            "sess-1",
            "shared-pk-hex",
            "inner-event-id",
            Some("phase3-default"),
            Some("pol-hash"),
            600,
        )
        .unwrap();
        let (kind, payload, bundle, hash): (String, String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT kind, payload_json, prompt_bundle_id, policy_hash
                 FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "outbound_sent");
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["shared_pubkey"], "shared-pk-hex");
        assert_eq!(parsed["inner_event_id"], "inner-event-id");
        assert_eq!(bundle.as_deref(), Some("phase3-default"));
        assert_eq!(hash.as_deref(), Some("pol-hash"));
    }

    #[test]
    fn classification_produced_references_rationale_without_inlining_text() {
        let conn = fresh_with_session();
        // rationale_id is an FK into reasoning_rationales; seed a
        // rationale so the constraint holds.
        let rationale_id_var = crate::db::rationales::insert_rationale(
            &conn,
            Some("sess-1"),
            "openai",
            "gpt-5",
            "phase3-default",
            "pol-hash",
            "rationale body for the classification",
            650,
        )
        .unwrap();
        let id = record_classification_produced(
            &conn,
            "sess-1",
            &rationale_id_var,
            "coordination_failure_resolvable",
            0.9,
            Some("phase3-default"),
            Some("pol-hash"),
            700,
        )
        .unwrap();
        let (kind, payload, rationale_id, bundle, hash): (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT kind, payload_json, rationale_id, prompt_bundle_id, policy_hash
                 FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(kind, "classification_produced");
        assert_eq!(rationale_id.as_deref(), Some(rationale_id_var.as_str()));
        assert_eq!(bundle.as_deref(), Some("phase3-default"));
        assert_eq!(hash.as_deref(), Some("pol-hash"));
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["classification"], "coordination_failure_resolvable");
        assert!((parsed["confidence"].as_f64().unwrap() - 0.9).abs() < 1e-9);
        assert_eq!(parsed["rationale_id"], rationale_id_var.as_str());
        // Sanity: the full rationale text is not present in the
        // payload (FR-120).
        assert!(
            !payload.contains("rationale_text"),
            "rationale_text must never be inlined into payload_json"
        );
    }

    #[test]
    fn daemon_level_event_allows_null_session_id() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let id = record_event(
            &conn,
            MediationEventKind::AuthRetryAttempt,
            None,
            "{\"attempt\":1,\"outcome\":\"pending\"}",
            None,
            None,
            None,
            42,
        )
        .unwrap();
        let sid: Option<String> = conn
            .query_row(
                "SELECT session_id FROM mediation_events WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            sid.is_none(),
            "daemon-level events may have NULL session_id"
        );
    }

    // ---------------------------------------------------------
    // Phase 10 — start-flow constructors (T097)
    // ---------------------------------------------------------

    /// A fresh in-memory DB without any seeded session. Lets the
    /// tests confirm that the start-flow constructors work
    /// dispute-scoped (session_id = NULL) when no session row
    /// exists yet — which is the normal case during a pre-take
    /// attempt.
    fn fresh_without_session() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-ph10', 'e1', 'm1', 'buyer',
                       'initiated', 1, 2, 'new')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn start_attempt_started_dispute_scoped() {
        let conn = fresh_without_session();
        let id = record_start_attempt_started(&conn, None, "d-ph10", "detected", 100).unwrap();
        let (kind, sid, payload): (String, Option<String>, String) = conn
            .query_row(
                "SELECT kind, session_id, payload_json
                 FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "start_attempt_started");
        assert!(
            sid.is_none(),
            "dispute-scoped row must have NULL session_id"
        );
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["dispute_id"], "d-ph10");
        assert_eq!(parsed["trigger"], "detected");
    }

    #[test]
    fn start_attempt_stopped_captures_stop_reason() {
        let conn = fresh_without_session();
        let id =
            record_start_attempt_stopped(&conn, None, "d-ph10", "reasoning_verdict_negative", 150)
                .unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM mediation_events WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["dispute_id"], "d-ph10");
        assert_eq!(parsed["stop_reason"], "reasoning_verdict_negative");
    }

    #[test]
    fn reasoning_verdict_references_rationale_id_dispute_scoped() {
        let conn = fresh_without_session();
        // Persist a rationale dispute-scoped (session_id = NULL).
        let rationale_id_var = crate::db::rationales::insert_rationale(
            &conn,
            None,
            "openai",
            "gpt-5",
            "phase3-default",
            "pol-hash",
            "rationale for a dispute-scoped verdict",
            200,
        )
        .unwrap();
        let id = record_reasoning_verdict(
            &conn,
            None,
            "d-ph10",
            "mediation_eligible",
            "coordination_failure_resolvable",
            0.87,
            &rationale_id_var,
            Some("phase3-default"),
            Some("pol-hash"),
            210,
        )
        .unwrap();
        let (sid, rationale_id, payload): (Option<String>, Option<String>, String) = conn
            .query_row(
                "SELECT session_id, rationale_id, payload_json
                 FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(sid.is_none());
        assert_eq!(rationale_id.as_deref(), Some(rationale_id_var.as_str()));
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["dispute_id"], "d-ph10");
        assert_eq!(parsed["verdict"], "mediation_eligible");
        assert_eq!(parsed["classification"], "coordination_failure_resolvable");
        // Full rationale text must not leak into the event payload
        // (FR-120).
        assert!(!payload.contains("rationale body"));
        assert!(!payload.contains("rationale for a dispute-scoped"));
    }

    #[test]
    fn take_dispute_issued_failure_carries_reason() {
        let conn = fresh_without_session();
        let id = record_take_dispute_issued(
            &conn,
            None,
            "d-ph10",
            "failure",
            Some("relay refused AdminTakeDispute"),
            300,
        )
        .unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM mediation_events WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["outcome"], "failure");
        assert_eq!(parsed["reason"], "relay refused AdminTakeDispute");
    }

    #[test]
    fn take_dispute_issued_success_omits_reason() {
        let conn = fresh_without_session();
        let id = record_take_dispute_issued(&conn, None, "d-ph10", "success", None, 310).unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM mediation_events WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["outcome"], "success");
        assert!(
            parsed.get("reason").is_none(),
            "success payload must omit reason"
        );
    }

    #[test]
    fn resolved_externally_reported_records_all_flags() {
        let conn = fresh_with_session();
        let id = record_resolved_externally_reported(
            &conn,
            Some("sess-1"),
            "d1",
            "settled",
            2,
            true,
            false,
            "targeted",
            Some("phase3-default"),
            Some("pol-hash"),
            900,
        )
        .unwrap();
        let (kind, sid, payload, bundle, hash): (
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT kind, session_id, payload_json,
                        prompt_bundle_id, policy_hash
                 FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(kind, "resolved_externally_reported");
        assert_eq!(sid.as_deref(), Some("sess-1"));
        assert_eq!(bundle.as_deref(), Some("phase3-default"));
        assert_eq!(hash.as_deref(), Some("pol-hash"));
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["dispute_id"], "d1");
        assert_eq!(parsed["final_dispute_status"], "settled");
        assert_eq!(parsed["outbound_party_messages_count"], 2);
        assert_eq!(parsed["had_classification"], true);
        assert_eq!(parsed["had_escalation_recommendation"], false);
        assert_eq!(parsed["notifier_route"], "targeted");
    }

    #[test]
    fn resolved_externally_reported_allows_null_session() {
        let conn = fresh_without_session();
        let id = record_resolved_externally_reported(
            &conn,
            None,
            "d-ph10",
            "released",
            0,
            true,
            false,
            "broadcast",
            None,
            None,
            950,
        )
        .unwrap();
        let sid: Option<String> = conn
            .query_row(
                "SELECT session_id FROM mediation_events WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            sid.is_none(),
            "FR-124 reasoning-verdict-only case must emit the report with session_id = NULL"
        );
    }

    // --- Phase 4 typed-constructor payload shape tests ---
    // The four kinds carry distinct payload shapes; each test pins
    // the exact keys per contracts/audit-events.md so a
    // payload-formatting drift fails loudly rather than showing up
    // as an operator-facing bug.

    #[test]
    fn escalation_dispatched_payload_carries_required_keys() {
        let mut conn = fresh_with_session();
        let tx = conn.transaction().unwrap();
        let id = record_escalation_dispatched(
            &tx,
            Some("sess-1"),
            "dispatch-abc",
            "d-ph4",
            42,
            "solver-pk",
            "dispatched",
            false,
            Some("phase3-default"),
            Some("policy-hash"),
            1000,
        )
        .unwrap();
        tx.commit().unwrap();
        let (kind, payload): (String, String) = conn
            .query_row(
                "SELECT kind, payload_json FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "escalation_dispatched");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["dispatch_id"], "dispatch-abc");
        assert_eq!(v["dispute_id"], "d-ph4");
        assert_eq!(v["handoff_event_id"], 42);
        assert_eq!(v["target_solver"], "solver-pk");
        assert_eq!(v["status"], "dispatched");
        assert_eq!(v["fallback_broadcast"], false);
    }

    #[test]
    fn escalation_dispatched_payload_preserves_fallback_flag() {
        let mut conn = fresh_with_session();
        let tx = conn.transaction().unwrap();
        let id = record_escalation_dispatched(
            &tx,
            None,
            "dispatch-fb",
            "d-ph4",
            99,
            "pk-1,pk-2,pk-3",
            "dispatched",
            true,
            None,
            None,
            1100,
        )
        .unwrap();
        tx.commit().unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM mediation_events WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["fallback_broadcast"], true);
        assert_eq!(v["target_solver"], "pk-1,pk-2,pk-3");
    }

    #[test]
    fn escalation_superseded_payload_pins_reason_key() {
        let conn = fresh_with_session();
        let id = record_escalation_superseded(
            &conn,
            Some("sess-1"),
            "d-ph4",
            77,
            "dispute_already_resolved",
            None,
            None,
            1200,
        )
        .unwrap();
        let (kind, payload): (String, String) = conn
            .query_row(
                "SELECT kind, payload_json FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "escalation_superseded");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["dispute_id"], "d-ph4");
        assert_eq!(v["handoff_event_id"], 77);
        assert_eq!(v["reason"], "dispute_already_resolved");
    }

    #[test]
    fn escalation_dispatch_unroutable_payload_shape() {
        let conn = fresh_without_session();
        let id = record_escalation_dispatch_unroutable(
            &conn, None, "d-ph4", 88, 2, false, None, None, 1300,
        )
        .unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM mediation_events WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["dispute_id"], "d-ph4");
        assert_eq!(v["handoff_event_id"], 88);
        assert_eq!(v["configured_solver_count"], 2);
        assert_eq!(v["fallback_to_all_solvers"], false);
    }

    #[test]
    fn escalation_dispatch_parse_failed_distinguishes_reasons() {
        let conn = fresh_without_session();
        // deserialize_failed
        let id1 = record_escalation_dispatch_parse_failed(
            &conn,
            None,
            "d-ph4",
            101,
            "deserialize_failed",
            "expected `,` or `}` at line 1 column 12",
            None,
            None,
            1400,
        )
        .unwrap();
        let payload1: String = conn
            .query_row(
                "SELECT payload_json FROM mediation_events WHERE id = ?1",
                params![id1],
                |r| r.get(0),
            )
            .unwrap();
        let v1: serde_json::Value = serde_json::from_str(&payload1).unwrap();
        assert_eq!(v1["reason"], "deserialize_failed");
        assert!(v1["detail"].as_str().unwrap().contains("column 12"));

        // orphan_dispute_reference
        let id2 = record_escalation_dispatch_parse_failed(
            &conn,
            None,
            "d-orphan",
            102,
            "orphan_dispute_reference",
            "dispute_id not found",
            None,
            None,
            1500,
        )
        .unwrap();
        let payload2: String = conn
            .query_row(
                "SELECT payload_json FROM mediation_events WHERE id = ?1",
                params![id2],
                |r| r.get(0),
            )
            .unwrap();
        let v2: serde_json::Value = serde_json::from_str(&payload2).unwrap();
        assert_eq!(v2["reason"], "orphan_dispute_reference");
        assert_eq!(v2["detail"], "dispute_id not found");
    }

    #[test]
    fn escalation_superseded_exists_for_handoff_is_false_before_any_write() {
        let conn = fresh_with_session();
        assert!(!escalation_superseded_exists_for_handoff(&conn, 42).unwrap());
    }

    #[test]
    fn escalation_superseded_exists_for_handoff_is_true_after_write_and_scoped_per_id() {
        let conn = fresh_with_session();
        // Seed a handoff_prepared row so the foreign-key chain stays
        // valid (the audit payload references a real handoff_event_id).
        let handoff_a: i64 = conn
            .query_row(
                "INSERT INTO mediation_events (
                    session_id, kind, payload_json, occurred_at
                 ) VALUES (NULL, 'handoff_prepared', '{}', 100)
                 RETURNING id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        record_escalation_superseded(
            &conn,
            None,
            "d-exists",
            handoff_a,
            "dispute_already_resolved",
            Some("phase3-default"),
            Some("hash-1"),
            200,
        )
        .unwrap();
        assert!(escalation_superseded_exists_for_handoff(&conn, handoff_a).unwrap());
        // A different handoff_event_id must still read as "absent" —
        // the dedup check is per handoff, not global.
        assert!(!escalation_superseded_exists_for_handoff(&conn, handoff_a + 999).unwrap());
    }
}
