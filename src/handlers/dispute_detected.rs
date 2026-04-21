use std::str::FromStr;
use std::sync::Arc;

use nostr_sdk::{Event, PublicKey, TagKind};
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument, warn};

use crate::db;
use crate::db::disputes::InsertOutcome;
use crate::error::{Error, Result};
use crate::models::{
    Dispute, DisputeStatus, InitiatorRole, LifecycleState, NotificationStatus, NotificationType,
    SolverConfig,
};
use crate::nostr::send_gift_wrap_notification;

pub struct HandlerContext {
    pub conn: Arc<Mutex<rusqlite::Connection>>,
    pub client: nostr_sdk::Client,
    pub solvers: Vec<SolverConfig>,
    /// Phase 3 runtime. `Some(..)` when the daemon brought up
    /// mediation + reasoning + a prompt bundle; `None` when
    /// Phase 3 is disabled. The event-driven start path
    /// (FR-121) in this handler calls
    /// [`crate::mediation::start::try_start_for`] only when this
    /// field is populated. Phase 1/2 behavior is unaffected by its
    /// absence (SC-105).
    pub phase3: Option<Arc<crate::mediation::Phase3HandlerCtx>>,
}

#[instrument(skip(ctx, event), fields(dispute_id, initiator_role, event_id))]
pub async fn handle(ctx: &HandlerContext, event: &Event) -> Result<()> {
    let dispute_id = event
        .tags
        .identifier()
        .ok_or_else(|| Error::InvalidEvent("missing `d` tag".into()))?
        .to_string();

    let initiator_role_str = find_tag_value(event, "initiator")
        .ok_or_else(|| Error::InvalidEvent("missing `initiator` tag".into()))?;
    let initiator_role = InitiatorRole::from_str(&initiator_role_str)?;

    // Mostro signs the dispute event with its own keys, so the event
    // author IS Mostro's pubkey. The `y` tag carries the platform name
    // (e.g. "mostro"), not the pubkey — do not read it from there.
    let mostro_pubkey = event.pubkey.to_hex();

    tracing::Span::current().record("dispute_id", dispute_id.as_str());
    tracing::Span::current().record("initiator_role", initiator_role_str.as_str());
    tracing::Span::current().record("event_id", event.id.to_string().as_str());

    info!(
        dispute_id = %dispute_id,
        initiator = %initiator_role_str,
        mostro_pubkey = %mostro_pubkey,
        "dispute_detected: processing new dispute event"
    );

    let now = current_timestamp();
    let dispute = Dispute {
        dispute_id: dispute_id.clone(),
        event_id: event.id.to_string(),
        mostro_pubkey,
        initiator_role,
        dispute_status: DisputeStatus::Initiated,
        event_timestamp: event.created_at.as_secs() as i64,
        detected_at: now,
        lifecycle_state: LifecycleState::New,
        assigned_solver: None,
        last_notified_at: None,
        last_state_change: None,
    };

    // Persistence-first policy: if the INSERT fails we deliberately do NOT
    // notify solvers and do NOT queue the event for retry. Per
    // plan.md §Deduplication Strategy and spec.md clarification 3, the
    // dispute may not be notified unless the same event is observed
    // again after persistence recovers (e.g., from a subsequent relay
    // retransmission or operator replay). This preserves dedup integrity
    // at the cost of at-most-once delivery in this failure mode.
    let insert_outcome = {
        let conn = ctx.conn.lock().await;
        match db::disputes::insert_dispute(&conn, &dispute) {
            Ok(outcome) => outcome,
            Err(e) => {
                error!(error = %e, "persistence_failed: skipping notification for this event");
                return Ok(());
            }
        }
    };

    match insert_outcome {
        InsertOutcome::Duplicate => {
            debug!("duplicate_skip: dispute already recorded");
            return Ok(());
        }
        InsertOutcome::Inserted => {
            info!("detected new dispute");
        }
    }

    if ctx.solvers.is_empty() {
        warn!("no solvers configured; dispute persisted but not notified");
        return Ok(());
    }

    let message = build_initial_notification_message(&dispute);

    let mut sent_any = false;
    for solver in &ctx.solvers {
        let pk = match PublicKey::parse(&solver.pubkey) {
            Ok(pk) => pk,
            Err(e) => {
                error!(
                    solver = %solver.pubkey,
                    error = %e,
                    "notification_failed: invalid solver pubkey"
                );
                let conn = ctx.conn.lock().await;
                db::notifications::record_notification_logged(
                    &conn,
                    &dispute.dispute_id,
                    &solver.pubkey,
                    current_timestamp(),
                    NotificationStatus::Failed,
                    Some(&format!("invalid pubkey: {e}")),
                    NotificationType::Initial,
                );
                continue;
            }
        };

        match send_gift_wrap_notification(&ctx.client, &pk, &message).await {
            Ok(()) => {
                sent_any = true;
                info!(solver = %solver.pubkey, "notification_sent");
                let conn = ctx.conn.lock().await;
                db::notifications::record_notification_logged(
                    &conn,
                    &dispute.dispute_id,
                    &solver.pubkey,
                    current_timestamp(),
                    NotificationStatus::Sent,
                    None,
                    NotificationType::Initial,
                );
            }
            Err(e) => {
                error!(solver = %solver.pubkey, error = %e, "notification_failed");
                let conn = ctx.conn.lock().await;
                db::notifications::record_notification_logged(
                    &conn,
                    &dispute.dispute_id,
                    &solver.pubkey,
                    current_timestamp(),
                    NotificationStatus::Failed,
                    Some(&e.to_string()),
                    NotificationType::Initial,
                );
            }
        }
    }

    if sent_any {
        let mut conn = ctx.conn.lock().await;
        if let Err(e) = db::disputes::set_lifecycle_state(
            &mut conn,
            &dispute.dispute_id,
            LifecycleState::Notified,
            Some("initial_notification"),
            current_timestamp(),
        ) {
            warn!(error = %e, "failed to transition to notified");
        }
        if let Err(e) =
            db::disputes::update_last_notified_at(&conn, &dispute.dispute_id, current_timestamp())
        {
            warn!(error = %e, "failed to update last_notified_at");
        }
    }

    // FR-121 event-driven start. Runs only when Phase 3 is
    // configured. Failures here MUST NOT abort the handler — the
    // Phase 1/2 persist + solver-notification path above is already
    // committed at this point, and the start attempt is an
    // independent side effect (SC-105 preserves Phase 1/2 behavior
    // regardless of Phase 3 outcomes).
    if let Some(phase3) = ctx.phase3.as_ref() {
        try_start_mediation(ctx, phase3, &dispute).await;
    } else {
        debug!("phase 3 disabled; skipping event-driven start");
    }

    Ok(())
}

