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
//! FR-120 discipline: `evidence_refs` carries rationale ids only
//! (content-hashes), never rationale text. The heuristic below
//! splits the caller-supplied list into `rationale_refs` (64-char
//! lowercase hex) and generic `evidence_refs` so Phase 4 can fan
//! the two kinds out to its own downstream audit.

use std::sync::Arc;

use rusqlite::params;
use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, instrument, warn};

use crate::db;
use crate::db::mediation_events::MediationEventKind;
use crate::error::{Error, Result};
use crate::models::mediation::{EscalationTrigger, MediationSessionState};

/// Phase 4 handoff package. Persisted as the `handoff_prepared`
/// mediation event's payload so Phase 4 can consume it later by
/// reading the audit log — no additional table needed.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffPackage {
    pub dispute_id: String,
    pub session_id: String,
    /// `EscalationTrigger::to_string()` — the snake-case form so
    /// the payload is operator-readable and grep-friendly.
    pub trigger: String,
    /// Caller-supplied non-rationale evidence refs (inner_event_ids,
    /// outbound event ids, free-form notes). Filtered out of the
    /// 64-hex-looking items in the input list.
    pub evidence_refs: Vec<String>,
    pub prompt_bundle_id: String,
    pub policy_hash: String,
    /// 64-char lowercase hex entries from the input list —
    /// heuristically matched to SHA-256 rationale ids in
    /// `reasoning_rationales`.
    pub rationale_refs: Vec<String>,
    pub assembled_at: i64,
}

/// Parameters for [`recommend`]. Grouped so the call site stays
/// compact and clippy does not flag too_many_arguments.
pub struct RecommendParams<'a> {
    pub conn: &'a Arc<AsyncMutex<rusqlite::Connection>>,
    pub session_id: &'a str,
    pub trigger: EscalationTrigger,
    /// Caller-supplied list mixing inner_event_ids, rationale_ids,
    /// and other forensic refs. Split by the heuristic below.
    pub evidence_refs: Vec<String>,
    pub prompt_bundle_id: &'a str,
    pub policy_hash: &'a str,
}

/// Mark a session `escalation_recommended`, record the trigger and
/// assemble the Phase 4 handoff package — all in one transaction.
///
/// Does NOT send any outbound chat message; does NOT notify solvers
/// (the engine owns that); does NOT retry on DB error (the single
/// transaction either lands or rolls back).
#[instrument(skip_all, fields(session_id = %params.session_id, trigger = %params.trigger))]
pub async fn recommend(params: RecommendParams<'_>) -> Result<()> {
    let RecommendParams {
        conn,
        session_id,
        trigger,
        evidence_refs,
        prompt_bundle_id,
        policy_hash,
    } = params;

    let now = super::current_ts_secs()?;

    let (evidence_refs, rationale_refs) = split_rationale_refs(evidence_refs);

    let mut guard = conn.lock().await;

    // (1) dispute_id lookup — load-bearing for the handoff package.
    //     Missing row is a real bug; surface InvalidEvent rather
    //     than fabricating an empty string into the handoff.
    let dispute_id: String = match guard.query_row(
        "SELECT dispute_id FROM mediation_sessions WHERE session_id = ?1",
        params![session_id],
        |r| r.get::<_, String>(0),
    ) {
        Ok(s) => s,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(Error::InvalidEvent(format!(
                "escalation::recommend: no mediation_sessions row for session_id={session_id}"
            )));
        }
        Err(e) => return Err(Error::Db(e)),
    };

    // (2)–(4) tx: state flip + 2 events in one atomic block.
    let tx = guard.transaction()?;

    db::mediation::set_session_state(
        &tx,
        session_id,
        MediationSessionState::EscalationRecommended,
        now,
    )?;

    let escalation_payload = serde_json::json!({
        "trigger": trigger.to_string(),
        "evidence_refs": evidence_refs,
        "rationale_refs": rationale_refs,
    })
    .to_string();
    db::mediation_events::record_event(
        &tx,
        MediationEventKind::EscalationRecommended,
        Some(session_id),
        &escalation_payload,
        None,
        Some(prompt_bundle_id),
        Some(policy_hash),
        now,
    )?;

    let package = HandoffPackage {
        dispute_id: dispute_id.clone(),
        session_id: session_id.to_string(),
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
        Some(session_id),
        &handoff_payload,
        None,
        Some(prompt_bundle_id),
        Some(policy_hash),
        now,
    )?;

    tx.commit()?;
    drop(guard);

    warn!(
        session_id = %session_id,
        trigger = %trigger,
        dispute_id = %dispute_id,
        "escalation_recommended"
    );
    info!(session_id = %session_id, "handoff_prepared");

    Ok(())
}

/// Heuristic: a rationale id is the lowercase hex form of a SHA-256
/// digest — exactly 64 chars, `[0-9a-f]`. Everything else (nostr
/// event ids, inbound/outbound ids, free-form notes) stays in
/// `evidence_refs`.
fn split_rationale_refs(input: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut evidence = Vec::new();
    let mut rationales = Vec::new();
    for r in input {
        if looks_like_rationale_id(&r) {
            rationales.push(r);
        } else {
            evidence.push(r);
        }
    }
    (evidence, rationales)
}

fn looks_like_rationale_id(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_rationale_id_accepts_lowercase_64_hex() {
        // SHA-256("abc") — deterministic known-good vector.
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha.len(), 64);
        assert!(looks_like_rationale_id(sha));
    }

    #[test]
    fn looks_like_rationale_id_rejects_uppercase_and_wrong_length() {
        assert!(!looks_like_rationale_id(
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        ));
        assert!(!looks_like_rationale_id("too-short"));
        assert!(!looks_like_rationale_id(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015adXX"
        ));
        assert!(!looks_like_rationale_id(""));
    }

    #[test]
    fn split_rationale_refs_partitions_correctly() {
        let input = vec![
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            "inner-event-123".into(),
            "some-note".into(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        ];
        let (evidence, rationales) = split_rationale_refs(input);
        assert_eq!(evidence, vec!["inner-event-123", "some-note"]);
        assert_eq!(rationales.len(), 2);
    }
}
