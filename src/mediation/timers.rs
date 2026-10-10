//! The timer task (`docs/judgments.md` §4.2): reminders, `unresponsive` and
//! `self_resolution_stalled`, from each live session's clocks. Timers never
//! call the judge for a turn; a handoff reuses the last turn's reading.

use std::sync::Arc;
use std::time::Duration;

use super::guide::FINISH_LOOKBACK_SECS;
use super::handoff::{BRIEF_PENDING, BRIEF_SENT, HANDOFF_NOTICE, TurnReading};
use super::hold;
use super::{Mediator, OPENING_NOTICE_PENDING, OPENING_NOTICE_SENT, ReadyJudge};
use crate::error::Result;
use crate::judge::facts::{self, Facts};
use crate::judge::{Answers, state};
use crate::nostr::dm::DmSender;
use crate::policy::timers::{Clocks, PartyClock, Timeouts, Timer, check};
use crate::policy::{HandoffReason, Path, Phase, template};
use crate::solver::Subject;
use crate::store::events::Event;
use crate::store::messages::{self, Direction, Message};
use crate::store::sessions::{self, Party, Session, SessionState};
use crate::store::{disputes, evaluations, events};

/// How often live sessions are checked.
pub const TICK: Duration = Duration::from_secs(30);

/// The clocks of one session, read from its messages and events.
///
/// Whether a party replied is decided by arrival order (the stored id), not
/// by the `created_at` the party's client chose: a reply that arrived after
/// the last question counts as answering it, whatever its timestamp.
pub fn clocks(session: &Session, messages: &[Message], history: &[Event], now: i64) -> Clocks {
    let party_clock = |party: Party| {
        let theirs = || messages.iter().filter(move |m| m.party == party);
        let last_sent = |pick: &dyn Fn(&str) -> bool| {
            theirs()
                .filter(|m| m.direction == Direction::Out)
                .filter(|m| m.template_id.as_deref().is_some_and(pick))
                .max_by_key(|m| m.id)
        };
        let question = last_sent(&template::is_question);
        let replied =
            question.is_some_and(|q| theirs().any(|m| m.direction == Direction::In && m.id > q.id));
        PartyClock {
            question_at: question.map(|q| q.created_at),
            replied_at: question.filter(|_| replied).map(|q| q.created_at),
            reminded_at: last_sent(&|t| t == template::REMINDER).map(|m| m.created_at),
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
        held_at: hold::current(session, history, messages).map(|h| h.at),
    }
}

/// How long after a handoff or guidance started its notices are left to
/// that call before a tick resends what is missing.
pub(super) const RETRY_GRACE_SECS: i64 = 120;

/// The session's briefs no solver received yet: each `brief_pending` event
/// id with what it was about, unless a `brief_sent` event covers it.
fn pending_briefs(session: &Session, history: &[Event]) -> Vec<(i64, Subject)> {
    let ours = || {
        history
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(&session.session_id))
    };
    ours()
        .filter(|e| e.kind == BRIEF_PENDING)
        .filter(|e| {
            !ours().any(|s| s.kind == BRIEF_SENT && s.payload["pending"].as_i64() == Some(e.id))
        })
        .filter_map(|e| {
            let subject = if let Some(reason) = e.payload["reason"].as_str() {
                Subject::Handoff(HandoffReason::parse(reason)?)
            } else {
                Subject::Guide(Path::parse(e.payload["path"].as_str()?)?)
            };
            Some((e.id, subject))
        })
        .collect()
}

/// What a handoff from a timer shows the solvers: the state as it is now,
/// read with the last turn's answers, if the judge answered one.
pub(super) struct LastReading {
    pub(super) state: serde_json::Value,
    pub(super) answers: Answers,
    pub(super) facts: Facts,
    pub(super) last_message_id: i64,
}

