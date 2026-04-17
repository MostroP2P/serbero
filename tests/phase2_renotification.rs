mod common;

use std::time::Duration;

use common::{publish_dispute, publisher, solver_cfg, spawn_daemon, SolverListener, TestHarness};
use serbero::models::{SolverPermission, TimeoutsConfig};

#[tokio::test]
async fn unattended_dispute_is_re_notified_after_timeout() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;

    let cfg = harness.config(
        vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        TimeoutsConfig {
            renotification_seconds: 2,
            renotification_check_interval_seconds: 1,
        },
    );
    let (shutdown, handle) = spawn_daemon(cfg);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mostro_client = publisher(&harness.relay_url, harness.mostro_keys.clone()).await;
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-renotify",
        "initiated",
        "buyer",
        vec![],
    )
    .await;

    // First notification (initial).
    assert!(solver.wait_for(1, 30).await, "initial notification missed");

    // Snapshot last_notified_at before the re-notification fires so we
    // can assert the timer actually advanced it.
    let pre: i64 = {
        let conn = rusqlite::Connection::open(&harness.db_path).unwrap();
        conn.query_row(
            "SELECT last_notified_at FROM disputes WHERE dispute_id='dispute-renotify'",
            [],
            |r| r.get::<_, Option<i64>>(0).map(|v| v.unwrap_or(0)),
        )
        .unwrap()
    };

    // Within a few seconds, one re-notification should fire.
    assert!(
        solver.wait_for(2, 15).await,
        "re-notification should fire within timeout window"
    );

    let conn = rusqlite::Connection::open(&harness.db_path).unwrap();
    let renotif: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notifications \
             WHERE dispute_id='dispute-renotify' AND notif_type='re-notification'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        renotif >= 1,
        "expected at least one re-notification row, got {renotif}"
    );

    let post: i64 = conn
        .query_row(
            "SELECT last_notified_at FROM disputes WHERE dispute_id='dispute-renotify'",
            [],
            |r| r.get::<_, Option<i64>>(0).map(|v| v.unwrap_or(0)),
        )
        .unwrap();
    assert!(
        post > pre,
        "last_notified_at must advance on re-notification (pre={pre} post={post})"
    );

    let _ = shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn taken_dispute_is_not_re_notified() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;

    // Keep the re-notification window wide enough that the
    // `in-progress` event is guaranteed to be processed (and the
    // dispute transitioned to `Taken`) before the timer could ever
    // fire — otherwise the initial→renotify→taken race could produce
    // a spurious third notification.
    let cfg = harness.config(
        vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        TimeoutsConfig {
            renotification_seconds: 30,
            renotification_check_interval_seconds: 1,
        },
    );
    let (shutdown, handle) = spawn_daemon(cfg);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mostro_client = publisher(&harness.relay_url, harness.mostro_keys.clone()).await;
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-taken-nore",
        "initiated",
        "buyer",
        vec![],
    )
    .await;
    assert!(solver.wait_for(1, 30).await, "initial notification missed");

    // Take the dispute right away.
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-taken-nore",
        "in-progress",
        "buyer",
        vec![],
    )
    .await;
    assert!(
        solver.wait_for(2, 30).await,
        "assignment notification missed"
    );

    // Wait for several timer ticks; expect no third notification
    // because the dispute is now in `Taken` state.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        solver.count().await,
        2,
        "taken disputes must not trigger re-notifications"
    );

    // Also verify via DB that only initial + assignment rows exist —
    // no re-notification row ever got written.
    let conn = rusqlite::Connection::open(&harness.db_path).unwrap();
    let renotif: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notifications \
             WHERE dispute_id='dispute-taken-nore' AND notif_type='re-notification'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        renotif, 0,
        "no re-notification rows expected for a taken dispute"
    );

    let _ = shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}
