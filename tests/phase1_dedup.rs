mod common;

use std::time::Duration;

use common::{
    publish_dispute, publisher, solver_cfg, spawn_daemon, wait_for_row_count, SolverListener,
    TestHarness,
};
use serbero::models::{SolverPermission, TimeoutsConfig};

#[tokio::test]
async fn duplicate_event_is_not_renotified_within_session_or_across_restart() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;

    let cfg = harness.config(
        vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
        TimeoutsConfig {
            renotification_seconds: 3600,
            renotification_check_interval_seconds: 3600,
        },
    );
    let (shutdown, handle) = spawn_daemon(cfg.clone());
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mostro_client = publisher(&harness.relay_url, harness.mostro_keys.clone()).await;
    // Publish the same dispute twice in a row.
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-dup",
        "initiated",
        "buyer",
        vec![],
    )
    .await;
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-dup",
        "initiated",
        "buyer",
        vec![],
    )
    .await;

    // Wait for the first notification to arrive.
    assert!(
        solver.wait_for(1, 30).await,
        "solver should receive first notification"
    );

    // Wait for Serbero to persist the dedup'd duplicate event — the
    // fact that we only ever expect ONE row is what we assert. Once the
    // disputes row is present the dispatcher has processed both copies.
    assert!(
        wait_for_row_count(
            &harness.db_path,
            "SELECT COUNT(*) FROM disputes WHERE dispute_id='dispute-dup'",
            1,
            15,
        )
        .await,
        "expected exactly one disputes row for dispute-dup"
    );

    // Poll briefly to catch any spurious duplicate notification; if
    // count stays at 1 across several short checks we trust the dedup.
    for _ in 0..10 {
        assert_eq!(
            solver.count().await,
            1,
            "solver should not receive duplicate notification"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Stop daemon.
    shutdown.send(()).expect("shutdown signal should send");
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("daemon should shut down within 2 seconds")
        .expect("daemon handle should complete successfully")
        .expect("daemon should exit cleanly");

    // Restart daemon, republish event. Expect no additional notification.
    let (shutdown2, handle2) = spawn_daemon(cfg);
    tokio::time::sleep(Duration::from_millis(500)).await;
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-dup",
        "initiated",
        "buyer",
        vec![],
    )
    .await;
    // Poll for a stable count rather than a fixed sleep.
    for _ in 0..20 {
        assert_eq!(
            solver.count().await,
            1,
            "restart should not re-notify for a known dispute"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    shutdown2.send(()).expect("shutdown signal should send");
    tokio::time::timeout(Duration::from_secs(2), handle2)
        .await
        .expect("daemon should shut down within 2 seconds")
        .expect("daemon handle should complete successfully")
        .expect("daemon should exit cleanly");
}
