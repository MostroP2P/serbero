//! Phase 4 — escalation execution surface.
//!
//! Consumes `handoff_prepared` audit events produced by Phase 3
//! and dispatches structured DMs to write-permission solvers. The
//! dispatcher is strictly additive: it reads Phase 1/2/3 state,
//! writes only to its own table (`escalation_dispatches`) plus
//! append-only rows in `mediation_events`, never issues
//! `TakeDispute`, and does not retry / ack / re-escalate.
//!
//! The background task runs in parallel with Phase 3's engine
//! tick. The two loops share no state beyond the audit tables
//! (read-only from Phase 4's side), so FR-218 holds by
//! construction: a future change to Phase 3's tick interval,
//! retry discipline, or reasoning-health gate cannot affect Phase
//! 4, and vice-versa.
//!
//! Discipline (mirrors `mediation::run_engine`):
//! - The loop NEVER panics: every error path logs and continues
//!   with the next handoff.
//! - Shutdown is not handled here: the daemon wraps the returned
//!   future in a `tokio::select!` with its shutdown signal and
//!   aborts on shutdown.
//! - `[escalation].enabled = false` keeps Phase 4 entirely inert —
//!   the daemon does not even spawn the task. Phase 1/2/3 behavior
//!   is unaffected (FR-216 / SC-207).

pub mod consumer;
pub mod dispatcher;
pub mod router;
pub mod tracker;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nostr_sdk::prelude::{Client, Keys};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, error, info, warn};

use crate::db::disputes::get_dispute;
use crate::db::escalation_dispatches::PendingHandoff;
use crate::mediation::escalation::HandoffPackage;
use crate::models::{EscalationConfig, SolverConfig};

use self::dispatcher::{build_dm_body, send_to_recipients};
use self::router::{resolve_recipients, Recipients};

/// Cap on the number of pending handoffs processed per cycle.
///
/// Bounded so a backlog after a daemon restart cannot starve other
/// tokio tasks on the same runtime. Chosen generously — a healthy
/// deployment produces handoffs in the low units per hour, so 128
/// is several weeks of backlog in one tick. The limit is not a
/// policy knob (the operator controls pacing via
/// `dispatch_interval_seconds`).
const SCAN_BATCH_LIMIT: i64 = 128;

/// Phase 4 background task entry.
///
/// Spawned by `crate::daemon::run` when `config.escalation.enabled`.
/// Each tick scans for pending handoffs, deserializes each one,
/// resolves recipients via the FR-202 rule table, sends the DM,
/// and records both the dispatch-tracking row and its paired audit
/// event inside a single transaction.
///
/// US2 (supersession) and US3 (unroutable + parse-failed) branches
/// are currently TODO stubs — they land in T019/T022/T028.
pub async fn run_dispatcher(
    conn: Arc<AsyncMutex<rusqlite::Connection>>,
    client: Client,
    serbero_keys: Keys,
    solvers: Vec<SolverConfig>,
    cfg: EscalationConfig,
) {
    // Defensive guard. `validate_escalation` in `crate::config` already
    // rejects `0` at config-load time with a loud `Error::Config`, so
    // this branch is unreachable via the normal daemon startup path.
    // Tests and future callers that build `EscalationConfig` directly
    // (bypassing `load_config`) could still reach it; coercing to `1`
    // keeps `tokio::time::interval` from panicking on
    // `Duration::from_secs(0)`.
    let interval_secs = cfg.dispatch_interval_seconds.max(1);
    info!(
        dispatch_interval_seconds = interval_secs,
        fallback_to_all_solvers = cfg.fallback_to_all_solvers,
        "phase4_dispatcher_loop_started"
    );

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate first tick so we align to the cadence
    // rather than hammering the DB once on boot.
    ticker.tick().await;

    loop {
        ticker.tick().await;
        debug!("phase4_dispatcher_tick");
        if let Err(e) = run_once(&conn, &client, &serbero_keys, &solvers, &cfg).await {
            // Only infrastructure failures reach this branch (DB
            // lock poisoning, non-recoverable handle loss). Per-
            // handoff failures are swallowed inside `process_one`
            // so one bad event never blocks the batch.
            error!(error = %e, "phase4_dispatcher_cycle_failed");
        }
    }
}

/// Run one dispatcher cycle. `pub` so integration tests can drive
/// a single cycle without having to spin up the full
/// `run_dispatcher` interval loop and wait for ticks. Inside the
/// daemon, `run_dispatcher` is the only caller.
pub async fn run_once(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    serbero_keys: &Keys,
    solvers: &[SolverConfig],
    cfg: &EscalationConfig,
) -> crate::error::Result<()> {
    let pending = consumer::scan_pending(conn, SCAN_BATCH_LIMIT).await?;
    if pending.is_empty() {
        return Ok(());
    }
    debug!(count = pending.len(), "phase4_cycle_pending");
    for handoff in pending {
        // Per-handoff failure is absorbed inside `process_one`.
        // An outer Err is reserved for "the whole cycle cannot
        // continue", which today doesn't fire — but we keep the
        // Result shape so a future mid-cycle "cannot reach DB at
        // all" signal has a home.
        process_one(conn, client, serbero_keys, solvers, cfg, handoff).await;
    }
    Ok(())
}

