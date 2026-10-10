//! The hold before a `facts_gathered` handoff (`docs/spec.md` §7.6): once
//! both parties were heard and the facts are gathered, Serbero tells them
//! so and gives them `handoff_grace` to resolve the dispute themselves. The
//! timer task hands off when the grace is over; the notifier closes the
//! session as self-resolved if the dispute ends first.

use std::time::Duration;

use serde_json::json;

use super::Mediator;
use super::timers::RETRY_GRACE_SECS;
use crate::error::Result;
use crate::nostr::dm::DmSender;
use crate::policy::HandoffReason;
use crate::store::events::{self, Event};
use crate::store::sessions::{Party, Session};

/// The party notice sent when the hold starts (`docs/messages.md` §2).
pub(super) const HOLD_NOTICE: &str = "hold_notice";

/// Records that the session holds for the parties; the timer reads its time.
pub(super) const HELD_EVENT: &str = "held";

/// How long after the hold started the timer task resends a notice that
/// is missing: the usual retry grace, but never more than half of
/// `handoff_grace`, so a short grace still leaves the parties time to act
/// on the notice before the handoff.
pub(super) fn retry_after(handoff_grace: Duration) -> i64 {
    let half = i64::try_from(handoff_grace.as_secs() / 2).unwrap_or(i64::MAX);
    RETRY_GRACE_SECS.min(half)
}

/// When the session started holding, if it did.
pub(super) fn held_at(session: &Session, history: &[Event]) -> Option<i64> {
    history
        .iter()
        .filter(|e| e.session_id.as_deref() == Some(&session.session_id) && e.kind == HELD_EVENT)
        .map(|e| e.created_at)
        .max()
}

impl<S: DmSender> Mediator<S> {
    /// Starts the hold: recorded first, so a restart finds it and the timer
    /// still ends it, then both parties are told. A notice that failed is
    /// retried by the timer task.
    pub async fn hold(&self, session: &Session, now: i64) -> Result<()> {
        {
            let store = self.lock_store()?;
            events::append(
                store.conn(),
                &events::NewEvent {
                    dispute_id: &session.dispute_id,
                    session_id: Some(&session.session_id),
                    kind: HELD_EVENT,
                    payload: json!({ "reason": HandoffReason::FactsGathered.as_str() }),
                    now,
                },
            )?;
        }
        self.send_hold_notices(session).await;
        Ok(())
    }

    /// Sends `hold_notice` to each party who has not received it, each on
    /// its own, so one unreachable party does not keep the other
    /// uninformed.
    pub(super) async fn send_hold_notices(&self, session: &Session) {
        for party in [Party::Buyer, Party::Seller] {
            match self.received(session, party, HOLD_NOTICE) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(session_id = %session.session_id, %party, error = %e, "cannot read the hold notices");
                    continue;
                }
            }
            if let Err(e) = self.send_template(session, party, HOLD_NOTICE).await {
                tracing::warn!(session_id = %session.session_id, %party, error = %e, "hold notice not sent; the timer retries it");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_notice_is_retried_within_half_the_grace() {
        assert_eq!(retry_after(Duration::from_secs(1800)), RETRY_GRACE_SECS);
        assert_eq!(retry_after(Duration::from_secs(240)), RETRY_GRACE_SECS);
        assert_eq!(retry_after(Duration::from_secs(100)), 50);
        assert_eq!(retry_after(Duration::MAX), RETRY_GRACE_SECS);
    }
}
