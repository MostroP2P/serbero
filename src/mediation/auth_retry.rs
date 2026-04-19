//! Solver-authorization bounded revalidation loop (T042).
//!
//! Scope-control from `plan.md`: a single `tokio::task` with
//! truncated exponential backoff between
//! `solver_auth_retry_initial_seconds` and
//! `solver_auth_retry_max_interval_seconds`, terminating at the
//! first of `solver_auth_retry_max_total_seconds` or
//! `solver_auth_retry_max_attempts` with a terminal `error!` alert.
//! Phase 1/2 runs unaffected throughout (SC-105). No generic retry
//! framework. No state machine beyond `Authorized` / `Unauthorized`
//! / `Terminated`.
//!
//! This slice wires the loop against a **stub** `check_authorization`
//! that always succeeds — the real Mostro solver-registration DM
//! exchange is US3 territory (see TODO on the stub). Until that
//! lands, the daemon's handle will always reach `Authorized` on the
//! initial check, so the loop code is never exercised in production.
//! It *is* exercised in `#[cfg(test)]` via an injectable checker, so
//! the backoff / termination / audit discipline is locked in before
//! the real verification protocol arrives.
//!
//! The handle is **read-only** from session.rs's point of view — the
//! gate in `session::open_session` calls `current_state()` and
//! never writes. That one-way coupling is the SC-105 guarantee:
//! whatever the auth state does, Phase 1/2 detection and solver
//! notification cannot observe a regression through this handle.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{error, info, warn};

use crate::db;
use crate::db::mediation_events::MediationEventKind;

/// In-memory state of the auth-retry state machine. Cheap to clone
/// and cheap to read — the session-open gate polls this once per
/// attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthState {
    /// Serbero is currently authorized as a Mostro solver. Session
    /// opens proceed.
    Authorized,
    /// Serbero's initial authorization check failed and the bounded
    /// revalidation loop is running. Session opens refuse with
    /// `RefusedAuthPending`.
    Unauthorized,
    /// The revalidation loop exhausted its bounds without recovery.
    /// Terminal for this daemon run; session opens refuse with
    /// `RefusedAuthTerminated`.
    Terminated,
}

/// Handle the engine / session-open path reads to decide whether
/// Serbero is currently authorized. Cloning the handle shares the
/// underlying state.
///
/// Callers read; they never write. The loop spawned by
/// [`ensure_authorized_or_enter_loop`] is the only writer.
#[derive(Clone)]
pub struct AuthRetryHandle {
    state: Arc<Mutex<AuthState>>,
}

impl AuthRetryHandle {
    /// Build a handle that already reports `Authorized`. Two valid
    /// callers:
    ///
    /// - `ensure_authorized_or_enter_loop` when the initial check
    ///   passes on the first try (no retry loop spawned).
    /// - Integration tests under `tests/` that want to pin the
    ///   session-open gate to `Authorized` without running the real
    ///   check.
    ///
    /// Both of those only need the `Authorized` seed — the other
    /// two states (`Unauthorized` / `Terminated`) are loop-driven,
    /// so there is no legitimate production reason to fabricate
    /// them. Seeding those states is covered by [`Self::with_state_for_testing`]
    /// below, which is test-gated.
    pub fn new_authorized() -> Self {
        Self::with_state(AuthState::Authorized)
    }

