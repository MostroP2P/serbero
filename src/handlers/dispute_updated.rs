use nostr_sdk::{Event, PublicKey, TagKind};
use tracing::{debug, error, info, instrument, warn};

use crate::db;
use crate::error::{Error, Result};
use crate::handlers::dispute_detected::{current_timestamp, HandlerContext};
use crate::models::{LifecycleState, NotificationStatus, NotificationType};
use crate::nostr::send_gift_wrap_notification;

#[instrument(skip(ctx, event), fields(dispute_id, event_id))]
pub async fn handle(ctx: &HandlerContext, event: &Event) -> Result<()> {
    let dispute_id = event
        .tags
        .identifier()
        .ok_or_else(|| Error::InvalidEvent("missing `d` tag".into()))?
        .to_string();

    tracing::Span::current().record("dispute_id", dispute_id.as_str());
    tracing::Span::current().record("event_id", event.id.to_string().as_str());

    let existing = {
        let conn = ctx.conn.lock().await;
        db::disputes::get_dispute(&conn, &dispute_id)?
    };
    let Some(existing) = existing else {
        debug!("in-progress event for unknown dispute; ignoring");
        return Ok(());
    };

    if matches!(
        existing.lifecycle_state,
        LifecycleState::Taken
            | LifecycleState::Waiting
            | LifecycleState::Escalated
            | LifecycleState::Resolved
    ) {
        debug!(state = %existing.lifecycle_state, "already past notified; idempotent no-op");
        return Ok(());
    }

    let solver_pubkey = extract_assigned_solver(event);

    {
        let mut conn = ctx.conn.lock().await;
        db::disputes::set_lifecycle_state(
            &mut conn,
            &dispute_id,
            LifecycleState::Taken,
            Some(&event.id.to_string()),
            current_timestamp(),
        )?;
        if let Some(ref pk) = solver_pubkey {
            db::disputes::set_assigned_solver(&conn, &dispute_id, pk)?;
        }
    }

    info!(
        assigned_solver = solver_pubkey.as_deref().unwrap_or("unknown"),
        "assignment_detected"
    );

    let message = format!(
        "Mostro dispute has been taken.\n\
         dispute_id: {}\n\
         assigned_solver: {}\n\
         lifecycle_state: taken",
        dispute_id,
        solver_pubkey.as_deref().unwrap_or("unknown"),
    );

    for solver in &ctx.solvers {
        let pk = match PublicKey::parse(&solver.pubkey) {
            Ok(pk) => pk,
            Err(e) => {
                error!(solver = %solver.pubkey, error = %e, "assignment_notification_failed: invalid pubkey");
                let conn = ctx.conn.lock().await;
                db::notifications::record_notification_logged(
                    &conn,
                    &dispute_id,
                    &solver.pubkey,
                    current_timestamp(),
                    NotificationStatus::Failed,
                    Some(&format!("invalid pubkey: {e}")),
                    NotificationType::Assignment,
                );
                continue;
            }
        };

        match send_gift_wrap_notification(&ctx.client, &pk, &message).await {
            Ok(()) => {
                info!(solver = %solver.pubkey, "assignment_notification_sent");
                let conn = ctx.conn.lock().await;
                db::notifications::record_notification_logged(
                    &conn,
                    &dispute_id,
                    &solver.pubkey,
                    current_timestamp(),
                    NotificationStatus::Sent,
                    None,
                    NotificationType::Assignment,
                );
            }
            Err(e) => {
                warn!(solver = %solver.pubkey, error = %e, "assignment_notification_failed");
                let conn = ctx.conn.lock().await;
                db::notifications::record_notification_logged(
                    &conn,
                    &dispute_id,
                    &solver.pubkey,
                    current_timestamp(),
                    NotificationStatus::Failed,
                    Some(&e.to_string()),
                    NotificationType::Assignment,
                );
            }
        }
    }

    Ok(())
}

fn extract_assigned_solver(event: &Event) -> Option<String> {
    // NIP-01 single-letter tags are case-sensitive — only match lowercase `p`.
    event
        .tags
        .iter()
        .find(|t| match t.kind() {
            TagKind::SingleLetter(slt) => slt.as_char() == 'p',
            _ => false,
        })
        .and_then(|t| t.content().map(|s| s.to_string()))
}
