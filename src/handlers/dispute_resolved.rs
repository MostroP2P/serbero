//! US6 handler — dispute resolved externally while Serbero is
//! mediating.
//!
//! Wired from [`crate::dispatcher`] for the Mostro
//! [`DisputeStatus::SellerRefunded`], [`DisputeStatus::Settled`] and
//! [`DisputeStatus::Released`] states carried on the kind-38386
//! replaceable event's `s` tag. Runs in the Phase 1/2 event loop, so
//! per SC-105 it MUST NOT panic or abort the loop on failure — every
//! error path logs and returns `Ok(())`.
//!
//! Happy path:
//! 1. Extract `dispute_id` from the `d` tag.
//! 2. Look up the dispute; if unknown (or already `Resolved`),
//!    short-circuit.
//! 3. Flip `lifecycle_state` to [`LifecycleState::Resolved`].
//! 4. If there is an active mediation session, close it via
//!    `AwaitingResponse → SupersededByHuman → Closed` inside a SINGLE
//!    transaction together with both `superseded_by_human` and
//!    `session_closed` audit rows, then send a resolution report DM
//!    to the solver(s).
//! 5. If the session is already at `escalation_recommended`, skip the
//!    session closure entirely — that transition is not legal from
//!    `EscalationRecommended`, and the solver already has the handoff
//!    package. Only the `lifecycle_state` update in step 3 applies.

use nostr_sdk::Event;
use serde_json::json;
use tracing::{debug, error, info, instrument, warn};

use crate::db;
use crate::db::mediation_events::MediationEventKind;
use crate::error::Result;
use crate::handlers::dispute_detected::{current_timestamp, HandlerContext};
use crate::mediation::report;
use crate::models::mediation::MediationSessionState;
use crate::models::LifecycleState;

