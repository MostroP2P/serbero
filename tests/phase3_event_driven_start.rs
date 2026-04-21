//! Phase 10 / T103 / SC-109 — event-driven mediation start.
//!
//! The handler path `handlers::dispute_detected::handle` must open a
//! mediation session and dispatch the first party-facing clarifying
//! DMs *synchronously* after Phase 1/2 persistence and solver
//! notification, without waiting for the background engine tick.
//!
//! This test spins up the mock relay, a Mostro chat simulator, and a
//! scripted reasoning provider, then calls `dispute_detected::handle`
//! directly — no engine task, no tokio interval. The assertions
//! cover:
//!
//! - `mediation_sessions` has exactly one row for the dispute in
//!   state `awaiting_response`.
//! - `mediation_messages` has exactly two outbound rows (one per party).
//! - `mediation_events` has a `start_attempt_started` row whose
//!   payload carries `trigger = "detected"` (and, by construction
//!   since the engine tick never runs, no `trigger = "tick_retry"`
//!   row exists).
//!
//! If the handler silently defers the session-open to the engine
//! tick, every assertion here fails: no session row is written, no
//! outbound messages land in the DB, and no `start_attempt_started`
//! row exists at all.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::{
    Alphabet, EventBuilder, Keys, Kind, SingleLetterTag, Tag, TagKind, Timestamp,
};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use serbero::db;
use serbero::handlers::dispute_detected::{self, HandlerContext};
use serbero::mediation::auth_retry::AuthRetryHandle;
use serbero::mediation::{Phase3HandlerCtx, SessionKeyCache};
use serbero::models::{SolverConfig, SolverPermission};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

use common::{MockReasoningProvider, MostroChatSim};
use nostr_relay_builder::MockRelay;

const DISPUTE_EVENT_KIND: u16 = 38386;

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
async fn detected_handler_opens_session_without_engine_tick() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    // Serbero identity + buyer/seller trade keys + an unrelated
    // keypair that signs the kind-38386 event. The take-flow target
    // (Phase3HandlerCtx.mostro_pubkey) points at MostroChatSim's
    // identity; the dispute-event signer is recorded in the
    // disputes row but never reused for DMs.
    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();
    let dispute_signer = Keys::generate();

    let mostro_sim = MostroChatSim::start(
        &relay_url,
        buyer_trade.public_key(),
        seller_trade.public_key(),
    )
    .await;

    // Serbero's nostr client: same relay, used to send the take-
    // dispute gift-wrap from inside `open_session`.
    let serbero_client = nostr_sdk::Client::new(serbero_keys.clone());
    serbero_client.add_relay(&relay_url).await.unwrap();
    serbero_client.connect().await;
    serbero_client
        .wait_for_connection(Duration::from_secs(5))
        .await;

    let reasoning: Arc<dyn ReasoningProvider> = Arc::new(MockReasoningProvider {
        clarification: "Please describe the last fiat payment attempt.".into(),
    });
    let bundle = fixture_bundle();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    let conn = Arc::new(AsyncMutex::new(raw));

    let session_key_cache: SessionKeyCache = Arc::new(AsyncMutex::new(HashMap::new()));
    let auth_handle = AuthRetryHandle::new_authorized();

    // Solver pubkey is unused by the event-driven start — it's
    // required only to keep the Phase 1/2 notification path from
    // short-circuiting on an empty solver list (which would return
    // before we reach `try_start_mediation`).
    let solver_keys = Keys::generate();
    let solver_cfg = SolverConfig {
        pubkey: solver_keys.public_key().to_hex(),
        permission: SolverPermission::Write,
    };

    let phase3 = Arc::new(Phase3HandlerCtx {
        serbero_keys: serbero_keys.clone(),
        mostro_pubkey: mostro_sim.pubkey(),
        reasoning: Arc::clone(&reasoning),
        prompt_bundle: Arc::clone(&bundle),
        provider_name: "mock-provider".into(),
        model_name: "mock-model".into(),
        auth_handle,
        session_key_cache,
        solvers: vec![solver_cfg.clone()],
    });

    let ctx = HandlerContext {
        conn: Arc::clone(&conn),
        client: serbero_client.clone(),
        solvers: vec![solver_cfg.clone()],
        phase3: Some(phase3),
    };

    // Kind-38386 dispute event. dispute_id MUST be a valid UUID so
    // the take-flow (which expects `Uuid`) can parse it. We also
    // match the handler's expected tag shape.
    let dispute_id = Uuid::new_v4().to_string();
    let tags = vec![
        Tag::identifier(dispute_id.clone()),
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::S)),
            ["initiated"],
        ),
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
            ["dispute"],
        ),
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Y)),
            [dispute_signer.public_key().to_hex().as_str()],
        ),
        Tag::custom(TagKind::Custom("initiator".into()), ["buyer"]),
    ];
    let event = EventBuilder::new(Kind::Custom(DISPUTE_EVENT_KIND), "")
        .tags(tags)
        .custom_created_at(Timestamp::now())
        .sign_with_keys(&dispute_signer)
        .unwrap();

    // --- Invoke the handler once, synchronously. ----------------
    dispute_detected::handle(&ctx, &event)
        .await
        .expect("handler must succeed");

    // --- Assertions ---------------------------------------------
    // (1) Exactly one mediation_sessions row in awaiting_response.
    let (session_count, state): (i64, String) = {
        let c = conn.lock().await;
        let count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_sessions WHERE dispute_id = ?1",
                rusqlite::params![dispute_id],
                |r| r.get(0),
            )
            .unwrap();
        let state: String = c
            .query_row(
                "SELECT state FROM mediation_sessions WHERE dispute_id = ?1",
                rusqlite::params![dispute_id],
                |r| r.get(0),
            )
            .unwrap();
        (count, state)
    };
    assert_eq!(
        session_count, 1,
        "expected exactly one mediation session row for dispute {dispute_id}"
    );
    assert_eq!(
        state, "awaiting_response",
        "session must be in awaiting_response after the first clarifying round"
    );

    // (2) Exactly two outbound mediation_messages rows.
    let outbound_count: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_messages mm
             JOIN mediation_sessions s ON mm.session_id = s.session_id
             WHERE s.dispute_id = ?1 AND mm.direction = 'outbound'",
            rusqlite::params![dispute_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        outbound_count, 2,
        "expected two outbound party messages (one buyer, one seller)"
    );

    // (3) `start_attempt_started` event exists dispute-scoped with
    //     trigger = "detected", and NO tick_retry variant exists
    //     (the engine was never started in this test).
    let (detected_count, tick_retry_count): (i64, i64) = {
        let c = conn.lock().await;
        let d: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE kind = 'start_attempt_started'
                   AND json_extract(payload_json, '$.dispute_id') = ?1
                   AND json_extract(payload_json, '$.trigger') = 'detected'",
                rusqlite::params![dispute_id],
                |r| r.get(0),
            )
            .unwrap();
        let t: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE kind = 'start_attempt_started'
                   AND json_extract(payload_json, '$.dispute_id') = ?1
                   AND json_extract(payload_json, '$.trigger') = 'tick_retry'",
                rusqlite::params![dispute_id],
                |r| r.get(0),
            )
            .unwrap();
        (d, t)
    };
    assert_eq!(
        detected_count, 1,
        "expected one start_attempt_started row with trigger = detected"
    );
    assert_eq!(
        tick_retry_count, 0,
        "no tick_retry start_attempt_started row should exist — \
         the engine task never ran in this test (SC-109)"
    );
}
