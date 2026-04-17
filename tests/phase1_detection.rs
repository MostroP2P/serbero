mod common;

use std::time::Duration;

use common::{publish_dispute, publisher, solver_cfg, spawn_daemon, SolverListener, TestHarness};
use serbero::models::{SolverPermission, TimeoutsConfig};

#[tokio::test]
async fn detects_new_dispute_and_notifies_all_solvers() {
    let harness = TestHarness::new().await;
    let solver_a = SolverListener::start(&harness.relay_url).await;
    let solver_b = SolverListener::start(&harness.relay_url).await;

    let cfg = harness.config(
        vec![
            solver_cfg(solver_a.pubkey_hex(), SolverPermission::Read),
            solver_cfg(solver_b.pubkey_hex(), SolverPermission::Write),
        ],
        TimeoutsConfig {
            renotification_seconds: 3600,
            renotification_check_interval_seconds: 3600,
        },
    );
    let (shutdown, handle) = spawn_daemon(cfg);

    // Allow Serbero time to connect and subscribe.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mostro_client = publisher(&harness.relay_url, harness.mostro_keys.clone()).await;
    let event = publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-001",
        "initiated",
        "buyer",
        vec![],
    )
    .await;

    assert!(
        solver_a.wait_for(1, 30).await,
        "solver A did not receive notification"
    );
    assert!(
        solver_b.wait_for(1, 30).await,
        "solver B did not receive notification"
    );

    let ts = event.created_at.as_secs().to_string();
    for (label, listener) in [("A", &solver_a), ("B", &solver_b)] {
        let msg = listener.messages().await.pop().unwrap();
        assert!(
            msg.contains("dispute-001"),
            "solver {label} missing dispute id: {msg}"
        );
        assert!(
            msg.contains("buyer"),
            "solver {label} missing initiator role: {msg}"
        );
        assert!(
            msg.contains(&ts),
            "solver {label} missing event timestamp: {msg}"
        );
    }

    shutdown.send(()).expect("shutdown signal should send");
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("daemon should shut down within 2 seconds")
        .expect("daemon handle should complete successfully")
        .expect("daemon should exit cleanly");
}
