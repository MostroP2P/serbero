//! Cooperative summary pipeline (US3 / T059).
//!
//! Produces a [`MediationSummary`] for the assigned solver when the
//! policy layer returns [`crate::mediation::policy::PolicyDecision::Summarize`].
//! The rationale text lands in the controlled audit store
//! ([`crate::db::rationales`]); general logs reference it by id
//! only (FR-120).
//!
//! Boundary: [`summarize`] is the only code path that calls
//! [`crate::reasoning::ReasoningProvider::summarize`]. Authority-
//! boundary suppression (fund-moving or dispute-closing instructions)
//! runs here against the returned text so the caller never sees a
//! raw string that would cross the Phase 3 boundary — an unsafe
//! response short-circuits to [`Error::PolicyViolation`] and the
//! engine escalates with `AuthorityBoundaryAttempt`.
//!
//! Persistence: the rationale insert + `mediation_summaries` row
//! insert + `summary_generated` audit event all run inside a single
//! DB transaction. A failure in any of the three rolls the whole
//! thing back so the audit log and the summary row cannot drift.

use std::sync::Arc;

use rusqlite::params;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info, instrument};

use crate::db;
use crate::db::mediation_events::MediationEventKind;
use crate::error::{Error, Result};
use crate::models::mediation::ClassificationLabel;
use crate::models::reasoning::{SummaryRequest, TranscriptEntry};
use crate::prompts::PromptBundle;
use crate::reasoning::ReasoningProvider;

/// Phrases the model is forbidden from surfacing in
/// `summary_text` / `suggested_next_step`. Matched case-insensitively.
/// Kept as a small static list — the contract (`reasoning-provider.md`
/// §Policy-Layer Validation, §Authority Boundary) lists the concrete
/// Mostro actions Serbero does NOT execute; those are the only
/// phrases worth suppressing. A phrase match escalates the session
/// with `AuthorityBoundaryAttempt` rather than silently stripping,
/// because a model that asks for fund movement has produced an
/// output worth surfacing to a human solver.
const AUTHORITY_BOUNDARY_PHRASES: &[&str] = &[
    "admin-settle",
    "admin_settle",
    "admin-cancel",
    "admin_cancel",
    "release funds",
    "release the funds",
    "settle the trade",
    "settle funds",
    "cancel trade",
    "cancel the trade",
    "force close",
    "force-close",
    "move funds",
    "transfer funds",
    "disburse funds",
    "close the dispute",
];

/// Structured summary produced by the reasoning provider, ready for
/// solver delivery. Owned by the engine after a successful
/// [`summarize`] return; also persisted to `mediation_summaries`.
#[derive(Debug, Clone)]
pub struct MediationSummary {
    pub session_id: String,
    pub dispute_id: String,
    pub classification: ClassificationLabel,
    pub confidence: f64,
    pub suggested_next_step: String,
    pub summary_text: String,
    pub rationale_id: String,
    pub prompt_bundle_id: String,
    pub policy_hash: String,
    pub generated_at: i64,
}

/// Parameters for [`summarize`]. Grouped so the call site stays
/// compact and clippy does not flag too_many_arguments.
pub struct SummarizeParams<'a> {
    pub conn: &'a Arc<AsyncMutex<rusqlite::Connection>>,
    pub session_id: &'a str,
    pub dispute_id: &'a str,
    pub classification: ClassificationLabel,
    pub confidence: f64,
    pub transcript: Vec<TranscriptEntry>,
    pub prompt_bundle: &'a Arc<PromptBundle>,
    pub reasoning: &'a dyn ReasoningProvider,
    pub provider_name: &'a str,
    pub model_name: &'a str,
}

