use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRecord {
    pub id: i64,
    pub dispute_id: String,
    pub solver_pubkey: String,
    pub sent_at: i64,
    pub status: NotificationStatus,
    pub error_message: Option<String>,
    pub notif_type: NotificationType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationStatus {
    Sent,
    Failed,
}

impl fmt::Display for NotificationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sent => f.write_str("sent"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

impl FromStr for NotificationStatus {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "sent" => Ok(Self::Sent),
            "failed" => Ok(Self::Failed),
            other => Err(Error::InvalidEvent(format!(
                "unknown notification status: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationType {
    Initial,
    ReNotification,
    Assignment,
    Escalation,
}

impl fmt::Display for NotificationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initial => f.write_str("initial"),
            Self::ReNotification => f.write_str("re-notification"),
            Self::Assignment => f.write_str("assignment"),
            Self::Escalation => f.write_str("escalation"),
        }
    }
}

impl FromStr for NotificationType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "initial" => Ok(Self::Initial),
            "re-notification" => Ok(Self::ReNotification),
            "assignment" => Ok(Self::Assignment),
            "escalation" => Ok(Self::Escalation),
            other => Err(Error::InvalidEvent(format!(
                "unknown notification type: {other}"
            ))),
        }
    }
}
