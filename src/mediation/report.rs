//! FR-124 final solver-facing report for externally resolved disputes
//! (Phase 10 / T107).
//!
//! Called from `handlers/dispute_resolved.rs` whenever Mostro signals
//! a terminal `DisputeStatus` (`seller-refunded`, `buyer-refunded`,
//! etc.) for a dispute Serbero has touched. The report closes the
//! loop: every dispute that reached `mediation_sessions` OR that
//! produced Phase 3 audit events (dispute-scoped `reasoning_verdict`,
//! `start_attempt_started`, etc.) gets exactly one
//! `MediationResolutionReport` DM to the configured solver(s) with a
//! short narrative of what Serbero did, what classification it
//! recorded (if any), and how the dispute ultimately resolved.
//!
//! Two entry points:
//!
//! - [`has_any_mediation_context`] — boolean predicate. Cheap three-
//!   row-count SQL that answers "did Serbero ever write anything to
//!   the mediation tables for this dispute?" The handler uses this
//!   to skip disputes that were Phase 1/2-only (no session, no
//!   dispute-scoped event). Phase 1/2 notification stays untouched
//!   for those disputes.
//!
//! - [`emit_final_report`] — build the report payload, deliver it via
//!   the Phase 1/2 notifier, and record
//!   `resolved_externally_reported` with the full payload summary so
//!   the audit trail captures exactly what the solvers saw.
//!
//! ## Idempotency
//!
//! The outer handler (`handlers/dispute_resolved.rs`) already
//! short-circuits on
//! `disputes.lifecycle_state == LifecycleState::Resolved`, so a
//! replay of the same `DisputeStatus` event never re-enters this
//! module. `emit_final_report` is therefore NOT required to carry
//! its own `(dispute_id, final_status)` dedup: it only needs to be
//! safe against accidental double-invocation inside a single handler
//! call, which the deterministic SQL reads give for free.
//!
//! ## FR-120 discipline
//!
//! The DM body contains a short narrative, the dispute id, the
//! session id (if any), and the classification label + confidence
//! (if any). The full rationale text NEVER leaves the controlled
//! audit store — if a rationale reference is relevant, only the
//! rationale id (SHA-256 hex) goes into the body. That keeps DMs
//! replay-friendly and prevents operator-visible logs from leaking
//! privileged reasoning text.

use std::sync::Arc;

use nostr_sdk::prelude::*;
use rusqlite::params;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info, instrument, warn};

use crate::db;
use crate::error::Result;
use crate::models::mediation::ClassificationLabel;
use crate::models::SolverConfig;

/// Structured payload consumed by [`build_report_body`] and
/// [`deliver_report`]. Extracted from the DB in a single read so the
/// body is deterministic w.r.t. the state at call time.
#[derive(Debug, Clone)]
pub struct FinalReportPayload {
    pub dispute_id: String,
    /// `Some` when a `mediation_sessions` row exists (including
    /// terminal and escalated states); `None` for the "reasoning
    /// verdict but no session row" path — FR-122's dispute-scoped
    /// handoff shape.
    pub session_id: Option<String>,
    /// The most recent classification Serbero recorded for the
    /// dispute, drawn from the `classification_produced` audit
    /// event. `None` when no classification_produced event exists
    /// for this dispute (e.g. reasoning-call failed before
    /// rationale creation).
    pub classification: Option<(ClassificationLabel, f64)>,
    /// Number of distinct outbound party messages Serbero dispatched.
    /// Clamped 0..=2 because Phase 3 sessions address at most the
    /// buyer + the seller; a higher value would be a schema bug.
    pub outbound_party_messages_count: u8,
    /// The Mostro-reported final `DisputeStatus` for this dispute
    /// (e.g. `"seller-refunded"`, `"buyer-refunded"`).
    pub final_dispute_status: String,
    /// Operator-facing narrative derived from the session/event
    /// history. Never embeds the full rationale text (FR-120).
    pub narrative: String,
}

/// Cheap-yet-authoritative test for "did Serbero touch this dispute
/// at all beyond Phase 1/2 notification?".
///
/// Three disjoint ways a mediation context can exist:
/// 1. A `mediation_sessions` row — any state, terminal or otherwise.
/// 2. A session-scoped `mediation_events` row joining back to this
///    dispute via `mediation_sessions.dispute_id`.
/// 3. A dispute-scoped `mediation_events` row whose `payload_json`
///    references this `dispute_id` — covers the FR-122 shape where
///    reasoning ran but no take was issued.
///
/// The three checks are plain `EXISTS` probes; each uses an index so
/// the overall cost is bounded regardless of the table size.
#[instrument(skip(conn), fields(dispute_id = %dispute_id))]
pub async fn has_any_mediation_context(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
) -> Result<bool> {
    let guard = conn.lock().await;
    let exists: bool = guard.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM mediation_sessions WHERE dispute_id = ?1
            UNION ALL
            SELECT 1 FROM mediation_events me
             JOIN mediation_sessions s ON me.session_id = s.session_id
             WHERE s.dispute_id = ?1
            UNION ALL
            SELECT 1 FROM mediation_events
             WHERE session_id IS NULL AND payload_json LIKE ?2
         )",
        params![dispute_id, format!("%{dispute_id}%")],
        |r| r.get::<_, bool>(0),
    )?;
    Ok(exists)
}

