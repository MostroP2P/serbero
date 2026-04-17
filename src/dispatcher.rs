use nostr_sdk::{Event, Kind, TagKind};
use tracing::{info, warn};

use crate::error::Result;
use crate::handlers::dispute_detected::HandlerContext;
use crate::handlers::{dispute_detected, dispute_updated};

const DISPUTE_EVENT_KIND: u16 = 38386;

pub async fn dispatch(ctx: &HandlerContext, event: &Event) -> Result<()> {
    if event.kind != Kind::Custom(DISPUTE_EVENT_KIND) {
        warn!(
            kind = ?event.kind,
            event_id = %event.id,
            "dispatcher: ignoring non-dispute event (kind != 38386)"
        );
        return Ok(());
    }

    let status = status_tag(event);
    match status.as_deref() {
        Some("initiated") => {
            info!(event_id = %event.id, "dispatcher: routing to dispute_detected (s=initiated)");
            dispute_detected::handle(ctx, event).await
        }
        Some("in-progress") => {
            info!(event_id = %event.id, "dispatcher: routing to dispute_updated (s=in-progress)");
            dispute_updated::handle(ctx, event).await
        }
        Some(other) => {
            warn!(
                status = other,
                event_id = %event.id,
                "dispatcher: skipping dispute event with unrecognised s= value"
            );
            Ok(())
        }
        None => {
            warn!(
                event_id = %event.id,
                tag_count = event.tags.len(),
                tags = ?event.tags,
                "dispatcher: dispute event has no `s` tag — cannot route"
            );
            Ok(())
        }
    }
}

fn status_tag(event: &Event) -> Option<String> {
    // NIP-01 single-letter tags are case-sensitive — only match lowercase `s`.
    event
        .tags
        .iter()
        .find(|t| match t.kind() {
            TagKind::SingleLetter(slt) => slt.as_char() == 's',
            _ => false,
        })
        .and_then(|t| t.content().map(|s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::{Alphabet, EventBuilder, Keys, SingleLetterTag, Tag, TagKind};

    fn build_dispute_event(
        keys: &Keys,
        dispute_id: &str,
        status: &str,
        mostro_pubkey: &str,
        initiator: &str,
    ) -> Event {
        let tags = vec![
            Tag::identifier(dispute_id),
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::S)),
                [status],
            ),
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                ["dispute"],
            ),
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Y)),
                [mostro_pubkey],
            ),
            Tag::custom(TagKind::Custom("initiator".into()), [initiator]),
        ];
        EventBuilder::new(Kind::Custom(DISPUTE_EVENT_KIND), "")
            .tags(tags)
            .sign_with_keys(keys)
            .unwrap()
    }

    #[test]
    fn status_tag_extraction() {
        let keys = Keys::generate();
        let ev = build_dispute_event(&keys, "d1", "initiated", "mostro_pk", "buyer");
        assert_eq!(status_tag(&ev).as_deref(), Some("initiated"));
        let ev2 = build_dispute_event(&keys, "d2", "in-progress", "mostro_pk", "seller");
        assert_eq!(status_tag(&ev2).as_deref(), Some("in-progress"));
    }

    #[test]
    fn status_tag_returns_none_when_missing() {
        let keys = Keys::generate();
        let ev = EventBuilder::new(Kind::Custom(1), "noise")
            .sign_with_keys(&keys)
            .unwrap();
        assert!(status_tag(&ev).is_none());
    }

    #[test]
    fn status_tag_is_case_sensitive() {
        let keys = Keys::generate();
        // Build an event with uppercase `S` tag — must NOT be picked up.
        let ev = EventBuilder::new(Kind::Custom(DISPUTE_EVENT_KIND), "")
            .tags(vec![Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::uppercase(Alphabet::S)),
                ["initiated"],
            )])
            .sign_with_keys(&keys)
            .unwrap();
        assert!(status_tag(&ev).is_none());
    }
}
