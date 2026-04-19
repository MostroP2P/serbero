//! US3 delivery-failure escalation coverage.
//!
//! The happy-path tests (`phase3_cooperative_summary`,
//! `phase3_routing_model`) verify that `deliver_summary` closes the
//! session after a successful DM send. This test pins the other
//! branch: when the summary persists but cannot be delivered, the
//! session MUST NOT stay stranded at `summary_pending` — it escalates
//! with trigger `notification_failed` and lands at
//! `escalation_recommended` with a matching `mediation_events` row.
//!
//! The deterministic way to force an empty recipient list is to pass
//! `solvers = []` with no `assigned_solver`. The router resolves to
//! `Broadcast(vec![])`, which `deliver_summary` treats as a delivery
//! failure and escalates automatically.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nostr_relay_builder::MockRelay;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;

use serbero::db;
use serbero::mediation::deliver_summary;
use serbero::models::mediation::ClassificationLabel;
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, RationaleText, ReasoningError, SummaryRequest,
    SummaryResponse,
};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

struct SummaryOnlyProvider;

#[async_trait]
impl ReasoningProvider for SummaryOnlyProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        panic!("classify not expected in the summary-escalation test");
    }
    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        Ok(SummaryResponse {
            summary_text: "Cooperative summary with no configured recipients.".into(),
            suggested_next_step: "Review and close manually.".into(),
            rationale: RationaleText("both parties agreed on timing".into()),
        })
    }
    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Ok(())
    }
}

fn fixture_bundle() -> Arc<PromptBundle> {
    let cfg = serbero::models::PromptsConfig {
        system_instructions_path: "./tests/fixtures/prompts/phase3-system.md".into(),
        classification_policy_path: "./tests/fixtures/prompts/phase3-classification.md".into(),
        escalation_policy_path: "./tests/fixtures/prompts/phase3-escalation-policy.md".into(),
        mediation_style_path: "./tests/fixtures/prompts/phase3-mediation-style.md".into(),
        message_templates_path: "./tests/fixtures/prompts/phase3-message-templates.md".into(),
    };
    Arc::new(prompts::load_bundle(&cfg).expect("fixture bundle must load"))
}

fn seed_session(
    conn: &rusqlite::Connection,
    dispute_id: &str,
    session_id: &str,
    bundle: &PromptBundle,
) {
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state,
            assigned_solver
         ) VALUES (?1, 'evt-us3-esc', 'mostro-us3', 'buyer',
                   'initiated', 0, 0, 'notified', NULL)",
        rusqlite::params![dispute_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mediation_sessions (
            session_id, dispute_id, state, round_count,
            prompt_bundle_id, policy_hash,
            buyer_shared_pubkey, seller_shared_pubkey,
            started_at, last_transition_at
         ) VALUES (?1, ?2, 'classified', 0,
                   ?3, ?4,
                   'buyer-shared-pk', 'seller-shared-pk',
                   100, 100)",
        rusqlite::params![session_id, dispute_id, bundle.id, bundle.policy_hash],
    )
    .unwrap();
}

#[tokio::test]
async fn empty_recipient_list_escalates_with_notification_failed() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();
    let serbero_keys = Keys::generate();
    let client = Client::new(serbero_keys.clone());
    client.add_relay(&relay_url).await.unwrap();
    client.connect().await;
    client.wait_for_connection(Duration::from_secs(5)).await;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    let bundle = fixture_bundle();
    let dispute_id = "dispute-us3-notif-failed";
    let session_id = "sess-us3-notif-failed";
    seed_session(&raw, dispute_id, session_id, &bundle);
    let conn = Arc::new(AsyncMutex::new(raw));

    let reasoning: Arc<dyn ReasoningProvider> = Arc::new(SummaryOnlyProvider);

    deliver_summary(
        &conn,
        &client,
        &serbero_keys,
        session_id,
        dispute_id,
        ClassificationLabel::CoordinationFailureResolvable,
        0.93,
        Vec::new(),
        &bundle,
        reasoning.as_ref(),
        &[], // no configured solvers → recipient_list is empty
        "mock-provider",
        "mock-model",
    )
    .await
    .expect("deliver_summary must return Ok on the auto-escalation path");

    // (a) Session state flipped all the way to `escalation_recommended` —
    //     NOT stranded at `summary_pending`.
    let state: String = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT state FROM mediation_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        state, "escalation_recommended",
        "empty-recipients path must escalate the session, not leave it at summary_pending"
    );

    // (b) Summary row DID land — the rationale audit is preserved
    //     even though delivery failed (persisted-but-undelivered
    //     semantics; the rationale stays recoverable from the
    //     audit store).
    let summary_count: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_summaries WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(summary_count, 1);

    // (c) No notification rows: there was no one to notify, and we
    //     must not fabricate a `Failed` row against a phantom solver
    //     pubkey.
    let notif_count: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM notifications WHERE dispute_id = ?1",
            rusqlite::params![dispute_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        notif_count, 0,
        "no recipients → no notifications table rows"
    );

    // (d) `escalation_recommended` audit event carries the
    //     `notification_failed` trigger in its payload.
    let (evt_count, evt_payload): (i64, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*), MIN(payload_json)
             FROM mediation_events
             WHERE session_id = ?1 AND kind = 'escalation_recommended'",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(evt_count, 1);
    assert!(
        evt_payload.contains("notification_failed"),
        "escalation event payload must tag trigger = notification_failed: {evt_payload}"
    );
    assert!(
        evt_payload.contains("no solver recipients configured"),
        "escalation event payload must carry the sub-case reason: {evt_payload}"
    );
}
