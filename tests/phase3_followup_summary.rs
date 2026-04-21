//! Phase 11 / T123 — mid-session Summarize branch (SC-114).
//!
//! Verifies that when `policy::evaluate` returns `Summarize` on a
//! mid-session call, `advance_session_round`:
//!
//! - pre-transitions the session `awaiting_response → classified`;
//! - invokes `deliver_summary` exactly once, which walks the
//!   session through `classified → summary_pending →
//!   summary_delivered → closed`;
//! - writes one `mediation_summaries` row;
//! - delivers the solver DM (via the existing Phase 1/2 notifier);
//! - advances `round_count_last_evaluated` via the post-commit marker
//!   write (the existing-summary-delivered path is idempotent).
//!
//! Harness mirrors T122's seeding strategy: a pre-seeded session at
//! `round_count = 1` with round-0 outbound rows and party-reply
//! inbound rows, plus a scripted provider whose `classify` returns
//! `SuggestedAction::Summarize` and whose `summarize` returns a
//! valid `SummaryResponse`.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nostr_relay_builder::MockRelay;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;

use serbero::chat::dispute_chat_flow::DisputeChatMaterial;
use serbero::chat::shared_key::derive_shared_keys;
use serbero::db;
use serbero::mediation::follow_up::advance_session_round;
use serbero::mediation::SessionKeyCache;
use serbero::models::mediation::ClassificationLabel;
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, RationaleText, ReasoningError, SuggestedAction,
    SummaryRequest, SummaryResponse,
};
use serbero::models::{SolverConfig, SolverPermission};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

use common::SolverListener;

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

/// A scripted provider whose `classify` returns a cooperative
/// resolution (Summarize branch) and whose `summarize` returns a
/// minimal valid `SummaryResponse`. Used only by this test.
struct SummarizingProvider {
    summary_text: String,
    suggested_next_step: String,
}

#[async_trait]
impl ReasoningProvider for SummarizingProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        Ok(ClassificationResponse {
            classification: ClassificationLabel::CoordinationFailureResolvable,
            confidence: 0.9,
            suggested_action: SuggestedAction::Summarize,
            rationale: RationaleText("parties appear aligned; recommending closure".into()),
            flags: Vec::new(),
        })
    }

    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        Ok(SummaryResponse {
            summary_text: self.summary_text.clone(),
            suggested_next_step: self.suggested_next_step.clone(),
            rationale: RationaleText("buyer paid, seller acknowledged; recommend release".into()),
        })
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Ok(())
    }
}

