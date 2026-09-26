//! Direct messages to solvers: Mostro protocol v2 `send-dm` messages
//! (`kind 14`, NIP-44), the format Mostro clients already read
//! (`docs/messages.md` §3).

use std::future::Future;

use mostro_core::message::{Action, Message, Payload};
use mostro_core::nip59::WrapOptions;
use mostro_core::transport::wrap_message_nip44;
use nostr_sdk::prelude::*;

use crate::error::{Error, Result};

/// Builds a signed `send-dm` event from Serbero to `solver`.
pub fn solver_dm(serbero: &Keys, solver: PublicKey, text: &str) -> Result<Event> {
    let message = Message::new_dm(
        None,
        None,
        Action::SendDm,
        Some(Payload::TextMessage(text.to_owned())),
    );
    // Serbero's key is both the identity and the author: there is no trade
    // key to hide behind, and solvers need to know who is writing.
    wrap_message_nip44(&message, serbero, serbero, solver, WrapOptions::default())
        .map_err(|e| Error::Nostr(format!("cannot build solver DM: {e}")))
}

/// Something that can deliver a text DM to a solver. The notifier depends
/// on this rather than on a relay client, so it is testable offline.
pub trait DmSender {
    fn send_dm(&self, to: PublicKey, text: &str) -> impl Future<Output = Result<()>> + Send;
}

/// Sends DMs through the relay client.
pub struct RelayDmSender {
    client: Client,
    keys: Keys,
}

impl RelayDmSender {
    pub fn new(client: Client, keys: Keys) -> Self {
        Self { client, keys }
    }
}

impl DmSender for RelayDmSender {
    async fn send_dm(&self, to: PublicKey, text: &str) -> Result<()> {
        let event = solver_dm(&self.keys, to, text)?;
        let output = self
            .client
            .send_event(&event)
            .await
            .map_err(|e| Error::Nostr(format!("cannot send DM: {e}")))?;
        if output.success.is_empty() {
            let reasons: Vec<String> = output.failed.values().cloned().collect();
            return Err(Error::Nostr(format!(
                "no relay accepted the DM: {}",
                reasons.join("; ")
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mostro_core::transport::unwrap_message_nip44;

    use super::*;

    #[test]
    fn solver_can_read_the_dm_and_see_who_sent_it() {
        let serbero = Keys::generate();
        let solver = Keys::generate();

        let event = solver_dm(&serbero, solver.public_key(), "New Mostro dispute").unwrap();

        assert_eq!(event.kind, Kind::PrivateDirectMessage);
        assert_eq!(event.pubkey, serbero.public_key());
        let opened = unwrap_message_nip44(&event, &solver).unwrap().unwrap();
        assert_eq!(opened.identity, serbero.public_key());
        let kind = opened.message.get_inner_message_kind();
        assert_eq!(kind.action, Action::SendDm);
        assert!(matches!(
            &kind.payload,
            Some(Payload::TextMessage(text)) if text == "New Mostro dispute"
        ));
    }

    #[test]
    fn other_keys_cannot_read_the_dm() {
        let serbero = Keys::generate();
        let solver = Keys::generate();
        let stranger = Keys::generate();

        let event = solver_dm(&serbero, solver.public_key(), "secret").unwrap();

        assert!(unwrap_message_nip44(&event, &stranger).unwrap().is_none());
    }
}
