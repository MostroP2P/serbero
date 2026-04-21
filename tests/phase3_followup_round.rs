//! Phase 11 / T122 — mid-session follow-up round (SC-112 + SC-113).
//!
//! Verifies the second-round outbound fires exactly once after party
//! replies land, and that subsequent ticks without new inbound are
//! no-ops (FR-127 idempotency via `round_count_last_evaluated`).
//!
//! Harness:
//! - `MockRelay` via `nostr-relay-builder`.
//! - Serbero identity, buyer trade keys, seller trade keys, with the
//!   ECDH-derived per-party shared keys.
//! - A pre-seeded `mediation_sessions` row in `awaiting_response` at
//!   `round_count = 1`, plus two outbound rows for round 0 (the
//!   US1 opening drafter's work, simulated directly to keep the
//!   test focused on Phase 11) and two inbound rows for the
//!   parties' replies (simulating what the US2 ingest tick would
//!   have persisted).
//! - A `MockReasoningProvider` returning `AskClarification(...)`
//!   so `policy::evaluate` → `PolicyDecision::AskClarification`,
//!   which should drive the follow-up drafter.
//! - `SessionKeyCache` populated with `DisputeChatMaterial` so
//!   `advance_session_round` can reach the drafter.
//!
//! Assertions:
//! - SC-112: after `advance_session_round`, there are 4 outbound
//!   rows in total (round 0 × 2 from the seed + round 1 × 2 from
//!   this call), the session is back in `awaiting_response`, and
//!   `round_count_last_evaluated = 1`. Each round-1 row carries a
//!   `"Round 1. "` body prefix (the drafter's marker).
//! - SC-113: calling `advance_session_round` a second time without
//!   a new Fresh ingest produces ZERO new outbound rows and leaves
//!   the marker at `1`.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nostr_relay_builder::MockRelay;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as AsyncMutex;

use serbero::chat::dispute_chat_flow::DisputeChatMaterial;
use serbero::chat::shared_key::derive_shared_keys;
use serbero::db;
use serbero::mediation::follow_up::advance_session_round;
use serbero::mediation::SessionKeyCache;
use serbero::models::mediation::TranscriptParty;
use serbero::prompts::{self, PromptBundle};
use serbero::reasoning::ReasoningProvider;

use common::MockReasoningProvider;

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