/// Call the reasoning provider's `summarize` method, suppress
/// authority-boundary outputs, and persist the rationale + summary
/// + audit event in one transaction.
///
/// Error surface:
/// - `Err(Error::ReasoningUnavailable(_))` on any adapter error.
///   The caller is expected to escalate with `ReasoningUnavailable`.
/// - `Err(Error::PolicyViolation(_))` when the response text would
///   cross Serbero's authority boundary. Caller escalates with
///   `AuthorityBoundaryAttempt`.
/// - `Err(Error::Db(_))` on persistence failure. The transaction
///   rolls back; caller can retry on the next tick.
#[instrument(
    skip_all,
    fields(session_id = %params.session_id, dispute_id = %params.dispute_id)
)]
pub async fn summarize(params: SummarizeParams<'_>) -> Result<MediationSummary> {
    // (1) Build the request. Clone the transcript + bundle Arc; the
    //     bundle bytes flow to the model so the policy_hash invariant
    //     holds (SC-103).
    let request = SummaryRequest {
        session_id: params.session_id.to_string(),
        dispute_id: params.dispute_id.to_string(),
        prompt_bundle: Arc::clone(params.prompt_bundle),
        transcript: params.transcript,
        classification: params.classification,
        confidence: params.confidence,
    };

    // (2) Call the adapter. Any error → ReasoningUnavailable; the
    //     caller owns the escalation decision.
    let response = params
        .reasoning
        .summarize(request)
        .await
        .map_err(|e| Error::ReasoningUnavailable(e.to_string()))?;

    // (3a) Reject empty / whitespace-only fields. Both
    //      `summary_text` and `suggested_next_step` flow to the
    //      solver DM, so an empty either side produces a
    //      meaningless message — worse than escalating. Matches
    //      the empty-clarification guard in policy.rs and the
    //      session-open `build_wrap` guard. We classify this as
    //      `PolicyViolation` so the caller escalates with
    //      `AuthorityBoundaryAttempt` — an empty response from
    //      an LLM prompted to summarize is likely a guard-rail
    //      refusal, which is the shape the operator most wants
    //      to inspect.
    if response.summary_text.trim().is_empty() || response.suggested_next_step.trim().is_empty() {
        return Err(Error::PolicyViolation(
            "empty summary or suggested next step from reasoning provider".into(),
        ));
    }

    // (3b) Authority-boundary suppression. BOTH the summary text and
    //      the suggested next step flow to the solver DM, so both are
    //      checked. Any match is loud: we return a distinct
    //      `PolicyViolation` so the caller can tag the escalation
    //      with `AuthorityBoundaryAttempt` instead of a generic
    //      reasoning failure.
    if contains_authority_boundary_phrase(&response.summary_text)
        || contains_authority_boundary_phrase(&response.suggested_next_step)
    {
        return Err(Error::PolicyViolation(
            "authority boundary attempt in summary".into(),
        ));
    }

    // (4)–(6) Persist rationale + summary row + audit event
    //         atomically. One transaction; any failure rolls back.
    let now = current_ts_secs()?;
    let rationale_text = response.rationale.0;
    let prompt_bundle_id = params.prompt_bundle.id.clone();
    let policy_hash = params.prompt_bundle.policy_hash.clone();
    let classification = params.classification;
    let confidence = params.confidence;
    let suggested_next_step = response.suggested_next_step;
    let summary_text = response.summary_text;
    let session_id_owned = params.session_id.to_string();
    let dispute_id_owned = params.dispute_id.to_string();

    let rationale_id = {
        let mut guard = params.conn.lock().await;
        let tx = guard.transaction()?;

        let rationale_id = db::rationales::insert_rationale(
            &tx,
            Some(&session_id_owned),
            params.provider_name,
            params.model_name,
            &prompt_bundle_id,
            &policy_hash,
            &rationale_text,
            now,
        )?;
        debug!(
            session_id = %session_id_owned,
            rationale_id = %rationale_id,
            "rationale persisted"
        );

        tx.execute(
            "INSERT INTO mediation_summaries (
                session_id, dispute_id, classification, confidence,
                suggested_next_step, summary_text,
                prompt_bundle_id, policy_hash, rationale_id, generated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                session_id_owned,
                dispute_id_owned,
                classification.to_string(),
                confidence,
                suggested_next_step,
                summary_text,
                prompt_bundle_id,
                policy_hash,
                rationale_id,
                now,
            ],
        )?;

        let payload = json!({
            "rationale_id": rationale_id,
            "classification": classification.to_string(),
            "confidence": confidence,
        })
        .to_string();
        db::mediation_events::record_event(
            &tx,
            MediationEventKind::SummaryGenerated,
            Some(&session_id_owned),
            &payload,
            Some(&rationale_id),
            Some(&prompt_bundle_id),
            Some(&policy_hash),
            now,
        )?;

        tx.commit()?;
        rationale_id
    };

    info!(
        session_id = %session_id_owned,
        rationale_id = %rationale_id,
        classification = %classification,
        confidence = confidence,
        "summary_generated"
    );

    Ok(MediationSummary {
        session_id: session_id_owned,
        dispute_id: dispute_id_owned,
        classification,
        confidence,
        suggested_next_step,
        summary_text,
        rationale_id,
        prompt_bundle_id,
        policy_hash,
        generated_at: now,
    })
}

fn contains_authority_boundary_phrase(text: &str) -> bool {
    let lower = text.to_lowercase();
    AUTHORITY_BOUNDARY_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
}

// Shares the `current_ts_secs` helper with `session.rs` and the
// deliver-summary path in `mediation/mod.rs`.
use super::current_ts_secs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_boundary_phrases_match_case_insensitively() {
        assert!(contains_authority_boundary_phrase(
            "Please use admin-settle to release the escrow."
        ));
        assert!(contains_authority_boundary_phrase(
            "Recommend ADMIN-CANCEL on this order."
        ));
        assert!(contains_authority_boundary_phrase(
            "The solver should release funds immediately."
        ));
        assert!(contains_authority_boundary_phrase(
            "Force-close the dispute right away."
        ));
    }

    #[test]
    fn benign_summary_text_passes() {
        assert!(!contains_authority_boundary_phrase(
            "Buyer confirms receipt; seller acknowledges the transfer landed."
        ));
        assert!(!contains_authority_boundary_phrase(
            "Both parties agree the fiat payment completed at 14:05."
        ));
        assert!(!contains_authority_boundary_phrase(""));
    }
}
