//! Live chat channels across restarts (`docs/spec.md` §7.2, §10).
//!
//! Chat keys are never stored. On start, every live session's channels are
//! re-derived from the stored trade pubkeys and re-subscribed from the
//! stored cursors; inner-id dedup in `messages` makes the replay harmless.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use nostr_sdk::prelude::*;

use super::inbound::{Inbox, Received, Rejected};
use crate::daemon::relays::{self, Registry};
use crate::error::{Error, Result};
use crate::store::Store;
use crate::store::sessions::{self, Session};

/// Id of a session's chat subscription.
pub fn subscription_id(session_id: &str) -> SubscriptionId {
    SubscriptionId::new(format!("serbero-chat-{session_id}"))
}

/// Serbero's chat channels with the parties of every live session.
pub struct Chats {
    client: Client,
    serbero: Keys,
    store: Arc<Mutex<Store>>,
    inbox: Mutex<Inbox>,
    registry: Registry,
}

impl Chats {
    pub fn new(
        client: Client,
        serbero: Keys,
        store: Arc<Mutex<Store>>,
        max_message_chars: usize,
        registry: Registry,
    ) -> Self {
        Self {
            client,
            serbero,
            store,
            inbox: Mutex::new(Inbox::new(max_message_chars)),
            registry,
        }
    }

    /// Re-opens every live session's channels. Returns how many sessions.
    pub async fn resume(&self) -> Result<usize> {
        let live = {
            let store = self.store.lock().map_err(|_| poisoned())?;
            sessions::list_live(store.conn())?
        };
        for session in &live {
            self.open(session).await?;
        }
        if !live.is_empty() {
            tracing::info!(sessions = live.len(), "resumed chat channels");
        }
        Ok(live.len())
    }

    /// Registers a session's two channels and subscribes to them from the
    /// stored cursors, under a per-session subscription id.
    pub async fn open(&self, session: &Session) -> Result<()> {
        let filter = self.inbox.lock().map_err(|_| poisoned())?.add_session(
            &self.serbero,
            session,
            Instant::now(),
        )?;
        let id = subscription_id(&session.session_id);
        relays::register(&self.registry, &id.to_string(), filter.clone());
        self.client
            .subscribe(filter)
            .with_id(id)
            .await
            .map_err(|e| Error::Nostr(format!("cannot subscribe to session chat: {e}")))?;
        Ok(())
    }

    /// Forgets a session's channels and closes its subscription.
    pub async fn close(&self, session_id: &str) -> Result<()> {
        self.inbox
            .lock()
            .map_err(|_| poisoned())?
            .remove_session(session_id);
        relays::unregister(&self.registry, &subscription_id(session_id).to_string());
        self.client
            .unsubscribe(&subscription_id(session_id))
            .await
            .map_err(|e| Error::Nostr(format!("cannot close session chat subscription: {e}")))?;
        Ok(())
    }

    /// Handles one relay event if it belongs to a chat channel.
    pub fn handle(&self, event: &Event) -> Result<std::result::Result<Received, Rejected>> {
        self.inbox.lock().map_err(|_| poisoned())?.handle(
            &self.store,
            event,
            Timestamp::now(),
            Instant::now(),
        )
    }

    /// Consumes relay notifications until the stream ends. Open the stream
    /// before `resume` so nothing a relay replays can be missed.
    pub async fn run(&self, mut notifications: impl StreamExt<Item = ClientNotification> + Unpin) {
        while let Some(notification) = notifications.next().await {
            let ClientNotification::Event { event, .. } = notification else {
                continue;
            };
            if event.kind != Kind::PrivateDirectMessage {
                continue;
            }
            match self.handle(&event) {
                Ok(Ok(received)) => tracing::info!(
                    session_id = %received.session_id,
                    party = %received.party,
                    attachments = received.attachments,
                    "party message received"
                ),
                Ok(Err(Rejected::UnknownAuthor)) => {}
                Ok(Err(rejected)) => {
                    tracing::debug!(event_id = %event.id, ?rejected, "chat event dropped")
                }
                Err(e) => {
                    tracing::error!(event_id = %event.id, error = %e, "cannot store chat message")
                }
            }
        }
    }
}

fn poisoned() -> Error {
    Error::Schema("store lock poisoned".into())
}
