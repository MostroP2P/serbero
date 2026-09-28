//! Handing a session to a human (`docs/spec.md` §7.6): the brief and the
//! transcript to the solvers, the notice to the parties, and afterwards the
//! parties' new messages forwarded as updates.

use serde_json::{Value, json};

use super::{Mediator, ReadyJudge};
use crate::chat::{Outbound, send_to_party};
use crate::config::Permission;
use crate::error::{Error, Result};
use crate::judge::Answers;
use crate::judge::brief::{self, Brief, BriefQuestions};
use crate::judge::facts::Facts;
use crate::nostr::dm::DmSender;
use crate::notifier::{Solver, notify_solvers};
use crate::policy::{Action, HandoffReason};
use crate::solver::{self, BriefInput, Line, Order, Reading, Speaker, Subject};
use crate::store::messages::{self, Direction, Message};
use crate::store::sessions::{self, Party, Session};
use crate::store::{disputes, evaluations, events};

/// The party notice sent on every handoff (`docs/messages.md` §2).
const HANDOFF_NOTICE: &str = "handoff_notice";

/// Event kinds that record the last party message the solvers have seen.
const HANDOFF_EVENT: &str = "handoff";
const UPDATE_EVENT: &str = "update_sent";

/// A handoff brief no solver received yet; the timer task retries it.
pub(super) const BRIEF_PENDING: &str = "brief_pending";

/// The last turn's judge reading, when there is one.
#[derive(Debug, Clone, Copy)]
pub struct TurnReading<'a> {
    pub state: &'a Value,
    pub answers: &'a Answers,
    pub facts: &'a Facts,
}

/// Who gets the brief: the solver assigned to the dispute if Serbero knows
/// one, otherwise every `write` solver, otherwise every solver
/// (`docs/spec.md` §7.6).
pub fn recipients(solvers: &[Solver], assigned: Option<&str>) -> Vec<Solver> {
    if let Some(assigned) = assigned {
        let chosen: Vec<Solver> = solvers
            .iter()
            .filter(|s| s.pubkey.to_hex() == assigned)
            .copied()
            .collect();
        if !chosen.is_empty() {
            return chosen;
        }
    }
    let writers: Vec<Solver> = solvers
        .iter()
        .filter(|s| s.permission == Permission::Write)
        .copied()
        .collect();
    if writers.is_empty() {
        solvers.to_vec()
    } else {
        writers
    }
}

impl<S: DmSender> Mediator<S> {
    /// Hands the session to a human. The session is claimed first, so it is
    /// handed off once and never after it ended; then the solvers are
    /// briefed and the parties told. A brief no solver received is recorded
    /// as `brief_pending`, and a notice that failed is retried by the timer
    /// task (`mediation::timers`). Returns whether this call claimed it.
    pub async fn hand_off(
        &self,
        session: &Session,
        reason: HandoffReason,
        reading: Option<TurnReading<'_>>,
        now: i64,
    ) -> Result<bool> {
        {
            let store = self.lock_store()?;
            if !sessions::hand_off(store.conn(), &session.session_id, reason.as_str(), now)? {
                return Ok(false);
            }
            // Recorded with the claim: updates forward only later messages.
            let last = messages::list_for_session(store.conn(), &session.session_id)?
                .iter()
                .map(|m| m.id)
                .max()
                .unwrap_or(0);
            events::append(
                store.conn(),
                &events::NewEvent {
                    dispute_id: &session.dispute_id,
                    session_id: Some(&session.session_id),
                    kind: HANDOFF_EVENT,
                    payload: json!({ "reason": reason.as_str(), "last_message_id": last }),
                    now,
                },
            )?;
        }
        let delivered = self
            .brief_solvers(session, Subject::Handoff(reason), reading, now)
            .await?;
        if delivered == 0 && !self.solvers.is_empty() {
            let store = self.lock_store()?;
            events::append(
                store.conn(),
                &events::NewEvent {
                    dispute_id: &session.dispute_id,
                    session_id: Some(&session.session_id),
                    kind: BRIEF_PENDING,
                    payload: json!({ "reason": reason.as_str() }),
                    now,
                },
            )?;
        }
        for party in [Party::Buyer, Party::Seller] {
            if let Err(e) = self.send_template(session, party, HANDOFF_NOTICE).await {
                tracing::warn!(session_id = %session.session_id, %party, error = %e, "handoff notice not sent; the timer retries it");
            }
        }
        Ok(true)
    }

