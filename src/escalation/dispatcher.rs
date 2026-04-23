//! Phase 4 dispatcher — builds the `escalation_handoff/v1` DM
//! body and sends it via the existing Phase 1/2 notifier.
//!
//! Two pure concerns, split across [`build_dm_body`] and the
//! async send helper [`send_to_recipients`] (T014). The body is
//! pure enough to unit-test without any IO; the sender is a thin
//! wrapper around the gift-wrap notifier + `notifications`-table
//! bookkeeping that lives in Phase 1/2.

use std::sync::Arc;

use nostr_sdk::prelude::{Client, Keys, PublicKey};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

use crate::db;
use crate::error::Result;
use crate::mediation::escalation::HandoffPackage;
use crate::models::{NotificationStatus, NotificationType};
use crate::nostr::send_gift_wrap_notification;

/// Version prefix written on the first line of every Phase 4 DM.
/// Consumers (log parsers, solver-side tooling) branch on this
/// prefix; incompatible body changes MUST bump to `v2`, etc.
pub(crate) const DM_VERSION: &str = "escalation_handoff/v1";

/// Build the `escalation_handoff/v1` body for one handoff package.
///
/// Pure function. No IO, no logging, no allocation beyond the
/// returned `String`. Shape matches
/// `specs/004-escalation-execution/contracts/dm-payload.md`
/// verbatim:
///
/// ```text
/// escalation_handoff/v1
/// Dispute: <dispute_id>
/// Session: <session_id or "<none — dispute-scoped handoff>">
/// Trigger: <trigger>
///
/// Escalation required for dispute <dispute_id>. Trigger: <trigger>.
/// This dispute was evaluated by Serbero's mediation assistance
/// system and requires human judgment. Please run TakeDispute for
/// dispute <dispute_id> on your Mostro instance to review the full
/// context.
///
/// Handoff payload (JSON):
/// { ... one-line serialized HandoffPackage ... }
/// ```
///
/// FR-206 compliance: the JSON line serializes the `HandoffPackage`
/// struct, which carries `rationale_refs` (content-hash ids) but
/// NEVER the rationale text itself. A defensive sanity check is
/// worth adding once `HandoffPackage` gains any new string field
/// that could accidentally carry privileged text; today the struct
/// shape makes a rationale-text leak structurally impossible.
pub fn build_dm_body(pkg: &HandoffPackage) -> String {
    let session_header = match &pkg.session_id {
        Some(sid) => format!("Session: {sid}"),
        None => "Session: <none — dispute-scoped handoff>".to_string(),
    };

    // Serialization cannot fail on a well-formed HandoffPackage
    // (all fields are owned primitives / Vec<String>). If it does,
    // fall back to a best-effort payload line with the dispute id
    // so the DM still carries SOMETHING operator-readable — we'd
    // rather ship a degraded DM than drop the whole dispatch.
    let payload_line = serde_json::to_string(pkg).unwrap_or_else(|e| {
        warn!(
            dispute_id = %pkg.dispute_id,
            error = %e,
            "build_dm_body: HandoffPackage failed to serialize; emitting degraded payload line"
        );
        format!(
            r#"{{"dispute_id":"{}","serialization_error":true}}"#,
            pkg.dispute_id
        )
    });

    format!(
        "{DM_VERSION}\n\
         Dispute: {dispute}\n\
         {session_header}\n\
         Trigger: {trigger}\n\
         \n\
         Escalation required for dispute {dispute}. Trigger: {trigger}. \
         This dispute was evaluated by Serbero's mediation assistance \
         system and requires human judgment. Please run TakeDispute for \
         dispute {dispute} on your Mostro instance to review the full \
         context.\n\
         \n\
         Handoff payload (JSON):\n\
         {payload_line}",
        dispute = pkg.dispute_id,
        trigger = pkg.trigger,
    )
}