    /// Module-private constructor used by
    /// [`ensure_authorized_or_enter_loop_inner`] to seed an
    /// `Unauthorized` handle before spawning the retry task. Kept
    /// private so the one-writer invariant (only the spawned loop
    /// mutates state away from its initial value) is enforced at
    /// the type system level — no production caller outside this
    /// module can fabricate a handle in any state.
    fn with_state(state: AuthState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Test-only seed for arbitrary states. Gated behind
    /// `#[cfg(test)]` so integration tests in `tests/*.rs` (which
    /// compile against the non-test lib) cannot reach it, and so
    /// the public API of a release build does NOT expose a way to
    /// bypass the retry loop's one-writer invariant. Unit tests in
    /// sibling modules (e.g. `mediation::session::tests`) use this
    /// to pin the `Unauthorized` / `Terminated` gate paths.
    #[cfg(test)]
    pub(crate) fn with_state_for_testing(state: AuthState) -> Self {
        Self::with_state(state)
    }

    /// Cheap read of the current state. Never panics: the inner
    /// mutex is only held for micro-scopes to copy out an enum
    /// variant, so a panicked writer is a programmer bug worth
    /// surfacing rather than masking.
    pub fn current_state(&self) -> AuthState {
        *self.state.lock().expect("auth-retry state mutex poisoned")
    }

    pub fn is_authorized(&self) -> bool {
        matches!(self.current_state(), AuthState::Authorized)
    }

    fn set_state(&self, new_state: AuthState) {
        let mut guard = self.state.lock().expect("auth-retry state mutex poisoned");
        *guard = new_state;
    }
}

impl fmt::Debug for AuthRetryHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthRetryHandle")
            .field("state", &self.current_state())
            .finish()
    }
}

/// Reason a single `check_authorization` attempt failed. The
/// `check_authorization` adapter surface is plain String-carrying
/// until US3 defines the real verification protocol — using a named
/// type from day one means call sites do not need to be edited when
/// the real error taxonomy lands.
#[derive(Debug)]
pub struct AuthCheckError(pub String);

impl fmt::Display for AuthCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AuthCheckError {}

/// Check whether Serbero is currently authorized as a Mostro solver.
///
/// US1: stub — always returns `Ok(())`. The real verification
/// protocol is a DM exchange with Mostro using the same chat
/// transport as `chat::dispute_chat_flow`; implementing it here
/// before US3 would either duplicate that module or pre-commit to
/// a shape we do not yet have signoff on.
///
/// TODO(US3): implement the real authorization check via a Mostro
/// DM exchange and surface structured `AuthCheckError` variants.
async fn check_authorization(
    _client: &nostr_sdk::Client,
    _serbero_keys: &nostr_sdk::Keys,
    _mostro_pubkey: &nostr_sdk::PublicKey,
) -> std::result::Result<(), AuthCheckError> {
    Ok(())
}

/// Loop parameters. Exposed as a struct so the `#[cfg(test)]`
/// path can inject tight bounds without polluting the production
/// call signature.
#[derive(Debug, Clone, Copy)]
struct LoopConfig {
    initial_delay: Duration,
    max_interval: Duration,
    max_attempts: u32,
    max_total: Duration,
}

impl LoopConfig {
    const fn production() -> Self {
        Self {
            initial_delay: Duration::from_secs(60),
            max_interval: Duration::from_secs(3600),
            max_attempts: 24,
            max_total: Duration::from_secs(86_400),
        }
    }
}

/// Compute the next backoff delay. Pure for easy unit testing.
fn next_delay(current: Duration, cap: Duration) -> Duration {
    let doubled = current.saturating_mul(2);
    if doubled > cap {
        cap
    } else {
        doubled
    }
}

/// Run the initial authorization check. If it passes, return a
/// handle in `Authorized` state immediately. If it fails, spawn the
/// bounded revalidation loop as a background task and return a
/// handle the caller can poll; the handle moves to `Authorized` or
/// `Terminated` as the loop runs.
///
/// Backoff schedule: 60s → doubling → capped at 3600s.
/// Terminal condition: first of 24 attempts or 86 400s cumulative.
/// On terminal failure the loop emits exactly one `auth_retry_terminated`
/// event, one `error!` log, and sets the handle to `Terminated`.
///
/// SC-105: the spawned task MUST NOT reach into Phase 1/2 state.
/// Its only write surface is the `Arc<Mutex<AuthState>>` inside the
/// returned handle and the `mediation_events` table.
pub async fn ensure_authorized_or_enter_loop(
    conn: Arc<AsyncMutex<rusqlite::Connection>>,
    client: nostr_sdk::Client,
    serbero_keys: nostr_sdk::Keys,
    mostro_pubkey: nostr_sdk::PublicKey,
) -> AuthRetryHandle {
    let checker = std::sync::Arc::new(move || {
        let client = client.clone();
        let serbero_keys = serbero_keys.clone();
        let mostro_pubkey = mostro_pubkey;
        async move { check_authorization(&client, &serbero_keys, &mostro_pubkey).await }
    });
    ensure_authorized_or_enter_loop_inner(conn, checker, LoopConfig::production()).await
}

