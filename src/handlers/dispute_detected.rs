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

    Ok(())
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