    /// Asks the judge the brief questions (when a reading exists), then
    /// sends the brief and the transcript to the recipients. Returns how
    /// many solvers received the brief.
    pub async fn brief_solvers(
        &self,
        session: &Session,
        subject: Subject,
        reading: Option<TurnReading<'_>>,
        now: i64,
    ) -> Result<usize> {
        let (messages, dispute) = {
            let store = self.lock_store()?;
            (
                messages::list_for_session(store.conn(), &session.session_id)?,
                disputes::get(store.conn(), &session.dispute_id)?,
            )
        };
        let ready = self.ready_judge();
        let brief = match (reading, &ready) {
            (Some(reading), Some(ready)) => {
                self.ask_brief(ready, session, reading.state, subject, now)
                    .await
            }
            _ => Brief::default(),
        };
        let default = self.settings.default_language.as_str();
        let first_seen = dispute.as_ref().map(|d| d.first_seen_at);
        let text = solver::brief(&BriefInput {
            dispute_id: &session.dispute_id,
            subject,
            rounds: session.rounds,
            duration_secs: now - session.opened_at,
            order: Order {
                amount: session.fiat_amount.as_deref(),
                currency: session.fiat_code.as_deref(),
                payment_method: session.payment_method.as_deref(),
                created_before_dispute_secs: first_seen
                    .zip(session.order_published_at)
                    .map(|(seen, created)| seen - created),
            },
            buyer_language: session.language(Party::Buyer, default),
            seller_language: session.language(Party::Seller, default),
            reading: reading.map(|r| Reading {
                state: r.state,
                answers: r.answers,
                facts: r.facts,
                brief: &brief,
            }),
            transcript_messages: messages.len(),
        });
        let to = recipients(
            &self.solvers,
            dispute.as_ref().and_then(|d| d.assigned_solver.as_deref()),
        );
        let dispute_id = &session.dispute_id;
        let delivered = notify_solvers(
            &self.store,
            &self.sender,
            &to,
            dispute_id,
            "brief",
            &text,
            now,
        )
        .await?;
        for part in solver::transcript(dispute_id, &lines(&messages)) {
            notify_solvers(
                &self.store,
                &self.sender,
                &to,
                dispute_id,
                "transcript",
                &part,
                now,
            )
            .await?;
        }
        Ok(delivered)
    }

    /// The brief request (`docs/judgments.md` §5). A failure costs the
    /// quotes, not the brief: it is logged and an empty brief is used.
    async fn ask_brief(
        &self,
        ready: &ReadyJudge,
        session: &Session,
        state: &Value,
        subject: Subject,
        now: i64,
    ) -> Brief {
        let questions = BriefQuestions::new().for_state(state);
        let judged = match ready.judge.evaluate(state, &questions).await {
            Ok(judged) => judged,
            Err(e) => {
                tracing::warn!(session_id = %session.session_id, error = %e, "brief request failed");
                return Brief::default();
            }
        };
        let action = match subject {
            Subject::Handoff(reason) => Action::Handoff(reason),
            Subject::Guide(path) => Action::Guide(path),
        };
        if let Err(e) = self.record_brief(ready, session, &questions.version, &judged, &action, now)
        {
            tracing::error!(session_id = %session.session_id, error = %e, "cannot record the brief evaluation");
        }
        brief::from_answers(state, &judged.answers, &ready.thresholds)
    }

    fn record_brief(
        &self,
        ready: &ReadyJudge,
        session: &Session,
        question_set: &str,
        judged: &crate::judge::Judged,
        action: &Action,
        now: i64,
    ) -> Result<()> {
        let answers = serde_json::to_value(&judged.answers)
            .map_err(|e| Error::Schema(format!("answers do not serialize: {e}")))?;
        let action = serde_json::to_value(action)
            .map_err(|e| Error::Schema(format!("action does not serialize: {e}")))?;
        let store = self.lock_store()?;
        let last = messages::list_for_session(store.conn(), &session.session_id)?
            .last()
            .map_or(0, |m| m.id);
        evaluations::insert(
            store.conn(),
            &evaluations::NewEvaluation {
                session_id: &session.session_id,
                question_set_version: question_set,
                judge_id: ready.judge.id(),
                last_message_id: last,
                answers: &answers,
                action: &action,
                input_tokens: judged.input_tokens,
                latency_ms: None,
                now,
            },
        )?;
        Ok(())
    }

