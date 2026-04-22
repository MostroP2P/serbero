//! Phase 11 / T124 — mid-session reasoning-failure escalation (SC-115).
//!
//! Verifies FR-130's bounded-failure path: three consecutive
//! `reasoning.classify` failures on the mid-session loop escalate
//! the session with `EscalationTrigger::ReasoningUnavailable`.
//!
//! Harness:
//! - Same seeding strategy as T122 (session at `round_count = 1`,
//!   round-0 outbound rows, party-reply inbound rows).
//! - A `FailingProvider` that returns `ReasoningError::Unreachable`
//!   from every `classify` call, tracking how many times it was
//!   invoked.
//! - Solver + `SolverListener` as the escalation-notification
//!   recipient (the `ReasoningUnavailable` escalation fans out via
//!   `notify_solvers_escalation`).
//!
//! Flow: call `advance_session_round` three times back-to-back.
//! Because the marker is never advanced on the failure path, the
//! FR-127 gate (`round_count > round_count_last_evaluated`) keeps
//! letting each subsequent call through — which is exactly the
//! production case where three consecutive ticks each see a fresh
//! inbound and each fail to classify. After the third call the
//! session MUST be in `escalation_recommended` with trigger
//! `ReasoningUnavailable`.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
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
use serbero::models::reasoning::{
    ClassificationRequest, ClassificationResponse, ReasoningError, SummaryRequest, SummaryResponse,
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

/// Reasoning provider that fails every `classify` call and counts
/// how many times it was invoked. `summarize` panics — the
/// failure-escalation path must never reach it.
struct FailingProvider {
    classify_calls: AtomicUsize,
}

#[async_trait]
impl ReasoningProvider for FailingProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        self.classify_calls.fetch_add(1, Ordering::SeqCst);
        Err(ReasoningError::Unreachable(
            "scripted failure for SC-115".into(),
        ))
    }

    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        panic!("summarize must not be reached on the reasoning-failure path")
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Ok(())
    }
}

/// Seed helper — identical to T122's, duplicated here to keep each
/// integration file self-contained (cheaper than a shared-harness
/// module for two callers).
#[allow(clippy::too_many_arguments)]
fn seed_session_ready_for_failure(
    conn: &rusqlite::Connection,
    session_id: &str,
    dispute_id: &str,
    bundle: &PromptBundle,
    buyer_shared_pk: &str,
    seller_shared_pk: &str,
    assigned_solver_hex: &str,
) {
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state,
            assigned_solver
         ) VALUES (?1, 'evt-t124', 'mostro-t124', 'buyer',
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
    // One outbound per party (round 0 opening) + one inbound per
    // party (the replies that would have triggered the first
    // mid-session evaluation). Keeps the transcript loader happy
    // and the round_count consistent at 1.
    for (party, shared_pk, inner_id, content) in [
        (
            "buyer",
            buyer_shared_pk,
            "inner-r0-buyer",
            "Buyer: opening clarification",
        ),
        (
            "seller",
            seller_shared_pk,
            "inner-r0-seller",
            "Seller: opening clarification",
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
            "Buyer: here is my side",
            300_i64,
        ),
        (
            "seller",
            seller_shared_pk,
            "inner-r0-seller-reply",
            "Seller: here is my side",
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
async fn three_consecutive_classify_failures_escalate_reasoning_unavailable() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();

    let bundle = fixture_bundle();

    let solver = SolverListener::start(&relay_url).await;
    let solver_cfg = SolverConfig {
        pubkey: solver.pubkey_hex(),
        permission: SolverPermission::Write,
    };

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    let session_id = "sess-t124";
    let dispute_id = "dispute-t124";
    seed_session_ready_for_failure(
        &raw,
        session_id,
        dispute_id,
        &bundle,
        &buyer_shared.public_key().to_hex(),
        &seller_shared.public_key().to_hex(),
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

    let reasoning = Arc::new(FailingProvider {
        classify_calls: AtomicUsize::new(0),
    });
    let reasoning_dyn: Arc<dyn ReasoningProvider> = reasoning.clone();

    // Three back-to-back invocations. The first two MUST bump the
    // failure counter without escalating; the third MUST escalate.
    // None of them should advance `round_count_last_evaluated`
    // because every attempt failed before dispatch.
    for attempt in 1..=3 {
        advance_session_round(
            &conn,
            &serbero_client,
            &serbero_keys,
            reasoning_dyn.as_ref(),
            &bundle,
            session_id,
            &session_key_cache,
            std::slice::from_ref(&solver_cfg),
            "mock-provider",
            "mock-model",
        )
        .await
        .unwrap_or_else(|e| {
            panic!("attempt #{attempt}: advance_session_round should absorb the failure, got {e}")
        });

        let (failures, state, marker): (i64, String, i64) = {
            let c = conn.lock().await;
            c.query_row(
                "SELECT consecutive_eval_failures, state, round_count_last_evaluated
                 FROM mediation_sessions WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        };

        if attempt < 3 {
            assert_eq!(
                failures, attempt as i64,
                "attempt #{attempt}: consecutive_eval_failures must bump to {attempt}"
            );
            assert_eq!(
                state, "awaiting_response",
                "attempt #{attempt}: session stays in awaiting_response before threshold"
            );
            assert_eq!(
                marker, 0,
                "attempt #{attempt}: marker never advances on the failure path"
            );
        } else {
            // After the third failure: escalation path kicks in.
            // `escalation::recommend` flips the session to
            // `escalation_recommended` and writes the handoff
            // audit rows. The `consecutive_eval_failures` counter
            // is NOT reset by the escalation path itself (nothing
            // writes it back to 0 there); it simply stops
            // mattering because future ticks see the session
            // state != `awaiting_response` and short-circuit.
            assert_eq!(
                state, "escalation_recommended",
                "attempt #3: FR-130 threshold reached → escalate with ReasoningUnavailable"
            );
        }
    }

    // Provider was consulted exactly 3 times — no retry inside
    // advance_session_round, no duplicate calls.
    assert_eq!(
        reasoning.classify_calls.load(Ordering::SeqCst),
        3,
        "provider MUST have been consulted exactly three times"
    );

    // Escalation audit row carries the ReasoningUnavailable trigger.
    // The mediation_events row's payload_json embeds the trigger
    // string.
    let (escalation_kind_count, handoff_kind_count): (i64, i64) = {
        let c = conn.lock().await;
        let e: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = ?1 AND kind = 'escalation_recommended'",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        let h: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = ?1 AND kind = 'handoff_prepared'",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap();
        (e, h)
    };
    assert_eq!(
        escalation_kind_count, 1,
        "exactly one escalation_recommended audit row"
    );
    assert_eq!(
        handoff_kind_count, 1,
        "exactly one handoff_prepared audit row"
    );

    // Trigger string. `escalation_recommended` payload carries the
    // canonical snake_case form (`reasoning_unavailable`).
    let payload: String = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT payload_json FROM mediation_events
             WHERE session_id = ?1 AND kind = 'escalation_recommended'",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(
        payload.contains("reasoning_unavailable"),
        "escalation trigger must be reasoning_unavailable; got payload: {payload}"
    );

    // Solver receives the escalation DM. The existing
    // notify_solvers_escalation path fires from advance_session_round
    // after escalation::recommend succeeds; give the relay a
    // moment to deliver.
    assert!(
        solver.wait_for(1, 5).await,
        "solver must receive the escalation DM within 5 s"
    );
}
