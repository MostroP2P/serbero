//! Escalation pipeline (US4 / T067).
//!
//! One public entry point, [`recommend`]. Every trigger path —
//! model-side (`conflicting_claims`, `fraud_indicator`,
//! `low_confidence`, `authority_boundary_attempt`,
//! `reasoning_unavailable`), engine-side (`round_limit`,
//! `party_unresponsive`, `authorization_lost`,
//! `policy_bundle_missing`), and the T060
//! `notification_failed` / `invalid_model_output` variants — funnels
//! here. That one chokepoint keeps the Phase 4 handoff contract
//! honest: every escalation produces exactly one
//! `escalation_recommended` event and exactly one
//! `handoff_prepared` event with a serialized [`HandoffPackage`],
//! both inside a single DB transaction so the state flip + audit
//! rows cannot drift.
//!
//! Phase 4 is explicitly NOT implemented by this module.
//! `recommend` stops the mediation flow — it does not DM solvers,
//! does not route the handoff, and must never call
//! `draft_and_send_initial_message` or any other outbound path.
//! The operator-facing solver alert (the new
//! `MediationEscalationRecommended` notification) is fired by the
//! engine after `recommend` returns, not by `recommend` itself.
//!
//! FR-120 discipline: evidence refs carry ids only (content-hashes
//! for rationales, event ids for chat / outbound), never raw text.
//! The caller partitions the two kinds at the call site —
//! [`RecommendParams`] exposes two distinct fields so we do not
//! have to guess via a hex-length heuristic. Nostr event ids and
//! rationale ids are both lowercase 64-char SHA-256 hex, so any
//! such heuristic would misclassify them.
//!
//! Exactly-once handoff: the state flip is a conditional UPDATE
//! that only advances out of the non-terminal live states. If the
//! session is already at `escalation_recommended` (or any other
//! state from which this transition is illegal), the UPDATE affects
//! zero rows, the transaction is rolled back, and
//! [`recommend`] returns an error — preventing a duplicate
//! `handoff_prepared` event in release builds where the
//! `debug_assert!` inside `set_session_state` is stripped.

use std::sync::Arc;

use rusqlite::params;
use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, instrument, warn};

use crate::db;
use crate::db::mediation_events::MediationEventKind;
use crate::error::{Error, Result};
use crate::models::mediation::EscalationTrigger;
#[cfg(test)]
use crate::models::mediation::MediationSessionState;

/// The set of `mediation_sessions.state` values from which a
/// transition to `escalation_recommended` is legal. Single source of
/// truth used by both the SQL conditional UPDATE in [`recommend`]
/// and the invariant pin test below, so the whitelist cannot drift
/// away from [`MediationSessionState::can_transition_to`].
const ESCALATABLE_STATES: &[&str] = &[
    "opening",
    "awaiting_response",
    "classified",
    "follow_up_pending",
    "summary_pending",
];

/// Phase 4 handoff package. Persisted as the `handoff_prepared`
/// mediation event's payload so Phase 4 can consume it later by
/// reading the audit log — no additional table needed.
///
/// `session_id` is optional: mid-session escalations always carry a
/// session id, but opening-call escalations (policy returned
/// `Escalate` before the `TakeDispute` step) never committed a
/// session row in the first place and the field stays `None`.
/// Phase 4 consumers MUST accept both shapes — see FR-122 and the
/// dispute-scoped handoff flow added in T104d.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffPackage {
    pub dispute_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// `EscalationTrigger::to_string()` — the snake-case form so
    /// the payload is operator-readable and grep-friendly.
    pub trigger: String,
    /// Caller-supplied non-rationale evidence refs
    /// (inner_event_ids, outbound event ids, free-form notes).
    pub evidence_refs: Vec<String>,
    pub prompt_bundle_id: String,
    pub policy_hash: String,
    /// Rationale ids the caller already extracted from
    /// `reasoning_rationales` (SHA-256 content hashes). Kept
    /// separate from `evidence_refs` so Phase 4 can fan the two
    /// kinds out to distinct audit consumers.
    pub rationale_refs: Vec<String>,
    pub assembled_at: i64,
}

