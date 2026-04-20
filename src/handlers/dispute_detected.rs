use std::str::FromStr;
use std::sync::Arc;

use nostr_sdk::{Event, PublicKey, TagKind};
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::db;
use crate::db::disputes::InsertOutcome;
use crate::error::Result;
use crate::mediation;
use crate::models::{
    Dispute, DisputeStatus, InitiatorRole, LifecycleState, NotificationStatus, NotificationType,
    SolverConfig,
};
use crate::nostr::send_gift_wrap_notification;

/// Subset of Phase 3 runtime state needed by the event-driven
/// mediation trigger. Kept separate from the full `Phase3Runtime`
/// in `daemon.rs` to avoid circular imports. All `Arc` fields are
/// cheap to clone into a spawned background task.
#[derive(Clone)]
pub struct Phase3Runtime {
    pub client: Arc<nostr_sdk::Client>,
    pub serbero_keys: Arc<nostr_sdk::Keys>,
    pub mostro_pubkey: Arc<nostr_sdk::PublicKey>,
    pub reasoning: Arc<dyn crate::reasoning::ReasoningProvider>,
    pub prompt_bundle: Arc<crate::prompts::PromptBundle>,
    pub auth_handle: crate::mediation::auth_retry::AuthRetryHandle,
    pub solvers: Vec<SolverConfig>,
    pub provider_name: String,
    pub model_name: String,
}

pub struct HandlerContext {
    pub conn: Arc<Mutex<rusqlite::Connection>>,
    pub client: nostr_sdk::Client,
    pub solvers: Vec<SolverConfig>,
    /// Phase 3 runtime for event-driven mediation triggering.
    /// `None` when Phase 3 is not configured or not yet available.
    /// Cloned cheaply into background tasks on every `notified`
    /// transition so the handler never blocks.
    pub phase3_runtime: Arc<Option<Phase3Runtime>>,
}

#[instrument(skip(ctx, event), fields(dispute_id, initiator_role, event_id))]
pub async fn handle(ctx: &HandlerContext, event: &Event) -> Result<()> {
    let dispute_id = event
        .tags
        .identifier()
        .ok_or_else(|| crate::error::Error::InvalidEvent("missing `d` tag".into()))?
        .to_string();

    let initiator_role_str = find_tag_value(event, "initiator")
        .ok_or_else(|| crate::error::Error::InvalidEvent("missing `initiator` tag".into()))?;
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
        drop(conn);

        // Event-driven Phase 3 trigger (Issue #20 / spec fix A):
        // Immediately attempt to open a mediation session when the
        // dispute transitions to `notified`. This replaces the old
        // behaviour where Phase 3 only reacted to disputes via the
        // periodic polling loop (every 30 s), which meant disputes
        // that resolved quickly were never mediated.
        //
        // The polling loop (`run_engine_tick`) continues as a belt-
        // and-suspenders fallback and as the mechanism for re-
        // evaluating open sessions — this immediate call does NOT
        // replace it.
        try_immediate_mediation(ctx, &dispute.dispute_id, initiator_role).await;
    }

    Ok(())
}

