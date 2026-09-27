//! The `disputes` table: one row per Mostro dispute and its notification
//! lifecycle (`docs/spec.md` §6).

use std::fmt;
use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initiator {
    Buyer,
    Seller,
    /// Mostro publishes `unknown` when its own dispute flags disagree.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    New,
    Notified,
    Taken,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispute {
    pub dispute_id: String,
    pub initiator: Initiator,
    pub status: String,
    pub status_at: i64,
    pub lifecycle: Lifecycle,
    pub assigned_solver: Option<String>,
    pub first_seen_at: i64,
    pub last_notified_at: Option<i64>,
    pub updated_at: i64,
}

/// A dispute as first observed in a dispute event.
#[derive(Debug, Clone)]
pub struct NewDispute<'a> {
    pub dispute_id: &'a str,
    pub initiator: Initiator,
    pub status: &'a str,
    /// `created_at` of the dispute event this row comes from.
    pub status_at: i64,
    pub now: i64,
}

/// Inserts a dispute in lifecycle `new` unless it already exists. Returns
/// `true` if a row was inserted, `false` for a duplicate.
pub fn insert_if_new(conn: &Connection, dispute: &NewDispute<'_>) -> Result<bool> {
    let inserted = conn.execute(
        "INSERT INTO disputes
             (dispute_id, initiator, status, status_at, lifecycle, first_seen_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'new', ?5, ?5)
         ON CONFLICT (dispute_id) DO NOTHING",
        params![
            dispute.dispute_id,
            dispute.initiator.to_string(),
            dispute.status,
            dispute.status_at,
            dispute.now,
        ],
    )?;
    Ok(inserted == 1)
}

pub fn get(conn: &Connection, dispute_id: &str) -> Result<Option<Dispute>> {
    conn.query_row(
        "SELECT dispute_id, initiator, status, status_at, lifecycle, assigned_solver,
                first_seen_at, last_notified_at, updated_at
         FROM disputes WHERE dispute_id = ?1",
        [dispute_id],
        from_row,
    )
    .optional()?
    .transpose()
}

/// Result of applying a dispute event revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusUpdate {
    Applied,
    /// The stored revision is as new or newer; relays may deliver
    /// revisions out of order.
    Stale,
    NotFound,
}

/// Records a dispute event revision if it is newer than the stored one.
pub fn apply_status(
    conn: &Connection,
    dispute_id: &str,
    status: &str,
    status_at: i64,
    now: i64,
) -> Result<StatusUpdate> {
    let updated = conn.execute(
        "UPDATE disputes SET status = ?2, status_at = ?3, updated_at = ?4
         WHERE dispute_id = ?1 AND status_at < ?3",
        params![dispute_id, status, status_at, now],
    )?;
    if updated == 1 {
        return Ok(StatusUpdate::Applied);
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM disputes WHERE dispute_id = ?1)",
        [dispute_id],
        |row| row.get(0),
    )?;
    Ok(if exists {
        StatusUpdate::Stale
    } else {
        StatusUpdate::NotFound
    })
}

pub fn set_lifecycle(
    conn: &Connection,
    dispute_id: &str,
    lifecycle: Lifecycle,
    now: i64,
) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE disputes SET lifecycle = ?2, updated_at = ?3 WHERE dispute_id = ?1",
        params![dispute_id, lifecycle.to_string(), now],
    )?;
    Ok(updated == 1)
}

/// Moves a dispute waiting for a solver (`new` or `notified`) to `notified`
/// and stamps the notification time. A dispute already taken or resolved is
/// left alone, so a notification that finishes late cannot regress it.
pub fn mark_notified(conn: &Connection, dispute_id: &str, now: i64) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE disputes SET lifecycle = 'notified', last_notified_at = ?2, updated_at = ?2
         WHERE dispute_id = ?1 AND lifecycle IN ('new', 'notified')",
        params![dispute_id, now],
    )?;
    Ok(updated == 1)
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Result<Dispute>> {
    let initiator: String = row.get(1)?;
    let lifecycle: String = row.get(4)?;
    let initiator = match initiator.parse() {
        Ok(v) => v,
        Err(e) => return Ok(Err(e)),
    };
    let lifecycle = match lifecycle.parse() {
        Ok(v) => v,
        Err(e) => return Ok(Err(e)),
    };
    Ok(Ok(Dispute {
        dispute_id: row.get(0)?,
        initiator,
        status: row.get(2)?,
        status_at: row.get(3)?,
        lifecycle,
        assigned_solver: row.get(5)?,
        first_seen_at: row.get(6)?,
        last_notified_at: row.get(7)?,
        updated_at: row.get(8)?,
    }))
}

impl fmt::Display for Initiator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Buyer => "buyer",
            Self::Seller => "seller",
            Self::Unknown => "unknown",
        })
    }
}

impl FromStr for Initiator {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "buyer" => Ok(Self::Buyer),
            "seller" => Ok(Self::Seller),
            "unknown" => Ok(Self::Unknown),
            other => Err(Error::Schema(format!("unknown initiator {other:?}"))),
        }
    }
}

impl fmt::Display for Lifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::New => "new",
            Self::Notified => "notified",
            Self::Taken => "taken",
            Self::Resolved => "resolved",
        })
    }
}

