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

// Sub-modules filled in by later tasks (T011–T024). Each is a
// pure-function layer that mod.rs's run_dispatcher stitches
// together.
pub mod consumer;
pub mod dispatcher;
pub mod router;
pub mod tracker;

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::{Client, Keys};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, info};

use crate::models::{EscalationConfig, SolverConfig};

/// Cap on the number of pending handoffs processed per cycle.
///
/// Bounded so a backlog after a daemon restart cannot starve other
/// tokio tasks on the same runtime. Chosen generously — a healthy
/// deployment produces handoffs in the low units per hour, so 128
/// is several weeks of backlog in one tick. The limit is not a
/// policy knob (the operator controls pacing via
/// `dispatch_interval_seconds`).
///
/// Unused at the Phase 2 Foundational stage; wired up by T016.
#[allow(dead_code)]
const SCAN_BATCH_LIMIT: i64 = 128;

/// Phase 4 background task entry.
///
/// Spawned by `crate::daemon::run` when `config.escalation.enabled`.
/// The loop blocks on a `tokio::time::interval` so the daemon
/// shutdown signal (owned by the daemon, wrapped in a
/// `tokio::select!`) aborts the task cleanly on the first missed
/// tick boundary.
///
/// At the Phase 2 (Foundational) stage the loop only logs at
/// debug level; the consumer → router → dispatcher → tracker
/// wiring is filled in by Phase 3 (T011–T016). Importantly, the
/// Foundational smoke test (T010) pins the empty-loop shape NOW,
/// so any later edit that accidentally writes a Phase 1/2/3 row
/// or an `escalation_dispatches` row before T016 lands will fail
/// loudly.
pub async fn run_dispatcher(
    _conn: Arc<AsyncMutex<rusqlite::Connection>>,
    _client: Client,
    _serbero_keys: Keys,
    _solvers: Vec<SolverConfig>,
    cfg: EscalationConfig,
) {
    let interval_secs = cfg.dispatch_interval_seconds;
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
        // Consumer / router / dispatcher / tracker wiring lands in
        // T016. Until then the tick is a no-op — this is the
        // Foundational-phase shape pinned by the T010 smoke test.
    }
}