/// Spawn a background task that attempts to open a mediation session
/// for `dispute_id` immediately (event-driven path).
///
/// Returns immediately; errors are logged inside the spawned task.
/// The task holds cheap clones of all `Arc` fields, so it is safe
/// to drop `ctx` after this call returns.
///
/// Does nothing when `ctx.phase3_runtime` is `None` (Phase 3 not
/// configured or reasoning unavailable — SC-105 keeps Phase 1/2
/// detection fully operational regardless).
async fn try_immediate_mediation(
    ctx: &HandlerContext,
    dispute_id: &str,
    initiator_role: InitiatorRole,
) {
    let Some(phase3) = ctx.phase3_runtime.as_ref().as_ref() else {
        // Phase 3 not configured — nothing to do.
        return;
    };

    // dispute_id from Mostro is a valid UUID. Skip if malformed —
    // the polling loop skips it anyway and it should never happen
    // in practice.
    let dispute_uuid = match Uuid::parse_str(dispute_id) {
        Ok(u) => u,
        Err(e) => {
            warn!(
                dispute_id = %dispute_id,
                error = %e,
                "event-driven mediation: dispute_id is not a valid UUID; skipping"
            );
            return;
        }
    };

    // Clone everything the background task needs. Each Arc field is
    // cheap to clone; the inner types (Client, Keys, etc.) are
    // backed by Arc internally in nostr-sdk.
    let conn = Arc::clone(&ctx.conn);
    let client = Arc::clone(&phase3.client);
    let serbero_keys = Arc::clone(&phase3.serbero_keys);
    let mostro_pubkey = Arc::clone(&phase3.mostro_pubkey);
    let reasoning = Arc::clone(&phase3.reasoning);
    let prompt_bundle = Arc::clone(&phase3.prompt_bundle);
    let auth_handle = phase3.auth_handle.clone();
    let solvers = phase3.solvers.clone();
    let provider_name = phase3.provider_name.clone();
    let model_name = phase3.model_name.clone();
    let dispute_id_owned = dispute_id.to_string();

    tokio::spawn(async move {
        let outcome = mediation::open_dispute_session(
            &conn,
            &client,
            &serbero_keys,
            &mostro_pubkey,
            reasoning.as_ref(),
            &prompt_bundle,
            &dispute_id_owned,
            initiator_role,
            dispute_uuid,
            &provider_name,
            &model_name,
            &auth_handle,
        )
        .await;

        match outcome {
            Ok(mediation::session::OpenOutcome::Opened { session_id }) => {
                info!(
                    dispute_id = %dispute_id_owned,
                    session_id = %session_id,
                    "event-driven: opened mediation session immediately"
                );
            }
            Ok(mediation::session::OpenOutcome::ReadyForSummary {
                session_id,
                classification,
                confidence,
            }) => {
                info!(
                    dispute_id = %dispute_id_owned,
                    session_id = %session_id,
                    classification = %classification,
                    confidence,
                    "event-driven: session opened; delivering cooperative summary immediately"
                );
                if let Err(e) = mediation::deliver_summary(
                    &conn,
                    &client,
                    &serbero_keys,
                    &session_id,
                    &dispute_id_owned,
                    classification,
                    confidence,
                    Vec::new(),
                    &prompt_bundle,
                    reasoning.as_ref(),
                    &solvers,
                    &provider_name,
                    &model_name,
                )
                .await
                {
                    error!(
                        dispute_id = %dispute_id_owned,
                        session_id = %session_id,
                        error = %e,
                        "event-driven: deliver_summary failed"
                    );
                }
            }
            Ok(mediation::session::OpenOutcome::AlreadyOpen { session_id }) => {
                debug!(
                    dispute_id = %dispute_id_owned,
                    session_id = %session_id,
                    "event-driven: dispute already has an open session (concurrent or race with polling loop)"
                );
            }
            Ok(mediation::session::OpenOutcome::EscalatedOnOpen {
                session_id,
                trigger,
            }) => {
                warn!(
                    dispute_id = %dispute_id_owned,
                    session_id = %session_id,
                    trigger = %trigger,
                    "event-driven: session escalated on open"
                );
                if let Err(e) =
                    mediation::escalation::recommend(mediation::escalation::RecommendParams {
                        conn: &conn,
                        session_id: &session_id,
                        trigger,
                        evidence_refs: Vec::new(),
                        rationale_refs: Vec::new(),
                        prompt_bundle_id: &prompt_bundle.id,
                        policy_hash: &prompt_bundle.policy_hash,
                    })
                    .await
                {
                    error!(
                        dispute_id = %dispute_id_owned,
                        session_id = %session_id,
                        error = %e,
                        "event-driven: escalation::recommend failed"
                    );
                } else {
                    mediation::notify_solvers_escalation(
                        &conn,
                        &client,
                        &solvers,
                        &dispute_id_owned,
                        &session_id,
                        trigger,
                    )
                    .await;
                }
            }
            Ok(mediation::session::OpenOutcome::RefusedReasoningUnavailable { reason }) => {
                warn!(
                    dispute_id = %dispute_id_owned,
                    reason = %reason,
                    "event-driven: reasoning provider unavailable; skipping (SC-105)"
                );
            }
            Ok(mediation::session::OpenOutcome::RefusedAuthPending { reason }) => {
                warn!(
                    dispute_id = %dispute_id_owned,
                    reason = %reason,
                    "event-driven: auth pending; skipping dispute"
                );
            }
            Ok(mediation::session::OpenOutcome::RefusedAuthTerminated { reason }) => {
                error!(
                    dispute_id = %dispute_id_owned,
                    reason = %reason,
                    "event-driven: auth terminated; skipping dispute (SC-105)"
                );
            }
            Err(e) => {
                error!(
                    dispute_id = %dispute_id_owned,
                    error = %e,
                    "event-driven: open_dispute_session failed"
                );
            }
        }
    });
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
