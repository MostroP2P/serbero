//! Taking a dispute as a solver (`docs/spec.md` §5.1, §7.2).
//!
//! Serbero sends `admin-take-dispute` over Mostro protocol v2 and waits for
//! the node's `admin-took-dispute` (with `SolverDisputeInfo`) or `cant-do`,
//! matched by `request_id`.

use std::time::Duration;

use mostro_core::dispute::SolverDisputeInfo;
use mostro_core::message::{Action, Message, Payload};
use mostro_core::transport::{WrapOptions, unwrap_message_nip44, wrap_message_nip44};
use nostr_sdk::prelude::*;
use uuid::Uuid;

use crate::error::{Error, Result};

/// Id of the subscription to the node's replies to Serbero.
pub const DAEMON_SUBSCRIPTION_ID: &str = "serbero-daemon";

/// Why a take did not succeed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TakeError {
    #[error("the node did not answer within {0:?}")]
    Timeout(Duration),
    #[error("the node refused the take: {0}")]
    Refused(String),
    #[error("the node answered without dispute information")]
    MissingInfo,
    #[error("{0}")]
    Transport(String),
}

impl From<Error> for TakeError {
    fn from(e: Error) -> Self {
        Self::Transport(e.to_string())
    }
}

/// Filter for protocol v2 messages from the node to Serbero.
pub fn daemon_replies_filter(mostro: PublicKey, serbero: PublicKey, since: Timestamp) -> Filter {
    Filter::new()
        .kind(Kind::PrivateDirectMessage)
        .author(mostro)
        .pubkey(serbero)
        .since(since)
}

/// Builds the signed `admin-take-dispute` event for `dispute_id`.
pub fn take_request(
    serbero: &Keys,
    mostro: PublicKey,
    dispute_id: Uuid,
    request_id: u64,
    pow: u8,
) -> Result<Event> {
    let message = Message::new_dispute(
        Some(dispute_id),
        Some(request_id),
        None,
        Action::AdminTakeDispute,
        None,
    );
    let opts = WrapOptions {
        pow,
        ..WrapOptions::default()
    };
    wrap_message_nip44(&message, serbero, serbero, mostro, opts)
        .map_err(|e| Error::Nostr(format!("cannot build admin-take-dispute: {e}")))
}

/// What a reply from the node means for a pending take.
fn interpret(
    message: &Message,
    request_id: u64,
) -> Option<std::result::Result<SolverDisputeInfo, TakeError>> {
    let kind = message.get_inner_message_kind();
    if kind.request_id != Some(request_id) {
        return None;
    }
    Some(match (message, &kind.action, &kind.payload) {
        (Message::CantDo(_), _, payload) | (_, Action::CantDo, payload) => {
            Err(TakeError::Refused(format!("{payload:?}")))
        }
        (_, Action::AdminTookDispute, Some(Payload::Dispute(_, Some(info)))) => Ok(info.clone()),
        (_, Action::AdminTookDispute, _) => Err(TakeError::MissingInfo),
        _ => return None,
    })
}

/// Takes `dispute_id` and returns the node's `SolverDisputeInfo`.
pub async fn take_dispute(
    client: &Client,
    serbero: &Keys,
    mostro: PublicKey,
    dispute_id: Uuid,
    pow: u8,
    timeout: Duration,
) -> std::result::Result<SolverDisputeInfo, TakeError> {
    // Listen before asking, so the reply cannot slip past (AGENTS.md, relays).
    let mut notifications = client.notifications();
    let since = Timestamp::now() - Duration::from_secs(60);
    client
        .subscribe(daemon_replies_filter(mostro, serbero.public_key(), since))
        .with_id(SubscriptionId::new(DAEMON_SUBSCRIPTION_ID))
        .await
        .map_err(|e| TakeError::Transport(format!("cannot subscribe to node replies: {e}")))?;

    let request_id = Uuid::new_v4().as_u64_pair().0;
    let request = take_request(serbero, mostro, dispute_id, request_id, pow)?;
    let output = client
        .send_event(&request)
        .await
        .map_err(|e| TakeError::Transport(format!("cannot send admin-take-dispute: {e}")))?;
    if output.success.is_empty() {
        return Err(TakeError::Transport(
            "no relay accepted admin-take-dispute".into(),
        ));
    }
    tracing::info!(%dispute_id, request_id, "admin-take-dispute sent");

    let reply = tokio::time::timeout(timeout, async {
        while let Some(notification) = notifications.next().await {
            let ClientNotification::Event { event, .. } = notification else {
                continue;
            };
            if event.kind != Kind::PrivateDirectMessage || event.pubkey != mostro {
                continue;
            }
            match unwrap_message_nip44(&event, serbero) {
                Ok(Some(unwrapped)) => {
                    if let Some(result) = interpret(&unwrapped.message, request_id) {
                        return result;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::debug!(event_id = %event.id, error = %e, "unreadable node reply")
                }
            }
        }
        Err(TakeError::Transport("notification stream ended".into()))
    })
    .await;
    reply.unwrap_or(Err(TakeError::Timeout(timeout)))
}

#[cfg(test)]
mod tests {
    use mostro_core::error::CantDoReason;

    use super::*;

    fn reply(action: Action, payload: Option<Payload>, request_id: u64) -> Message {
        Message::new_dispute(Some(Uuid::nil()), Some(request_id), None, action, payload)
    }

    #[test]
    fn took_dispute_with_info_succeeds() {
        let info = SolverDisputeInfo {
            fiat_amount: 50_000,
            ..Default::default()
        };
        let message = reply(
            Action::AdminTookDispute,
            Some(Payload::Dispute(Uuid::nil(), Some(info))),
            7,
        );

        let result = interpret(&message, 7).unwrap().unwrap();

        assert_eq!(result.fiat_amount, 50_000);
    }

    #[test]
    fn replies_to_other_requests_are_ignored() {
        let message = reply(Action::AdminTookDispute, None, 8);

        assert!(interpret(&message, 7).is_none());
    }

    #[test]
    fn cant_do_is_a_refusal() {
        let message = Message::cant_do(
            Some(Uuid::nil()),
            Some(7),
            Some(Payload::CantDo(Some(CantDoReason::NotAllowedByStatus))),
        );

        let result = interpret(&message, 7).unwrap();

        assert!(matches!(result, Err(TakeError::Refused(r)) if r.contains("NotAllowedByStatus")));
    }

    #[test]
    fn took_dispute_without_info_is_an_error() {
        let message = reply(Action::AdminTookDispute, None, 7);

        assert!(matches!(
            interpret(&message, 7),
            Some(Err(TakeError::MissingInfo))
        ));
    }

    #[test]
    fn take_request_is_readable_by_the_node() {
        let serbero = Keys::generate();
        let mostro = Keys::generate();
        let dispute = Uuid::new_v4();

        let event = take_request(&serbero, mostro.public_key(), dispute, 42, 0).unwrap();

        let opened = unwrap_message_nip44(&event, &mostro).unwrap().unwrap();
        let kind = opened.message.get_inner_message_kind();
        assert_eq!(kind.action, Action::AdminTakeDispute);
        assert_eq!(kind.id, Some(dispute));
        assert_eq!(kind.request_id, Some(42));
        assert_eq!(opened.identity, serbero.public_key());
    }
}
