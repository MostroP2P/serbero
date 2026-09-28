//! The timer task (`docs/judgments.md` §4.2): reminders, `unresponsive` and
//! `self_resolution_stalled`, from each live session's clocks. Timers never
//! call the judge for a turn; a handoff reuses the last turn's reading.

use std::sync::Arc;
use std::time::Duration;

use super::handoff::TurnReading;
use super::{Mediator, ReadyJudge};
use crate::error::Result;
use crate::judge::facts::{self, Facts};
use crate::judge::{Answers, state};
use crate::nostr::dm::DmSender;
use crate::policy::timers::{Clocks, PartyClock, Timer, check};
use crate::policy::{Phase, template};
use crate::store::events::Event;
use crate::store::messages::{self, Direction, Message};
use crate::store::sessions::{self, Party, Session, SessionState};
use crate::store::{disputes, evaluations, events};

/// How often live sessions are checked.
pub const TICK: Duration = Duration::from_secs(30);

/// The clocks of one session, read from its messages and events.
pub fn clocks(session: &Session, messages: &[Message], history: &[Event], now: i64) -> Clocks {
    let party_clock = |party: Party| {
        let theirs = || messages.iter().filter(move |m| m.party == party);
        let sent = |pick: &dyn Fn(&str) -> bool| {
            theirs()
                .filter(|m| m.direction == Direction::Out)
                .filter(|m| m.template_id.as_deref().is_some_and(pick))
                .map(|m| m.created_at)
                .max()
        };
        PartyClock {
            question_at: sent(&template::is_question),
            replied_at: theirs()
                .filter(|m| m.direction == Direction::In)
                .map(|m| m.created_at)
                .max(),
            reminded_at: sent(&|t| t == template::REMINDER),
        }
    };
    Clocks {
        phase: if session.state == SessionState::Guiding {
            Phase::Guiding
        } else {
            Phase::Gathering
        },
        now,
        buyer: party_clock(Party::Buyer),
        seller: party_clock(Party::Seller),
        guided_at: history
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(&session.session_id) && e.kind == "guided")
            .map(|e| e.created_at)
            .max(),
    }
}

/// What a handoff from a timer shows the solvers: the state as it is now,
/// read with the last turn's answers, if the judge answered one.
struct LastReading {
    state: serde_json::Value,
    answers: Answers,
    facts: Facts,
}