/// Handle one pending handoff end-to-end.
///
/// Infallible from the caller's perspective: every per-handoff
/// error path logs + advances. The outer cycle loop iterates
/// `scan_pending`'s result set without aborting.
async fn process_one(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    client: &Client,
    serbero_keys: &Keys,
    solvers: &[SolverConfig],
    cfg: &EscalationConfig,
    handoff: PendingHandoff,
) {
    // (1) Deserialize. Parse failures (FR-214 /
    //     `deserialize_failed` sub-reason) land as
    //     `escalation_dispatch_parse_failed` audit rows via T028.
    //     Until T028 ships, a deserialize failure logs WARN and
    //     advances — the payload stays in `mediation_events` so a
    //     later cycle after the T028 landing will record the
    //     parse_failed audit row.
    let pkg: HandoffPackage = match serde_json::from_str(&handoff.payload_json) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                handoff_event_id = handoff.handoff_event_id,
                error = %e,
                "phase4_dispatch: handoff payload deserialize failed (T028 handler not yet live)"
            );
            return;
        }
    };

    // (2) Supersession gate (FR-208 / T019). Until T019 lands we
    //     cannot skip-and-record; for US1's MVP scope we only log
    //     at DEBUG if the dispute is already resolved and press
    //     on — the solver getting a second "please review" DM
    //     after resolution is the documented at-least-once
    //     trade-off the spec accepts.
    let assigned_solver: Option<String> = match dispute_metadata(conn, &pkg.dispute_id).await {
        Ok(md) => md.assigned_solver,
        Err(e) => {
            warn!(
                dispute_id = %pkg.dispute_id,
                error = %e,
                "phase4_dispatch: dispute lookup failed (T028 orphan handler not yet live)"
            );
            return;
        }
    };

    // (3) Resolve recipients per FR-202.
    let recipients = resolve_recipients(
        solvers,
        assigned_solver.as_deref(),
        cfg.fallback_to_all_solvers,
    );

    let (pubkeys, via_fallback) = match recipients {
        Recipients::Targeted(pk) => (vec![pk], false),
        Recipients::Broadcast {
            pubkeys,
            via_fallback,
        } => (pubkeys, via_fallback),
        Recipients::Unroutable => {
            // T022 handles this with an `escalation_dispatch_unroutable`
            // audit row + ERROR log. For US1 we emit the ERROR and
            // leave the handoff unconsumed so the T022 landing picks
            // it up on the first cycle after.
            error!(
                dispute_id = %pkg.dispute_id,
                handoff_event_id = handoff.handoff_event_id,
                "phase4_dispatch: no Write-permission solvers configured and \
                 fallback_to_all_solvers = false; handoff remains unconsumed \
                 (T022 handler will add escalation_dispatch_unroutable audit row)"
            );
            return;
        }
    };

    // `pubkeys` is always non-empty here: the router collapses
    // the "fallback-on + zero configured solvers" edge case to
    // `Recipients::Unroutable` so the handler above is the only
    // code path for "can't route", and the two Broadcast arms
    // (normal write-set and via_fallback) both require at least
    // one entry. A future router change MUST preserve that
    // invariant — a bare `debug_assert!` would be appropriate if
    // this becomes a concern.

    // (4) Build the DM body.
    let body = build_dm_body(&pkg);

    // (5) Send to every recipient. Per-recipient outcomes live in
    //     `notifications`; the aggregate flows into the dispatch
    //     row's `status`.
    let now = current_unix_seconds();
    let outcome = match send_to_recipients(
        conn,
        client,
        serbero_keys,
        &pkg.dispute_id,
        &pubkeys,
        &body,
        now,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            error!(
                dispute_id = %pkg.dispute_id,
                handoff_event_id = handoff.handoff_event_id,
                error = %e,
                "phase4_dispatch: send loop errored; handoff remains unconsumed"
            );
            return;
        }
    };

    // (6) Record the dispatch row + audit event atomically.
    if let Err(e) = tracker::record_successful_dispatch(
        conn,
        &handoff,
        &pkg.dispute_id,
        &outcome,
        via_fallback,
        now,
    )
    .await
    {
        error!(
            dispute_id = %pkg.dispute_id,
            handoff_event_id = handoff.handoff_event_id,
            error = %e,
            "phase4_dispatch: record_successful_dispatch failed AFTER send; \
             next cycle will re-dispatch per the at-least-once semantics"
        );
    } else {
        info!(
            dispute_id = %pkg.dispute_id,
            handoff_event_id = handoff.handoff_event_id,
            recipients = pubkeys.len(),
            via_fallback,
            "phase4_dispatched"
        );
    }
}

#[derive(Debug)]
struct DisputeMetadata {
    assigned_solver: Option<String>,
}

