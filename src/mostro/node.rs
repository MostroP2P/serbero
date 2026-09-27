//! The Mostro node's instance-info event (`kind 38385`, `docs/spec.md` §5.1).

use std::time::Duration;

use mostro_core::prelude::NOSTR_INFO_EVENT_KIND;
use nostr_sdk::prelude::*;

use crate::error::{Error, Result};
use crate::nostr::first_answer::newest_event;

/// What Serbero needs to know about the node before talking to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    /// `protocol_version` tag; a missing tag means the node predates it.
    pub protocol_version: Option<String>,
    /// `pow` tag: proof-of-work difficulty for ordinary events.
    pub pow: u8,
    /// `pow_first_contact` tag, if published.
    pub pow_first_contact: Option<u8>,
}

impl NodeInfo {
    /// Serbero speaks only Mostro protocol v2.
    pub fn speaks_v2(&self) -> bool {
        self.protocol_version.as_deref() == Some("2")
    }

    /// Difficulty to mine Serbero's messages to the node with. Serbero may be
    /// a sender the node does not associate with an active dispute, so it
    /// pays the first-contact price when the node publishes one.
    pub fn pow_for_serbero(&self) -> u8 {
        self.pow_first_contact.unwrap_or(0).max(self.pow)
    }

    /// Reads the tags Serbero uses from an instance-info event.
    pub fn from_event(event: &Event) -> Result<Self> {
        let tag = |name: &str| {
            event.tags.iter().find_map(|t| match t.as_slice() {
                [key, value, ..] if key == name => Some(value.clone()),
                _ => None,
            })
        };
        let difficulty = |name: &str| -> Result<Option<u8>> {
            tag(name)
                .map(|v| {
                    v.parse::<u8>().map_err(|_| {
                        Error::InvalidEvent(format!("{name} tag {v:?} is not a difficulty"))
                    })
                })
                .transpose()
        };
        Ok(Self {
            protocol_version: tag("protocol_version"),
            pow: difficulty("pow")?.unwrap_or(0),
            pow_first_contact: difficulty("pow_first_contact")?,
        })
    }
}

/// Fetches the node's newest instance-info event, without waiting for slow
/// relays. `None` if no relay has it.
pub async fn fetch(
    client: &Client,
    mostro: PublicKey,
    timeout: Duration,
) -> Result<Option<NodeInfo>> {
    let filter = Filter::new()
        .kind(Kind::Custom(NOSTR_INFO_EVENT_KIND))
        .author(mostro)
        .identifier(mostro.to_hex());
    newest_event(client, filter, timeout)
        .await?
        .map(|event| NodeInfo::from_event(&event))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(tags: &[&[&str]]) -> Event {
        let tags = tags.iter().map(|t| Tag::parse(t.iter().copied()).unwrap());
        EventBuilder::new(Kind::Custom(NOSTR_INFO_EVENT_KIND), "")
            .tags(tags)
            .finalize(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn reads_version_and_difficulties() {
        let event = info(&[
            &["protocol_version", "2"],
            &["pow", "4"],
            &["pow_first_contact", "12"],
        ]);

        let node = NodeInfo::from_event(&event).unwrap();

        assert!(node.speaks_v2());
        assert_eq!(node.pow_for_serbero(), 12);
    }

    #[test]
    fn missing_protocol_version_is_not_v2() {
        let node = NodeInfo::from_event(&info(&[&["pow", "0"]])).unwrap();

        assert!(!node.speaks_v2());
        assert_eq!(node.pow_for_serbero(), 0);
    }

    #[test]
    fn version_1_is_not_v2() {
        assert!(
            !NodeInfo::from_event(&info(&[&["protocol_version", "1"]]))
                .unwrap()
                .speaks_v2()
        );
    }

    #[test]
    fn malformed_difficulty_is_rejected() {
        assert!(NodeInfo::from_event(&info(&[&["pow", "hard"]])).is_err());
    }
}
