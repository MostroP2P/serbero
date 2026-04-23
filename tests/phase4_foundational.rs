//! Phase 4 — T010 Foundational smoke test.
//!
//! Spawns the daemon with `[escalation].enabled = true` and NO
//! pending handoffs. Asserts that the dispatcher task starts its
//! loop and does NOT touch any Phase 1/2/3 table or its own
//! `escalation_dispatches` table when there is nothing to do.
//!
//! Pins three invariants load-bearing for the Phase 4 rollout:
//!
//! - **FR-217**: Phase 4 MUST NOT modify `disputes`,
//!   `mediation_sessions`, or any Phase 1/2/3 table other than
//!   appending to `mediation_events` (and the empty-loop has no
//!   business writing any row at all).
//! - **FR-218**: Phase 4 does not depend on or alter Phase 3's
//!   engine tick. This test disables Phase 3 entirely and still
//!   expects the Phase 4 loop to spawn and tick cleanly.
//! - **SC-207**: Phase 1/2/3 behavior is unchanged. The daemon
//!   boots, migrates the schema to v5, and runs with no observed
//!   effect on any row besides the migration-time
//!   `schema_version` row.

mod common;

use std::time::Duration;

use common::{spawn_daemon, TestHarness};
use serbero::models::TimeoutsConfig;

#[tokio::test]
async fn phase4_enabled_with_no_handoffs_does_not_touch_any_table() {
    let harness = TestHarness::new().await;

    // Build the default Phase 1/2/3-disabled config and flip Phase
    // 4 on with a fast cadence so the loop actually ticks within
    // the test timeout.
    let mut cfg = harness.config(
        Vec::new(),
        TimeoutsConfig {
            renotification_seconds: 3600,
            renotification_check_interval_seconds: 3600,
        },
    );
    cfg.escalation.enabled = true;
    cfg.escalation.dispatch_interval_seconds = 1;

    let (shutdown, handle) = spawn_daemon(cfg);

    // Give the daemon time to: bring up, migrate to v5, spawn the
    // dispatcher, and tick at least twice (the first tick is
    // consumed by the interval alignment, so we wait ~3 s to see
    // two real ticks).
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Open a fresh read-only handle to the same DB file — the
    // daemon still holds its own connection. SQLite's WAL mode
    // permits concurrent reads while the daemon runs.
    let db_path = harness.db_path.clone();
    let read_conn = rusqlite::Connection::open(&db_path).expect("open db");

    // Migration landed: schema version is v5.
    let version: i64 = read_conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .expect("query schema_version");
    assert_eq!(version, 5, "Phase 4 must apply migration v5 at startup");

    // The escalation_dispatches table exists AND is empty.
    let dispatches: i64 = read_conn
        .query_row("SELECT COUNT(*) FROM escalation_dispatches", [], |r| {
            r.get(0)
        })
        .expect("count escalation_dispatches");
    assert_eq!(
        dispatches, 0,
        "empty-loop must NOT write to escalation_dispatches (FR-211 only fires on real dispatches)"
    );

    // Phase 1/2/3 tables are all empty (we published no events and
    // FR-217 forbids Phase 4 from writing to them).
    for (table, label) in [
        ("disputes", "FR-217: Phase 4 must NOT write to disputes"),
        (
            "notifications",
            "FR-217: Phase 4 must NOT write to notifications",
        ),
        (
            "dispute_state_transitions",
            "FR-217: Phase 4 must NOT write to dispute_state_transitions",
        ),
        (
            "mediation_sessions",
            "FR-217: Phase 4 must NOT write to mediation_sessions",
        ),
        (
            "mediation_messages",
            "FR-217: Phase 4 must NOT write to mediation_messages",
        ),
    ] {
        let n: i64 = read_conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("count {table}: {e}"));
        assert_eq!(n, 0, "{label}");
    }

    // No audit rows either — the empty-loop never calls
    // record_event and zero handoff_prepared rows are present
    // for it to react to.
    let events: i64 = read_conn
        .query_row("SELECT COUNT(*) FROM mediation_events", [], |r| r.get(0))
        .expect("count mediation_events");
    assert_eq!(
        events, 0,
        "empty-loop must NOT append to mediation_events (no handoffs, no Phase 4 events)"
    );

    // Shut down cleanly — the dispatcher task is aborted inside
    // `run_with_shutdown`, so this should complete in ms, not
    // after another interval.
    drop(read_conn);
    let _ = shutdown.send(());
    let join_res = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("daemon should exit within 5s of shutdown signal")
        .expect("daemon join");
    assert!(join_res.is_ok(), "daemon returned error: {join_res:?}");
}