/// Seed the session row and the four rows that a real US1+US2 run
/// would have left on disk by the time the follow-up loop fires:
/// two outbound (round 0) + two inbound (the parties' replies).
/// `round_count` is set to 1 so the Phase 11 idempotency gate
/// (`round_count > round_count_last_evaluated`) passes on the first
/// `advance_session_round` call.
#[allow(clippy::too_many_arguments)]
fn seed_session_with_round_zero(
    conn: &rusqlite::Connection,
    session_id: &str,
    dispute_id: &str,
    bundle: &PromptBundle,
    buyer_shared_pk: &str,
    seller_shared_pk: &str,
    buyer_inbound_content: &str,
    seller_inbound_content: &str,
) {
    conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at, lifecycle_state
         ) VALUES (?1, 'evt-t122', 'mostro-t122', 'buyer',
                   'initiated', 0, 0, 'notified')",
        rusqlite::params![dispute_id],
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
    // Round-0 outbound rows (Serbero's opening clarification).
    // inner_event_ids and outer_event_ids are synthetic — no real
    // gift-wrap exists, but the unique index only cares about
    // (session_id, inner_event_id) pairs.
    for (party, shared_pk, inner_id, content) in [
        (
            "buyer",
            buyer_shared_pk,
            "inner-r0-buyer",
            "Buyer: tell me what happened",
        ),
        (
            "seller",
            seller_shared_pk,
            "inner-r0-seller",
            "Seller: tell me what happened",
        ),
    ] {
        conn.execute(
            "INSERT INTO mediation_messages (
                session_id, direction, party, shared_pubkey,
                inner_event_id, inner_event_created_at, outer_event_id,
                content, prompt_bundle_id, policy_hash,
                persisted_at, stale
             ) VALUES (?1, 'outbound', ?2, ?3, ?4, 200, 'outer-r0-buyer',
                       ?5, ?6, ?7, 200, 0)",
            rusqlite::params![session_id, party, shared_pk, inner_id, content, bundle.id, bundle.policy_hash],
        )
        .unwrap();
    }
    // Inbound rows from the parties' replies. Inner timestamps
    // strictly after the outbounds so the transcript loader
    // preserves ordering.
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
async fn second_round_outbound_fires_once_and_is_idempotent() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    // Keys: Serbero + each party's trade keys.
    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();

    // Per-party shared keys — the same ECDH output that Serbero
    // computed when it opened the session. In a real run these
    // would live in `SessionKeyCache` via the take-flow; here we
    // derive and insert them directly.
    let buyer_shared = derive_shared_keys(&serbero_keys, &buyer_trade.public_key()).unwrap();
    let seller_shared = derive_shared_keys(&serbero_keys, &seller_trade.public_key()).unwrap();

    // Prompt bundle (fixture — same id / hash stack as production).
    let bundle = fixture_bundle();

    // DB with the seeded session + round-0 rows + two inbound
    // replies waiting to be re-classified.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_string_lossy().into_owned();
    let mut raw = db::open_connection(&db_path).unwrap();
    db::migrations::run_migrations(&mut raw).unwrap();
    let session_id = "sess-t122";
    let dispute_id = "dispute-t122";
    let buyer_content = "Buyer: I sent the fiat via bank transfer at 14:05.";
    let seller_content = "Seller: I have not seen the funds arrive yet.";
    seed_session_with_round_zero(
        &raw,
        session_id,
        dispute_id,
        &bundle,
        &buyer_shared.public_key().to_hex(),
        &seller_shared.public_key().to_hex(),
        buyer_content,
        seller_content,
    );
    let conn = Arc::new(AsyncMutex::new(raw));

    // Serbero's live nostr client (for the drafter's publish path).
    let serbero_client = Client::new(serbero_keys.clone());
    serbero_client.add_relay(&relay_url).await.unwrap();
    serbero_client.connect().await;
    serbero_client
        .wait_for_connection(Duration::from_secs(5))
        .await;

    // `SessionKeyCache` with the chat material. In production this
    // is populated by the session-open path; here we insert directly.
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

    // Scripted reasoning provider returns AskClarification for the
    // follow-up question. The policy layer's mid-session rule path
    // (PolicyRound::MidSession) applies the low-confidence gate, so
    // we set confidence = 0.9 to pass through cleanly.
    let follow_up_question = "Could you both share the fiat payment reference id and the timezone?";
    let reasoning: Arc<dyn ReasoningProvider> = Arc::new(MockReasoningProvider {
        clarification: follow_up_question.into(),
    });

    // --- First advance: should draft + publish round 1. ----------
    advance_session_round(
        &conn,
        &serbero_client,
        &serbero_keys,
        reasoning.as_ref(),
        &bundle,
        session_id,
        &session_key_cache,
        &[], // no solvers needed on the AskClarification branch
        "mock-provider",
        "mock-model",
    )
    .await
    .expect("advance_session_round first call must succeed");

    // Assert: 4 outbound rows (round 0 × 2 + round 1 × 2), the new
    // two carry the `"Round 1. "` prefix, and the session stays in
    // `awaiting_response` with the marker advanced.
    let outbound_rows: Vec<(String, String)> = {
        let c = conn.lock().await;
        let mut stmt = c
            .prepare(
                "SELECT party, content
                 FROM mediation_messages
                 WHERE session_id = ?1 AND direction = 'outbound'
                 ORDER BY inner_event_created_at ASC",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![session_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
    };
    assert_eq!(
        outbound_rows.len(),
        4,
        "SC-112: 4 outbound rows expected (2 round-0 seeded + 2 round-1 dispatched), got {:?}",
        outbound_rows
    );
    // Last two rows are the round-1 pair. Both MUST carry the
    // "Round 1. " prefix and the follow-up question.
    for (_party, content) in &outbound_rows[2..] {
        assert!(
            content.starts_with("Round 1. "),
            "SC-112: follow-up row must carry the round-number marker; got {content:?}"
        );
        assert!(
            content.contains(follow_up_question),
            "SC-112: follow-up row must contain the question; got {content:?}"
        );
    }
    let round_1_parties: Vec<String> = outbound_rows[2..]
        .iter()
        .map(|(p, _)| p.clone())
        .collect::<Vec<_>>();
    assert!(
        round_1_parties.contains(&"buyer".to_string())
            && round_1_parties.contains(&"seller".to_string()),
        "SC-112: both buyer and seller MUST get the follow-up"
    );

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
    assert_eq!(state, "awaiting_response");
    assert_eq!(round_count, 1);
    assert_eq!(marker, 1, "FR-127: marker must advance to round_count");

    // --- Second advance: no new inbound, must be a no-op. -------
    advance_session_round(
        &conn,
        &serbero_client,
        &serbero_keys,
        reasoning.as_ref(),
        &bundle,
        session_id,
        &session_key_cache,
        &[],
        "mock-provider",
        "mock-model",
    )
    .await
    .expect("advance_session_round second call must succeed as a no-op");

    let outbound_count_after_second: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_messages
             WHERE session_id = ?1 AND direction = 'outbound'",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        outbound_count_after_second, 4,
        "SC-113: second advance with no new inbound MUST produce zero new outbounds"
    );
    // And no duplicate classification_produced row for round 1
    // either — only the one the first advance wrote.
    let classification_count: i64 = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT COUNT(*) FROM mediation_events
             WHERE session_id = ?1 AND kind = 'classification_produced'",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        classification_count, 1,
        "SC-113: second advance MUST NOT trigger another classify/evaluate (exactly one audit row)"
    );

    // And for belt-and-braces, the state + marker are unchanged.
    let (state_after, marker_after): (String, i64) = {
        let c = conn.lock().await;
        c.query_row(
            "SELECT state, round_count_last_evaluated
             FROM mediation_sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(state_after, "awaiting_response");
    assert_eq!(marker_after, 1);
}

/// Defensive TranscriptParty import — silences the "unused" warning
/// in case future refactors drop the manual `party` column checks
/// above. The enum is listed here so a `use` is not removed by
/// tools like rustfmt's import pruner during churn.
#[allow(dead_code)]
fn _party_import_anchor() -> TranscriptParty {
    TranscriptParty::Buyer
}
