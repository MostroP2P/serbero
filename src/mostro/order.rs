//! Facts Serbero reads from the order behind a dispute (`kind 38383`,
//! `docs/spec.md` §5.1): the fiat code and the order's creation time.

use std::time::Duration;

use mostro_core::prelude::NOSTR_ORDER_EVENT_KIND;
use nostr_sdk::prelude::*;

use crate::error::Result;
use crate::nostr::first_answer::newest_event;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderFacts {
    /// `f` tag: ISO 4217 fiat code.
    pub fiat_code: Option<String>,
    /// `published_at` tag: when the order was created (Unix seconds).
    pub published_at: Option<i64>,
}

impl OrderFacts {
    /// Reads `f` and `published_at`. Supported nodes always publish both; a
    /// missing or malformed one is logged and left unknown. A `created_at`
    /// tag (from unsupported nodes) is ignored, and so is the event's own
    /// `created_at`, which is the time of the latest revision.
    pub fn from_event(event: &Event) -> Self {
        let tag = |name: &str| {
            event.tags.iter().find_map(|t| match t.as_slice() {
                [key, value, ..] if key == name => Some(value.clone()),
                _ => None,
            })
        };
        let fiat_code = tag("f");
        let published_at = tag("published_at").and_then(|v| v.parse().ok());
        if fiat_code.is_none() || published_at.is_none() {
            tracing::warn!(event_id = %event.id, "order event lacks f or published_at; order facts unknown");
        }
        Self {
            fiat_code,
            published_at,
        }
    }
}

/// Fetches the newest revision of an order's event, without waiting for
/// slow relays. Unknown facts, not an error, if no relay has it.
pub async fn fetch(
    client: &Client,
    mostro: PublicKey,
    order_id: &str,
    timeout: Duration,
) -> Result<OrderFacts> {
    let filter = Filter::new()
        .kind(Kind::Custom(NOSTR_ORDER_EVENT_KIND))
        .author(mostro)
        .identifier(order_id);
    match newest_event(client, filter, timeout).await? {
        Some(event) => Ok(OrderFacts::from_event(&event)),
        None => {
            tracing::warn!(order_id, "order event not found; order facts unknown");
            Ok(OrderFacts::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(tags: &[&[&str]]) -> Event {
        let tags = tags.iter().map(|t| Tag::parse(t.iter().copied()).unwrap());
        EventBuilder::new(Kind::Custom(NOSTR_ORDER_EVENT_KIND), "")
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(9_999))
            .finalize(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn reads_fiat_code_and_published_at() {
        let facts = OrderFacts::from_event(&order(&[
            &["d", "o1"],
            &["f", "ARS"],
            &["published_at", "1700"],
        ]));

        assert_eq!(facts.fiat_code.as_deref(), Some("ARS"));
        assert_eq!(facts.published_at, Some(1_700));
    }

    #[test]
    fn legacy_created_at_tag_and_event_time_are_ignored() {
        let facts = OrderFacts::from_event(&order(&[
            &["d", "o1"],
            &["f", "USD"],
            &["created_at", "1600"],
        ]));

        assert_eq!(facts.published_at, None);
    }

    #[test]
    fn missing_tags_leave_facts_unknown() {
        assert_eq!(
            OrderFacts::from_event(&order(&[&["d", "o1"]])),
            OrderFacts::default()
        );
    }
}
