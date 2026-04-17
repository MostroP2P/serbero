use std::str::FromStr;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::models::{Dispute, DisputeStatus, InitiatorRole, LifecycleState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    Duplicate,
}

pub fn insert_dispute(conn: &Connection, dispute: &Dispute) -> Result<InsertOutcome> {
    let changed = conn.execute(
        "INSERT INTO disputes (
            dispute_id, event_id, mostro_pubkey, initiator_role,
            dispute_status, event_timestamp, detected_at,
            lifecycle_state, assigned_solver, last_notified_at, last_state_change
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(dispute_id) DO NOTHING",
        params![
            dispute.dispute_id,
            dispute.event_id,
            dispute.mostro_pubkey,
            dispute.initiator_role.to_string(),
            dispute.dispute_status.to_string(),
            dispute.event_timestamp,
            dispute.detected_at,
            dispute.lifecycle_state.to_string(),
            dispute.assigned_solver,
            dispute.last_notified_at,
            dispute.last_state_change,
        ],
    )?;
    if changed == 0 {
        Ok(InsertOutcome::Duplicate)
    } else {
        Ok(InsertOutcome::Inserted)
    }
}

pub fn get_dispute(conn: &Connection, dispute_id: &str) -> Result<Option<Dispute>> {
    conn.query_row(
        "SELECT dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at,
                lifecycle_state, assigned_solver, last_notified_at, last_state_change
         FROM disputes WHERE dispute_id = ?1",
        params![dispute_id],
        row_to_dispute,
    )
    .optional()
    .map_err(Error::from)
}

pub fn set_lifecycle_state(
    conn: &mut Connection,
    dispute_id: &str,
    new_state: LifecycleState,
    trigger: Option<&str>,
    now_ts: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    let current: Option<String> = tx
        .query_row(
            "SELECT lifecycle_state FROM disputes WHERE dispute_id = ?1",
            params![dispute_id],
            |row| row.get(0),
        )
        .optional()?;
    let from = match current {
        Some(s) => LifecycleState::from_str(&s)?,
        None => {
            return Err(Error::InvalidEvent(format!(
                "dispute {dispute_id} not found for state transition"
            )))
        }
    };
    if from != new_state && !from.can_transition_to(new_state) {
        return Err(Error::InvalidStateTransition {
            from: from.to_string(),
            to: new_state.to_string(),
        });
    }

    tx.execute(
        "UPDATE disputes SET lifecycle_state = ?1, last_state_change = ?2
         WHERE dispute_id = ?3",
        params![new_state.to_string(), now_ts, dispute_id],
    )?;
    tx.execute(
        "INSERT INTO dispute_state_transitions
            (dispute_id, from_state, to_state, transitioned_at, trigger)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            dispute_id,
            from.to_string(),
            new_state.to_string(),
            now_ts,
            trigger,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_assigned_solver(conn: &Connection, dispute_id: &str, solver_pubkey: &str) -> Result<()> {
    conn.execute(
        "UPDATE disputes SET assigned_solver = ?1 WHERE dispute_id = ?2",
        params![solver_pubkey, dispute_id],
    )?;
    Ok(())
}

pub fn update_last_notified_at(conn: &Connection, dispute_id: &str, ts: i64) -> Result<()> {
    conn.execute(
        "UPDATE disputes SET last_notified_at = ?1 WHERE dispute_id = ?2",
        params![ts, dispute_id],
    )?;
    Ok(())
}

fn row_to_dispute(row: &rusqlite::Row<'_>) -> rusqlite::Result<Dispute> {
    let initiator_role_str: String = row.get(3)?;
    let dispute_status_str: String = row.get(4)?;
    let lifecycle_state_str: String = row.get(7)?;
    let parse_err = |field: &str, val: &str| {
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
            .map_err(|_| parse_err("initiator_role", &initiator_role_str))?,
        dispute_status: DisputeStatus::from_str(&dispute_status_str)
            .map_err(|_| parse_err("dispute_status", &dispute_status_str))?,
        event_timestamp: row.get(5)?,
        detected_at: row.get(6)?,
        lifecycle_state: LifecycleState::from_str(&lifecycle_state_str)
            .map_err(|_| parse_err("lifecycle_state", &lifecycle_state_str))?,
        assigned_solver: row.get(8)?,
        last_notified_at: row.get(9)?,
        last_state_change: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    fn sample_dispute(id: &str, event_id: &str) -> Dispute {
        Dispute {
            dispute_id: id.to_string(),
            event_id: event_id.to_string(),
            mostro_pubkey: "mostro_pk".to_string(),
            initiator_role: InitiatorRole::Buyer,
            dispute_status: DisputeStatus::Initiated,
            event_timestamp: 1_700_000_000,
            detected_at: 1_700_000_010,
            lifecycle_state: LifecycleState::New,
            assigned_solver: None,
            last_notified_at: None,
            last_state_change: None,
        }
    }

    fn setup() -> Connection {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    #[test]
    fn insert_and_fetch_dispute() {
        let conn = setup();
        let d = sample_dispute("d1", "e1");
        assert_eq!(insert_dispute(&conn, &d).unwrap(), InsertOutcome::Inserted);
        let fetched = get_dispute(&conn, "d1").unwrap().unwrap();
        assert_eq!(fetched.dispute_id, "d1");
        assert_eq!(fetched.initiator_role, InitiatorRole::Buyer);
    }

    #[test]
    fn duplicate_dispute_is_noop() {
        let conn = setup();
        let d = sample_dispute("d1", "e1");
        assert_eq!(insert_dispute(&conn, &d).unwrap(), InsertOutcome::Inserted);
        let d2 = sample_dispute("d1", "e2");
        assert_eq!(
            insert_dispute(&conn, &d2).unwrap(),
            InsertOutcome::Duplicate
        );
    }

    #[test]
    fn transition_records_history_and_validates() {
        let mut conn = setup();
        let d = sample_dispute("d1", "e1");
        insert_dispute(&conn, &d).unwrap();
        set_lifecycle_state(&mut conn, "d1", LifecycleState::Notified, Some("t1"), 100).unwrap();
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispute_state_transitions WHERE dispute_id='d1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);

        let err = set_lifecycle_state(&mut conn, "d1", LifecycleState::New, Some("bad"), 101)
            .unwrap_err();
        assert!(matches!(err, Error::InvalidStateTransition { .. }));
    }
}
