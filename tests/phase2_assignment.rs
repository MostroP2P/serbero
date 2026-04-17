mod common;

use std::time::Duration;

use common::{
    publish_dispute, publisher, solver_cfg, spawn_daemon, wait_for_row_count, SolverListener,
    TestHarness,
};
use nostr_sdk::{Alphabet, SingleLetterTag, Tag, TagKind};
use serbero::models::{SolverPermission, TimeoutsConfig};

#[tokio::test]
async fn assignment_event_transitions_to_taken_and_notifies_all_solvers() {
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
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mostro_client = publisher(&harness.relay_url, harness.mostro_keys.clone()).await;
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-assign",
        "initiated",
        "buyer",
        vec![],
    )
    .await;

    assert!(solver_a.wait_for(1, 30).await, "solver A initial missed");
    assert!(solver_b.wait_for(1, 30).await, "solver B initial missed");

    // Publish an in-progress event with a `p` tag naming the assigned solver.
    let fake_solver_pk = "solver_assigned_pk_hex";
    publish_dispute(
        &mostro_client,
        &harness.mostro_keys,
        "dispute-assign",
        "in-progress",
        "buyer",
        vec![Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::P)),
            [fake_solver_pk],
        )],
    )
    .await;

    let taken_query = format!(
        "SELECT COUNT(*) FROM disputes WHERE dispute_id='dispute-assign' \
         AND lifecycle_state='taken' AND assigned_solver='{fake_solver_pk}'"
    );
    assert!(
        wait_for_row_count(&harness.db_path, &taken_query, 1, 15).await,
        "dispute should be in taken state with assigned_solver={fake_solver_pk}"
    );

    // Each solver should also get an assignment notification (2 total per solver).
    assert!(
        solver_a.wait_for(2, 30).await,
        "solver A should get assignment notification"
    );
    assert!(
        solver_b.wait_for(2, 30).await,
        "solver B should get assignment notification"
    );

    // No further notifications should arrive over the next 2 seconds.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(solver_a.count().await, 2);
    assert_eq!(solver_b.count().await, 2);

    let conn = rusqlite::Connection::open(&harness.db_path).unwrap();
    let assigned: String = conn
        .query_row(
            "SELECT assigned_solver FROM disputes WHERE dispute_id='dispute-assign'",
            [],
            |r| r.get::<_, Option<String>>(0).map(|s| s.unwrap_or_default()),
        )
        .unwrap();
    assert_eq!(assigned, fake_solver_pk);

    let assignment_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notifications WHERE dispute_id='dispute-assign' \
             AND notif_type='assignment'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(assignment_count, 2);

    let _ = shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}