/// Parameters for [`recommend`]. Grouped so the call site stays
/// compact and clippy does not flag too_many_arguments.
///
/// Two shapes:
/// - **Session-scoped** (mid-session / post-open): `session_id` is
///   `Some`. The session row's state flips to
///   `escalation_recommended` and the audit events carry the
///   session id. `dispute_id` must still match the row (checked at
///   runtime).
/// - **Dispute-scoped** (opening-call escalation per FR-122):
///   `session_id` is `None` because no `mediation_sessions` row
///   was ever committed. No state flip is attempted; the audit
///   events land with `session_id = NULL` and the dispute id lives
///   in the payload.
pub struct RecommendParams<'a> {
    pub conn: &'a Arc<AsyncMutex<rusqlite::Connection>>,
    pub session_id: Option<&'a str>,
    /// Required in both shapes. For session-scoped calls it is
    /// cross-checked against `mediation_sessions.dispute_id`; for
    /// dispute-scoped calls it is written into the event payload.
    pub dispute_id: &'a str,
    pub trigger: EscalationTrigger,
    /// Non-rationale evidence refs (inner/outer event ids,
    /// outbound wrap ids, free-form notes). Caller-partitioned.
    pub evidence_refs: Vec<String>,
    /// Rationale ids from the `reasoning_rationales` audit store.
    /// Caller-partitioned — `recommend` does not attempt to
    /// distinguish these from event ids at runtime.
    pub rationale_refs: Vec<String>,
    pub prompt_bundle_id: &'a str,
    pub policy_hash: &'a str,
}