impl FromStr for Lifecycle {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "new" => Ok(Self::New),
            "notified" => Ok(Self::Notified),
            "taken" => Ok(Self::Taken),
            "resolved" => Ok(Self::Resolved),
            other => Err(Error::Schema(format!("unknown lifecycle {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn new_dispute(id: &str, status_at: i64) -> NewDispute<'_> {
        NewDispute {
            dispute_id: id,
            initiator: Initiator::Seller,
            status: "initiated",
            status_at,
            now: 1_000,
        }
    }

    #[test]
    fn inserts_new_dispute_in_lifecycle_new() {
        let store = Store::open_in_memory().unwrap();

        let inserted = insert_if_new(store.conn(), &new_dispute("d1", 900)).unwrap();

        assert!(inserted);
        let dispute = get(store.conn(), "d1").unwrap().unwrap();
        assert_eq!(dispute.lifecycle, Lifecycle::New);
        assert_eq!(dispute.initiator, Initiator::Seller);
        assert_eq!(dispute.status, "initiated");
        assert_eq!(dispute.first_seen_at, 1_000);
        assert_eq!(dispute.last_notified_at, None);
    }

    #[test]
    fn duplicate_insert_is_ignored() {
        let store = Store::open_in_memory().unwrap();
        insert_if_new(store.conn(), &new_dispute("d1", 900)).unwrap();

        let inserted = insert_if_new(store.conn(), &new_dispute("d1", 950)).unwrap();

        assert!(!inserted);
        assert_eq!(get(store.conn(), "d1").unwrap().unwrap().status_at, 900);
    }

    #[test]
    fn missing_dispute_is_none() {
        let store = Store::open_in_memory().unwrap();

        assert_eq!(get(store.conn(), "nope").unwrap(), None);
    }

    #[test]
    fn newer_revision_updates_status() {
        let store = Store::open_in_memory().unwrap();
        insert_if_new(store.conn(), &new_dispute("d1", 900)).unwrap();

        let update = apply_status(store.conn(), "d1", "in-progress", 950, 1_100).unwrap();

        assert_eq!(update, StatusUpdate::Applied);
        let dispute = get(store.conn(), "d1").unwrap().unwrap();
        assert_eq!(dispute.status, "in-progress");
        assert_eq!(dispute.status_at, 950);
        assert_eq!(dispute.updated_at, 1_100);
    }

    #[test]
    fn older_or_equal_revision_is_ignored() {
        let store = Store::open_in_memory().unwrap();
        insert_if_new(store.conn(), &new_dispute("d1", 900)).unwrap();
        apply_status(store.conn(), "d1", "settled", 960, 1_100).unwrap();

        let older = apply_status(store.conn(), "d1", "in-progress", 950, 1_200).unwrap();
        let equal = apply_status(store.conn(), "d1", "in-progress", 960, 1_200).unwrap();

        assert_eq!(older, StatusUpdate::Stale);
        assert_eq!(equal, StatusUpdate::Stale);
        assert_eq!(get(store.conn(), "d1").unwrap().unwrap().status, "settled");
    }

    #[test]
    fn revision_for_unknown_dispute_is_not_found() {
        let store = Store::open_in_memory().unwrap();

        let update = apply_status(store.conn(), "nope", "settled", 950, 1_100).unwrap();

        assert_eq!(update, StatusUpdate::NotFound);
    }

    #[test]
    fn mark_notified_sets_lifecycle_and_time() {
        let store = Store::open_in_memory().unwrap();
        insert_if_new(store.conn(), &new_dispute("d1", 900)).unwrap();

        mark_notified(store.conn(), "d1", 1_050).unwrap();

        let dispute = get(store.conn(), "d1").unwrap().unwrap();
        assert_eq!(dispute.lifecycle, Lifecycle::Notified);
        assert_eq!(dispute.last_notified_at, Some(1_050));
    }

    #[test]
    fn mark_notified_never_regresses_a_taken_or_resolved_dispute() {
        let store = Store::open_in_memory().unwrap();
        for (id, lifecycle) in [
            ("taken", Lifecycle::Taken),
            ("resolved", Lifecycle::Resolved),
        ] {
            insert_if_new(store.conn(), &new_dispute(id, 900)).unwrap();
            set_lifecycle(store.conn(), id, lifecycle, 1_000).unwrap();

            let updated = mark_notified(store.conn(), id, 1_100).unwrap();

            assert!(!updated);
            let dispute = get(store.conn(), id).unwrap().unwrap();
            assert_eq!(dispute.lifecycle, lifecycle);
            assert_eq!(dispute.last_notified_at, None);
        }
    }

    #[test]
    fn set_lifecycle_reports_missing_rows() {
        let store = Store::open_in_memory().unwrap();
        insert_if_new(store.conn(), &new_dispute("d1", 900)).unwrap();

        let found = set_lifecycle(store.conn(), "d1", Lifecycle::Taken, 1_200).unwrap();
        let missing = set_lifecycle(store.conn(), "d2", Lifecycle::Taken, 1_200).unwrap();

        assert!(found && !missing);
        assert_eq!(
            get(store.conn(), "d1").unwrap().unwrap().lifecycle,
            Lifecycle::Taken
        );
    }

    #[test]
    fn database_rejects_unknown_lifecycle_values() {
        let store = Store::open_in_memory().unwrap();
        insert_if_new(store.conn(), &new_dispute("d1", 900)).unwrap();

        let result = store.conn().execute(
            "UPDATE disputes SET lifecycle = 'bogus' WHERE dispute_id = 'd1'",
            [],
        );

        assert!(result.is_err());
    }

    #[test]
    fn enums_round_trip_through_text() {
        for lifecycle in [
            Lifecycle::New,
            Lifecycle::Notified,
            Lifecycle::Taken,
            Lifecycle::Resolved,
        ] {
            assert_eq!(
                lifecycle.to_string().parse::<Lifecycle>().unwrap(),
                lifecycle
            );
        }
        for initiator in [Initiator::Buyer, Initiator::Seller, Initiator::Unknown] {
            assert_eq!(
                initiator.to_string().parse::<Initiator>().unwrap(),
                initiator
            );
        }
    }
}
