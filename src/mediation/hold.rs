//! The hold before a `facts_gathered` handoff (`docs/spec.md` §7.6): once
//! both parties were heard and the facts are gathered, Serbero tells them
//! so and gives them `handoff_grace` to resolve the dispute themselves. The
//! timer task hands off when the grace is over; the notifier closes the
//! session as self-resolved if the dispute ends first. A later turn that
//! asks a fact question ends the hold: the facts are no longer gathered,
//! and a new hold starts once they are again.

use std::time::Duration;

use serde_json::json;

use super::Mediator;
use super::timers::RETRY_GRACE_SECS;
use crate::error::Result;
use crate::nostr::dm::DmSender;
use crate::policy::{HandoffReason, template};
use crate::store::events::{self, Event};
use crate::store::messages::{self, Direction, Message};
use crate::store::sessions::{Party, Session};

/// The party notice sent when the hold starts (`docs/messages.md` §2).
pub(super) const HOLD_NOTICE: &str = "hold_notice";

/// Records that the session holds for the parties; the timer reads its time.
pub(super) const HELD_EVENT: &str = "held";

/// The session's current hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Hold {
    /// When it started.
    pub at: i64,
    /// The newest stored message when it started: a question sent after
    /// it ends the hold, and its notices are the ones sent after it.
    pub cutoff: i64,
}

/// How long after the hold started the timer task resends a notice that
/// is missing: the usual retry grace, but never more than half of
/// `handoff_grace`, so a short grace still leaves the parties time to act
/// on the notice before the handoff.
pub(super) fn retry_after(handoff_grace: Duration) -> i64 {
    let half = i64::try_from(handoff_grace.as_secs() / 2).unwrap_or(i64::MAX);
    RETRY_GRACE_SECS.min(half)
}

/// The session's hold, if it holds now: the latest `held` event, unless a
/// fact question was sent to a party since.
pub(super) fn current(session: &Session, history: &[Event], messages: &[Message]) -> Option<Hold> {
    let held = history
        .iter()
        .filter(|e| e.session_id.as_deref() == Some(&session.session_id) && e.kind == HELD_EVENT)
        .max_by_key(|e| (e.created_at, e.id))?;
    let hold = Hold {
        at: held.created_at,
        // An event without a cutoff never ends by a question.
        cutoff: held.payload["last_message_id"].as_i64().unwrap_or(i64::MAX),
    };
    let asked_since = messages.iter().any(|m| {
        m.direction == Direction::Out
            && m.id > hold.cutoff
            && m.template_id.as_deref().is_some_and(template::is_question)
    });
    (!asked_since).then_some(hold)
}

impl<S: DmSender> Mediator<S> {
    /// Starts the hold: recorded first, so a restart finds it and the timer
    /// still ends it, then both parties are told. A notice that failed is
    /// retried by the timer task.
    pub async fn hold(&self, session: &Session, now: i64) -> Result<()> {
        let cutoff = {
            let store = self.lock_store()?;
            let cutoff = messages::list_for_session(store.conn(), &session.session_id)?
                .iter()
                .map(|m| m.id)
                .max()
                .unwrap_or(0);
            events::append(
                store.conn(),
                &events::NewEvent {
                    dispute_id: &session.dispute_id,
                    session_id: Some(&session.session_id),
                    kind: HELD_EVENT,
                    payload: json!({
                        "reason": HandoffReason::FactsGathered.as_str(),
                        "last_message_id": cutoff,
                    }),
                    now,
                },
            )?;
            cutoff
        };
        self.send_hold_notices(session, cutoff).await;
        Ok(())
    }

    /// Sends `hold_notice` to each party who has not received it since the
    /// hold started, each on its own, so one unreachable party does not
    /// keep the other uninformed.
    pub(super) async fn send_hold_notices(&self, session: &Session, cutoff: i64) {
        for party in [Party::Buyer, Party::Seller] {
            match self.received_after(session, party, HOLD_NOTICE, cutoff) {
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

    /// Whether the party received this template after message `cutoff`.
    fn received_after(
        &self,
        session: &Session,
        party: Party,
        template: &str,
        cutoff: i64,
    ) -> Result<bool> {
        let store = self.lock_store()?;
        Ok(
            messages::list_for_session(store.conn(), &session.session_id)?
                .iter()
                .any(|m| {
                    m.direction == Direction::Out
                        && m.party == party
                        && m.id > cutoff
                        && m.template_id.as_deref() == Some(template)
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sessions::{self, testing::store_with_session};

    fn session() -> Session {
        let store = store_with_session();
        sessions::get(store.conn(), "s1").unwrap().unwrap()
    }

    fn held(session: &str, at: i64, cutoff: i64) -> Event {
        Event {
            id: at,
            dispute_id: "d1".into(),
            session_id: Some(session.into()),
            kind: HELD_EVENT.into(),
            payload: json!({ "reason": "facts_gathered", "last_message_id": cutoff }),
            created_at: at,
        }
    }

    fn out(id: i64, party: Party, template: &str) -> Message {
        Message {
            id,
            session_id: "s1".into(),
            direction: Direction::Out,
            party,
            template_id: Some(template.into()),
            lang: None,
            content: String::new(),
            attachments: 0,
            inner_event_id: String::new(),
            created_at: id,
        }
    }

    #[test]
    fn a_missing_notice_is_retried_within_half_the_grace() {
        assert_eq!(retry_after(Duration::from_secs(1800)), RETRY_GRACE_SECS);
        assert_eq!(retry_after(Duration::from_secs(240)), RETRY_GRACE_SECS);
        assert_eq!(retry_after(Duration::from_secs(100)), 50);
        assert_eq!(retry_after(Duration::MAX), RETRY_GRACE_SECS);
    }

    #[test]
    fn the_hold_is_the_sessions_latest_held_event() {
        let history = [held("s0", 90, 0), held("s1", 40, 5), held("s1", 60, 7)];

        assert_eq!(
            current(&session(), &history, &[]),
            Some(Hold { at: 60, cutoff: 7 }),
            "another session's hold is not ours"
        );
        assert_eq!(current(&session(), &[], &[]), None);
    }

    #[test]
    fn a_question_asked_after_the_hold_ends_it() {
        let history = [held("s1", 60, 7)];
        let before = [out(7, Party::Buyer, template::ASK_BUYER_SENT)];
        let courtesy = [out(8, Party::Buyer, template::THANKS_WAITING)];
        let notice = [out(8, Party::Seller, HOLD_NOTICE)];
        let question = [out(8, Party::Seller, template::ASK_SELLER_RECEIVED_SIMPLE)];

        assert!(current(&session(), &history, &before).is_some());
        assert!(current(&session(), &history, &courtesy).is_some());
        assert!(current(&session(), &history, &notice).is_some());
        assert_eq!(current(&session(), &history, &question), None);
    }

    #[test]
    fn a_new_hold_after_a_question_counts_again() {
        let history = [held("s1", 60, 7), held("s1", 90, 9)];
        let messages = [
            out(8, Party::Seller, template::ASK_SELLER_RECEIVED_SIMPLE),
            out(10, Party::Seller, HOLD_NOTICE),
        ];

        assert_eq!(
            current(&session(), &history, &messages),
            Some(Hold { at: 90, cutoff: 9 })
        );
    }
}
