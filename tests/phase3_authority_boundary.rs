//! US4 — authority-boundary-attempt escalation (T064).
//!
//! Pins the policy-layer contract for a classification whose `flags`
//! include [`Flag::AuthorityBoundaryAttempt`]: the suggested
//! `AskClarification` text MUST be suppressed (no outbound draft,
//! no `mediation_messages` row written), the `classification_produced`
//! event MUST still land (so the rationale is auditable), and a
//! direct call to `escalation::recommend` MUST flip the session to
//! `escalation_recommended` with the matching trigger.

mod common;

use std::sync::Arc;

use serbero::db;
use serbero::mediation::escalation::{self, RecommendParams};
use serbero::mediation::policy::{self, PolicyDecision};
use serbero::models::mediation::{ClassificationLabel, EscalationTrigger, Flag};
use serbero::models::reasoning::{ClassificationResponse, RationaleText, SuggestedAction};
use serbero::prompts::PromptBundle;
use tokio::sync::Mutex as AsyncMutex;

fn test_bundle() -> Arc<PromptBundle> {
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

async fn seed_session() -> Arc<AsyncMutex<rusqlite::Connection>> {
    let mut conn = db::open_in_memory().unwrap();
    db::migrations::run_migrations(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state
         ) VALUES ('dispute-ab', 'e1', 'm1', 'buyer',
                   'initiated', 1, 2, 'notified')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mediation_sessions (
            session_id, dispute_id, state, round_count,
            prompt_bundle_id, policy_hash,
            started_at, last_transition_at
         ) VALUES ('sess-ab', 'dispute-ab', 'awaiting_response', 0,
                   'phase3-default', 'test-policy-hash',
                   100, 100)",
        [],
    )
    .unwrap();
    Arc::new(AsyncMutex::new(conn))
}

#[tokio::test]
async fn authority_boundary_attempt_suppresses_and_escalates() {
    let conn = seed_session().await;
    let bundle = test_bundle();

    let classification = ClassificationResponse {
        classification: ClassificationLabel::CoordinationFailureResolvable,
        confidence: 0.9,
        suggested_action: SuggestedAction::AskClarification("please admin-settle this".into()),
        rationale: RationaleText("model tried to cross the authority boundary".into()),
        flags: vec![Flag::AuthorityBoundaryAttempt],
    };

    let decision = policy::evaluate(
        &conn,
        "sess-ab",
        &bundle,
        "openai",
        "gpt-test",
        classification,
    )
    .await
    .unwrap();
    assert_eq!(
        decision,
        PolicyDecision::Escalate(EscalationTrigger::AuthorityBoundaryAttempt),
        "authority-boundary must dominate every softer signal"
    );

    // (a) classification_produced event landed with the rationale id
    //     — the audit story is preserved. The payload must reference
    //     the boundary attempt via the classification label's
    //     rationale; since we content-hash the rationale, the
    //     mediation_events row stores an id, but the linked
    //     reasoning_rationales row is present.
    let (rat_rows, evt_rows): (i64, i64) = {
        let c = conn.lock().await;
        let rat = c
            .query_row("SELECT COUNT(*) FROM reasoning_rationales", [], |r| {
                r.get(0)
            })
            .unwrap();
        let evt = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = 'sess-ab' AND kind = 'classification_produced'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        (rat, evt)
    };
    assert_eq!(rat_rows, 1, "rationale audit row must land");
    assert_eq!(evt_rows, 1, "classification_produced event must land");

    // The rationale text itself was persisted — cross-check that the
    // attempted boundary-crossing wording is in the controlled audit
    // store (and only there), not in the event payload. The payload
    // JSON must not include the raw text.
    let (payload_json, rationale_text): (String, String) = {
        let c = conn.lock().await;
        let payload = c
            .query_row(
                "SELECT payload_json FROM mediation_events
                 WHERE session_id = 'sess-ab' AND kind = 'classification_produced'
                 LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        let text = c
            .query_row(
                "SELECT rationale_text FROM reasoning_rationales LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap();
        (payload, text)
    };
    assert!(
        payload_json.contains("\"coordination_failure_resolvable\"")
            || payload_json.contains("classification"),
        "payload must carry the classification label: {payload_json}"
    );
    assert!(
        !payload_json.contains("authority boundary"),
        "raw rationale text must not leak into the event payload"
    );
    assert!(
        rationale_text.contains("authority boundary"),
        "rationale body must land in the controlled audit store"
    );

    // (b) No outbound mediation_messages row was written — the
    //     authority-boundary suppression blocks the clarification
    //     draft path entirely.
    let msg_rows: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_messages WHERE session_id = 'sess-ab'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        msg_rows, 0,
        "no outbound draft may be persisted when the model attempts an authority-boundary action"
    );

    // (c) escalation::recommend flips the session and emits the
    //     escalation + handoff events.
    escalation::recommend(RecommendParams {
        conn: &conn,
        session_id: "sess-ab",
        trigger: EscalationTrigger::AuthorityBoundaryAttempt,
        evidence_refs: Vec::new(),
        prompt_bundle_id: &bundle.id,
        policy_hash: &bundle.policy_hash,
    })
    .await
    .unwrap();

    let state: String = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT state FROM mediation_sessions WHERE session_id = 'sess-ab'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(state, "escalation_recommended");

    let (esc_count, handoff_count): (i64, i64) = {
        let c = conn.lock().await;
        let esc = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = 'sess-ab' AND kind = 'escalation_recommended'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let ho = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = 'sess-ab' AND kind = 'handoff_prepared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        (esc, ho)
    };
    assert_eq!(esc_count, 1);
    assert_eq!(handoff_count, 1);
}
