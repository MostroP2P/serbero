//! Parsing of Mostro dispute events (`kind 38386`, `docs/spec.md` §5.1).

use mostro_core::dispute::Status as DisputeStatus;
use mostro_core::prelude::NOSTR_DISPUTE_EVENT_KIND;
use nostr_sdk::prelude::*;

use crate::error::{Error, Result};
use crate::store::disputes::Initiator;

/// One revision of a dispute, as published by the Mostro node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisputeEvent {
    pub dispute_id: String,
    pub status: DisputeStatus,
    pub initiator: Initiator,
    /// When the dispute was opened (`created_at` tag). Older daemons omit it.
    pub opened_at: Option<i64>,
    /// The event's own `created_at`; later revisions have larger values.
    pub revision_at: i64,
}

/// Parses a dispute event, rejecting anything not authored by `mostro`.
pub fn parse(event: &Event, mostro: &PublicKey) -> Result<DisputeEvent> {
    if event.kind != Kind::Custom(NOSTR_DISPUTE_EVENT_KIND) {
        return invalid(format!(
            "expected kind {NOSTR_DISPUTE_EVENT_KIND}, got {}",
            event.kind
        ));
    }
    if &event.pubkey != mostro {
        return invalid("dispute event not authored by the configured Mostro node");
    }
    if tag_value(event, "z") != Some("dispute") {
        return invalid("missing tag z=dispute");
    }
    let dispute_id = event
        .tags
        .identifier()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::InvalidEvent("missing d tag".into()))?;
    let status = required(event, "s")?;
    let status: DisputeStatus = status
        .parse()
        .map_err(|()| Error::InvalidEvent(format!("unknown dispute status {status:?}")))?;
    let initiator = required(event, "initiator")?;
    let initiator: Initiator = initiator
        .parse()
        .map_err(|_| Error::InvalidEvent(format!("unknown initiator {initiator:?}")))?;
    let opened_at = match tag_value(event, "created_at") {
        Some(v) => Some(v.parse().map_err(|_| {
            Error::InvalidEvent(format!("created_at tag {v:?} is not a timestamp"))
        })?),
        None => None,
    };
    Ok(DisputeEvent {
        dispute_id,
        status,
        initiator,
        opened_at,
        revision_at: event.created_at.as_secs() as i64,
    })
}

/// Final statuses: a solver or the parties closed the dispute.
pub fn is_final(status: DisputeStatus) -> bool {
    !matches!(status, DisputeStatus::Initiated | DisputeStatus::InProgress)
}

/// Final statuses reached without a solver's decision.
pub fn resolved_by_parties(status: DisputeStatus) -> bool {
    matches!(
        status,
        DisputeStatus::Released | DisputeStatus::CooperativelyCanceled
    )
}

fn tag_value<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    event.tags.iter().find_map(|tag| match tag.as_slice() {
        [key, value, ..] if key == name => Some(value.as_str()),
        _ => None,
    })
}

