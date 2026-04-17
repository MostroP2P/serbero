mod common;

use std::time::Duration;

use common::{publish_dispute, publisher, solver_cfg, spawn_daemon, TestHarness};
use serbero::models::{SolverConfig, SolverPermission, TimeoutsConfig};

/// When one solver pubkey is malformed, Serbero records a failed notification row for it but
/// still delivers successfully to the valid solvers. The daemon must not crash.
#[tokio::test]
async fn invalid_solver_pubkey_is_logged_as_failed_without_crashing() {
    let harness = TestHarness::new().await;
    let valid = common::SolverListener::start(&harness.relay_url).await;

    let cfg = harness.config(
        vec![
            solver_cfg(valid.pubkey_hex(), SolverPermission::Read),
            SolverConfig {
                pubkey: "not-a-real-hex-pubkey".into(),
                permission: SolverPermission::Read,
            },
        ],
        TimeoutsConfig {
            renotification_seconds: 3600,
            renotification_check_interval_seconds: 3600,
        },
    );
    let (shutdown, handle) = spawn_daemon(cfg);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mostro_client = publisher(&harness.relay_url, harness.mostro_keys.clone()).await;
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-fail-1",
        "initiated",
        "buyer",
        vec![],
    )
    .await;

    assert!(
        valid.wait_for(1, 30).await,
        "valid solver should still receive notification"
    );

    // Give the failed-path its time to persist.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let conn = rusqlite::Connection::open(&harness.db_path).unwrap();
    let sent: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notifications WHERE dispute_id='dispute-fail-1' AND status='sent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let failed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notifications WHERE dispute_id='dispute-fail-1' AND status='failed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sent, 1, "exactly one sent notification expected");
    assert_eq!(failed, 1, "exactly one failed notification expected");

    let err_msg: Option<String> = conn
        .query_row(
            "SELECT error_message FROM notifications \
             WHERE dispute_id='dispute-fail-1' AND status='failed' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        err_msg.as_deref().unwrap_or("").contains("invalid pubkey"),
        "error_message should describe failure; got: {err_msg:?}"
    );

    // Handle still alive?
    assert!(!handle.is_finished(), "daemon should still be running");

    shutdown.send(()).expect("shutdown signal should send");
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("daemon should shut down within 2 seconds")
        .expect("daemon handle should complete successfully")
        .expect("daemon should exit cleanly");
}

/// With no solvers configured at all, the daemon logs a warning but still persists the dispute.
#[tokio::test]
async fn no_solvers_configured_persists_but_does_not_notify() {
    let harness = TestHarness::new().await;

    let cfg = harness.config(
        vec![],
        TimeoutsConfig {
            renotification_seconds: 3600,
            renotification_check_interval_seconds: 3600,
        },
    );
    let (shutdown, handle) = spawn_daemon(cfg);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mostro_client = publisher(&harness.relay_url, harness.mostro_keys.clone()).await;
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-no-solvers",
        "initiated",
        "seller",
        vec![],
    )
    .await;

    assert!(
        common::wait_for_row_count(
            &harness.db_path,
            "SELECT COUNT(*) FROM disputes WHERE dispute_id='dispute-no-solvers'",
            1,
            10,
        )
        .await,
        "dispute should still be persisted when no solvers configured"
    );

    let conn = rusqlite::Connection::open(&harness.db_path).unwrap();
    let notif_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        notif_count, 0,
        "no notifications should be recorded when no solvers configured"
    );

    shutdown.send(()).expect("shutdown signal should send");
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("daemon should shut down within 2 seconds")
        .expect("daemon handle should complete successfully")
        .expect("daemon should exit cleanly");
}
