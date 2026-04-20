use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispute {
    pub dispute_id: String,
    pub event_id: String,
    pub mostro_pubkey: String,
    pub initiator_role: InitiatorRole,
    pub dispute_status: DisputeStatus,
    pub event_timestamp: i64,
    pub detected_at: i64,
    pub lifecycle_state: LifecycleState,
    pub assigned_solver: Option<String>,
    pub last_notified_at: Option<i64>,
    pub last_state_change: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitiatorRole {
    Buyer,
    Seller,
}

impl fmt::Display for InitiatorRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buyer => f.write_str("buyer"),
            Self::Seller => f.write_str("seller"),
        }
    }
}

impl FromStr for InitiatorRole {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "buyer" => Ok(Self::Buyer),
            "seller" => Ok(Self::Seller),
            other => Err(Error::InvalidEvent(format!(
                "invalid initiator role: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeStatus {
    Initiated,
    InProgress,
    // Phase 3 (US6) resolution states. Mirror the
    // mostro-core::dispute::Status variants carried on the kind-38386
    // replaceable event's `s` tag when a dispute resolves externally.
    // Serialized form must track the Mostro `Display` impl — verified
    // against `RESOLUTION_STATUSES` in `src/dispatcher.rs`.
    SellerRefunded,
    Settled,
    Released,
}

impl fmt::Display for DisputeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initiated => f.write_str("initiated"),
            Self::InProgress => f.write_str("in-progress"),
            Self::SellerRefunded => f.write_str("seller-refunded"),
            Self::Settled => f.write_str("settled"),
            Self::Released => f.write_str("released"),
        }
    }
}

impl FromStr for DisputeStatus {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "initiated" => Ok(Self::Initiated),
            "in-progress" => Ok(Self::InProgress),
            "seller-refunded" => Ok(Self::SellerRefunded),
            "settled" => Ok(Self::Settled),
            "released" => Ok(Self::Released),
            other => Err(Error::InvalidEvent(format!(
                "unknown dispute status: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    New,
    Notified,
    Taken,
    Waiting,
    Escalated,
    Resolved,
}

impl LifecycleState {
    pub fn can_transition_to(self, next: LifecycleState) -> bool {
        use LifecycleState::*;
        // `(New, Taken)` is intentionally allowed: it covers the race where
        // a solver takes the dispute via Mostro before Serbero has managed
        // to deliver its initial notification (or before any notification
        // succeeded and moved the dispute to `Notified`).
        matches!(
            (self, next),
            (New, Notified)
                | (New, Taken)
                | (New, Resolved)
                | (Notified, Notified)
                | (Notified, Taken)
                | (Notified, Escalated)
                | (Notified, Resolved)
                | (Taken, Waiting)
                | (Taken, Escalated)
                | (Taken, Resolved)
                | (Waiting, Taken)
                | (Waiting, Escalated)
                | (Waiting, Resolved)
                | (Escalated, Resolved)
        )
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::New => f.write_str("new"),
            Self::Notified => f.write_str("notified"),
            Self::Taken => f.write_str("taken"),
            Self::Waiting => f.write_str("waiting"),
            Self::Escalated => f.write_str("escalated"),
            Self::Resolved => f.write_str("resolved"),
        }
    }
}

impl FromStr for LifecycleState {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "new" => Ok(Self::New),
            "notified" => Ok(Self::Notified),
            "taken" => Ok(Self::Taken),
            "waiting" => Ok(Self::Waiting),
            "escalated" => Ok(Self::Escalated),
            "resolved" => Ok(Self::Resolved),
            other => Err(Error::InvalidEvent(format!(
                "unknown lifecycle state: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_transitions_pass() {
        use LifecycleState::*;
        assert!(New.can_transition_to(Notified));
        assert!(New.can_transition_to(Taken));
        assert!(Notified.can_transition_to(Notified));
        assert!(Notified.can_transition_to(Taken));
        assert!(Taken.can_transition_to(Waiting));
        assert!(Waiting.can_transition_to(Escalated));
        assert!(Escalated.can_transition_to(Resolved));
    }

    #[test]
    fn disallowed_transitions_reject() {
        use LifecycleState::*;
        assert!(!Resolved.can_transition_to(Notified));
        assert!(!Taken.can_transition_to(New));
        assert!(!Resolved.can_transition_to(Escalated));
        // Self-transitions are rejected unless explicitly allowed
        // (only Notified→Notified is permitted, for re-notification).
        assert!(!Resolved.can_transition_to(Resolved));
        assert!(!Taken.can_transition_to(Taken));
        assert!(!New.can_transition_to(New));
    }

    #[test]
    fn parse_and_display_roundtrip() {
        for s in [
            "new",
            "notified",
            "taken",
            "waiting",
            "escalated",
            "resolved",
        ] {
            let parsed: LifecycleState = s.parse().unwrap();
            assert_eq!(parsed.to_string(), s);
        }
    }

    #[test]
    fn dispute_status_roundtrip_includes_resolution_states() {
        for s in [
            "initiated",
            "in-progress",
            "seller-refunded",
            "settled",
            "released",
        ] {
            let parsed: DisputeStatus = s.parse().unwrap();
            assert_eq!(parsed.to_string(), s);
        }
    }
}
