use rusqlite::{params, Connection};
use tracing::warn;

use crate::error::Result;
use crate::models::{NotificationStatus, NotificationType};

pub fn record_notification(
    conn: &Connection,
    dispute_id: &str,
    solver_pubkey: &str,
    sent_at: i64,
    status: NotificationStatus,
    error_message: Option<&str>,
    notif_type: NotificationType,
) -> Result<()> {
    conn.execute(
        "INSERT INTO notifications
            (dispute_id, solver_pubkey, sent_at, status, error_message, notif_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            dispute_id,
            solver_pubkey,
            sent_at,
            status.to_string(),
            error_message,
            notif_type.to_string(),
        ],
    )?;
    Ok(())
}

/// Record a notification attempt, logging a warning if the DB write fails.
///
/// Notification persistence is best-effort — failing to record an attempt
/// should not abort dispute handling, but it MUST be visible in logs so
/// operators can diagnose notification-table drift.
pub fn record_notification_logged(
    conn: &Connection,
    dispute_id: &str,
    solver_pubkey: &str,
    sent_at: i64,
    status: NotificationStatus,
    error_message: Option<&str>,
    notif_type: NotificationType,
) {
    if let Err(e) = record_notification(
        conn,
        dispute_id,
        solver_pubkey,
        sent_at,
        status,
        error_message,
        notif_type,
    ) {
        warn!(
            error = %e,
            dispute_id = dispute_id,
            solver_pubkey = solver_pubkey,
            status = %status,
            notif_type = %notif_type,
            "failed to record notification row"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::disputes::insert_dispute;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::models::{Dispute, DisputeStatus, InitiatorRole, LifecycleState};

    fn seed_dispute(conn: &Connection) {
        let d = Dispute {
            dispute_id: "d1".into(),
            event_id: "e1".into(),
            mostro_pubkey: "m1".into(),
            initiator_role: InitiatorRole::Buyer,
            dispute_status: DisputeStatus::Initiated,
            event_timestamp: 0,
            detected_at: 0,
            lifecycle_state: LifecycleState::New,
            assigned_solver: None,
            last_notified_at: None,
            last_state_change: None,
        };
        insert_dispute(conn, &d).unwrap();
    }

    #[test]
    fn records_sent_and_failed_rows() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        seed_dispute(&conn);

        record_notification(
            &conn,
            "d1",
            "solver_pk_1",
            100,
            NotificationStatus::Sent,
            None,
            NotificationType::Initial,
        )
        .unwrap();
        record_notification(
            &conn,
            "d1",
            "solver_pk_2",
            101,
            NotificationStatus::Failed,
            Some("relay rejected"),
            NotificationType::Initial,
        )
        .unwrap();

        let sent_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE status = 'sent'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let failed_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE status = 'failed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sent_count, 1);
        assert_eq!(failed_count, 1);
    }

    #[test]
    fn default_notif_type_is_initial() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        seed_dispute(&conn);
        conn.execute(
            "INSERT INTO notifications (dispute_id, solver_pubkey, sent_at, status)
             VALUES (?1, ?2, ?3, ?4)",
            params!["d1", "s1", 1, "sent"],
        )
        .unwrap();
        let t: String = conn
            .query_row("SELECT notif_type FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(t, "initial");
    }

    #[test]
    fn fk_rejects_orphan_notification() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        // No dispute seeded — FK violation expected
        let res = record_notification(
            &conn,
            "no_such_dispute",
            "s1",
            1,
            NotificationStatus::Sent,
            None,
            NotificationType::Initial,
        );
        assert!(res.is_err(), "expected FK violation");
    }
}
