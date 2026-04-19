//! T056 — US3 routing model.
//!
//! Three sub-tests drive `mediation::deliver_summary` end-to-end
//! against different `assigned_solver` × configured-solvers shapes
//! and assert that `notifications` rows land in the shape the
//! router's rules specify:
//!
//! 1. Targeted: `assigned_solver = "pk-a"` AND `pk-a` is configured
//!    → exactly one row for `pk-a`, zero rows for `pk-b`.
//! 2. Broadcast: `assigned_solver = NULL` → two rows, one per
//!    configured solver.
//! 3. Fallback: `assigned_solver = "pk-unknown"` not in configured
//!    list → two rows, one per configured solver.
//!
//! No relay is strictly needed for routing, but
//! `send_gift_wrap_notification` will return `Err` without a
//! connected client. We wire a MockRelay so the send succeeds and
//! `notifications` rows carry `status = 'sent'`; the assertions
//! count rows by `dispute_id + solver_pubkey` and do not depend on
//! the send's exact status.

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
use serbero::models::{SolverConfig, SolverPermission};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

struct SummaryOnlyProvider;

#[async_trait]
impl ReasoningProvider for SummaryOnlyProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        panic!("classify not expected in the routing-model test");
    }
    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        Ok(SummaryResponse {
            summary_text: "Cooperative summary for routing test.".into(),
            suggested_next_step: "Follow normal post-mediation process.".into(),
            rationale: RationaleText("both parties agree".into()),
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
    assigned_solver: Option<&str>,
    bundle: &PromptBundle,
) {
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state,
            assigned_solver
         ) VALUES (?1, 'evt-us3-route', 'mostro-us3', 'buyer',
                   'initiated', 0, 0, 'notified', ?2)",
        rusqlite::params![dispute_id, assigned_solver],
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

/// Return both the connection and the `NamedTempFile` guard so the
/// caller can hold the guard for the duration of its assertions.
/// Previously this helper used `std::mem::forget(tmp)` to keep the
/// temp file alive past the function boundary — that leaked the
/// temp file on every test run. Returning the guard is both leak-
/// free and explicit about the lifetime relationship between the
/// SQLite connection and the backing file.
async fn run_scenario(
    dispute_id: &str,
    session_id: &str,
    assigned_solver: Option<&str>,
    configured_solvers: Vec<SolverConfig>,
) -> (
    Arc<AsyncMutex<rusqlite::Connection>>,
    tempfile::NamedTempFile,
) {
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
    seed_session(&raw, dispute_id, session_id, assigned_solver, &bundle);
    let conn = Arc::new(AsyncMutex::new(raw));

    let reasoning: Arc<dyn ReasoningProvider> = Arc::new(SummaryOnlyProvider);

    deliver_summary(
        &conn,
        &client,
        &serbero_keys,
        session_id,
        dispute_id,
        ClassificationLabel::CoordinationFailureResolvable,
        0.91,
        Vec::new(),
        &bundle,
        reasoning.as_ref(),
        &configured_solvers,
        "mock-provider",
        "mock-model",
    )
    .await
    .expect("deliver_summary must succeed on the routing happy paths");

    (conn, tmp)
}

async fn count_notifications_for(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    solver_pubkey: &str,
) -> i64 {
    let c = conn.lock().await;
    c.query_row(
        "SELECT COUNT(*) FROM notifications
         WHERE dispute_id = ?1 AND solver_pubkey = ?2 AND notif_type = 'mediation_summary'",
        rusqlite::params![dispute_id, solver_pubkey],
        |r| r.get(0),
    )
    .unwrap()
}

async fn total_mediation_summary_rows(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
) -> i64 {
    let c = conn.lock().await;
    c.query_row(
        "SELECT COUNT(*) FROM notifications
         WHERE dispute_id = ?1 AND notif_type = 'mediation_summary'",
        rusqlite::params![dispute_id],
        |r| r.get(0),
    )
    .unwrap()
}

fn solver(pk: &str) -> SolverConfig {
    SolverConfig {
        pubkey: pk.into(),
        permission: SolverPermission::Read,
    }
}

#[tokio::test]
async fn targeted_routing_delivers_only_to_assigned_solver() {
    let dispute_id = "dispute-route-targeted";
    let session_id = "sess-route-targeted";
    let pk_a = Keys::generate().public_key().to_hex();
    let pk_b = Keys::generate().public_key().to_hex();

    let (conn, _tmp) = run_scenario(
        dispute_id,
        session_id,
        Some(&pk_a),
        vec![solver(&pk_a), solver(&pk_b)],
    )
    .await;

    assert_eq!(
        count_notifications_for(&conn, dispute_id, &pk_a).await,
        1,
        "targeted routing must deliver exactly one row to pk-a"
    );
    assert_eq!(
        count_notifications_for(&conn, dispute_id, &pk_b).await,
        0,
        "targeted routing must NOT deliver to pk-b"
    );
    assert_eq!(total_mediation_summary_rows(&conn, dispute_id).await, 1);
}

#[tokio::test]
async fn broadcast_routing_when_assigned_solver_is_null() {
    let dispute_id = "dispute-route-broadcast";
    let session_id = "sess-route-broadcast";
    let pk_a = Keys::generate().public_key().to_hex();
    let pk_b = Keys::generate().public_key().to_hex();

    let (conn, _tmp) = run_scenario(
        dispute_id,
        session_id,
        None,
        vec![solver(&pk_a), solver(&pk_b)],
    )
    .await;

    assert_eq!(count_notifications_for(&conn, dispute_id, &pk_a).await, 1);
    assert_eq!(count_notifications_for(&conn, dispute_id, &pk_b).await, 1);
    assert_eq!(total_mediation_summary_rows(&conn, dispute_id).await, 2);
}

#[tokio::test]
async fn fallback_to_broadcast_when_assigned_solver_unknown() {
    let dispute_id = "dispute-route-fallback";
    let session_id = "sess-route-fallback";
    let pk_a = Keys::generate().public_key().to_hex();
    let pk_b = Keys::generate().public_key().to_hex();
    let unknown_pk = Keys::generate().public_key().to_hex();

    // `assigned_solver` references an unknown pk — router falls
    // back to broadcast with a single `warn!`. We still expect
    // two rows, one per configured solver.
    let (conn, _tmp) = run_scenario(
        dispute_id,
        session_id,
        Some(&unknown_pk),
        vec![solver(&pk_a), solver(&pk_b)],
    )
    .await;

    assert_eq!(count_notifications_for(&conn, dispute_id, &pk_a).await, 1);
    assert_eq!(count_notifications_for(&conn, dispute_id, &pk_b).await, 1);
    // The unknown pk is NEVER a direct recipient — fallback
    // replaces it with the configured list.
    assert_eq!(
        count_notifications_for(&conn, dispute_id, &unknown_pk).await,
        0
    );
    assert_eq!(total_mediation_summary_rows(&conn, dispute_id).await, 2);
}