/// Dispatch the FR-121 event-driven start attempt. Wrapped in its
/// own function so the `dispute_detected` handler body stays
/// readable and the outcome logging lives in one place.
async fn try_start_mediation(
    ctx: &HandlerContext,
    phase3: &crate::mediation::Phase3HandlerCtx,
    dispute: &Dispute,
) {
    use crate::mediation::session::OpenOutcome;
    use crate::mediation::start::{self, StartOutcome, StartParams, StartTrigger};

    let dispute_uuid = match uuid::Uuid::parse_str(&dispute.dispute_id) {
        Ok(u) => u,
        Err(e) => {
            // Phase 1/2 tolerates non-UUID dispute ids, but the
            // mostro-core take-flow does not. We log and skip; the
            // dispute stays notified and solver notifications have
            // already gone out.
            debug!(
                error = %e,
                dispute_id = %dispute.dispute_id,
                "skipping event-driven mediation start: dispute id is not a UUID"
            );
            return;
        }
    };

    let open_params = crate::mediation::session::OpenSessionParams {
        conn: &ctx.conn,
        client: &ctx.client,
        serbero_keys: &phase3.serbero_keys,
        mostro_pubkey: &phase3.mostro_pubkey,
        reasoning: phase3.reasoning.as_ref(),
        prompt_bundle: &phase3.prompt_bundle,
        dispute_id: &dispute.dispute_id,
        initiator_role: dispute.initiator_role,
        dispute_uuid,
        take_flow_timeout: crate::mediation::DEFAULT_TAKE_FLOW_TIMEOUT,
        take_flow_poll_interval: crate::mediation::DEFAULT_TAKE_FLOW_POLL_INTERVAL,
        provider_name: &phase3.provider_name,
        model_name: &phase3.model_name,
        auth_handle: &phase3.auth_handle,
        session_key_cache: Some(&phase3.session_key_cache),
        solvers: &phase3.solvers,
    };

    let outcome = start::try_start_for(StartParams {
        open: open_params,
        trigger: StartTrigger::Detected,
    })
    .await;

    match outcome {
        StartOutcome::NotEligible => {
            info!(
                dispute_id = %dispute.dispute_id,
                "event-driven start: dispute not eligible for mediation"
            );
        }
        StartOutcome::Started(OpenOutcome::Opened { session_id }) => {
            info!(
                dispute_id = %dispute.dispute_id,
                session_id = %session_id,
                "event-driven start: mediation session opened"
            );
        }
        StartOutcome::Started(OpenOutcome::AlreadyOpen { session_id }) => {
            info!(
                dispute_id = %dispute.dispute_id,
                session_id = %session_id,
                "event-driven start: dispute already has an open session; no-op"
            );
        }
        StartOutcome::Started(OpenOutcome::ReadyForSummary { session_id, .. }) => {
            // The cooperative-summary completion pass runs on the
            // engine tick. The session is persisted and classified;
            // the summary will be delivered on the next tick cycle.
            info!(
                dispute_id = %dispute.dispute_id,
                session_id = %session_id,
                "event-driven start: session opened in cooperative-summary mode; \
                 engine tick will deliver the summary"
            );
        }
        StartOutcome::Started(OpenOutcome::EscalatedOnOpen { session_id, trigger }) => {
            // Escalation fanout (the session-scoped `escalation_recommended`
            // + `handoff_prepared` audit rows plus the solver DM)
            // runs on the engine tick when it observes the
            // session in `escalation_recommended` state.
            info!(
                dispute_id = %dispute.dispute_id,
                session_id = %session_id,
                trigger = %trigger,
                "event-driven start: session escalated on open; engine tick will fan out"
            );
        }
        StartOutcome::Started(OpenOutcome::RefusedReasoningUnavailable { reason })
        | StartOutcome::Started(OpenOutcome::RefusedAuthPending { reason })
        | StartOutcome::Started(OpenOutcome::RefusedAuthTerminated { reason }) => {
            // Unreachable in practice: the three `Refused*` variants
            // are translated into `StoppedBeforeTake` by
            // `try_start_for`, so they never appear inside `Started`.
            warn!(
                dispute_id = %dispute.dispute_id,
                reason = %reason,
                "event-driven start: open_session refused (unexpected Started arm)"
            );
        }
        StartOutcome::StoppedBeforeTake { reason } => {
            info!(
                dispute_id = %dispute.dispute_id,
                reason = %reason,
                "event-driven start: stopped before take-dispute"
            );
        }
        StartOutcome::TakeFailed { reason } => {
            warn!(
                dispute_id = %dispute.dispute_id,
                reason = %reason,
                "event-driven start: take-dispute failed"
            );
        }
        StartOutcome::Error(e) => {
            warn!(
                dispute_id = %dispute.dispute_id,
                error = %e,
                "event-driven start: unexpected error"
            );
        }
    }
}

pub fn build_initial_notification_message(dispute: &Dispute) -> String {
    format!(
        "New Mostro dispute requires attention.\n\
         dispute_id: {}\n\
         initiator: {}\n\
         event_timestamp: {}\n\
         status: {}",
        dispute.dispute_id, dispute.initiator_role, dispute.event_timestamp, dispute.dispute_status,
    )
}

fn find_tag_value(event: &Event, key: &str) -> Option<String> {
    event
        .tags
        .iter()
        .find(|t| match t.kind() {
            TagKind::Custom(s) => s == key,
            _ => false,
        })
        .and_then(|t| t.content().map(|s| s.to_string()))
}

pub(crate) fn current_timestamp() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
