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
use serbero::models::{MediationConfig, SolverConfig, SolverPermission};
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

use common::SolverListener;

fn prompt_bundle() -> Arc<PromptBundle> {
    let cfg = serbero::models::PromptsConfig {
        system_instructions_path: "./prompts/phase3-system.md".into(),
        classification_policy_path: "./prompts/phase3-classification.md".into(),
        escalation_policy_path: "./prompts/phase3-escalation-policy.md".into(),
        mediation_style_path: "./prompts/phase3-mediation-style.md".into(),
        message_templates_path: "./prompts/phase3-message-templates.md".into(),
    };
    Arc::new(prompts::load_bundle(&cfg).expect("prompt bundle must load"))
}

struct CooperativeProvider {
    seller_confirmed_fiat_receipt: Option<bool>,
}

#[async_trait]
impl ReasoningProvider for CooperativeProvider {
    async fn classify(
        &self,
        _request: ClassificationRequest,
    ) -> std::result::Result<ClassificationResponse, ReasoningError> {
        Ok(ClassificationResponse {
            classification: ClassificationLabel::CoordinationFailureResolvable,
            confidence: 0.91,
            suggested_action: SuggestedAction::Summarize,
            rationale: RationaleText("cooperative case".into()),
            flags: Vec::new(),
            human_requested: false,
            buyer_language: Some("es".into()),
            seller_language: Some("es".into()),
            seller_confirmed_fiat_receipt: self.seller_confirmed_fiat_receipt,
        })
    }

    async fn summarize(
        &self,
        _request: SummaryRequest,
    ) -> std::result::Result<SummaryResponse, ReasoningError> {
        Ok(SummaryResponse {
            summary_text:
                "Transcript suggests the trade may resolve without adversarial intervention.".into(),
            suggested_next_step: "Continue monitoring for cooperative resolution.".into(),
            rationale: RationaleText("summary rationale".into()),
        })
    }

    async fn health_check(&self) -> std::result::Result<(), ReasoningError> {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn seed_session(
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
         ) VALUES (?1, 'evt-sr-role', 'mostro-sr-role', 'buyer',
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
    conn.execute(
        "INSERT INTO mediation_events (
            session_id, kind, payload_json,
            prompt_bundle_id, policy_hash, occurred_at
         ) VALUES (?1, 'classification_produced', '{}',
                   ?2, ?3, 250)",
        rusqlite::params![session_id, bundle.id, bundle.policy_hash],
    )
    .unwrap();
    for (party, shared_pk, inner_id, content) in [
        (
            "buyer",
            buyer_shared_pk,
            "inner-r0-buyer",
            "Buyer: please explain what happened",
        ),
        (
            "seller",
            seller_shared_pk,
            "inner-r0-seller",
            "Seller: please explain what happened",
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

async fn run_case(
    session_id: &str,
    dispute_id: &str,
    buyer_inbound_content: &str,
    seller_inbound_content: &str,
    seller_confirmed_fiat_receipt: Option<bool>,
) -> Arc<AsyncMutex<rusqlite::Connection>> {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();
    let bundle = prompt_bundle();

    let solver = SolverListener::start(&relay_url).await;
    let solver_cfg = SolverConfig {
        pubkey: solver.pubkey_hex(),
        permission: SolverPermission::Write,
    };

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    seed_session(
        &raw,
        session_id,
        dispute_id,
        &bundle,
        &buyer_shared.public_key().to_hex(),
        &seller_shared.public_key().to_hex(),
        buyer_inbound_content,
        seller_inbound_content,
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

    advance_session_round(
        &conn,
        &serbero_client,
        &serbero_keys,
        &CooperativeProvider {
            seller_confirmed_fiat_receipt,
        },
        &bundle,
        session_id,
        &session_key_cache,
        &[solver_cfg],
        "mock-provider",
        "mock-model",
        &MediationConfig {
            self_resolution_enabled: true,
            self_resolution_threshold: 0.75,
            ..MediationConfig::default()
        },
    )
    .await
    .expect("advance_session_round must succeed");

    assert!(
        solver.wait_for(1, 5).await,
        "solver should still receive the summary notification"
    );

    conn
}

#[tokio::test]
async fn buyer_only_fiat_claim_does_not_send_self_resolution_invitation() {
    let session_id = "sess-sr-guard-no";
    let conn = run_case(
        session_id,
        "dispute-sr-guard-no",
        "Buyer: Ya envie el fiat y tengo el comprobante.",
        "Seller: Todavia no recibi el dinero.",
        Some(false),
    )
    .await;

    let (offered_count, outbound_count, state): (i64, i64, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT
                (SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = ?1 AND kind = 'self_resolution_offered'),
                (SELECT COUNT(*) FROM mediation_messages
                 WHERE session_id = ?1 AND direction = 'outbound'),
                (SELECT state FROM mediation_sessions WHERE session_id = ?1)",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };

    assert_eq!(offered_count, 0);
    assert_eq!(
        outbound_count, 2,
        "no extra party invitation should be sent"
    );
    assert_eq!(state, "summary_delivered");
}

#[tokio::test]
async fn seller_receipt_confirmation_allows_self_resolution_invitation() {
    let session_id = "sess-sr-guard-yes";
    let conn = run_case(
        session_id,
        "dispute-sr-guard-yes",
        "Buyer: Ya envie el fiat.",
        "Seller: Ya me llego el dinero y ya veo la transferencia.",
        Some(true),
    )
    .await;

    let (offered_count, invite_count, state): (i64, i64, String) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT
                (SELECT COUNT(*) FROM mediation_events
                 WHERE session_id = ?1 AND kind = 'self_resolution_offered'),
                (SELECT COUNT(*) FROM mediation_messages
                 WHERE session_id = ?1
                   AND direction = 'outbound'
                   AND content LIKE '%coordinar el siguiente paso%'),
                (SELECT state FROM mediation_sessions WHERE session_id = ?1)",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };

    assert_eq!(offered_count, 1);
    assert_eq!(
        invite_count, 2,
        "buyer and seller should both receive the invitation"
    );
    assert_eq!(state, "summary_delivered");
}