/// Same seeding strategy as T122 — leave the session at
/// `round_count = 1` with round-0 outbound rows and inbound reply
/// rows. The single intentional difference is that `assigned_solver`
/// is populated so `deliver_summary` picks the targeted-recipient
/// branch; that is the common production case.
#[allow(clippy::too_many_arguments)]
fn seed_session_ready_for_summary(
    conn: &rusqlite::Connection,
    session_id: &str,
    dispute_id: &str,
    bundle: &PromptBundle,
    buyer_shared_pk: &str,
    seller_shared_pk: &str,
    buyer_inbound_content: &str,
    seller_inbound_content: &str,
    assigned_solver_hex: &str,
) {
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state,
            assigned_solver
         ) VALUES (?1, 'evt-t123', 'mostro-t123', 'buyer',
                   'initiated', 0, 0, 'taken', ?2)",
        rusqlite::params![dispute_id, assigned_solver_hex],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO mediation_sessions (
            session_id, dispute_id, state, round_count,
            round_count_last_evaluated, consecutive_eval_failures,
            prompt_bundle_id, policy_hash,
            buyer_shared_pubkey, seller_shared_pubkey,
            started_at, last_transition_at
         ) VALUES (?1, ?2, 'awaiting_response', 1, 0, 0,
                   ?3, ?4, ?5, ?6, 100, 100)",
        rusqlite::params![
            session_id,
            dispute_id,
            bundle.id,
            bundle.policy_hash,
            buyer_shared_pk,
            seller_shared_pk,
        ],
    )
    .unwrap();
    for (party, shared_pk, inner_id, content) in [
        (
            "buyer",
            buyer_shared_pk,
            "inner-r0-buyer",
            "Buyer: please confirm the payment",
        ),
        (
            "seller",
            seller_shared_pk,
            "inner-r0-seller",
            "Seller: please confirm the payment",
        ),
    ] {
        conn.execute(
            "INSERT INTO mediation_messages (
                session_id, direction, party, shared_pubkey,
                inner_event_id, inner_event_created_at, outer_event_id,
                content, prompt_bundle_id, policy_hash,
                persisted_at, stale
             ) VALUES (?1, 'outbound', ?2, ?3, ?4, 200, 'outer-r0',
                       ?5, ?6, ?7, 200, 0)",
            rusqlite::params![
                session_id,
                party,
                shared_pk,
                inner_id,
                content,
                bundle.id,
                bundle.policy_hash
            ],
        )
        .unwrap();
    }
    for (party, shared_pk, inner_id, content, ts) in [
        (
            "buyer",
            buyer_shared_pk,
            "inner-r0-buyer-reply",
            buyer_inbound_content,
            300_i64,
        ),
        (
            "seller",
            seller_shared_pk,
            "inner-r0-seller-reply",
            seller_inbound_content,
            301_i64,
        ),
    ] {
        conn.execute(
            "INSERT INTO mediation_messages (
                session_id, direction, party, shared_pubkey,
                inner_event_id, inner_event_created_at, outer_event_id,
                content, prompt_bundle_id, policy_hash,
                persisted_at, stale
             ) VALUES (?1, 'inbound', ?2, ?3, ?4, ?5, NULL,
                       ?6, NULL, NULL, ?5, 0)",
            rusqlite::params![session_id, party, shared_pk, inner_id, ts, content],
        )
        .unwrap();
    }
}

