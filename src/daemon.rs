use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::{PublicKey, RelayPoolNotification, Timestamp};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::db;
use crate::dispatcher;
use crate::error::{Error, Result};
use crate::handlers::dispute_detected::{current_timestamp, HandlerContext};
use crate::models::{Config, LifecycleState, NotificationStatus, NotificationType};
use crate::nostr::{build_client, dispute_filter, send_gift_wrap_notification};

/// Small buffer (seconds) subtracted from the last-seen event timestamp
/// when computing the `since` filter on warm restart. Accounts for
/// clock skew between Mostro, relays, and Serbero so we do not miss
/// events published near the previous shutdown moment.
const SINCE_SKEW_SECONDS: u64 = 60;

pub async fn run(config: Config) -> Result<()> {
    run_with_shutdown(config, wait_for_shutdown_signal()).await
}

/// Resolve shutdown on either SIGINT (Ctrl-C) or, on Unix, SIGTERM.
/// On non-Unix targets only ctrl_c is awaited.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "failed to install SIGTERM handler; only SIGINT will stop the daemon");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received SIGINT (Ctrl-C)");
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM");
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn run_with_shutdown<F>(config: Config, shutdown: F) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    log_startup_summary(&config);

    let mostro_pubkey = PublicKey::parse(&config.mostro.pubkey)
        .map_err(|e| Error::InvalidKey(format!("invalid mostro pubkey: {e}")))?;

    let mut conn = db::open_connection(&config.serbero.db_path)?;
    db::migrations::run_migrations(&mut conn)?;
    info!(db_path = %config.serbero.db_path, "database opened and migrations applied");

    // Resume from just before the last-seen Mostro dispute event so we
    // do not miss events that arrived while Serbero was offline. Fall
    // back to "now" on a cold start (empty DB) to avoid replaying the
    // full relay history.
    let since = match db::disputes::max_event_timestamp(&conn)? {
        Some(ts) => {
            let resume = (ts as u64).saturating_sub(SINCE_SKEW_SECONDS);
            info!(
                last_seen_event_ts = ts,
                resume_since_ts = resume,
                skew_seconds = SINCE_SKEW_SECONDS,
                "resuming Nostr subscription from last-seen event timestamp (minus skew buffer)"
            );
            Timestamp::from_secs(resume)
        }
        None => {
            info!("no prior disputes recorded; subscribing from current time");
            Timestamp::now()
        }
    };

    let conn = Arc::new(Mutex::new(conn));

    if config.solvers.is_empty() {
        warn!("no solvers configured; disputes will be persisted but no notifications sent");
    } else {
        info!(
            solver_count = config.solvers.len(),
            "configured solvers ready to be notified"
        );
    }

    // ---- Phase 3 bring-up (gated, non-fatal on failure) ------------
    //
    // Phase 3 is additive: Phase 1/2 MUST remain fully operational if
    // any Phase 3 bring-up step fails. The engine task (US1+) is NOT
    // spawned here yet — only the prompt bundle is loaded (to confirm
    // the files exist and the hash is stable) and the reasoning
    // provider is built + health-checked. Real mediation wiring is
    // deferred to US1 per the Option A scope for this phase.
    //
    // `phase3_ready` is the durable state the future engine spawn
    // site will consume. It is `true` iff all three bring-up steps
    // succeeded (bundle loaded AND provider built AND health check
    // passed). Inferring readiness from log lines later would be
    // brittle; this flag is the single source of truth.
    let mut phase3_ready = false;
    if config.mediation.enabled {
        let bundle_ok = match crate::prompts::load_bundle(&config.prompts) {
            Ok(bundle) => {
                info!(
                    prompt_bundle_id = %bundle.id,
                    policy_hash = %bundle.policy_hash,
                    "Phase 3 prompt bundle loaded"
                );
                true
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Phase 3 prompt bundle failed to load; mediation will stay disabled this run"
                );
                false
            }
        };

        let provider_ok = match crate::reasoning::build_provider(&config.reasoning) {
            Ok(provider) => {
                match crate::reasoning::health::run_startup_health_check(&*provider).await {
                    Ok(()) => true,
                    Err(e) => {
                        warn!(
                            provider = %config.reasoning.provider,
                            model = %config.reasoning.model,
                            api_base = %config.reasoning.api_base,
                            error = %e,
                            "Phase 3 reasoning provider health check failed; \
                             mediation will stay disabled this run"
                        );
                        false
                    }
                }
            }
            Err(e) => {
                warn!(
                    provider = %config.reasoning.provider,
                    error = %e,
                    "Phase 3 reasoning provider could not be built; \
                     mediation will stay disabled this run"
                );
                false
            }
        };

        phase3_ready = bundle_ok && provider_ok;
        if phase3_ready {
            info!(
                "Phase 3 mediation is fully configured but the engine task is NOT yet spawned \
                 — US1+ pending. See src/chat/ and src/mediation/ module headers for the \
                 verification points still open."
            );
        } else {
            info!(
                bundle_ok,
                provider_ok,
                "Phase 3 is only partially configured; mediation will stay disabled this run"
            );
        }
    } else {
        debug!("Phase 3 mediation disabled by configuration");
    }
    // The engine spawn site will read `phase3_ready` when US1 lands.
    // Silence the unused warning until then.
    let _ = phase3_ready;
    // ----------------------------------------------------------------

    let client = build_client(&config).await?;

    let filter = dispute_filter(&mostro_pubkey, since);
    info!(
        kind = 38386,
        author = %mostro_pubkey.to_hex(),
        since_ts = since.as_secs(),
        "subscribing to dispute events (kind=38386, author=<mostro_pubkey>)"
    );
    let sub = client
        .subscribe(filter, None)
        .await
        .map_err(|e| Error::Nostr(format!("failed to subscribe: {e}")))?;
    info!(
        subscription_id = %sub.val,
        success_relays = ?sub.success,
        failed_relays = ?sub.failed,
        "subscription delivered to relay pool"
    );

    let ctx = Arc::new(HandlerContext {
        conn: conn.clone(),
        client: client.clone(),
        solvers: config.solvers.clone(),
    });

    let renotif_handle = spawn_renotification_timer(
        Arc::clone(&conn),
        client.clone(),
        config.solvers.clone(),
        config.timeouts.renotification_seconds,
        config.timeouts.renotification_check_interval_seconds,
    );

    let notif_ctx = Arc::clone(&ctx);
    let notification_future = client.handle_notifications(move |notif| {
        let ctx = Arc::clone(&notif_ctx);
        async move {
            match notif {
                RelayPoolNotification::Event {
                    relay_url,
                    subscription_id,
                    event,
                } => {
                    info!(
                        relay = %relay_url,
                        subscription_id = %subscription_id,
                        event_id = %event.id,
                        event_kind = ?event.kind,
                        event_author = %event.pubkey.to_hex(),
                        event_tag_count = event.tags.len(),
                        "nostr event received"
                    );
                    if let Err(e) = dispatcher::dispatch(&ctx, &event).await {
                        error!(error = %e, event_id = %event.id, "dispatcher error");
                    }
                }
                RelayPoolNotification::Message { relay_url, message } => {
                    debug!(
                        relay = %relay_url,
                        message = ?message,
                        "relay message"
                    );
                }
                RelayPoolNotification::Shutdown => {
                    info!("relay pool shutdown notification received");
                }
            }
            Ok(false)
        }
    });

    info!("entering notification loop — awaiting Mostro dispute events");

    tokio::select! {
        res = notification_future => {
            if let Err(e) = res {
                error!(error = %e, "handle_notifications exited with error");
            }
        }
        _ = shutdown => {
            info!("shutdown signal received, stopping daemon");
        }
    }

    renotif_handle.abort();
    let _ = renotif_handle.await;

    Ok(())
}

