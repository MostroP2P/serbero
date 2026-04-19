//! T055 — US3 cooperative summary happy path.
//!
//! Direct-seed pattern (no take-flow, no party replies): seed a
//! dispute row with `assigned_solver` set, seed an open mediation
//! session, and drive `mediation::deliver_summary` with a scripted
//! reasoning provider that returns a cooperative `SummaryResponse`.
//!
//! Pins the observable outcomes listed in the task spec:
//! - Exactly one `mediation_summaries` row for the session.
//! - Its `rationale_id` matches the SHA-256 of the rationale text.
//! - `mediation_sessions.state = 'closed'`.
//! - Exactly one `notifications` row, `notif_type = 'mediation_summary'`,
//!   `solver_pubkey = '<assigned solver>'` (targeted routing because
//!   `assigned_solver` is set).
//! - `mediation_events` carries a `summary_generated` row referencing
//!   the rationale id (FR-120: rationale text NEVER inlined in the
//!   event payload).
//!
//! Uses a real `MockRelay` + a reader `Client` because
//! `send_gift_wrap_notification` drives `client.send_private_msg`,
//! which needs at least one connected relay to accept the publish.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nostr_relay_builder::MockRelay;
use nostr_sdk::prelude::*;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use serbero::db;
use serbero::mediation::deliver_summary;
use serbero::models::mediation::ClassificationLabel;
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, RationaleText, ReasoningError, SummaryRequest,
    SummaryResponse,
};
use serbero::models::{SolverConfig, SolverPermission};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

/// Reasoning provider that returns a scripted `SummaryResponse` on
/// every `summarize` call and panics on `classify` (the US3 direct-
/// seed tests never call classify — the policy decision is already
/// known).
struct SummaryOnlyProvider {
    summary_text: String,
    suggested_next_step: String,
    rationale_text: String,
}

#[async_trait]
impl ReasoningProvider for SummaryOnlyProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        panic!("classify not expected in the cooperative-summary direct-seed test");
    }
    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        Ok(SummaryResponse {
            summary_text: self.summary_text.clone(),
            suggested_next_step: self.suggested_next_step.clone(),
            rationale: RationaleText(self.rationale_text.clone()),
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
    assigned_solver: &str,
    bundle: &PromptBundle,
) {
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state,
            assigned_solver
         ) VALUES (?1, 'evt-us3', 'mostro-us3', 'buyer',
                   'initiated', 0, 0, 'notified', ?2)",
        rusqlite::params![dispute_id, assigned_solver],
    )
    .unwrap();
    // The cooperative-summary session is at `classified` when
    // deliver_summary is entered (that is what `open_session` leaves
    // it at on the `ReadyForSummary` path).
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

fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    format!("{:x}", h.finalize())
}

#[tokio::test]
async fn cooperative_summary_closes_session_and_notifies_assigned_solver() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    let serbero_keys = Keys::generate();
    let solver_keys = Keys::generate();
    let solver_pk_hex = solver_keys.public_key().to_hex();

    // Serbero client that will publish the gift-wrap summary.
    let client = Client::new(serbero_keys.clone());
    client.add_relay(&relay_url).await.unwrap();
    client.connect().await;
    client.wait_for_connection(Duration::from_secs(5)).await;

    // DB: temp-file so the test is isolated.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    let bundle = fixture_bundle();
    let session_id = "sess-us3-coop";
    let dispute_id = "dispute-us3-coop";
    seed_session(&raw, dispute_id, session_id, &solver_pk_hex, &bundle);
    let conn = Arc::new(AsyncMutex::new(raw));

    // Scripted reasoning provider.
    let rationale = "Both parties confirm the transfer landed at 14:05 UTC; cooperative path.";
    let summary_text =
        "Buyer sent fiat 14:05; seller confirms receipt. Recommend marking the dispute resolved.";
    // Phrase "close the dispute" is an authority-boundary trigger, so
    // the next-step text stays generic.
    let suggested_next_step = "Mark the trade complete and follow the normal timeout procedure.";
    let reasoning: Arc<dyn ReasoningProvider> = Arc::new(SummaryOnlyProvider {
        summary_text: summary_text.into(),
        suggested_next_step: suggested_next_step.into(),
        rationale_text: rationale.into(),
    });

    // One configured solver — the targeted-routing path requires
    // the assigned_solver to also appear in the configured list.
    let solvers = vec![SolverConfig {
        pubkey: solver_pk_hex.clone(),
        permission: SolverPermission::Read,
    }];

    deliver_summary(
        &conn,
        &client,
        &serbero_keys,
        session_id,
        dispute_id,
        ClassificationLabel::CoordinationFailureResolvable,
        0.92,
        Vec::new(), // empty transcript — cooperative summary on first call
        &bundle,
        reasoning.as_ref(),
        &solvers,
        "mock-provider",
        "mock-model",
    )
    .await
    .expect("deliver_summary must succeed on the happy path");

    // ---- Assertions ---------------------------------------------------

    // (a) Exactly one `mediation_summaries` row; rationale_id matches
    //     the SHA-256 of the rationale text (content-addressed id
    //     invariant from src/db/rationales.rs).
    let (summary_count, rationale_id, summary_text_db): (i64, String, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*), MIN(rationale_id), MIN(summary_text)
             FROM mediation_summaries WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(summary_count, 1);
    assert_eq!(rationale_id, sha256_hex(rationale));
    assert_eq!(summary_text_db, summary_text);

    // (b) mediation_sessions.state = 'closed'
    let state: String = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT state FROM mediation_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(state, "closed");

    // (c) Exactly one `notifications` row, targeted to the assigned
    //     solver, with notif_type = mediation_summary AND
    //     status = 'sent'. Pinning the status here keeps the test
    //     from passing if the relay publish silently fails and a
    //     `Failed` row lands instead — we want to verify the
    //     successful-delivery path specifically.
    let (notif_count, notif_solver, notif_type, notif_status): (i64, String, String, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*), MIN(solver_pubkey), MIN(notif_type), MIN(status)
             FROM notifications
             WHERE dispute_id = ?1
               AND notif_type = 'mediation_summary'
               AND status = 'sent'",
            rusqlite::params![dispute_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    assert_eq!(notif_count, 1, "targeted routing → one notification row");
    assert_eq!(notif_solver, solver_pk_hex);
    assert_eq!(notif_type, "mediation_summary");
    assert_eq!(
        notif_status, "sent",
        "targeted delivery must record status = sent"
    );

    // (d) A `summary_generated` mediation_events row referencing the
    //     rationale id by content-hash. The raw rationale text MUST
    //     NOT appear in the payload (FR-120).
    let (evt_count, evt_rationale, evt_payload): (i64, String, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*), MIN(rationale_id), MIN(payload_json)
             FROM mediation_events
             WHERE session_id = ?1 AND kind = 'summary_generated'",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(evt_count, 1);
    assert_eq!(evt_rationale, rationale_id);
    assert!(
        !evt_payload.contains(rationale),
        "FR-120: rationale text MUST NOT appear in event payload_json: {evt_payload}"
    );
    assert!(
        evt_payload.contains(&rationale_id),
        "event payload must reference the rationale id: {evt_payload}"
    );
}