/// Read the fields of `disputes` that Phase 4 actually needs.
///
/// Only `assigned_solver` today; the supersession gate (T019) will
/// extend this to also read `lifecycle_state`. Kept as a small
/// helper so the `get_dispute` FK lookup runs once per handoff,
/// not once per branch.
async fn dispute_metadata(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    dispute_id: &str,
) -> crate::error::Result<DisputeMetadata> {
    let guard = conn.lock().await;
    let d = get_dispute(&guard, dispute_id)?.ok_or_else(|| {
        crate::error::Error::InvalidEvent(format!(
            "phase4_dispatch: handoff references unknown dispute {dispute_id} \
             (T028 orphan_dispute_reference handler not yet live)"
        ))
    })?;
    Ok(DisputeMetadata {
        assigned_solver: d.assigned_solver,
    })
}

/// Current Unix-epoch seconds. A thin wrapper so we can swap the
/// clock in tests if needed without threading a closure through
/// every function. Today all callers take the live system clock.
fn current_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;
    use crate::mediation::escalation::HandoffPackage;
    use crate::models::SolverPermission;
    use nostr_sdk::Keys;
    use rusqlite::params;

    async fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
        let mut c = open_in_memory().unwrap();
        run_migrations(&mut c).unwrap();
        Arc::new(AsyncMutex::new(c))
    }

    fn solver(pk: &str, perm: SolverPermission) -> SolverConfig {
        SolverConfig {
            pubkey: pk.to_string(),
            permission: perm,
        }
    }

    fn sample_cfg(interval_secs: u64, fallback: bool) -> EscalationConfig {
        EscalationConfig {
            enabled: true,
            dispatch_interval_seconds: interval_secs,
            fallback_to_all_solvers: fallback,
        }
    }

    async fn seed_handoff_for_dispute(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        dispute_id: &str,
        assigned_solver: Option<&str>,
        pkg: &HandoffPackage,
    ) -> i64 {
        let c = conn.lock().await;
        c.execute(
            "INSERT INTO disputes (
                dispute_id, event_id, mostro_pubkey, initiator_role,
                dispute_status, event_timestamp, detected_at, lifecycle_state,
                assigned_solver
             ) VALUES (?1, ?2, 'mostro', 'buyer', 'initiated', 10, 11, 'notified', ?3)",
            params![dispute_id, format!("evt-{dispute_id}"), assigned_solver],
        )
        .unwrap();
        let payload = serde_json::to_string(pkg).unwrap();
        c.query_row(
            "INSERT INTO mediation_events (
                session_id, kind, payload_json,
                prompt_bundle_id, policy_hash, occurred_at
             ) VALUES (NULL, 'handoff_prepared', ?1,
                       'phase3-default', 'hash-1', 100)
             RETURNING id",
            params![payload],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    }

    fn sample_package(dispute_id: &str) -> HandoffPackage {
        HandoffPackage {
            dispute_id: dispute_id.to_string(),
            session_id: None,
            trigger: "conflicting_claims".to_string(),
            evidence_refs: Vec::new(),
            prompt_bundle_id: "phase3-default".to_string(),
            policy_hash: "hash-1".to_string(),
            rationale_refs: Vec::new(),
            assembled_at: 100,
        }
    }

    #[tokio::test]
    async fn empty_pending_set_is_cycle_noop() {
        let conn = fresh_conn().await;
        let keys = Keys::generate();
        let client = nostr_sdk::Client::new(keys.clone());
        let cfg = sample_cfg(1, false);

        run_once(&conn, &client, &keys, &[], &cfg).await.unwrap();

        let count: i64 = {
            let c = conn.lock().await;
            c.query_row("SELECT COUNT(*) FROM escalation_dispatches", [], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn handoff_with_no_write_solvers_and_fallback_off_stays_unconsumed() {
        // US3 scenario — zero write solvers + fallback off. The
        // full T022 handler lands later; for US1 we assert that
        // the handoff stays in the pending set (no
        // escalation_dispatches row, no escalation_dispatched
        // event). Operators see the loud ERROR log line.
        let conn = fresh_conn().await;
        let keys = Keys::generate();
        let client = nostr_sdk::Client::new(keys.clone());
        let pkg = sample_package("d-us3");
        seed_handoff_for_dispute(&conn, "d-us3", None, &pkg).await;
        let solvers = vec![solver("pk-r1", SolverPermission::Read)];
        let cfg = sample_cfg(1, false);

        run_once(&conn, &client, &keys, &solvers, &cfg)
            .await
            .unwrap();

        let dispatches: i64 = {
            let c = conn.lock().await;
            c.query_row("SELECT COUNT(*) FROM escalation_dispatches", [], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(
            dispatches, 0,
            "unroutable handoff must not create a dispatch row"
        );

        let events: i64 = {
            let c = conn.lock().await;
            c.query_row(
                "SELECT COUNT(*) FROM mediation_events
                 WHERE kind IN ('escalation_dispatched',
                                'escalation_dispatch_unroutable')",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            events, 0,
            "T022 handler not live yet; no unroutable audit row should fire — \
             handoff stays in the pending set for a future cycle to pick up"
        );
    }
}