fn log_startup_summary(config: &Config) {
    info!(
        mostro_pubkey = %config.mostro.pubkey,
        db_path = %config.serbero.db_path,
        relay_count = config.relays.len(),
        solver_count = config.solvers.len(),
        renotification_seconds = config.timeouts.renotification_seconds,
        renotification_check_interval_seconds = config.timeouts.renotification_check_interval_seconds,
        "loaded config"
    );
    for relay in &config.relays {
        info!(url = %relay.url, "configured relay");
    }
    for (i, solver) in config.solvers.iter().enumerate() {
        info!(
            idx = i,
            pubkey = %solver.pubkey,
            permission = ?solver.permission,
            "configured solver (Phase 1/2: notified regardless of permission)"
        );
    }
}

fn spawn_renotification_timer(
    conn: Arc<Mutex<rusqlite::Connection>>,
    client: nostr_sdk::Client,
    solvers: Vec<crate::models::SolverConfig>,
    renotification_seconds: u64,
    check_interval_seconds: u64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(check_interval_seconds.max(1)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) =
                run_renotification_cycle(&conn, &client, &solvers, renotification_seconds).await
            {
                warn!(error = %e, "renotification cycle failed");
            }
        }
    })
}

async fn run_renotification_cycle(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    client: &nostr_sdk::Client,
    solvers: &[crate::models::SolverConfig],
    renotification_seconds: u64,
) -> Result<()> {
    let now = current_timestamp();
    let cutoff = now - renotification_seconds as i64;
    let unattended = {
        let conn = conn.lock().await;
        db::state_transitions::list_unattended_disputes(&conn, cutoff)?
    };
    if unattended.is_empty() {
        debug!("renotification_tick: no unattended disputes");
        return Ok(());
    }
    info!(
        count = unattended.len(),
        "renotification_tick: unattended disputes found"
    );

    for dispute in unattended {
        if dispute.lifecycle_state != LifecycleState::Notified {
            continue;
        }
        let elapsed = now - dispute.event_timestamp;
        let message = format!(
            "Mostro dispute is still unattended.\n\
             dispute_id: {}\n\
             lifecycle_state: {}\n\
             time_elapsed_seconds: {}",
            dispute.dispute_id, dispute.lifecycle_state, elapsed
        );
        let mut sent_any = false;
        for solver in solvers {
            let pk = match nostr_sdk::PublicKey::parse(&solver.pubkey) {
                Ok(pk) => pk,
                Err(e) => {
                    let conn = conn.lock().await;
                    db::notifications::record_notification_logged(
                        &conn,
                        &dispute.dispute_id,
                        &solver.pubkey,
                        current_timestamp(),
                        NotificationStatus::Failed,
                        Some(&format!("invalid pubkey: {e}")),
                        NotificationType::ReNotification,
                    );
                    continue;
                }
            };
            match send_gift_wrap_notification(client, &pk, &message).await {
                Ok(()) => {
                    sent_any = true;
                    info!(
                        dispute_id = %dispute.dispute_id,
                        solver = %solver.pubkey,
                        "renotification_sent"
                    );
                    let conn = conn.lock().await;
                    db::notifications::record_notification_logged(
                        &conn,
                        &dispute.dispute_id,
                        &solver.pubkey,
                        current_timestamp(),
                        NotificationStatus::Sent,
                        None,
                        NotificationType::ReNotification,
                    );
                }
                Err(e) => {
                    warn!(
                        dispute_id = %dispute.dispute_id,
                        solver = %solver.pubkey,
                        error = %e,
                        "renotification_failed"
                    );
                    let conn = conn.lock().await;
                    db::notifications::record_notification_logged(
                        &conn,
                        &dispute.dispute_id,
                        &solver.pubkey,
                        current_timestamp(),
                        NotificationStatus::Failed,
                        Some(&e.to_string()),
                        NotificationType::ReNotification,
                    );
                }
            }
        }

        // Only advance `last_notified_at` when at least one solver
        // actually received the re-notification. If every send failed
        // we want the next timer tick to retry instead of silently
        // suppressing the dispute for another full window.
        if sent_any {
            let conn = conn.lock().await;
            if let Err(e) = db::disputes::update_last_notified_at(&conn, &dispute.dispute_id, now) {
                warn!(error = %e, "failed to update last_notified_at after re-notification");
            }
        } else {
            warn!(
                dispute_id = %dispute.dispute_id,
                "all re-notification sends failed; keeping last_notified_at so the next tick retries"
            );
        }
    }

    Ok(())
}
