//! Wiring: configuration, store, relays, the event loop, and the background
//! tasks (backlog sync, relay watcher, reminders).

pub mod relays;
pub mod sync;

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

pub use self::relays::{dispute_filter, subscribe_disputes};
pub use self::sync::{newest_revisions, sync_backlog};
use crate::config::Settings;
use crate::error::Result;
use crate::nostr::dm::{DmSender, RelayDmSender};
use crate::notifier::{Notifier, Solver};
use crate::store::Store;

/// How long startup waits for the first relay connections.
const RELAY_CONNECT_WAIT: Duration = Duration::from_secs(10);

/// How often the reminder timer looks for unattended disputes.
const REMINDER_TICK: Duration = Duration::from_secs(60);

/// Runs Serbero until Ctrl-C or SIGTERM.
pub async fn run(settings: &Settings) -> Result<()> {
    let config = &settings.config;
    let store = Arc::new(Mutex::new(Store::open(&config.serbero.db_path)?));
    let keys = crate::nostr::keys_from_secret(&settings.secrets.private_key)?;
    let mostro = crate::nostr::public_key("mostro.pubkey", &config.mostro.pubkey)?;
    let client = crate::nostr::connect(&config.mostro.relays, RELAY_CONNECT_WAIT).await?;
    let solvers = config
        .solvers
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Ok(Solver {
                pubkey: crate::nostr::public_key(&format!("solvers[{i}].pubkey"), &s.pubkey)?,
                permission: s.permission,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let sender = RelayDmSender::new(client.clone(), keys.clone());
    let notifier = Arc::new(Notifier::new(
        Arc::clone(&store),
        sender,
        solvers.clone(),
        mostro,
    ));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pubkey = %keys.public_key(),
        mostro = %mostro,
        relays = config.mostro.relays.len(),
        solvers = config.solvers.len(),
        mediation = config.mediation.enabled,
        "serbero starting"
    );

    let renotify_after = config.notify.renotify_after.as_secs() as i64;
    let (notifications, mut background) = start(&client, &notifier, mostro, renotify_after).await?;

    // Resume the chat channels of live sessions (restart-safe, §10), in the
    // background, so no chat subscription gates live disputes.
    let chats = Arc::new(crate::chat::channels::Chats::new(
        client.clone(),
        keys.clone(),
        Arc::clone(&store),
        config.mediation.max_message_chars,
        background.registry(),
    ));
    let chat_notifications = client.notifications();
    let chat_task = Arc::clone(&chats);
    background.push(tokio::spawn(async move {
        if let Err(e) = chat_task.resume().await {
            tracing::error!(error = %e, "cannot resume chat channels");
        }
        chat_task.run(chat_notifications).await;
    }));
    if config.mediation.enabled {
        start_mediation(
            settings,
            &client,
            &keys,
            mostro,
            &store,
            &notifier,
            &chats,
            &solvers,
            &mut background,
        )?;
    }
    let result = event_loop(notifications, &notifier, crate::signal::shutdown()).await;
    drop(background);
    tracing::info!("shutting down");
    crate::nostr::shutdown(&client).await;
    result
}

/// Builds the mediator, hooks it to new disputes, and checks the judge in
/// the background: until the checks pass, no dispute is taken, and a
/// failed check leaves notification as it is (`docs/spec.md` §7.1).
#[allow(clippy::too_many_arguments)]
fn start_mediation(
    settings: &Settings,
    client: &Client,
    keys: &Keys,
    mostro: PublicKey,
    store: &Arc<Mutex<Store>>,
    notifier: &Arc<Notifier<RelayDmSender>>,
    chats: &Arc<crate::chat::channels::Chats>,
    solvers: &[Solver],
    background: &mut Background,
) -> Result<()> {
    let config = &settings.config;
    let catalogs = crate::catalog::Catalogs::embedded()?;
    let mediator = Arc::new(crate::mediation::Mediator {
        client: client.clone(),
        keys: keys.clone(),
        mostro,
        store: Arc::clone(store),
        gate: notifier.outbound_gate(),
        chats: Arc::clone(chats),
        catalogs: catalogs.clone(),
        settings: crate::mediation::MediationSettings {
            enabled: true,
            default_language: config.mediation.default_language.clone(),
            languages: config.mediation.languages.clone(),
            max_rounds: config.mediation.max_rounds,
            max_message_chars: config.mediation.max_message_chars,
        },
        sender: RelayDmSender::new(client.clone(), keys.clone()),
        solvers: solvers.to_vec(),
        own_takes: notifier.own_takes(),
        judge: Default::default(),
    });
    // Party messages reach the turn task through the chat channels.
    let (forward, received) = tokio::sync::mpsc::unbounded_channel();
    chats.forward_to(forward);
    background.push(tokio::spawn(
        Arc::clone(&mediator).run_turns(received, config.mediation.quiet_period),
    ));
    let hook = Arc::clone(&mediator);
    notifier.on_new_dispute(Box::new(move |dispute_id| {
        let mediator = Arc::clone(&hook);
        let dispute_id = dispute_id.to_owned();
        tokio::spawn(async move {
            mediator.consider(&dispute_id, now()).await;
        });
    }));

    let languages: Vec<(String, String)> = config
        .mediation
        .languages
        .iter()
        .filter_map(|code| catalogs.get(code).map(|c| (code.clone(), c.name.clone())))
        .collect();
    let judge = crate::mediation::judge_from_config(
        &config.judge,
        settings.secrets.judge_api_key.as_ref().map(|k| k.expose()),
    );
    let thresholds = config.judge.active_thresholds().cloned();
    background.push(tokio::spawn(async move {
        let judge = match judge {
            Ok(judge) => judge,
            Err(reason) => {
                tracing::warn!(reason, "mediation off");
                return;
            }
        };
        let languages: Vec<crate::judge::questions::Language<'_>> = languages
            .iter()
            .map(|(code, name)| crate::judge::questions::Language { code, name })
            .collect();
        let turn = crate::judge::questions::TurnQuestions::new(&languages);
        let readiness =
            crate::mediation::check_judge(judge.as_ref(), thresholds.as_ref(), &turn).await;
        match (readiness, thresholds) {
            (crate::mediation::eligibility::Readiness::Ready, Some(thresholds)) => {
                tracing::info!(
                    judge = judge.id(),
                    question_set = turn.id(),
                    "mediation ready"
                );
                mediator.set_ready(crate::mediation::ReadyJudge {
                    judge,
                    thresholds,
                    turn,
                });
            }
            (crate::mediation::eligibility::Readiness::Off(reason), _) => {
                tracing::warn!(reason, "mediation off");
            }
            (crate::mediation::eligibility::Readiness::Ready, None) => {
                tracing::warn!("mediation off: no thresholds for the configured judge");
            }
        }
    }));
    Ok(())
}

/// Background tasks started by `start`; dropping it stops them.
pub struct Background {
    tasks: Vec<JoinHandle<()>>,
    synced: watch::Receiver<bool>,
    registry: relays::Registry,
}

impl Background {
    /// The long-lived subscriptions the relay watcher repairs.
    pub fn registry(&self) -> relays::Registry {
        self.registry.clone()
    }

    /// Adds a task that stops with the others.
    pub fn push(&mut self, task: JoinHandle<()>) {
        self.tasks.push(task);
    }

    /// Waits until the first backlog sync has been applied.
    pub async fn first_sync(&mut self) {
        let _ = self.synced.wait_for(|done| *done).await;
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Subscribes to live dispute events at once and starts the background
/// tasks. Nothing relay-bound runs before the live subscription, so a slow
/// relay can never delay a new dispute (`docs/spec.md` §6). Returns the
/// notification stream to feed `event_loop`.
pub async fn start<S: DmSender + Send + Sync + 'static>(
    client: &Client,
    notifier: &Arc<Notifier<S>>,
    mostro: PublicKey,
    renotify_after: i64,
) -> Result<(
    impl StreamExt<Item = ClientNotification> + Unpin + Send + use<S>,
    Background,
)> {
    // Opened before any REQ, so nothing a relay sends can be missed.
    let notifications = client.notifications();
    let since = Timestamp::now();
    subscribe_disputes(client, mostro, since).await?;
    let registry = relays::Registry::default();
    relays::register(
        &registry,
        relays::SUBSCRIPTION_ID,
        dispute_filter(mostro)?.since(since),
    );

    let resync = Arc::new(Notify::new());
    let (synced_tx, synced_rx) = watch::channel(false);
    let tasks = vec![
        tokio::spawn(sync::sync_loop(
            client.clone(),
            Arc::clone(notifier),
            mostro,
            Arc::clone(&resync),
            synced_tx,
        )),
        tokio::spawn(relays::watch(client.clone(), registry.clone(), resync)),
        tokio::spawn(reminder_loop(
            Arc::clone(notifier),
            renotify_after,
            synced_rx.clone(),
        )),
    ];
    Ok((
        notifications,
        Background {
            tasks,
            synced: synced_rx,
            registry,
        },
    ))
}

/// Feeds relay events to the notifier until `shutdown` completes or the
/// client shuts down. A failure handling one event is logged and does not
/// stop the loop.
pub async fn event_loop<S: DmSender>(
    mut notifications: impl StreamExt<Item = ClientNotification> + Unpin,
    notifier: &Notifier<S>,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            notification = notifications.next() => match notification {
                Some(ClientNotification::Event { event, .. }) => {
                    if let Err(e) = notifier.handle_event(&event, now()).await {
                        tracing::error!(event_id = %event.id, error = %e, "failed to handle event");
                    }
                }
                Some(ClientNotification::Shutdown) | None => return Ok(()),
                Some(_) => {}
            },
        }
    }
}

/// Runs the reminder timer in its own task, so reminder delivery never
/// delays dispute events. It waits for the first backlog sync, then checks
/// every `REMINDER_TICK`.
pub async fn reminder_loop<S: DmSender + Send + Sync + 'static>(
    notifier: Arc<Notifier<S>>,
    renotify_after: i64,
    mut synced: watch::Receiver<bool>,
) {
    let _ = synced.wait_for(|done| *done).await;
    let first = tokio::time::Instant::now() + REMINDER_TICK;
    let mut ticks = tokio::time::interval_at(first, REMINDER_TICK);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        if let Err(e) = notifier.remind(renotify_after, now()).await {
            tracing::error!(error = %e, "reminder tick failed");
        }
    }
}

/// Current Unix time in seconds.
pub fn now() -> i64 {
    Timestamp::now().as_secs() as i64
}