/// Generic entry point used by both the production wrapper and the
/// inline unit tests. The `checker` closure is called once per
/// attempt; returning `Ok(())` ends the loop with
/// [`AuthState::Authorized`] and returning `Err(_)` records an
/// `auth_retry_attempt` event and schedules another attempt.
async fn ensure_authorized_or_enter_loop_inner<C, Fut>(
    conn: Arc<AsyncMutex<rusqlite::Connection>>,
    checker: Arc<C>,
    config: LoopConfig,
) -> AuthRetryHandle
where
    C: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = std::result::Result<(), AuthCheckError>> + Send + 'static,
{
    let handle = AuthRetryHandle::with_state(AuthState::Unauthorized);

    // Initial check (attempt 1). A pass here short-circuits the
    // spawn — the daemon never starts the background task unless it
    // truly needs it.
    match checker().await {
        Ok(()) => {
            handle.set_state(AuthState::Authorized);
            return handle;
        }
        Err(e) => {
            let now = current_ts_secs();
            let payload = json!({ "attempt": 1, "error": e.to_string() }).to_string();
            if let Err(db_err) =
                record_auth_event(&conn, MediationEventKind::AuthRetryAttempt, &payload, now).await
            {
                // Logging only — a failed audit write must not prevent
                // the retry loop from running, otherwise one sqlite
                // glitch would mask a live auth problem.
                warn!(error = %db_err, "failed to record auth_retry_attempt (initial)");
            }
            warn!(attempt = 1, error = %e, "solver authorization check failed; entering retry loop");
        }
    }

    let state = Arc::clone(&handle.state);
    tokio::spawn(run_retry_loop(state, conn, checker, config));

    handle
}

async fn run_retry_loop<C, Fut>(
    state: Arc<Mutex<AuthState>>,
    conn: Arc<AsyncMutex<rusqlite::Connection>>,
    checker: Arc<C>,
    config: LoopConfig,
) where
    C: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = std::result::Result<(), AuthCheckError>> + Send + 'static,
{
    let mut current_delay = config.initial_delay;
    let mut cumulative = Duration::ZERO;
    // Initial check counted as attempt 1; the first loop iteration
    // runs attempt 2 after the first sleep.
    let mut attempt: u32 = 1;

    loop {
        tokio::time::sleep(current_delay).await;
        cumulative = cumulative.saturating_add(current_delay);
        attempt += 1;

        match checker().await {
            Ok(()) => {
                {
                    let mut guard = state.lock().expect("auth-retry state mutex poisoned");
                    *guard = AuthState::Authorized;
                }
                let payload = json!({ "attempt": attempt }).to_string();
                let now = current_ts_secs();
                if let Err(db_err) =
                    record_auth_event(&conn, MediationEventKind::AuthRetryRecovered, &payload, now)
                        .await
                {
                    warn!(error = %db_err, "failed to record auth_retry_recovered");
                }
                info!(attempt = attempt, "solver auth retry recovered");
                return;
            }
            Err(e) => {
                let payload = json!({ "attempt": attempt, "error": e.to_string() }).to_string();
                let now = current_ts_secs();
                if let Err(db_err) =
                    record_auth_event(&conn, MediationEventKind::AuthRetryAttempt, &payload, now)
                        .await
                {
                    warn!(error = %db_err, "failed to record auth_retry_attempt");
                }
                warn!(attempt = attempt, error = %e, "solver auth retry attempt failed");
            }
        }

        // Termination check runs AFTER the attempt + its audit row,
        // so `attempt` / `cumulative_secs` in the terminated payload
        // reflect what actually ran (no +1 drift) and we never sleep
        // one extra round-trip past the documented bounds. The
        // cumulative branch also gets to consume its full budget —
        // the last attempt inside the cumulative window always
        // runs, which is the last realistic chance to recover.
        if attempt >= config.max_attempts || cumulative >= config.max_total {
            {
                let mut guard = state.lock().expect("auth-retry state mutex poisoned");
                *guard = AuthState::Terminated;
            }
            let payload = json!({
                "attempt": attempt,
                "cumulative_secs": cumulative.as_secs(),
            })
            .to_string();
            let now = current_ts_secs();
            if let Err(db_err) = record_auth_event(
                &conn,
                MediationEventKind::AuthRetryTerminated,
                &payload,
                now,
            )
            .await
            {
                warn!(error = %db_err, "failed to record auth_retry_terminated");
            }
            error!(
                attempt = attempt,
                cumulative_secs = cumulative.as_secs(),
                "solver auth retry loop terminated without recovery"
            );
            return;
        }

        current_delay = next_delay(current_delay, config.max_interval);
    }
}