/// Outcome of a send loop across one or more recipients.
///
/// The tracker (T015) maps this onto
/// [`crate::db::escalation_dispatches::DispatchStatus`]: both
/// `AllSucceeded` and `PartialSuccess` map to `Dispatched` (per
/// FR-211 "at least one recipient succeeded"), while `AllFailed`
/// maps to `SendFailed`.
///
/// **Order discipline**: every variant's `attempted_recipients`
/// projection MUST return the recipients in the original send-loop
/// order. The tracker persists that ordering verbatim into
/// `escalation_dispatches.target_solver`, and operator
/// reconciliation correlates it with `notifications` rows by
/// timestamp. A shuffled partial-success list would break that
/// correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Every targeted recipient's gift-wrap publish succeeded.
    /// `recipients` is in send-loop order.
    AllSucceeded { recipients: Vec<String> },
    /// Every targeted recipient's gift-wrap publish failed.
    /// `attempted` is in send-loop order.
    AllFailed { attempted: Vec<String> },
    /// Some recipients succeeded, others failed.
    /// - `attempted`: the original send-loop order (authoritative
    ///   source for [`attempted_recipients`] so `target_solver`
    ///   matches the `notifications`-row timestamps).
    /// - `succeeded` / `failed`: analytical splits of `attempted`,
    ///   useful for logs + future handlers. Each is a subset of
    ///   `attempted` in its original relative order.
    PartialSuccess {
        attempted: Vec<String>,
        succeeded: Vec<String>,
        failed: Vec<String>,
    },
}

impl DispatchOutcome {
    /// Full recipient list in the order the send loop attempted.
    /// Used by the tracker to fill
    /// `escalation_dispatches.target_solver` and to correlate the
    /// dispatch row back to per-recipient `notifications` rows.
    pub fn attempted_recipients(&self) -> Vec<String> {
        match self {
            DispatchOutcome::AllSucceeded { recipients } => recipients.clone(),
            DispatchOutcome::AllFailed { attempted } => attempted.clone(),
            DispatchOutcome::PartialSuccess { attempted, .. } => attempted.clone(),
        }
    }
}

/// Send the DM body to every recipient in turn and record each
/// per-recipient outcome in `notifications`.
///
/// Runs the gift-wrap publish sequentially (not concurrently) to
/// mirror the Phase 1/2 notifier's discipline and to keep the
/// notifications-table insert order deterministic for operator
/// reconciliation queries. A single `relay-down` failure therefore
/// does not abort the batch — every recipient in `recipients` gets
/// exactly one `notifications` row with `status = 'sent'` or
/// `'failed'`.
///
/// Returns the aggregate [`DispatchOutcome`] so the tracker (T015)
/// can derive the dispatch-row `status` without re-reading the
/// notifications table.
pub async fn send_to_recipients(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    _serbero_keys: &Keys,
    dispute_id: &str,
    recipients: &[String],
    body: &str,
    now_ts: i64,
) -> Result<DispatchOutcome> {
    let mut succeeded: Vec<String> = Vec::with_capacity(recipients.len());
    let mut failed: Vec<String> = Vec::new();

    for pk_hex in recipients {
        let parsed_pk = match PublicKey::parse(pk_hex) {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    dispute_id = %dispute_id,
                    solver_pubkey = %pk_hex,
                    error = %e,
                    "phase4_dispatch: recipient pubkey malformed"
                );
                // Record the failure in notifications so the
                // audit trail shows the attempt. dispatch_id lives
                // in escalation_dispatches; notifications keys off
                // `solver_pubkey` and `dispute_id`.
                insert_notification(
                    conn,
                    dispute_id,
                    pk_hex,
                    NotificationStatus::Failed,
                    Some(format!("invalid pubkey: {e}")),
                    now_ts,
                )
                .await;
                failed.push(pk_hex.clone());
                continue;
            }
        };
        match send_gift_wrap_notification(client, &parsed_pk, body).await {
            Ok(()) => {
                debug!(
                    dispute_id = %dispute_id,
                    solver_pubkey = %pk_hex,
                    "phase4_dispatch: recipient send ok"
                );
                insert_notification(
                    conn,
                    dispute_id,
                    pk_hex,
                    NotificationStatus::Sent,
                    None,
                    now_ts,
                )
                .await;
                succeeded.push(pk_hex.clone());
            }
            Err(e) => {
                warn!(
                    dispute_id = %dispute_id,
                    solver_pubkey = %pk_hex,
                    error = %e,
                    "phase4_dispatch: recipient send failed"
                );
                insert_notification(
                    conn,
                    dispute_id,
                    pk_hex,
                    NotificationStatus::Failed,
                    Some(e.to_string()),
                    now_ts,
                )
                .await;
                failed.push(pk_hex.clone());
            }
        }
    }

    // `recipients` was walked in order, so a clone is the
    // authoritative send-loop ordering used by `target_solver`.
    // We deliberately do NOT concatenate `succeeded + failed` —
    // that would reorder recipients whenever a failure arrived
    // before a later success in the loop, breaking the
    // ordering invariant documented on DispatchOutcome.
    let attempted: Vec<String> = recipients.to_vec();
    let outcome = if failed.is_empty() {
        DispatchOutcome::AllSucceeded {
            recipients: attempted,
        }
    } else if succeeded.is_empty() {
        DispatchOutcome::AllFailed { attempted }
    } else {
        DispatchOutcome::PartialSuccess {
            attempted,
            succeeded,
            failed,
        }
    };
    Ok(outcome)
}

