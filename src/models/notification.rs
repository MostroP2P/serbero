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
    /// Phase 3 (US3): cooperative mediation summary delivered to the
    /// assigned (or broadcast) solver(s). Recorded in the existing
    /// `notifications` table — `notif_type` is TEXT, no schema
    /// migration needed.
    MediationSummary,
    /// Phase 3 (US4): "needs human judgment" alert delivered to the
    /// assigned (or broadcast) solver(s) when a session escalates.
    /// Reuses the Phase 1/2 notifier verbatim; the Phase 4 handoff
    /// package lives alongside in `mediation_events` as a
    /// `handoff_prepared` row, not in `notifications`.
    MediationEscalationRecommended,
    /// Phase 3 (US6): informational report delivered to solver(s) when
    /// a dispute resolves externally while mediation was active. NOT
    /// an escalation — the dispute is already resolved; this is a
    /// "for your records" notification.
    MediationResolutionReport,
}

impl fmt::Display for NotificationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initial => f.write_str("initial"),
            Self::ReNotification => f.write_str("re-notification"),
            Self::Assignment => f.write_str("assignment"),
            Self::Escalation => f.write_str("escalation"),
            Self::MediationSummary => f.write_str("mediation_summary"),
            Self::MediationEscalationRecommended => f.write_str("mediation_escalation_recommended"),
            Self::MediationResolutionReport => f.write_str("mediation_resolution_report"),
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
            "mediation_summary" => Ok(Self::MediationSummary),
            "mediation_escalation_recommended" => Ok(Self::MediationEscalationRecommended),
            "mediation_resolution_report" => Ok(Self::MediationResolutionReport),
            other => Err(Error::InvalidEvent(format!(
                "unknown notification type: {other}"
            ))),
        }
    }
}
