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

use rusqlite::{params, Connection};
use serde_json::json;

use crate::error::Result;

/// Enumerated event kinds. Text form is the snake_case spelling
/// stored in `mediation_events.kind` (see `data-model.md` table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediationEventKind {
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
    SessionClosed,
}

impl MediationEventKind {
    pub fn as_str(&self) -> &'static str {
        use MediationEventKind::*;
        match self {
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
            SessionClosed => "session_closed",
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
pub fn record_outbound_sent(
    conn: &Connection,
    session_id: &str,
    shared_pubkey: &str,
    inner_event_id: &str,
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
        None,
        None,
        occurred_at,
    )
}

/// Record a `classification_produced` event. `rationale_id`
/// references [`crate::db::rationales`]; the raw rationale text is
/// NEVER inlined into the payload, per FR-120.
pub fn record_classification_produced(
    conn: &Connection,
    session_id: &str,
    rationale_id: &str,
    classification: &str,
    confidence: f64,
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
        None,
        None,
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
            (MediationEventKind::SessionClosed, "session_closed"),
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
    fn outbound_sent_constructor_encodes_payload() {
        let conn = fresh_with_session();
        let id =
            record_outbound_sent(&conn, "sess-1", "shared-pk-hex", "inner-event-id", 600).unwrap();
        let (kind, payload): (String, String) = conn
            .query_row(
                "SELECT kind, payload_json FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "outbound_sent");
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["shared_pubkey"], "shared-pk-hex");
        assert_eq!(parsed["inner_event_id"], "inner-event-id");
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
            700,
        )
        .unwrap();
        let (kind, payload, rationale_id): (String, String, Option<String>) = conn
            .query_row(
                "SELECT kind, payload_json, rationale_id
                 FROM mediation_events WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "classification_produced");
        assert_eq!(rationale_id.as_deref(), Some(rationale_id_var.as_str()));
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
}