/// Build the report payload from the DB. Pure reader — no writes.
///
/// Resolution order when picking the representative classification:
/// 1. The most recent session-scoped `classification_produced` event
///    for any session row tied to this dispute — that row is the
///    policy layer's final word.
/// 2. The dispute-scoped `reasoning_verdict` event's payload —
///    present on FR-122 paths where reasoning ran but no session
///    was opened.
/// 3. `None` — no classification was ever recorded.
#[instrument(skip_all, fields(dispute_id = %dispute_id))]
async fn build_payload(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    final_dispute_status: &str,
) -> Result<FinalReportPayload> {
    use std::str::FromStr;
    let guard = conn.lock().await;

    // (1) session_id — the most recent session row for this dispute,
    // even when it is terminal or escalated. `list_live_sessions`
    // excludes terminal states, so we hit the table directly.
    let session_id: Option<String> = guard
        .query_row(
            "SELECT session_id FROM mediation_sessions
             WHERE dispute_id = ?1
             ORDER BY started_at DESC
             LIMIT 1",
            params![dispute_id],
            |r| r.get::<_, String>(0),
        )
        .ok();

    // (2) classification + confidence — session-scoped event first,
    // dispute-scoped reasoning_verdict as fallback. The payload
    // shape for both kinds puts `classification` (label) and
    // `confidence` at the top level.
    let mut classification: Option<(ClassificationLabel, f64)> = None;
    if let Some(sid) = session_id.as_deref() {
        if let Ok(payload_json) = guard.query_row(
            "SELECT payload_json FROM mediation_events
             WHERE session_id = ?1 AND kind = 'classification_produced'
             ORDER BY occurred_at DESC, id DESC
             LIMIT 1",
            params![sid],
            |r| r.get::<_, String>(0),
        ) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload_json) {
                let label = v["classification"]
                    .as_str()
                    .and_then(|s| ClassificationLabel::from_str(s).ok());
                let conf = v["confidence"].as_f64();
                if let (Some(l), Some(c)) = (label, conf) {
                    classification = Some((l, c));
                }
            }
        }
    }
    if classification.is_none() {
        if let Ok(payload_json) = guard.query_row(
            "SELECT payload_json FROM mediation_events
             WHERE session_id IS NULL
               AND kind = 'reasoning_verdict'
               AND payload_json LIKE ?1
             ORDER BY occurred_at DESC, id DESC
             LIMIT 1",
            params![format!("%{dispute_id}%")],
            |r| r.get::<_, String>(0),
        ) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload_json) {
                let label = v["classification"]
                    .as_str()
                    .and_then(|s| ClassificationLabel::from_str(s).ok());
                let conf = v["confidence"].as_f64();
                if let (Some(l), Some(c)) = (label, conf) {
                    classification = Some((l, c));
                }
            }
        }
    }

    // (3) outbound_party_messages_count — DISTINCT party count. A
    // session with one dispatched round addresses both buyer and
    // seller → value = 2. A session that escalated before take has
    // 0. Clamped 0..=2 defensively.
    let outbound_party_messages_count: u8 = if let Some(sid) = session_id.as_deref() {
        let n: i64 = guard
            .query_row(
                "SELECT COUNT(DISTINCT party) FROM mediation_messages
                 WHERE session_id = ?1 AND direction = 'outbound'",
                params![sid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        n.clamp(0, 2) as u8
    } else {
        0
    };

    // (4) narrative — short, operator-readable. Derived from the
    // session state (or absence), classification, and outbound count.
    // Never embeds raw rationale text (FR-120).
    let session_state: Option<String> = session_id.as_deref().and_then(|sid| {
        guard
            .query_row(
                "SELECT state FROM mediation_sessions WHERE session_id = ?1",
                params![sid],
                |r| r.get::<_, String>(0),
            )
            .ok()
    });

    drop(guard);

    let narrative = build_narrative(
        &session_id,
        session_state.as_deref(),
        &classification,
        outbound_party_messages_count,
        final_dispute_status,
    );

    Ok(FinalReportPayload {
        dispute_id: dispute_id.to_string(),
        session_id,
        classification,
        outbound_party_messages_count,
        final_dispute_status: final_dispute_status.to_string(),
        narrative,
    })
}

/// Compose the operator-facing narrative. Kept as a small pure
/// function so the shape is easy to pin in unit tests.
fn build_narrative(
    session_id: &Option<String>,
    session_state: Option<&str>,
    classification: &Option<(ClassificationLabel, f64)>,
    outbound_party_messages_count: u8,
    final_dispute_status: &str,
) -> String {
    let session_clause = match (session_id, session_state) {
        (Some(sid), Some(state)) => format!("Session {sid} was in state `{state}`."),
        (Some(sid), None) => format!("Session {sid} was active."),
        (None, _) => "No mediation session was opened (Serbero evaluated the dispute and \
                      the reasoning verdict declined to take it)."
            .to_string(),
    };
    let classification_clause = match classification {
        Some((label, conf)) => {
            format!("Serbero's last recorded classification was `{label}` (confidence {conf:.2}).")
        }
        None => "No classification was recorded.".to_string(),
    };
    let outbound_clause = match outbound_party_messages_count {
        0 => "No outbound party-facing messages were dispatched.".to_string(),
        1 => "Serbero messaged one party before the dispute was resolved externally.".to_string(),
        _ => {
            "Serbero messaged both parties before the dispute was resolved externally.".to_string()
        }
    };
    format!(
        "Dispute closed with final status `{final_dispute_status}`. \
         {session_clause} {classification_clause} {outbound_clause}"
    )
}

/// Build the DM body delivered to each recipient. FR-120-safe —
/// embeds payload fields but never raw rationale text.
pub fn build_report_body(payload: &FinalReportPayload) -> String {
    let session_line = match &payload.session_id {
        Some(sid) => format!("Session: {sid}\n"),
        None => "Session: <none — dispute-scoped handoff>\n".to_string(),
    };
    let classification_line = match &payload.classification {
        Some((label, conf)) => format!("Classification: {label} (confidence {conf:.2})\n"),
        None => "Classification: <none recorded>\n".to_string(),
    };
    format!(
        "mediation_resolution_report/v1\n\
         Dispute: {}\n\
         {session_line}\
         {classification_line}\
         Outbound party messages: {}\n\
         Final dispute status: {}\n\
         \n\
         {}",
        payload.dispute_id,
        payload.outbound_party_messages_count,
        payload.final_dispute_status,
        payload.narrative,
    )
}

/// Fire the FR-124 DM to every configured solver, route via the
/// existing Phase 1/2 notifier, and record
/// `resolved_externally_reported` with the payload summary.
///
/// Returns after both the DM dispatch and the audit write are done.
/// DM errors are absorbed inside the notifier (each recipient is
/// tried best-effort); an audit-write error is logged and the
/// function still returns `Ok(())` so the caller's outer handler
/// can proceed — the report has been delivered even if the log row
/// didn't land.
#[instrument(skip_all, fields(dispute_id = %dispute_id, final_dispute_status = %final_dispute_status))]
pub async fn emit_final_report(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    solvers: &[SolverConfig],
    dispute_id: &str,
    final_dispute_status: &str,
) -> Result<()> {
    let payload = build_payload(conn, dispute_id, final_dispute_status).await?;
    let body = build_report_body(&payload);

    info!(
        dispute_id = %dispute_id,
        session_id = payload.session_id.as_deref().unwrap_or("<none>"),
        final_dispute_status,
        "emit_final_report: delivering FR-124 DM to solvers"
    );

    // Deliver via the existing Phase 1/2 notifier. The broadcast
    // path is correct here: for a session-less handoff there is no
    // `assigned_solver` to target; for a session-backed report the
    // session may have been closed for long enough that the
    // assigned-solver field is stale. Broadcasting matches FR-124's
    // "for your records" shape.
    super::notify_solvers_final_resolution_report(
        conn,
        client,
        solvers,
        dispute_id,
        payload.session_id.as_deref(),
        &body,
    )
    .await;

    // Audit row — best-effort; a failure here is logged but does
    // not mask the successful delivery above. Built via `json!`
    // rather than a serde-derive so `ClassificationLabel`'s
    // `Display` rendering (snake_case tokens) ends up in the
    // payload — consistent with how other audit rows in this crate
    // store classification labels.
    let now = super::current_ts_secs()?;
    let audit_payload = serde_json::json!({
        "dispute_id": payload.dispute_id,
        "session_id": payload.session_id,
        "classification": payload
            .classification
            .as_ref()
            .map(|(l, _)| l.to_string()),
        "confidence": payload.classification.as_ref().map(|(_, c)| *c),
        "outbound_party_messages_count": payload.outbound_party_messages_count,
        "final_dispute_status": payload.final_dispute_status,
        "narrative": payload.narrative,
    })
    .to_string();
    {
        let guard = conn.lock().await;
        if let Err(e) = db::mediation_events::record_event(
            &guard,
            db::mediation_events::MediationEventKind::ResolvedExternallyReported,
            payload.session_id.as_deref(),
            &audit_payload,
            None,
            None,
            None,
            now,
        ) {
            warn!(
                dispute_id = %dispute_id,
                error = %e,
                "failed to record resolved_externally_reported event"
            );
        }
    }

    debug!(
        dispute_id = %dispute_id,
        "emit_final_report: done"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    async fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state
             ) VALUES ('d-r', 'e1', 'm1', 'buyer',
                       'initiated', 1, 2, 'notified')",
            [],
        )
        .unwrap();
        Arc::new(AsyncMutex::new(conn))
    }

    #[tokio::test]
    async fn no_context_predicate_false_for_untouched_dispute() {
        let conn = fresh_conn().await;
        assert!(!has_any_mediation_context(&conn, "d-r").await.unwrap());
    }

    #[tokio::test]
    async fn session_row_satisfies_predicate() {
        let conn = fresh_conn().await;
        {
            let c = conn.lock().await;
            c.execute(
                "INSERT INTO mediation_sessions (
                    session_id, dispute_id, state, round_count,
                    prompt_bundle_id, policy_hash,
                    started_at, last_transition_at
                 ) VALUES ('s-r', 'd-r', 'awaiting_response', 0,
                           'phase3-default', 'hash', 100, 100)",
                [],
            )
            .unwrap();
        }
        assert!(has_any_mediation_context(&conn, "d-r").await.unwrap());
    }

    #[tokio::test]
    async fn dispute_scoped_event_alone_satisfies_predicate() {
        // FR-122 shape: reasoning ran and recorded a dispute-scoped
        // verdict, but no session was ever opened.
        let conn = fresh_conn().await;
        let now = super::super::current_ts_secs().unwrap();
        {
            let c = conn.lock().await;
            db::mediation_events::record_event(
                &c,
                db::mediation_events::MediationEventKind::ReasoningVerdict,
                None,
                r#"{"dispute_id":"d-r","decision":"escalate","trigger":"fraud_indicator"}"#,
                None,
                Some("phase3-default"),
                Some("hash"),
                now,
            )
            .unwrap();
        }
        assert!(has_any_mediation_context(&conn, "d-r").await.unwrap());
    }

    #[tokio::test]
    async fn payload_picks_dispute_scoped_verdict_when_no_session_event() {
        let conn = fresh_conn().await;
        let now = super::super::current_ts_secs().unwrap();
        {
            let c = conn.lock().await;
            db::mediation_events::record_event(
                &c,
                db::mediation_events::MediationEventKind::ReasoningVerdict,
                None,
                r#"{"dispute_id":"d-r","classification":"suspected_fraud","confidence":0.95,"decision":"escalate","trigger":"fraud_indicator"}"#,
                None,
                Some("phase3-default"),
                Some("hash"),
                now,
            )
            .unwrap();
        }
        let payload = build_payload(&conn, "d-r", "seller-refunded")
            .await
            .unwrap();
        assert_eq!(payload.session_id, None);
        assert_eq!(
            payload.classification,
            Some((ClassificationLabel::SuspectedFraud, 0.95))
        );
        assert_eq!(payload.outbound_party_messages_count, 0);
        assert!(
            payload
                .narrative
                .contains("No mediation session was opened"),
            "narrative should say no session; got: {}",
            payload.narrative
        );
    }

    #[test]
    fn body_contains_versioning_and_key_fields() {
        let payload = FinalReportPayload {
            dispute_id: "d-1".into(),
            session_id: Some("s-1".into()),
            classification: Some((ClassificationLabel::CoordinationFailureResolvable, 0.88)),
            outbound_party_messages_count: 2,
            final_dispute_status: "seller-refunded".into(),
            narrative: "NARRATIVE".into(),
        };
        let body = build_report_body(&payload);
        assert!(body.starts_with("mediation_resolution_report/v1"));
        assert!(body.contains("d-1"));
        assert!(body.contains("s-1"));
        assert!(body.contains("coordination_failure_resolvable"));
        assert!(body.contains("0.88"));
        assert!(body.contains("seller-refunded"));
        assert!(body.contains("NARRATIVE"));
    }
}
