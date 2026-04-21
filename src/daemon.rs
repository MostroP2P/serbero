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
    // any Phase 3 bring-up step fails. When both the prompt bundle
    // and the reasoning provider come up successfully we keep them
    // in-scope as `Arc`s so the engine task spawned below can share
    // them with no extra clones.
    let phase3_runtime: Option<Phase3Runtime> =
        if config.mediation.enabled && config.reasoning.enabled {
            match phase3_bring_up(&config).await {
                Some(rt) => {
                    info!(
                        prompt_bundle_id = %rt.bundle.id,
                        policy_hash = %rt.bundle.policy_hash,
                        "Phase 3 mediation is fully configured; engine task will be spawned"
                    );
                    Some(rt)
                }
                None => {
                    info!("Phase 3 partially configured; mediation will stay disabled this run");
                    None
                }
            }
        } else if config.mediation.enabled && !config.reasoning.enabled {
            // Mediation enabled but reasoning is off: do not touch
            // the prompt bundle or the provider factory — just log
            // and keep Phase 1/2 running.
            info!(
                "Phase 3 mediation enabled but [reasoning].enabled = false; \
                 skipping bring-up (provider + bundle not initialized this run)"
            );
            None
        } else {
            debug!("Phase 3 mediation disabled by configuration");
            None
        };
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

    // `HandlerContext` is constructed below, after Phase 3 bring-up,
    // so the event-driven start path can receive the fully-populated
    // `Phase3HandlerCtx` when mediation is configured.

    let renotif_handle = spawn_renotification_timer(
        Arc::clone(&conn),
        client.clone(),
        config.solvers.clone(),
        config.timeouts.renotification_seconds,
        config.timeouts.renotification_check_interval_seconds,
    );

    // Engine task (spawned only when Phase 3 is fully configured).
    // We derive a fresh `Keys` from the same private key the nostr
    // client was built with — the client holds them internally but
    // does not expose them, and the mediation chat path needs a
    // direct `&Keys` handle (it signs inner events with the
    // sender's keys, not via the client signer).
    //
    // The Phase 3 bring-up block is also where we build the
    // `Phase3HandlerCtx` used by the event-driven start path
    // (FR-121). The engine task and the handler share the same
    // `session_key_cache` so sessions opened by either path are
    // visible to the ingest tick on the next cycle.
    let mut handler_phase3: Option<Arc<crate::mediation::Phase3HandlerCtx>> = None;
    let engine_handle: Option<JoinHandle<()>> = if let Some(rt) = phase3_runtime {
        let engine_keys = match nostr_sdk::Keys::parse(&config.serbero.private_key) {
            Ok(k) => k,
            Err(e) => {
                return Err(Error::InvalidKey(format!(
                    "failed to parse serbero private key for engine task: {e}"
                )))
            }
        };
        // T043: run the initial authorization check and get a
        // handle. US1's stub `check_authorization` always returns
        // `Ok(())`, so the handle reports `Authorized` and no retry
        // task is spawned. When US3 swaps the stub for the real
        // Mostro DM exchange the retry task will spawn itself here
        // without any daemon-side change.
        let auth_handle = crate::mediation::auth_retry::ensure_authorized_or_enter_loop(
            Arc::clone(&conn),
            client.clone(),
            engine_keys.clone(),
            mostro_pubkey,
        )
        .await;

        // Shared session-key cache: one `Arc` used by both the
        // handler (event-driven start) and the engine task (tick
        // retry + ingest).
        let session_key_cache: crate::mediation::SessionKeyCache =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

        handler_phase3 = Some(Arc::new(crate::mediation::Phase3HandlerCtx {
            serbero_keys: engine_keys.clone(),
            mostro_pubkey,
            reasoning: Arc::clone(&rt.reasoning),
            prompt_bundle: Arc::clone(&rt.bundle),
            provider_name: config.reasoning.provider.clone(),
            model_name: config.reasoning.model.clone(),
            auth_handle: auth_handle.clone(),
            session_key_cache: Arc::clone(&session_key_cache),
            solvers: config.solvers.clone(),
        }));

        let engine_conn = Arc::clone(&conn);
        let engine_client = client.clone();
        let engine_mostro_pk = mostro_pubkey;
        let engine_bundle = rt.bundle;
        let engine_reasoning = rt.reasoning;
        let engine_provider_name = config.reasoning.provider.clone();
        let engine_model_name = config.reasoning.model.clone();
        let engine_auth_handle = auth_handle.clone();
        let engine_solvers = config.solvers.clone();
        let engine_mediation_cfg = config.mediation.clone();
        let engine_key_cache = session_key_cache;
        Some(tokio::spawn(async move {
            crate::mediation::run_engine(
                engine_conn,
                engine_client,
                engine_keys,
                engine_mostro_pk,
                engine_reasoning,
                engine_bundle,
                engine_provider_name,
                engine_model_name,
                engine_auth_handle,
                engine_solvers,
                engine_mediation_cfg,
                engine_key_cache,
            )
            .await
        }))
    } else {
        None
    };

    let ctx = Arc::new(HandlerContext {
        conn: conn.clone(),
        client: client.clone(),
        solvers: config.solvers.clone(),
        phase3: handler_phase3,
    });

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
    if let Some(h) = engine_handle {
        h.abort();
        let _ = h.await;
    }

    Ok(())
}

/// Successful Phase 3 bring-up artifacts. Built when the config has
/// mediation enabled AND the prompt bundle loads AND the reasoning
/// provider builds AND its startup health check passes.
struct Phase3Runtime {
    bundle: Arc<crate::prompts::PromptBundle>,
    reasoning: Arc<dyn crate::reasoning::ReasoningProvider>,
}

async fn phase3_bring_up(config: &Config) -> Option<Phase3Runtime> {
    let bundle = match crate::prompts::load_bundle(&config.prompts) {
        Ok(b) => {
            info!(
                prompt_bundle_id = %b.id,
                policy_hash = %b.policy_hash,
                "Phase 3 prompt bundle loaded"
            );
            Arc::new(b)
        }
        Err(e) => {
            error!(
                error = %e,
                "Phase 3 prompt bundle failed to load; mediation will stay disabled this run"
            );
            return None;
        }
    };

    let reasoning = match crate::reasoning::build_provider(&config.reasoning) {
        Ok(p) => p,
        Err(e) => {
            error!(
                provider = %config.reasoning.provider,
                error = %e,
                "Phase 3 reasoning provider could not be built; \
                 mediation will stay disabled this run"
            );
            return None;
        }
    };
    if let Err(e) = crate::reasoning::health::run_startup_health_check(&*reasoning).await {
        // SC-105: a Phase 3 health-check failure MUST NOT exit the
        // daemon. Returning `None` here leaves `mediation.enabled`
        // effectively off for this run while Phase 1/2 detection and
        // solver notification continue unaffected in the caller.
        error!(
            provider = %config.reasoning.provider,
            model = %config.reasoning.model,
            api_base = %config.reasoning.api_base,
            error = %e,
            "Phase 3 reasoning health check failed; mediation disabled for this run \
             (Phase 1/2 detection and notification continue unaffected)"
        );
        return None;
    }

    Some(Phase3Runtime { bundle, reasoning })
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
