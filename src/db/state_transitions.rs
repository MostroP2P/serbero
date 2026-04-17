use std::str::FromStr;

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::models::{Dispute, DisputeStatus, InitiatorRole, LifecycleState};

pub fn list_unattended_disputes(conn: &Connection, cutoff_ts: i64) -> Result<Vec<Dispute>> {
    let mut stmt = conn.prepare(
        "SELECT dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at,
                lifecycle_state, assigned_solver, last_notified_at, last_state_change
         FROM disputes
         WHERE lifecycle_state = 'notified'
           AND last_notified_at IS NOT NULL
           AND last_notified_at < ?1",
    )?;
    let rows = stmt.query_map(params![cutoff_ts], |row| {
        let initiator_role_str: String = row.get(3)?;
        let dispute_status_str: String = row.get(4)?;
        let lifecycle_state_str: String = row.get(7)?;
        let invalid = |field: &str, val: &str| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid {field}: {val}"),
                )),
            )
        };
        Ok(Dispute {
            dispute_id: row.get(0)?,
            event_id: row.get(1)?,
            mostro_pubkey: row.get(2)?,
            initiator_role: InitiatorRole::from_str(&initiator_role_str)
                .map_err(|_| invalid("initiator_role", &initiator_role_str))?,
            dispute_status: DisputeStatus::from_str(&dispute_status_str)
                .map_err(|_| invalid("dispute_status", &dispute_status_str))?,
            event_timestamp: row.get(5)?,
            detected_at: row.get(6)?,
            lifecycle_state: LifecycleState::from_str(&lifecycle_state_str)
                .map_err(|_| invalid("lifecycle_state", &lifecycle_state_str))?,
            assigned_solver: row.get(8)?,
            last_notified_at: row.get(9)?,
            last_state_change: row.get(10)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::disputes::{insert_dispute, set_lifecycle_state, update_last_notified_at};
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    fn dispute(id: &str) -> Dispute {
        Dispute {
            dispute_id: id.into(),
            event_id: format!("e_{id}"),
            mostro_pubkey: "m".into(),
            initiator_role: InitiatorRole::Buyer,
            dispute_status: DisputeStatus::Initiated,
            event_timestamp: 0,
            detected_at: 0,
            lifecycle_state: LifecycleState::New,
            assigned_solver: None,
            last_notified_at: None,
            last_state_change: None,
        }
    }

    #[test]
    fn returns_only_notified_disputes_past_cutoff() {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();

        insert_dispute(&conn, &dispute("d_old")).unwrap();
        set_lifecycle_state(&mut conn, "d_old", LifecycleState::Notified, Some("t"), 50).unwrap();
        update_last_notified_at(&conn, "d_old", 50).unwrap();

        insert_dispute(&conn, &dispute("d_fresh")).unwrap();
        set_lifecycle_state(
            &mut conn,
            "d_fresh",
            LifecycleState::Notified,
            Some("t"),
            200,
        )
        .unwrap();
        update_last_notified_at(&conn, "d_fresh", 200).unwrap();

        insert_dispute(&conn, &dispute("d_taken")).unwrap();
        set_lifecycle_state(
            &mut conn,
            "d_taken",
            LifecycleState::Notified,
            Some("t"),
            40,
        )
        .unwrap();
        update_last_notified_at(&conn, "d_taken", 40).unwrap();
        set_lifecycle_state(&mut conn, "d_taken", LifecycleState::Taken, Some("t"), 60).unwrap();

        let unattended = list_unattended_disputes(&conn, 150).unwrap();
        let ids: Vec<_> = unattended.iter().map(|d| d.dispute_id.as_str()).collect();
        assert_eq!(ids, vec!["d_old"]);
    }
}