fn required<'a>(event: &'a Event, name: &str) -> Result<&'a str> {
    tag_value(event, name).ok_or_else(|| Error::InvalidEvent(format!("missing {name} tag")))
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::InvalidEvent(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_with(keys: &Keys, kind: u16, tags: &[&[&str]]) -> Event {
        let tags = tags
            .iter()
            .map(|t| Tag::parse(t.iter().copied()).unwrap())
            .collect::<Vec<_>>();
        EventBuilder::new(Kind::Custom(kind), "")
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(1_700))
            .finalize(keys)
            .unwrap()
    }

    fn dispute(keys: &Keys, status: &str) -> Event {
        event_with(
            keys,
            38386,
            &[
                &["d", "dispute-1"],
                &["s", status],
                &["initiator", "seller"],
                &["created_at", "1600"],
                &["y", "mostro", "My Mostro"],
                &["z", "dispute"],
            ],
        )
    }

    #[test]
    fn parses_every_status() {
        let mostro = Keys::generate();
        for (text, status) in [
            ("initiated", DisputeStatus::Initiated),
            ("in-progress", DisputeStatus::InProgress),
            ("settled", DisputeStatus::Settled),
            ("seller-refunded", DisputeStatus::SellerRefunded),
            ("released", DisputeStatus::Released),
            (
                "cooperatively-canceled",
                DisputeStatus::CooperativelyCanceled,
            ),
        ] {
            let parsed = parse(&dispute(&mostro, text), &mostro.public_key()).unwrap();

            assert_eq!(parsed.status, status, "{text}");
        }
    }

    #[test]
    fn reads_all_fields() {
        let mostro = Keys::generate();

        let parsed = parse(&dispute(&mostro, "initiated"), &mostro.public_key()).unwrap();

        assert_eq!(
            parsed,
            DisputeEvent {
                dispute_id: "dispute-1".into(),
                status: DisputeStatus::Initiated,
                initiator: Initiator::Seller,
                opened_at: Some(1_600),
                revision_at: 1_700,
            }
        );
    }

    #[test]
    fn missing_created_at_tag_is_allowed() {
        let mostro = Keys::generate();
        let event = event_with(
            &mostro,
            38386,
            &[
                &["d", "x"],
                &["s", "initiated"],
                &["initiator", "buyer"],
                &["z", "dispute"],
            ],
        );

        assert_eq!(parse(&event, &mostro.public_key()).unwrap().opened_at, None);
    }

    #[test]
    fn rejects_foreign_author() {
        let mostro = Keys::generate();
        let impostor = Keys::generate();

        let err = parse(&dispute(&impostor, "initiated"), &mostro.public_key()).unwrap_err();

        assert!(err.to_string().contains("not authored"), "{err}");
    }

    #[test]
    fn rejects_malformed_events() {
        let mostro = Keys::generate();
        let cases: [(&str, u16, &[&[&str]]); 6] = [
            (
                "expected kind",
                38383,
                &[
                    &["d", "x"],
                    &["s", "initiated"],
                    &["initiator", "buyer"],
                    &["z", "dispute"],
                ],
            ),
            (
                "z=dispute",
                38386,
                &[&["d", "x"], &["s", "initiated"], &["initiator", "buyer"]],
            ),
            (
                "missing d",
                38386,
                &[
                    &["s", "initiated"],
                    &["initiator", "buyer"],
                    &["z", "dispute"],
                ],
            ),
            (
                "unknown dispute status",
                38386,
                &[
                    &["d", "x"],
                    &["s", "closed"],
                    &["initiator", "buyer"],
                    &["z", "dispute"],
                ],
            ),
            (
                "unknown initiator",
                38386,
                &[
                    &["d", "x"],
                    &["s", "initiated"],
                    &["initiator", "admin"],
                    &["z", "dispute"],
                ],
            ),
            (
                "not a timestamp",
                38386,
                &[
                    &["d", "x"],
                    &["s", "initiated"],
                    &["initiator", "buyer"],
                    &["created_at", "soon"],
                    &["z", "dispute"],
                ],
            ),
        ];
        for (expected, kind, tags) in cases {
            let err = parse(&event_with(&mostro, kind, tags), &mostro.public_key()).unwrap_err();

            assert!(err.to_string().contains(expected), "{expected}: {err}");
        }
    }

    #[test]
    fn classifies_final_statuses() {
        assert!(!is_final(DisputeStatus::Initiated));
        assert!(!is_final(DisputeStatus::InProgress));
        assert!(is_final(DisputeStatus::Settled) && !resolved_by_parties(DisputeStatus::Settled));
        assert!(is_final(DisputeStatus::SellerRefunded));
        assert!(resolved_by_parties(DisputeStatus::Released));
        assert!(resolved_by_parties(DisputeStatus::CooperativelyCanceled));
    }
}