impl<S: DmSender + Send + Sync + 'static> Mediator<S> {
    /// Checks every live session every `TICK` until the task is dropped.
    pub async fn run_timers(self: Arc<Self>) {
        let mut ticks = tokio::time::interval(TICK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            if let Err(e) = self.tick(crate::daemon::now()).await {
                tracing::error!(error = %e, "mediation timer tick failed");
            }
        }
    }

    /// One pass over the live sessions that are gathering or guiding.
    pub async fn tick(&self, now: i64) -> Result<()> {
        let live = {
            let store = self.lock_store()?;
            sessions::list_live(store.conn())?
        };
        for session in live {
            if !matches!(session.state, SessionState::Active | SessionState::Guiding) {
                continue;
            }
            if let Err(e) = self.tick_session(&session, now).await {
                tracing::error!(session_id = %session.session_id, error = %e, "session timer failed");
            }
        }
        Ok(())
    }

    async fn tick_session(&self, session: &Session, now: i64) -> Result<()> {
        let (messages, history) = {
            let store = self.lock_store()?;
            (
                messages::list_for_session(store.conn(), &session.session_id)?,
                events::list_for_dispute(store.conn(), &session.dispute_id)?,
            )
        };
        let timer = check(
            &clocks(session, &messages, &history, now),
            self.settings.response_timeout,
            self.settings.self_resolution_timeout,
        );
        match timer {
            Timer::Nothing => {}
            Timer::Remind(parties) => {
                for party in parties {
                    self.send_template(session, party, template::REMINDER)
                        .await?;
                }
            }
            Timer::Handoff(reason) => {
                let last = self.last_reading(session, &messages)?;
                let reading = last.as_ref().map(|l| TurnReading {
                    state: &l.state,
                    answers: &l.answers,
                    facts: &l.facts,
                });
                self.hand_off(session, reason, reading, now).await?;
            }
        }
        Ok(())
    }

    /// The last turn's answers over the current state, if a turn was judged
    /// with the question set in use.
    fn last_reading(&self, session: &Session, messages: &[Message]) -> Result<Option<LastReading>> {
        let Some(ready) = self.ready_judge() else {
            return Ok(None);
        };
        let (last, opened_by) = {
            let store = self.lock_store()?;
            let last = evaluations::list_for_session(store.conn(), &session.session_id)?
                .into_iter()
                .rev()
                .find(|e| e.question_set_version == ready.turn.id());
            let opened_by = disputes::get(store.conn(), &session.dispute_id)?.map(|d| d.initiator);
            (last, opened_by)
        };
        let (Some(last), Some(opened_by)) = (last, opened_by) else {
            return Ok(None);
        };
        let Ok(answers) = serde_json::from_value::<Answers>(last.answers) else {
            return Ok(None);
        };
        Ok(Some(self.reading_from(
            &ready, session, opened_by, messages, answers,
        )))
    }

    fn reading_from(
        &self,
        ready: &ReadyJudge,
        session: &Session,
        opened_by: disputes::Initiator,
        messages: &[Message],
        answers: Answers,
    ) -> LastReading {
        let built = state::build(
            session,
            opened_by,
            messages,
            self.settings.max_message_chars,
        );
        let facts = facts::from_answers(&answers, &ready.thresholds, &self.settings.languages);
        LastReading {
            state: built.value,
            answers,
            facts,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::store::sessions::testing::store_with_session;

    fn message(direction: Direction, party: Party, template: Option<&str>, at: i64) -> Message {
        Message {
            id: 0,
            session_id: "s1".into(),
            direction,
            party,
            template_id: template.map(str::to_owned),
            lang: None,
            content: String::new(),
            attachments: 0,
            inner_event_id: String::new(),
            created_at: at,
        }
    }

    fn session(state: SessionState) -> Session {
        let store = store_with_session();
        let mut session = sessions::get(store.conn(), "s1").unwrap().unwrap();
        session.state = state;
        session
    }

    #[test]
    fn clocks_come_from_the_last_question_reply_and_reminder() {
        let messages = [
            message(
                Direction::Out,
                Party::Buyer,
                Some(template::ASK_BUYER_SENT),
                10,
            ),
            message(Direction::In, Party::Buyer, None, 20),
            message(
                Direction::Out,
                Party::Buyer,
                Some(template::THANKS_WAITING),
                25,
            ),
            message(
                Direction::Out,
                Party::Buyer,
                Some(template::ASK_BUYER_DETAILS),
                30,
            ),
            message(
                Direction::Out,
                Party::Seller,
                Some(template::ASK_SELLER_RECEIVED),
                10,
            ),
            message(Direction::Out, Party::Seller, Some(template::REMINDER), 40),
        ];

        let clocks = clocks(&session(SessionState::Active), &messages, &[], 99);

        assert_eq!(clocks.phase, Phase::Gathering);
        assert_eq!(clocks.now, 99);
        assert_eq!(
            clocks.buyer,
            PartyClock {
                question_at: Some(30),
                replied_at: Some(20),
                reminded_at: None
            }
        );
        assert_eq!(
            clocks.seller,
            PartyClock {
                question_at: Some(10),
                replied_at: None,
                reminded_at: Some(40)
            }
        );
    }

    #[test]
    fn guidance_time_comes_from_the_sessions_guided_event() {
        let event = |session: &str, kind: &str, at: i64| Event {
            id: 0,
            dispute_id: "d1".into(),
            session_id: Some(session.into()),
            kind: kind.into(),
            payload: json!({}),
            created_at: at,
        };
        let history = [
            event("s1", "guided", 50),
            event("s1", "handoff", 70),
            event("s0", "guided", 90),
        ];

        let clocks = clocks(&session(SessionState::Guiding), &[], &history, 99);

        assert_eq!(clocks.phase, Phase::Guiding);
        assert_eq!(clocks.guided_at, Some(50));
    }
}