/// Mark a session `escalation_recommended`, record the trigger and
/// assemble the Phase 4 handoff package — all in one transaction.
///
/// Exactly-once semantics: the state flip is guarded by a
/// conditional UPDATE that only transitions out of the live,
/// non-terminal states. A second call on the same session returns
/// an error (no rows updated) and writes no events.
///
/// Does NOT send any outbound chat message; does NOT notify solvers
/// (the engine owns that); does NOT retry on DB error (the single
/// transaction either lands or rolls back).
#[instrument(skip_all, fields(
    session_id = params.session_id.unwrap_or("<none>"),
    dispute_id = %params.dispute_id,
    trigger = %params.trigger,
))]
pub async fn recommend(params: RecommendParams<'_>) -> Result<()> {
    let RecommendParams {
        conn,
        session_id,
        dispute_id,
        trigger,
        evidence_refs,
        rationale_refs,
        prompt_bundle_id,
        policy_hash,
    } = params;

    let now = super::current_ts_secs()?;

    let mut guard = conn.lock().await;

    // When session-scoped, sanity-check that `dispute_id` matches
    // the stored row. Prevents a caller from escalating a session
    // under the wrong dispute id (which would poison the handoff
    // package and any FR-124 final-report lookup).
    if let Some(sid) = session_id {
        let stored: String = match guard.query_row(
            "SELECT dispute_id FROM mediation_sessions WHERE session_id = ?1",
            params![sid],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) => s,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(Error::InvalidEvent(format!(
                    "escalation::recommend: no mediation_sessions row for session_id={sid}"
                )));
            }
            Err(e) => return Err(Error::Db(e)),
        };
        if stored != dispute_id {
            return Err(Error::InvalidEvent(format!(
                "escalation::recommend: session_id={sid} stored dispute_id={stored} \
                 does not match caller-supplied dispute_id={dispute_id}"
            )));
        }
    }

    // (2)–(4) tx: state flip (session-scoped only) + 2 events in
    //            one atomic block.
    let tx = guard.transaction()?;

    // Conditional state flip. Only transitions OUT of the live
    // non-terminal states are allowed — a second call on an
    // already-escalated session updates zero rows, the tx is
    // rolled back below, and no duplicate events are written.
    //
    // Skipped entirely for dispute-scoped calls: no session row
    // exists to flip, and the audit events below will carry
    // `session_id = NULL` so Phase 4 can distinguish the two shapes.
    //
    // For session-scoped calls the WHERE clause binds the shared
    // [`ESCALATABLE_STATES`] whitelist so the allowed-transition set
    // stays in lockstep with [`MediationSessionState::can_transition_to`]
    // (pinned by the `escalatable_states_match_can_transition_to`
    // unit test). Encoding the rule in SQL makes the guarantee hold
    // in release builds where the `debug_assert!` inside
    // `set_session_state` is stripped.
    if let Some(sid) = session_id {
        let placeholders = (0..ESCALATABLE_STATES.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let update_sql = format!(
            "UPDATE mediation_sessions
             SET state = 'escalation_recommended', last_transition_at = ?1
             WHERE session_id = ?2 AND state IN ({placeholders})"
        );
        let mut sql_params: Vec<&dyn rusqlite::ToSql> =
            Vec::with_capacity(2 + ESCALATABLE_STATES.len());
        sql_params.push(&now);
        sql_params.push(&sid);
        for s in ESCALATABLE_STATES {
            sql_params.push(s);
        }
        let rows = tx.execute(&update_sql, sql_params.as_slice())?;
        if rows == 0 {
            // Read the actual current state inside the same
            // transaction so the error carries the real `from` value
            // rather than a placeholder string. Operators reading
            // the resulting log then see "classified ->
            // escalation_recommended" (or similar) — enough context
            // to tell a double-escalation attempt apart from a FK /
            // race bug.
            let actual: String = tx
                .query_row(
                    "SELECT state FROM mediation_sessions WHERE session_id = ?1",
                    params![sid],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_else(|_| "<session row missing>".to_string());
            return Err(Error::InvalidStateTransition {
                from: actual,
                to: "escalation_recommended".to_string(),
            });
        }
    }

    // Both events carry `dispute_id` in the payload so consumers
    // can always navigate back to the dispute even when
    // `session_id` is NULL (dispute-scoped shape).
    let escalation_payload = serde_json::json!({
        "dispute_id": dispute_id,
        "trigger": trigger.to_string(),
        "evidence_refs": evidence_refs,
        "rationale_refs": rationale_refs,
    })
    .to_string();
    db::mediation_events::record_event(
        &tx,
        MediationEventKind::EscalationRecommended,
        session_id,
        &escalation_payload,
        None,
        Some(prompt_bundle_id),
        Some(policy_hash),
        now,
    )?;

    let package = HandoffPackage {
        dispute_id: dispute_id.to_string(),
        session_id: session_id.map(|s| s.to_string()),
        trigger: trigger.to_string(),
        evidence_refs,
        prompt_bundle_id: prompt_bundle_id.to_string(),
        policy_hash: policy_hash.to_string(),
        rationale_refs,
        assembled_at: now,
    };
    let handoff_payload = serde_json::to_string(&package).map_err(|e| {
        Error::InvalidEvent(format!(
            "escalation::recommend: failed to serialize HandoffPackage: {e}"
        ))
    })?;
    db::mediation_events::record_event(
        &tx,
        MediationEventKind::HandoffPrepared,
        session_id,
        &handoff_payload,
        None,
        Some(prompt_bundle_id),
        Some(policy_hash),
        now,
    )?;

    tx.commit()?;
    drop(guard);

    warn!(
        session_id = session_id.unwrap_or("<none>"),
        dispute_id = %dispute_id,
        trigger = %trigger,
        "escalation_recommended"
    );
    info!(
        session_id = session_id.unwrap_or("<none>"),
        dispute_id = %dispute_id,
        "handoff_prepared"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    /// Drift-pin: the SQL whitelist used by `recommend` and the
    /// Rust-side `can_transition_to` table must agree on which
    /// states are escalatable. This test walks every
    /// [`MediationSessionState`] variant and asserts membership in
    /// [`ESCALATABLE_STATES`] matches
    /// `self.can_transition_to(EscalationRecommended)`. If either
    /// side adds or removes a state without touching the other,
    /// this fires.
    #[test]
    fn escalatable_states_match_can_transition_to() {
        for s in [
            MediationSessionState::Opening,
            MediationSessionState::AwaitingResponse,
            MediationSessionState::Classified,
            MediationSessionState::FollowUpPending,
            MediationSessionState::SummaryPending,
            MediationSessionState::SummaryDelivered,
            MediationSessionState::EscalationRecommended,
            MediationSessionState::SupersededByHuman,
            MediationSessionState::Closed,
        ] {
            let tag = s.to_string();
            let sql_allows = ESCALATABLE_STATES.contains(&tag.as_str());
            let rust_allows = s.can_transition_to(MediationSessionState::EscalationRecommended);
            assert_eq!(
                sql_allows, rust_allows,
                "drift for {tag}: SQL whitelist says {sql_allows}, \
                 can_transition_to says {rust_allows}"
            );
        }
    }

    fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-esc', 'e1', 'm1', 'buyer',
                       'initiated', 1, 2, 'notified')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mediation_sessions (
                session_id, dispute_id, state, round_count,
                prompt_bundle_id, policy_hash,
                started_at, last_transition_at
             ) VALUES ('sess-esc', 'd-esc', 'awaiting_response', 0,
                       'phase3-default', 'test-policy-hash',
                       100, 100)",
            [],
        )
        .unwrap();
        Arc::new(AsyncMutex::new(conn))
    }

    async fn count_events(conn: &Arc<AsyncMutex<rusqlite::Connection>>, kind: &str) -> i64 {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_events WHERE session_id = 'sess-esc' AND kind = ?1",
            params![kind],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn second_call_on_same_session_is_rejected_without_duplicate_events() {
        let conn = fresh_conn();
        recommend(RecommendParams {
            conn: &conn,
            session_id: Some("sess-esc"),
            dispute_id: "d-esc",
            trigger: EscalationTrigger::LowConfidence,
            evidence_refs: Vec::new(),
            rationale_refs: Vec::new(),
            prompt_bundle_id: "phase3-default",
            policy_hash: "test-policy-hash",
        })
        .await
        .expect("first call must succeed");

        assert_eq!(count_events(&conn, "escalation_recommended").await, 1);
        assert_eq!(count_events(&conn, "handoff_prepared").await, 1);

        // Second call: the conditional UPDATE affects 0 rows, the
        // tx rolls back, and no additional event rows land.
        let err = recommend(RecommendParams {
            conn: &conn,
            session_id: Some("sess-esc"),
            dispute_id: "d-esc",
            trigger: EscalationTrigger::RoundLimit,
            evidence_refs: Vec::new(),
            rationale_refs: Vec::new(),
            prompt_bundle_id: "phase3-default",
            policy_hash: "test-policy-hash",
        })
        .await
        .expect_err("second call on the same session must fail");
        // The error reflects the real persisted state so operators
        // can tell a double-escalation attempt apart from a FK bug.
        match err {
            Error::InvalidStateTransition { from, to } => {
                assert_eq!(from, "escalation_recommended");
                assert_eq!(to, "escalation_recommended");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        assert_eq!(
            count_events(&conn, "escalation_recommended").await,
            1,
            "no duplicate escalation_recommended event in release or debug"
        );
        assert_eq!(
            count_events(&conn, "handoff_prepared").await,
            1,
            "no duplicate handoff_prepared event in release or debug"
        );
    }

    #[tokio::test]
    async fn explicit_rationale_refs_are_preserved_verbatim() {
        let conn = fresh_conn();
        // Two 64-hex strings that would collide with the old
        // heuristic — one is an event id, one is a rationale id.
        // With explicit fields the caller's partitioning sticks.
        let event_id = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let rationale_id = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        recommend(RecommendParams {
            conn: &conn,
            session_id: Some("sess-esc"),
            dispute_id: "d-esc",
            trigger: EscalationTrigger::ConflictingClaims,
            evidence_refs: vec![event_id.into()],
            rationale_refs: vec![rationale_id.into()],
            prompt_bundle_id: "phase3-default",
            policy_hash: "test-policy-hash",
        })
        .await
        .unwrap();

        let payload: String = {
            let c = conn.lock().await;
            c.query_row(
                "SELECT payload_json FROM mediation_events
                 WHERE session_id='sess-esc' AND kind='handoff_prepared'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["evidence_refs"][0], event_id);
        assert_eq!(v["rationale_refs"][0], rationale_id);
    }
}
