//! US1 gating integration test — T027 / T044.
//!
//! Pins two invariants at once:
//!
//! 1. **T044 gate**: when the reasoning provider's `health_check`
//!    fails, `mediation::open_dispute_session` refuses deterministically
//!    without touching the relay or the `mediation_*` tables.
//!
//! 2. **SC-105 invariant**: while mediation is halted, Phase 1/2
//!    detection and solver notification continue to work — the same
//!    dispute event is still routed to the configured solvers exactly
//!    as in `phase1_detection.rs`.
//!
//! The test deliberately does NOT spawn a Phase 3 engine loop —
//! US1 does not ship one (T019 / T040 are deferred). Instead it
//! drives the gate the only way current code exposes it: by calling
//! `open_dispute_session` directly against an unhealthy reasoning
//! provider.

mod common;

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use serbero::db;
use serbero::mediation;
use serbero::models::dispute::InitiatorRole;
use serbero::models::{SolverPermission, TimeoutsConfig};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

use common::{
    publish_dispute, publisher, solver_cfg, spawn_daemon, SolverListener, TestHarness,
    UnhealthyReasoningProvider,
};

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

#[tokio::test]
async fn refuses_session_open_when_reasoning_health_fails_and_phase12_still_notifies() {
    // ---- Phase 1/2 half: daemon + solver listener ----------------------

    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;

    let cfg = harness.config(
        vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        TimeoutsConfig {
            renotification_seconds: 3600,
            renotification_check_interval_seconds: 3600,
        },
    );
    let (shutdown, handle) = spawn_daemon(cfg);

    // Give Serbero a beat to subscribe.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mostro_client = publisher(&harness.relay_url, harness.mostro_keys.clone()).await;
    let dispute_id = "dispute-gating-001";
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        dispute_id,
        "initiated",
        "buyer",
        vec![],
    )
    .await;

    // SC-105: Phase 1/2 solver notification must still fire when
    // Phase 3 is halted. If this fails, the halt leaked.
    assert!(
        solver.wait_for(1, 30).await,
        "Phase 1/2 solver must still receive notification while Phase 3 is halted"
    );

    // ---- Phase 3 half: direct gate exercise ----------------------------
    //
    // The daemon does NOT yet drive session opens (T019 / T040), so
    // we invoke the gate explicitly via `open_dispute_session`. The
    // daemon owns its own `AsyncMutex<Connection>`; we open a second
    // connection on the same `db_path` for the mediation-side
    // assertions — SQLite is content to serve it.

    let serbero_client = Client::new(harness.serbero_keys.clone());
    serbero_client.add_relay(&harness.relay_url).await.unwrap();
    serbero_client.connect().await;
    serbero_client
        .wait_for_connection(Duration::from_secs(5))
        .await;

    let reasoning: Arc<dyn ReasoningProvider> = Arc::new(UnhealthyReasoningProvider);
    let bundle = fixture_bundle();

    // Use the dispute id seeded above. The take-flow will never run
    // — the gate refuses first — so we do not need `dispute_id` to
    // be a real UUID, but the `open_session` params require one.
    let dispute_uuid = Uuid::new_v4();

    let conn_raw = db::open_connection(&harness.db_path).expect("open mediation-side conn");
    let mediation_conn = Arc::new(AsyncMutex::new(conn_raw));

    let outcome = mediation::open_dispute_session(
        &mediation_conn,
        &serbero_client,
        &harness.serbero_keys,
        &harness.mostro_keys.public_key(),
        reasoning.as_ref(),
        &bundle,
        dispute_id,
        InitiatorRole::Buyer,
        dispute_uuid,
    )
    .await
    .expect("open_dispute_session must not return an Err for the health-check gate path");

    match outcome {
        mediation::session::OpenOutcome::RefusedReasoningUnavailable { reason } => {
            assert!(
                reason.contains("unhealthy for the US1 gating test")
                    || reason.contains("Unreachable"),
                "refusal reason should carry the underlying provider error: {reason}"
            );
        }
        other => panic!("expected RefusedReasoningUnavailable, got {other:?}"),
    }

    // ---- Assertions on mediation tables --------------------------------

    let mediation_session_count: i64 = {
        let c = mediation_conn.lock().await;
        c.query_row("SELECT COUNT(*) FROM mediation_sessions", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        mediation_session_count, 0,
        "no mediation_sessions row may be written when the gate refuses"
    );

    let mediation_message_count: i64 = {
        let c = mediation_conn.lock().await;
        c.query_row("SELECT COUNT(*) FROM mediation_messages", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        mediation_message_count, 0,
        "no mediation_messages row may be written when the gate refuses"
    );

    // Directly count every Kind::GiftWrap (1059) the relay has
    // seen during this test. Phase 1/2 solver notification and
    // hypothetical mediation chat events both use Kind 1059, so
    // the total count of gift-wraps on the relay pins the "no
    // mediation chat event was emitted" invariant without relying
    // on the outbox-ordering inference from DB row counts.
    //
    // Expected: exactly 1 gift-wrap — the Phase 1/2 solver
    // notification delivered above. Any additional gift-wrap would
    // be a regression of the T044 gate.
    let observer = Client::new(Keys::generate());
    observer.add_relay(&harness.relay_url).await.unwrap();
    observer.connect().await;
    observer.wait_for_connection(Duration::from_secs(5)).await;
    let wide_since =
        Timestamp::from_secs(Timestamp::now().as_secs().saturating_sub(7 * 24 * 60 * 60));
    let all_gift_wraps = observer
        .fetch_events(
            Filter::new().kind(Kind::GiftWrap).since(wide_since),
            Duration::from_secs(3),
        )
        .await
        .expect("fetch gift-wraps from relay");
    assert_eq!(
        all_gift_wraps.len(),
        1,
        "expected exactly one gift-wrap (the Phase 1/2 solver notification); \
         any extra gift-wrap would mean the T044 gate leaked"
    );

    // ---- Clean shutdown ------------------------------------------------

    shutdown.send(()).expect("shutdown signal should send");
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("daemon should shut down within 2 seconds")
        .expect("daemon handle should complete successfully")
        .expect("daemon should exit cleanly");
}
