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

    // Wait for the first notification and then ensure no more arrive.
    assert!(
        solver.wait_for(1, 30).await,
        "solver should receive first notification"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        solver.count().await,
        1,
        "solver should not receive duplicate notification"
    );

    // Only one row in the disputes table.
    assert!(
        wait_for_row_count(
            &harness.db_path,
            "SELECT COUNT(*) FROM disputes WHERE dispute_id='dispute-dup'",
            1,
            5,
        )
        .await,
        "disputes table should contain exactly one row for dispute-dup"
    );

    // Stop daemon.
    let _ = shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;

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
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        solver.count().await,
        1,
        "restart should not re-notify for a known dispute"
    );

    let _ = shutdown2.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle2).await;
}