/// Record one per-recipient outcome in `notifications`. Best-effort
/// — a DB failure here is logged but NOT propagated, so a transient
/// table-lock issue does not abort the Phase 4 cycle mid-batch. The
/// per-recipient row is an audit artefact, not a correctness
/// dependency: the send either succeeded or it didn't, and the
/// dispatch-tracking row the tracker writes later is the
/// authoritative outcome.
async fn insert_notification(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
    solver_pubkey: &str,
    status: NotificationStatus,
    error_message: Option<String>,
    sent_at: i64,
) {
    let guard = conn.lock().await;
    db::notifications::record_notification_logged(
        &guard,
        dispute_id,
        solver_pubkey,
        sent_at,
        status,
        error_message.as_deref(),
        NotificationType::MediationEscalationRecommended,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_package(session_id: Option<&str>) -> HandoffPackage {
        HandoffPackage {
            dispute_id: "dispute-abc".to_string(),
            session_id: session_id.map(|s| s.to_string()),
            trigger: "conflicting_claims".to_string(),
            evidence_refs: vec!["inner-event-1".to_string(), "inner-event-2".to_string()],
            prompt_bundle_id: "phase3-default".to_string(),
            policy_hash: "abcd1234".to_string(),
            rationale_refs: vec!["9f86d081884c".to_string()],
            assembled_at: 1_745_000_000,
        }
    }

    #[test]
    fn body_starts_with_version_prefix() {
        let pkg = sample_package(Some("sess-1"));
        let body = build_dm_body(&pkg);
        assert!(
            body.starts_with("escalation_handoff/v1\n"),
            "first line must be exactly the version prefix; got: {body}"
        );
    }

    #[test]
    fn body_carries_dispute_id_and_trigger_in_headers_and_summary() {
        let pkg = sample_package(Some("sess-1"));
        let body = build_dm_body(&pkg);
        assert!(body.contains("Dispute: dispute-abc"));
        assert!(body.contains("Trigger: conflicting_claims"));
        assert!(body.contains("Escalation required for dispute dispute-abc"));
        // Action instruction (FR-204).
        assert!(body.contains("Please run TakeDispute for dispute dispute-abc"));
        // Assistance-not-authority identity (FR-207).
        assert!(body.contains("Serbero's mediation assistance system"));
    }

    #[test]
    fn body_session_header_uses_literal_marker_when_session_id_absent() {
        let pkg = sample_package(None);
        let body = build_dm_body(&pkg);
        assert!(
            body.contains("Session: <none — dispute-scoped handoff>"),
            "dispute-scoped (FR-122) handoff must render the <none> marker; got: {body}"
        );
    }

    #[test]
    fn body_session_header_uses_session_id_when_present() {
        let pkg = sample_package(Some("sess-xyz"));
        let body = build_dm_body(&pkg);
        assert!(body.contains("Session: sess-xyz"));
        // Must NOT contain the placeholder text.
        assert!(!body.contains("<none — dispute-scoped handoff>"));
    }

    #[test]
    fn json_payload_round_trips_to_handoff_package() {
        let pkg = sample_package(Some("sess-1"));
        let body = build_dm_body(&pkg);
        // Extract the JSON payload line — it's the last line after
        // the "Handoff payload (JSON):" marker.
        let json_line = body
            .lines()
            .skip_while(|l| !l.starts_with("Handoff payload (JSON)"))
            .nth(1)
            .expect("payload line must exist");
        let parsed: HandoffPackage =
            serde_json::from_str(json_line).expect("JSON round-trip must succeed");
        assert_eq!(parsed.dispute_id, pkg.dispute_id);
        assert_eq!(parsed.trigger, pkg.trigger);
        assert_eq!(parsed.evidence_refs, pkg.evidence_refs);
        assert_eq!(parsed.rationale_refs, pkg.rationale_refs);
        assert_eq!(parsed.assembled_at, pkg.assembled_at);
        assert_eq!(parsed.session_id, pkg.session_id);
    }

    #[test]
    fn json_payload_omits_session_id_key_when_none() {
        // FR-122 / data-model.md: "key absent" ≡ "no session", not
        // "session_id: null". Confirmed by the
        // `skip_serializing_if = "Option::is_none"` on HandoffPackage.
        let pkg = sample_package(None);
        let body = build_dm_body(&pkg);
        let json_line = body
            .lines()
            .skip_while(|l| !l.starts_with("Handoff payload (JSON)"))
            .nth(1)
            .expect("payload line must exist");
        assert!(
            !json_line.contains("session_id"),
            "absent session must NOT emit the session_id key at all (got: {json_line})"
        );
    }

    #[test]
    fn body_never_carries_raw_rationale_text() {
        // FR-206: only rationale reference ids (content-hash SHA-256)
        // may appear in the DM. The HandoffPackage struct already
        // excludes rationale text by construction, but this test
        // pins the expectation so a future struct extension that
        // adds a raw text field fails loudly.
        let mut pkg = sample_package(Some("sess-1"));
        pkg.rationale_refs = vec!["ref-abc123".to_string()];
        let body = build_dm_body(&pkg);
        assert!(body.contains("ref-abc123"));
        // Sentinel: no mention of the word "rationale_text" should
        // ever appear — that would indicate someone added a raw
        // text field to HandoffPackage.
        assert!(
            !body.contains("rationale_text"),
            "raw rationale text MUST NOT appear in the DM body"
        );
    }

    #[test]
    fn dispatch_outcome_attempted_recipients_preserves_order() {
        // AllSucceeded
        let o = DispatchOutcome::AllSucceeded {
            recipients: vec!["a".into(), "b".into(), "c".into()],
        };
        assert_eq!(o.attempted_recipients(), vec!["a", "b", "c"]);

        // AllFailed
        let o = DispatchOutcome::AllFailed {
            attempted: vec!["x".into(), "y".into()],
        };
        assert_eq!(o.attempted_recipients(), vec!["x", "y"]);

        // PartialSuccess — `attempted` is the authoritative order.
        // The `succeeded` / `failed` sub-lists are analytical
        // splits and do NOT determine the projection.
        let o = DispatchOutcome::PartialSuccess {
            attempted: vec!["ok-1".into(), "bad-1".into(), "ok-2".into()],
            succeeded: vec!["ok-1".into(), "ok-2".into()],
            failed: vec!["bad-1".into()],
        };
        assert_eq!(
            o.attempted_recipients(),
            vec!["ok-1", "bad-1", "ok-2"],
            "PartialSuccess must preserve the original send-loop order"
        );
    }

    #[test]
    fn partial_success_with_failure_before_later_success_keeps_original_order() {
        // Regression guard: the earlier implementation appended
        // `failed` after `succeeded` when building
        // `attempted_recipients`, which reordered recipients
        // whenever a failure came BEFORE a later success in the
        // send loop. Operators who correlate `target_solver` with
        // `notifications` rows by timestamp would then see a mismatch
        // between the comma-joined dispatch row and the actual send
        // order. Attempted sequence here: [A (fail), B (ok)] → the
        // projection must match, not [B, A].
        let o = DispatchOutcome::PartialSuccess {
            attempted: vec!["A".into(), "B".into()],
            succeeded: vec!["B".into()],
            failed: vec!["A".into()],
        };
        assert_eq!(
            o.attempted_recipients(),
            vec!["A", "B"],
            "attempted_recipients MUST reflect send-loop order, \
             not a succeeded-then-failed concatenation"
        );
    }
}