#[tokio::test]
async fn summarize_branch_delivers_summary_once_and_closes_session() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();

    let bundle = fixture_bundle();

    // Solver listener: receives the summary DM. Production uses a
    // gift-wrap notifier; SolverListener from tests/common is the
    // stand-in that decrypts and records every DM addressed to it.
    let solver = SolverListener::start(&relay_url).await;
    let solver_cfg = SolverConfig {
        pubkey: solver.pubkey_hex(),
        permission: SolverPermission::Write,
    };

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    let session_id = "sess-t123";
    let dispute_id = "dispute-t123";
    seed_session_ready_for_summary(
        &raw,
        session_id,
        dispute_id,
        &bundle,
        &buyer_shared.public_key().to_hex(),
        &seller_shared.public_key().to_hex(),
        "Buyer: I sent the bank transfer at 14:05 and attached the receipt.",
        "Seller: I see the transfer now and will release the sats.",
        &solver.pubkey_hex(),
    );
    let conn = Arc::new(AsyncMutex::new(raw));

    let serbero_client = Client::new(serbero_keys.clone());
    serbero_client.add_relay(&relay_url).await.unwrap();
    serbero_client.connect().await;
    serbero_client
        .wait_for_connection(Duration::from_secs(5))
        .await;

    let session_key_cache: SessionKeyCache = Arc::new(AsyncMutex::new(HashMap::new()));
    {
        let mut cache = session_key_cache.lock().await;
        cache.insert(
            session_id.to_string(),
            DisputeChatMaterial {
                buyer_shared_keys: buyer_shared.clone(),
                seller_shared_keys: seller_shared.clone(),
                buyer_pubkey: buyer_trade.public_key().to_hex(),
                seller_pubkey: seller_trade.public_key().to_hex(),
            },
        );
    }

    let summary_text =
        "Both parties confirmed payment and release; recommend closing via Mostro's settle flow.";
    let reasoning: Arc<dyn ReasoningProvider> = Arc::new(SummarizingProvider {
        summary_text: summary_text.into(),
        suggested_next_step: "Solver should invoke AdminSettleDispute on Mostro.".into(),
    });

    advance_session_round(
        &conn,
        &serbero_client,
        &serbero_keys,
        reasoning.as_ref(),
        &bundle,
        session_id,
        &session_key_cache,
        std::slice::from_ref(&solver_cfg),
        "mock-provider",
        "mock-model",
    )
    .await
    .expect("advance_session_round first call must succeed on Summarize branch");

    // --- Assertions ---------------------------------------------

    // (a) session ends `closed`, marker advanced.
    let (state, round_count, marker): (String, i64, i64) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT state, round_count, round_count_last_evaluated
             FROM mediation_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(
        state, "closed",
        "SC-114: deliver_summary must walk the session all the way to closed"
    );
    assert_eq!(round_count, 1);
    assert_eq!(
        marker, 1,
        "FR-127: marker must advance after the Summarize branch commits"
    );

    // (b) exactly one mediation_summaries row with the returned
    //     summary_text + suggested_next_step pinned to our session.
    let (summary_rows, stored_text, stored_next_step): (i64, String, String) = {
        let c = conn.lock().await;
        let count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_summaries WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let (text, step): (String, String) = c
            .query_row(
                "SELECT summary_text, suggested_next_step
                 FROM mediation_summaries WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        (count, text, step)
    };
    assert_eq!(summary_rows, 1, "SC-114: exactly one summary row");
    assert_eq!(stored_text, summary_text);
    assert!(!stored_next_step.is_empty());

    // (c) classification_produced + summary_generated audit rows.
    //     The key SC-114 assertion is that `deliver_summary` fires
    //     **exactly once**, captured by the single `summary_generated`
    //     row. Note: the `MediationEventKind::StateTransition` variant
    //     is defined but has no writer in `main` today — state
    //     changes are observable via the `mediation_sessions.state`
    //     column and via the domain-specific events (e.g.
    //     `summary_generated`, `session_opened`). We assert on those
    //     rather than on a `state_transition` count.
    let (cp, sg): (i64, i64) = {
        let c = conn.lock().await;
        let cp: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = ?1 AND kind = 'classification_produced'",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let sg: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = ?1 AND kind = 'summary_generated'",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        (cp, sg)
    };
    assert_eq!(cp, 1, "SC-114: exactly one classification_produced");
    assert_eq!(sg, 1, "SC-114: exactly one summary_generated");

    // (d) solver received exactly one DM containing the summary.
    //     The MockRelay + SolverListener polling loop may need a
    //     moment for the notification to propagate; use the
    //     helper's timed wait.
    assert!(
        solver.wait_for(1, 5).await,
        "SC-114: solver must receive the summary DM within 5 s"
    );
    let messages = solver.messages().await;
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains(summary_text)
            || messages[0].contains("summary")
            || messages[0].contains("Solver"),
        "solver DM must reference the summary; got {:?}",
        messages[0]
    );

    // (e) Idempotency for extra safety — calling advance_session_round
    //     again on a closed session must skip (state gate blocks) and
    //     NOT add more rows.
    advance_session_round(
        &conn,
        &serbero_client,
        &serbero_keys,
        reasoning.as_ref(),
        &bundle,
        session_id,
        &session_key_cache,
        std::slice::from_ref(&solver_cfg),
        "mock-provider",
        "mock-model",
    )
    .await
    .expect("second call on a closed session must be a no-op");

    let (summary_rows_after, sg_after): (i64, i64) = {
        let c = conn.lock().await;
        let s: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_summaries WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let g: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = ?1 AND kind = 'summary_generated'",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        (s, g)
    };
    assert_eq!(
        summary_rows_after, 1,
        "no duplicate summary on re-invocation"
    );
    assert_eq!(sg_after, 1);
}