    /// After a handoff, forwards the party messages the solvers have not
    /// seen as one update DM (`docs/spec.md` §7.5).
    pub async fn forward_updates(&self, session: &Session, now: i64) -> Result<usize> {
        let (fresh, dispute) = {
            let store = self.lock_store()?;
            let history = events::list_for_dispute(store.conn(), &session.dispute_id)?;
            let seen = last_seen(&history, &session.session_id);
            let fresh: Vec<Message> =
                messages::list_for_session(store.conn(), &session.session_id)?
                    .into_iter()
                    .filter(|m| m.direction == Direction::In && m.id > seen)
                    .collect();
            (fresh, disputes::get(store.conn(), &session.dispute_id)?)
        };
        let Some(last) = fresh.last().map(|m| m.id) else {
            return Ok(0);
        };
        let to = recipients(
            &self.solvers,
            dispute.as_ref().and_then(|d| d.assigned_solver.as_deref()),
        );
        let mut delivered = 0;
        for part in solver::update(&session.dispute_id, &lines(&fresh)) {
            delivered += notify_solvers(
                &self.store,
                &self.sender,
                &to,
                &session.dispute_id,
                "update",
                &part,
                now,
            )
            .await?;
        }
        if delivered == 0 {
            // Nobody got it: keep the messages unseen for the next attempt.
            return Ok(0);
        }
        let store = self.lock_store()?;
        events::append(
            store.conn(),
            &events::NewEvent {
                dispute_id: &session.dispute_id,
                session_id: Some(&session.session_id),
                kind: UPDATE_EVENT,
                payload: json!({ "last_message_id": last }),
                now,
            },
        )?;
        Ok(fresh.len())
    }

    /// A template rendered for a party, in its language.
    pub(super) fn render_for(
        &self,
        session: &Session,
        party: Party,
        template: &str,
    ) -> Result<(String, String)> {
        let lang = session.language(party, &self.settings.default_language);
        let catalog = self
            .catalogs
            .get(lang)
            .ok_or_else(|| Error::Catalog(format!("no catalog for {lang}")))?;
        let amount = match (&session.fiat_amount, &session.fiat_code) {
            (Some(value), Some(currency)) => Some(crate::catalog::Amount { value, currency }),
            _ => None,
        };
        Ok((catalog.render(template, amount)?, lang.to_owned()))
    }

    /// Renders a template for a party in its language and sends it.
    pub(super) async fn send_template(
        &self,
        session: &Session,
        party: Party,
        template: &str,
    ) -> Result<()> {
        let (text, lang) = self.render_for(session, party, template)?;
        send_to_party(
            &self.client,
            &self.gate,
            &self.store,
            &self.keys,
            session,
            &Outbound {
                party,
                text: &text,
                template_id: Some(template),
                lang: Some(&lang),
            },
        )
        .await?;
        Ok(())
    }
}

/// The newest message id the solvers have seen for this session.
fn last_seen(events: &[events::Event], session_id: &str) -> i64 {
    events
        .iter()
        .filter(|e| e.session_id.as_deref() == Some(session_id))
        .filter(|e| e.kind == HANDOFF_EVENT || e.kind == UPDATE_EVENT)
        .filter_map(|e| e.payload["last_message_id"].as_i64())
        .max()
        .unwrap_or(0)
}

fn lines(messages: &[Message]) -> Vec<Line<'_>> {
    messages
        .iter()
        .map(|m| Line {
            at: m.created_at,
            speaker: match m.direction {
                Direction::In => Speaker::Party(m.party),
                Direction::Out => Speaker::Serbero(m.party),
            },
            text: &m.content,
            attachments: m.attachments,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use nostr_sdk::prelude::Keys;

    use super::*;

    fn solver(permission: Permission) -> Solver {
        Solver {
            pubkey: Keys::generate().public_key(),
            permission,
        }
    }

    #[test]
    fn the_assigned_solver_gets_the_brief_alone() {
        let solvers = [solver(Permission::Write), solver(Permission::Read)];
        let assigned = solvers[1].pubkey.to_hex();

        assert_eq!(recipients(&solvers, Some(&assigned)), [solvers[1]]);
    }

    #[test]
    fn without_an_assigned_solver_every_write_solver_gets_it() {
        let solvers = [
            solver(Permission::Write),
            solver(Permission::Read),
            solver(Permission::Write),
        ];

        assert_eq!(recipients(&solvers, None), [solvers[0], solvers[2]]);
        assert_eq!(
            recipients(&solvers, Some("not-a-configured-solver")),
            [solvers[0], solvers[2]],
            "an unknown assigned solver falls back"
        );
    }

    #[test]
    fn without_write_solvers_every_solver_gets_it() {
        let solvers = [solver(Permission::Read), solver(Permission::Read)];

        assert_eq!(recipients(&solvers, None), solvers);
    }

    #[test]
    fn the_last_seen_message_is_the_newest_handoff_or_update() {
        let event = |kind: &str, session: &str, last: i64| events::Event {
            id: 0,
            dispute_id: "d1".into(),
            session_id: Some(session.into()),
            kind: kind.into(),
            payload: json!({ "last_message_id": last }),
            created_at: 0,
        };
        let history = [
            event(HANDOFF_EVENT, "s1", 7),
            event(UPDATE_EVENT, "s1", 12),
            event("brief", "s1", 99),
            event(UPDATE_EVENT, "s2", 50),
        ];

        assert_eq!(last_seen(&history, "s1"), 12);
        assert_eq!(last_seen(&history, "s3"), 0);
    }
}