impl<S: DmSender + Send + Sync + 'static> Mediator<S> {
    /// Checks every live session every `TICK` until the task is dropped.
    /// It starts once `resumed` completes (the chat subscriptions were
    /// re-sent) and one `TICK` later, so replies the relays replay after a
    /// restart are stored before any reminder or handoff is decided.
    pub async fn run_timers(self: Arc<Self>, resumed: impl Future<Output = ()>) {
        resumed.await;
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + TICK, TICK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            if let Err(e) = self.tick(crate::daemon::now()).await {
                tracing::error!(error = %e, "mediation timer tick failed");
            }
        }
    }

    /// One pass over the live sessions: timers for those gathering or
    /// guiding, then whatever did not reach its recipient before (a brief,
    /// a handoff notice, guidance, a resolved dispute's closing).
    pub async fn tick(&self, now: i64) -> Result<()> {
        let live = {
            let store = self.lock_store()?;
            sessions::list_live(store.conn())?
        };
        for session in live {
            if matches!(session.state, SessionState::Active | SessionState::Guiding) {
                let result = if self.chat_open(&session).await {
                    self.tick_session(&session, now).await
                } else {
                    // No reply could reach it, so no timer may judge its
                    // silence: a person takes it over.
                    self.hand_off(&session, HandoffReason::OpeningFailed, None, now)
                        .await
                        .map(drop)
                };
                if let Err(e) = result {
                    tracing::error!(session_id = %session.session_id, error = %e, "session timer failed");
                }
            }
            if let Err(e) = self.retry_undelivered(&session, now).await {
                tracing::warn!(session_id = %session.session_id, error = %e, "retry failed");
            }
        }
        self.finish_pending(now - FINISH_LOOKBACK_SECS, now).await;
        self.retry_opening_notices(now).await;
        Ok(())
    }

    /// Resends "mediation could not start" notices no solver received, for
    /// disputes not resolved since, however old.
    async fn retry_opening_notices(&self, now: i64) {
        let pending = match self.lock_store().and_then(|store| {
            // No age limit: a notice stays pending until a solver got it or
            // the dispute is resolved.
            events::still_pending(store.conn(), OPENING_NOTICE_PENDING, OPENING_NOTICE_SENT, 0)
        }) {
            Ok(pending) => pending,
            Err(e) => {
                tracing::error!(error = %e, "cannot list pending opening notices");
                return;
            }
        };
        for dispute_id in pending {
            if let Err(e) = self.retry_opening_notice(&dispute_id, now).await {
                tracing::warn!(dispute_id, error = %e, "opening notice not sent; retried on the next tick");
            }
        }
    }

    async fn retry_opening_notice(&self, dispute_id: &str, now: i64) -> Result<()> {
        let resolved = {
            let store = self.lock_store()?;
            disputes::get(store.conn(), dispute_id)?
                .is_some_and(|d| d.lifecycle == disputes::Lifecycle::Resolved)
        };
        if !resolved {
            self.send_opening_notice(dispute_id, now).await;
        }
        Ok(())
    }

    /// Whether replies can reach the session: its channels are open, or
    /// open now (they did not at startup).
    async fn chat_open(&self, session: &Session) -> bool {
        if self.chats.is_open(&session.session_id) {
            return true;
        }
        match self.chats.open(session).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(session_id = %session.session_id, error = %e, "chat not open; timers wait");
                false
            }
        }
    }

    /// Resends what a session's recipients did not get: pending briefs,
    /// the handoff notice, and guidance.
    async fn retry_undelivered(&self, session: &Session, now: i64) -> Result<()> {
        let (messages, history) = {
            let store = self.lock_store()?;
            (
                messages::list_for_session(store.conn(), &session.session_id)?,
                events::list_for_dispute(store.conn(), &session.dispute_id)?,
            )
        };
        // A hold still sending its notices is left to finish first, for a
        // delay that leaves the parties most of the grace.
        let held = hold::current(session, &history, &messages)
            .filter(|h| now - h.at >= hold::retry_after(self.settings.handoff_grace));
        // A handoff or guidance still sending its brief and notices is left
        // to finish first.
        let started = history
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(&session.session_id))
            .filter(|e| e.kind == "handoff" || e.kind == "guided")
            .map(|e| e.created_at)
            .max();
        if started.is_some_and(|at| now - at < RETRY_GRACE_SECS) {
            return Ok(());
        }
        for (pending, subject) in pending_briefs(session, &history) {
            let last = self.last_reading(session, &messages)?;
            let reading = last.as_ref().map(|l| TurnReading {
                state: &l.state,
                answers: &l.answers,
                facts: &l.facts,
                last_message_id: l.last_message_id,
            });
            if self.brief_solvers(session, subject, reading, now).await? > 0 {
                self.brief_delivered(session, pending, now)?;
            }
        }
        // An opening that failed never reached the parties: Serbero writes
        // nothing more to them (`docs/spec.md` §7.2).
        let opening_failed =
            session.handoff_reason.as_deref() == Some(HandoffReason::OpeningFailed.as_str());
        match session.state {
            SessionState::HandedOff if !opening_failed => {
                for party in [Party::Buyer, Party::Seller] {
                    let told = messages.iter().any(|m| {
                        m.direction == Direction::Out
                            && m.party == party
                            && m.template_id.as_deref() == Some(HANDOFF_NOTICE)
                    });
                    // Each party on its own: one unreachable party must not
                    // keep the other uninformed.
                    if !told
                        && let Err(e) = self.send_template(session, party, HANDOFF_NOTICE).await
                    {
                        tracing::warn!(session_id = %session.session_id, %party, error = %e, "handoff notice not sent; retried on the next tick");
                    }
                }
            }
            SessionState::Guiding => {
                self.resend_guides(session).await?;
            }
            SessionState::Active if let Some(hold) = held => {
                self.send_hold_notices(session, hold.cutoff).await;
            }
            _ => {}
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
            &Timeouts {
                response: self.settings.response_timeout,
                self_resolution: self.settings.self_resolution_timeout,
                handoff_grace: self.settings.handoff_grace,
            },
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
                    last_message_id: l.last_message_id,
                });
                self.hand_off(session, reason, reading, now).await?;
            }
        }
        Ok(())
    }

    /// The last turn's answers over the current state, if a turn was judged
    /// with the question set in use.
    pub(super) fn last_reading(
        &self,
        session: &Session,
        messages: &[Message],
    ) -> Result<Option<LastReading>> {
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
            last_message_id: messages.iter().map(|m| m.id).max().unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::hold::HELD_EVENT;
    use super::*;
    use crate::store::sessions::testing::store_with_session;

    /// A message stored in arrival order: its id follows `at`.
    fn message(direction: Direction, party: Party, template: Option<&str>, at: i64) -> Message {
        Message {
            id: at,
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
                replied_at: None,
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
    fn a_reply_counts_by_arrival_not_by_its_timestamp() {
        let at = |id: i64, created_at: i64, direction, template: Option<&str>| Message {
            id,
            created_at,
            ..message(direction, Party::Buyer, template, 0)
        };
        let question = at(1, 100, Direction::Out, Some(template::ASK_BUYER_SENT));
        let session = session(SessionState::Active);

        // Arrived after the question, with a timestamp before it.
        let backdated = [question.clone(), at(2, 5, Direction::In, None)];
        assert_eq!(
            clocks(&session, &backdated, &[], 99).buyer.replied_at,
            Some(100)
        );

        // Arrived before the question, with a timestamp after it.
        let future_dated = [
            at(1, 900, Direction::In, None),
            Message { id: 2, ..question },
        ];
        assert_eq!(
            clocks(&session, &future_dated, &[], 99).buyer.replied_at,
            None
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
        assert_eq!(clocks.held_at, None);
    }

    #[test]
    fn the_hold_time_comes_from_the_sessions_held_event() {
        let event = |session: &str, kind: &str, at: i64| Event {
            id: 0,
            dispute_id: "d1".into(),
            session_id: Some(session.into()),
            kind: kind.into(),
            payload: json!({}),
            created_at: at,
        };
        let history = [event("s0", HELD_EVENT, 40), event("s1", HELD_EVENT, 60)];

        let clocks = clocks(&session(SessionState::Active), &[], &history, 99);

        assert_eq!(
            clocks.held_at,
            Some(60),
            "another session's hold is not ours"
        );
    }
}