async fn record_auth_event(
    conn: &Arc<AsyncMutex<rusqlite::Connection>>,
    kind: MediationEventKind,
    payload_json: &str,
    occurred_at: i64,
) -> crate::error::Result<()> {
    let guard = conn.lock().await;
    db::mediation_events::record_event(
        &guard,
        kind,
        None,
        payload_json,
        None,
        None,
        None,
        occurred_at,
    )?;
    Ok(())
}

fn current_ts_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before UNIX_EPOCH")
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::db::migrations::run_migrations;
    use crate::db::open_in_memory;

    fn fresh_conn() -> Arc<AsyncMutex<rusqlite::Connection>> {
        let mut conn = open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        Arc::new(AsyncMutex::new(conn))
    }

    fn tight_config() -> LoopConfig {
        // Tight bounds so tokio::time::advance does not need to
        // bridge 86 400 seconds of virtual time. The backoff
        // schedule still doubles 1s → 2s → 4s (capped at 4s).
        LoopConfig {
            initial_delay: Duration::from_secs(1),
            max_interval: Duration::from_secs(4),
            max_attempts: 5,
            max_total: Duration::from_secs(3_600),
        }
    }

    /// Wait for `handle.current_state()` to reach `want` while time
    /// is paused. The spawned loop yields on every `sleep` boundary;
    /// advancing a hair past the pending sleep lets it make forward
    /// progress. We cap the number of advances so a bug does not
    /// spin the test forever.
    async fn wait_until(handle: &AuthRetryHandle, want: AuthState, max_advances: u32) {
        for _ in 0..max_advances {
            if handle.current_state() == want {
                return;
            }
            tokio::time::advance(Duration::from_secs(5)).await;
            tokio::task::yield_now().await;
        }
        panic!(
            "state never reached {want:?} (last observed: {:?})",
            handle.current_state()
        );
    }

    async fn count_events(
        conn: &Arc<AsyncMutex<rusqlite::Connection>>,
        kind: MediationEventKind,
    ) -> i64 {
        let guard = conn.lock().await;
        guard
            .query_row(
                "SELECT COUNT(*) FROM mediation_events WHERE kind = ?1",
                [kind.as_str()],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    }

    #[tokio::test]
    async fn check_succeeds_immediately_returns_authorized() {
        let conn = fresh_conn();
        let checker = Arc::new(|| async { Ok::<(), AuthCheckError>(()) });
        let handle =
            ensure_authorized_or_enter_loop_inner(Arc::clone(&conn), checker, tight_config()).await;
        assert_eq!(handle.current_state(), AuthState::Authorized);
        assert!(handle.is_authorized());
        // No audit events on the happy path.
        let attempts = count_events(&conn, MediationEventKind::AuthRetryAttempt).await;
        assert_eq!(attempts, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn loop_recovers_after_n_failures() {
        let conn = fresh_conn();
        let counter = Arc::new(AtomicU32::new(0));
        let checker = {
            let counter = Arc::clone(&counter);
            Arc::new(move || {
                let counter = Arc::clone(&counter);
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst);
                    if n < 3 {
                        Err(AuthCheckError(format!("mock failure #{n}")))
                    } else {
                        Ok(())
                    }
                }
            })
        };
        let handle =
            ensure_authorized_or_enter_loop_inner(Arc::clone(&conn), checker, tight_config()).await;
        // Initial check counts as attempt 1 and fails, so the handle
        // starts Unauthorized. The loop then drives it to Authorized
        // after the scripted failures run out.
        assert_eq!(handle.current_state(), AuthState::Unauthorized);
        wait_until(&handle, AuthState::Authorized, 20).await;

        let attempts = count_events(&conn, MediationEventKind::AuthRetryAttempt).await;
        let recovered = count_events(&conn, MediationEventKind::AuthRetryRecovered).await;
        let terminated = count_events(&conn, MediationEventKind::AuthRetryTerminated).await;
        assert_eq!(recovered, 1, "exactly one recovery event expected");
        assert_eq!(terminated, 0, "must not also emit a terminated event");
        assert!(attempts >= 3, "expected >=3 attempt events, got {attempts}");
    }

    #[tokio::test(start_paused = true)]
    async fn loop_terminates_after_max_attempts() {
        let conn = fresh_conn();
        let checker =
            Arc::new(|| async { Err::<(), _>(AuthCheckError("mock always fails".into())) });
        let cfg = tight_config();
        let handle = ensure_authorized_or_enter_loop_inner(Arc::clone(&conn), checker, cfg).await;
        assert_eq!(handle.current_state(), AuthState::Unauthorized);
        wait_until(&handle, AuthState::Terminated, 40).await;

        let terminated = count_events(&conn, MediationEventKind::AuthRetryTerminated).await;
        assert_eq!(
            terminated, 1,
            "exactly one auth_retry_terminated event must be emitted"
        );
        let recovered = count_events(&conn, MediationEventKind::AuthRetryRecovered).await;
        assert_eq!(recovered, 0);

        // Pin the post-fix semantics: exactly `max_attempts`
        // `auth_retry_attempt` rows (one per real checker call) and
        // the terminated payload's `attempt` matches the final
        // attempt that ran — no +1 drift from an extra sleep.
        let attempts = count_events(&conn, MediationEventKind::AuthRetryAttempt).await;
        assert_eq!(
            attempts as u32, cfg.max_attempts,
            "expected exactly max_attempts auth_retry_attempt rows"
        );
        let terminated_attempt: i64 = {
            let guard = conn.lock().await;
            guard
                .query_row(
                    "SELECT json_extract(payload_json, '$.attempt')
                     FROM mediation_events WHERE kind = 'auth_retry_terminated'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(
            terminated_attempt as u32, cfg.max_attempts,
            "terminated payload must report the final attempt, not max+1"
        );
    }

    #[test]
    fn backoff_doubles_up_to_cap() {
        let cap = Duration::from_secs(3600);
        // 60 → 120 → 240 → 480 → 960 → 1920 → 3600 (capped) → 3600 …
        let mut d = Duration::from_secs(60);
        let expected = [120, 240, 480, 960, 1920, 3600, 3600, 3600];
        for want in expected {
            d = next_delay(d, cap);
            assert_eq!(d, Duration::from_secs(want), "unexpected delay step");
        }
    }
}
