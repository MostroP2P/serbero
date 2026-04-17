mod common;

use std::time::Duration;

use common::{
    publish_dispute, publisher, solver_cfg, spawn_daemon, wait_for_row_count, SolverListener,
    TestHarness,
};
use serbero::models::{SolverPermission, TimeoutsConfig};

#[tokio::test]
async fn dispute_walks_through_new_notified_taken() {
    let harness = TestHarness::new().await;
    let solver = SolverListener::start(&harness.relay_url).await;

    let cfg = harness.config(
        vec![solver_cfg(solver.pubkey_hex(), SolverPermission::Read)],
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
        "dispute-lc",
        "initiated",
        "buyer",
        vec![],
    )
    .await;

    // After initial detection + notification, dispute should be in `notified`.
    assert!(
        wait_for_row_count(
            &harness.db_path,
            "SELECT COUNT(*) FROM disputes WHERE dispute_id='dispute-lc' \
             AND lifecycle_state='notified'",
            1,
            15,
        )
        .await,
        "dispute should be in notified state"
    );

    // Publish in-progress event to trigger transition to `taken`.
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-lc",
        "in-progress",
        "buyer",
        vec![],
    )
    .await;

    assert!(
        wait_for_row_count(
            &harness.db_path,
            "SELECT COUNT(*) FROM disputes WHERE dispute_id='dispute-lc' \
             AND lifecycle_state='taken'",
            1,
            15,
        )
        .await,
        "dispute should transition to taken"
    );

    // Transition history should include new->notified and notified->taken.
    let conn = rusqlite::Connection::open(&harness.db_path).unwrap();
    let transitions: Vec<(String, Option<String>, String)> = conn
        .prepare(
            "SELECT dispute_id, from_state, to_state
             FROM dispute_state_transitions
             WHERE dispute_id='dispute-lc'
             ORDER BY id ASC",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    // Assert the full transition chain and its order: new -> notified -> taken.
    let new_to_notified_idx = transitions
        .iter()
        .position(|t| t.1.as_deref() == Some("new") && t.2 == "notified")
        .expect("expected new -> notified transition");
    let notified_to_taken_idx = transitions
        .iter()
        .position(|t| t.1.as_deref() == Some("notified") && t.2 == "taken")
        .expect("expected notified -> taken transition");
    assert!(
        new_to_notified_idx < notified_to_taken_idx,
        "new -> notified must precede notified -> taken (got {new_to_notified_idx} >= {notified_to_taken_idx})"
    );

    let _ = shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}