#[instrument(skip(ctx, event), fields(dispute_id, event_id, resolution_status))]
pub async fn handle(ctx: &HandlerContext, event: &Event) -> Result<()> {
    let event_id_hex = event.id.to_string();
    let Some(dispute_id) = event.tags.identifier().map(|v| v.to_string()) else {
        error!(
            event_id = %event_id_hex,
            event_kind = ?event.kind,
            tags = ?event.tags,
            "dispute_resolved: missing `d` tag on resolved dispute event"
        );
        return Ok(());
    };

    let Some(resolution_status) = status_tag(event) else {
        error!(
            dispute_id = %dispute_id,
            event_id = %event_id_hex,
            event_kind = ?event.kind,
            tags = ?event.tags,
            "dispute_resolved: missing `s` tag on resolved dispute event"
        );
        return Ok(());
    };

    tracing::Span::current().record("dispute_id", dispute_id.as_str());
    tracing::Span::current().record("event_id", event_id_hex.as_str());
    tracing::Span::current().record("resolution_status", resolution_status.as_str());

    info!(
        dispute_id = %dispute_id,
        resolution_status = %resolution_status,
        "dispute_resolved_externally"
    );

    let now = current_timestamp();
    let mut closed_session_id: Option<String> = None;

    let existing = {
        let guard = ctx.conn.lock().await;
        match db::disputes::get_dispute(&guard, &dispute_id) {
            Ok(opt) => opt,
            Err(e) => {
                error!(
                    dispute_id = %dispute_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: dispute lookup failed"
                );
                return Ok(());
            }
        }
    };
    let Some(existing) = existing else {
        debug!("resolution event for unknown dispute; ignoring");
        return Ok(());
    };

    if existing.lifecycle_state == LifecycleState::Resolved {
        debug!(state = %existing.lifecycle_state, "dispute already resolved; idempotent no-op");
        return Ok(());
    }

    {
        let mut guard = ctx.conn.lock().await;
        let tx = match guard.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                error!(
                    dispute_id = %dispute_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: failed to open transaction"
                );
                return Ok(());
            }
        };

        if let Err(e) = tx.execute(
            "UPDATE disputes
             SET lifecycle_state = ?1, last_state_change = ?2
             WHERE dispute_id = ?3",
            rusqlite::params![LifecycleState::Resolved.to_string(), now, dispute_id],
        ) {
            error!(
                dispute_id = %dispute_id,
                event_id = %event_id_hex,
                sql = "UPDATE disputes SET lifecycle_state = ?1, last_state_change = ?2 WHERE dispute_id = ?3",
                error = %e,
                "dispute_resolved: failed to update dispute lifecycle_state"
            );
            return Ok(());
        }
        if let Err(e) = tx.execute(
            "INSERT INTO dispute_state_transitions
                (dispute_id, from_state, to_state, transitioned_at, trigger)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                dispute_id,
                existing.lifecycle_state.to_string(),
                LifecycleState::Resolved.to_string(),
                now,
                "dispute_resolved_externally",
            ],
        ) {
            error!(
                dispute_id = %dispute_id,
                event_id = %event_id_hex,
                sql = "INSERT INTO dispute_state_transitions (dispute_id, from_state, to_state, transitioned_at, trigger) VALUES (?1, ?2, ?3, ?4, ?5)",
                error = %e,
                "dispute_resolved: failed to insert dispute_state_transitions row"
            );
            return Ok(());
        }

        // Look up any live (non-terminal, non-handed-off) session. The
        // helper excludes `escalation_recommended`, so an already
        // escalated session is intentionally a no-op for mediation
        // closure while the dispute lifecycle still moves to `resolved`.
        let open_session = match db::mediation::latest_open_session_for(&tx, &dispute_id) {
            Ok(v) => v,
            Err(e) => {
                error!(
                    dispute_id = %dispute_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: latest_open_session_for lookup failed"
                );
                return Ok(());
            }
        };

        if let Some((session_id, _current_state)) = open_session {
            // Pull the pinned prompt bundle metadata from the session
            // row itself so the audit trail stays attached to the
            // bundle active when the session opened, not whatever
            // happens to be loaded now.
            let (pinned_bundle_id, pinned_policy_hash): (String, String) = match tx.query_row(
                "SELECT prompt_bundle_id, policy_hash
                 FROM mediation_sessions WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            ) {
                Ok(pair) => pair,
                Err(e) => {
                    error!(
                        dispute_id = %dispute_id,
                        session_id = %session_id,
                        event_id = %event_id_hex,
                        error = %e,
                        "dispute_resolved: failed to read pinned bundle for session"
                    );
                    return Ok(());
                }
            };

            let supersede_payload = json!({
                "reason": "dispute_resolved_externally",
                "resolution_status": resolution_status,
                "dispute_id": dispute_id,
                "event_id": event_id_hex,
            })
            .to_string();
            let closed_payload = json!({
                "reason": "dispute_resolved_externally",
                "dispute_id": dispute_id,
            })
            .to_string();

            if let Err(e) = db::mediation::set_session_state(
                &tx,
                &session_id,
                MediationSessionState::SupersededByHuman,
                now,
            ) {
                error!(
                    session_id = %session_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: set_session_state(SupersededByHuman) failed"
                );
                return Ok(());
            }
            if let Err(e) = db::mediation_events::record_event(
                &tx,
                MediationEventKind::SupersededByHuman,
                Some(&session_id),
                &supersede_payload,
                None,
                Some(&pinned_bundle_id),
                Some(&pinned_policy_hash),
                now,
            ) {
                error!(
                    session_id = %session_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: record_event(SupersededByHuman) failed"
                );
                return Ok(());
            }
            if let Err(e) = db::mediation::set_session_state(
                &tx,
                &session_id,
                MediationSessionState::Closed,
                now,
            ) {
                error!(
                    session_id = %session_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: set_session_state(Closed) failed"
                );
                return Ok(());
            }
            if let Err(e) = db::mediation_events::record_event(
                &tx,
                MediationEventKind::SessionClosed,
                Some(&session_id),
                &closed_payload,
                None,
                Some(&pinned_bundle_id),
                Some(&pinned_policy_hash),
                now,
            ) {
                error!(
                    session_id = %session_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: record_event(SessionClosed) failed"
                );
                return Ok(());
            }
            closed_session_id = Some(session_id);
        }

        // Phase 3 follow-up: a session that already reached
        // `summary_delivered` is excluded from `latest_open_session_for`
        // (and from `list_live_sessions`) because Serbero's mediation
        // job for it is finished — but we still want it to land in
        // `closed` once Mostro genuinely resolves the dispute, so the
        // state machine doesn't leave summarized sessions parked at
        // `summary_delivered` forever. The legal `summary_delivered
        // → closed` direct transition is exactly what's needed here
        // (no SupersededByHuman step — that variant is reserved for
        // sessions still in an open state when a human pre-empted
        // mediation). Eligibility was already blocked via the
        // lifecycle move to `Resolved` above; this is purely a
        // state-machine-hygiene close.
        let summarized_session: Option<(String, String, String)> = match tx.query_row(
            "SELECT session_id, prompt_bundle_id, policy_hash
                 FROM mediation_sessions
                 WHERE dispute_id = ?1 AND state = 'summary_delivered'
                 ORDER BY started_at DESC
                 LIMIT 1",
            rusqlite::params![dispute_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        ) {
            Ok(row) => Some(row),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                error!(
                    dispute_id = %dispute_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: lookup of summary_delivered session failed"
                );
                return Ok(());
            }
        };

        if let Some((session_id, pinned_bundle_id, pinned_policy_hash)) = summarized_session {
            let closed_payload = json!({
                "reason": "dispute_resolved_externally",
                "dispute_id": dispute_id,
                "from_state": "summary_delivered",
            })
            .to_string();
            if let Err(e) = db::mediation::set_session_state(
                &tx,
                &session_id,
                MediationSessionState::Closed,
                now,
            ) {
                error!(
                    session_id = %session_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: set_session_state(Closed from summary_delivered) failed"
                );
                return Ok(());
            }
            if let Err(e) = db::mediation_events::record_event(
                &tx,
                MediationEventKind::SessionClosed,
                Some(&session_id),
                &closed_payload,
                None,
                Some(&pinned_bundle_id),
                Some(&pinned_policy_hash),
                now,
            ) {
                error!(
                    session_id = %session_id,
                    event_id = %event_id_hex,
                    error = %e,
                    "dispute_resolved: record_event(SessionClosed from summary_delivered) failed"
                );
                return Ok(());
            }
            // Don't overwrite `closed_session_id` if the open-session
            // branch above already set it — that would be a session
            // table inconsistency (two non-terminal sessions for one
            // dispute), but if it ever happens we want to keep the
            // first close in the structured log.
            if closed_session_id.is_none() {
                closed_session_id = Some(session_id);
            }
        }

        if let Err(e) = tx.commit() {
            error!(
                dispute_id = %dispute_id,
                event_id = %event_id_hex,
                error = %e,
                "dispute_resolved: transaction commit failed"
            );
            return Ok(());
        }
    }

    if let Some(session_id) = closed_session_id.as_deref() {
        info!(
            dispute_id = %dispute_id,
            session_id = %session_id,
            "mediation_session_superseded"
        );
    }

    // FR-124 (T108/T109): the old "no active session → early return"
    // short-circuit misses three shapes Phase 3 routinely produces —
    // sessions already in `escalation_recommended` (terminal for the
    // mediation layer, still relevant for Phase 4), sessions in
    // other terminal states (`closed`, `summary_delivered`), and
    // the FR-122 dispute-scoped handoff path where reasoning ran
    // but no session row was ever committed. `has_any_mediation_context`
    // covers all three. Phase 1/2-only disputes (no context)
    // return here with a debug! line and no FR-124 DM — the Phase
    // 1/2 notifier already handled them.
    let has_context = match report::has_any_mediation_context(&ctx.conn, &dispute_id).await {
        Ok(v) => v,
        Err(e) => {
            warn!(
                dispute_id = %dispute_id,
                error = %e,
                "dispute_resolved: has_any_mediation_context query failed; skipping FR-124 DM"
            );
            return Ok(());
        }
    };
    if !has_context {
        debug!(
            dispute_id = %dispute_id,
            "dispute_resolved: no Phase 3 context for dispute; FR-124 report skipped"
        );
        return Ok(());
    }

    if let Err(e) = report::emit_final_report(
        &ctx.conn,
        &ctx.client,
        &ctx.solvers,
        &dispute_id,
        &resolution_status,
    )
    .await
    {
        warn!(
            dispute_id = %dispute_id,
            error = %e,
            "dispute_resolved: emit_final_report failed"
        );
    }

    Ok(())
}

fn status_tag(event: &Event) -> Option<String> {
    use nostr_sdk::TagKind;
    event
        .tags
        .iter()
        .find(|t| match t.kind() {
            TagKind::SingleLetter(slt) => slt.as_char() == 's',
            _ => false,
        })
        .and_then(|t| t.content().map(|s| s.to_string()))
}
