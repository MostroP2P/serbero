//! The turn loop (`docs/spec.md` §7.3): settle a burst, judge the session,
//! decide, act, and record the evaluation.

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use super::handoff::TurnReading;
use super::{Mediator, ReadyJudge, history, settle::Settle};
use crate::error::{Error, Result};
use crate::judge::facts::{self, Facts};
use crate::judge::state;
use crate::nostr::dm::DmSender;
use crate::policy::next::{History, next_questions};
use crate::policy::{Action, NextQuestions, Phase, Turn, decide};
use crate::store::messages::{self, Message};
use crate::store::sessions::{self, Party, Session, SessionState};
use crate::store::{disputes, evaluations};

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Nothing to judge: the session is not live, no party wrote, or the
    /// judge is not ready.
    Skipped(&'static str),
    /// The judge failed; the evaluation was not made.
    JudgeFailed(String),
    Decided(Action),
    /// After a handoff: this many new party messages went to the solvers.
    Forwarded(usize),
}

impl<S: DmSender + Send + Sync + 'static> Mediator<S> {
    /// Settles party messages into turns and runs them, one at a time,
    /// until the chat channels stop forwarding.
    pub async fn run_turns(
        self: std::sync::Arc<Self>,
        mut received: UnboundedReceiver<String>,
        quiet_period: Duration,
    ) {
        let mut settle = Settle::new(quiet_period);
        loop {
            let deadline = settle.next_deadline();
            tokio::select! {
                message = received.recv() => match message {
                    Some(session_id) => settle.touch(&session_id, Instant::now()),
                    None => return,
                },
                () = sleep_until(deadline), if deadline.is_some() => {
                    for session_id in settle.take_due(Instant::now()) {
                        self.turn_logged(&session_id, crate::daemon::now()).await;
                    }
                }
            }
        }
    }

    async fn turn_logged(&self, session_id: &str, now: i64) {
        match self.run_turn(session_id, now).await {
            Ok(TurnOutcome::Decided(action)) => {
                tracing::info!(session_id, action = %json!(action), "turn decided")
            }
            Ok(TurnOutcome::Skipped(why)) => tracing::debug!(session_id, why, "turn skipped"),
            Ok(TurnOutcome::Forwarded(count)) => {
                tracing::info!(session_id, count, "party messages forwarded to the solvers")
            }
            Ok(TurnOutcome::JudgeFailed(error)) => {
                tracing::warn!(session_id, error, "judge failed")
            }
            Err(e) => tracing::error!(session_id, error = %e, "turn failed"),
        }
    }

    /// One turn over the session as stored now.
    pub async fn run_turn(&self, session_id: &str, now: i64) -> Result<TurnOutcome> {
        let Some(ready) = self.ready_judge() else {
            return Ok(TurnOutcome::Skipped("judge not ready"));
        };
        let Some((session, messages, opened_by)) = self.load(session_id)? else {
            return Ok(TurnOutcome::Skipped("session not live"));
        };
        let phase = match session.state {
            SessionState::Active => Phase::Gathering,
            SessionState::Guiding => Phase::Guiding,
            SessionState::HandedOff => {
                return Ok(TurnOutcome::Forwarded(
                    self.forward_updates(&session, now).await?,
                ));
            }
            _ => return Ok(TurnOutcome::Skipped("session not live")),
        };
        let built = state::build(
            &session,
            opened_by,
            &messages,
            self.settings.max_message_chars,
        );
        let wrote: Vec<Party> = [Party::Buyer, Party::Seller]
            .into_iter()
            .filter(|p| {
                built.value["latest"][p.to_string()]
                    .as_array()
                    .is_some_and(|ids| !ids.is_empty())
            })
            .collect();
        if wrote.is_empty() {
            return Ok(TurnOutcome::Skipped("no new party messages"));
        }
        let questions = ready.turn.for_turn(&wrote, phase == Phase::Guiding);
        let started = std::time::Instant::now();
        let judged = match ready.judge.evaluate(&built.value, &questions).await {
            Ok(judged) => judged,
            Err(e) => return Ok(TurnOutcome::JudgeFailed(e.to_string())),
        };
        let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let facts =
            facts::from_answers(&judged.answers, &ready.thresholds, &self.settings.languages);
        let changed = self.update_languages(&session, &facts, now)?;

        let (asked, mut buyer, mut seller) = history::from_messages(&messages);
        buyer.language_changed = changed.contains(&Party::Buyer);
        seller.language_changed = changed.contains(&Party::Seller);
        let next = next_questions(
            &facts,
            &History {
                asked: &asked,
                buyer,
                seller,
            },
        );
        let session = self.reload(session_id)?;
        let default = self.settings.default_language.as_str();
        let action = decide(&Turn {
            phase,
            facts: &facts,
            buyer_language: session.language(Party::Buyer, default),
            seller_language: session.language(Party::Seller, default),
            validated_languages: &ready.thresholds.validated_languages,
            rounds: session.rounds,
            max_rounds: self.settings.max_rounds,
            asked: &asked,
            next: &next,
        });

        self.record_evaluation(
            &ready, &session, &messages, &judged, &action, latency_ms, now,
        )?;
        match &action {
            Action::Ask { buyer, seller } => {
                self.ask(&session, Party::Buyer, buyer).await?;
                self.ask(&session, Party::Seller, seller).await?;
                self.count_round(&session, &next, now)?;
            }
            Action::Handoff(reason) => {
                let reading = TurnReading {
                    state: &built.value,
                    answers: &judged.answers,
                    facts: &facts,
                };
                self.hand_off(&session, *reason, Some(reading), now).await?;
            }
            // Guidance is carried out in T5.4; until then it is recorded.
            Action::Guide(_) | Action::Wait => {}
        }
        Ok(TurnOutcome::Decided(action))
    }

    /// The live session, its messages in order, and who opened the dispute.
    fn load(
        &self,
        session_id: &str,
    ) -> Result<Option<(Session, Vec<Message>, disputes::Initiator)>> {
        let store = self.lock_store()?;
        let Some(session) = sessions::get(store.conn(), session_id)? else {
            return Ok(None);
        };
        let Some(dispute) = disputes::get(store.conn(), &session.dispute_id)? else {
            return Ok(None);
        };
        let messages = messages::list_for_session(store.conn(), session_id)?;
        Ok(Some((session, messages, dispute.initiator)))
    }

    fn reload(&self, session_id: &str) -> Result<Session> {
        let store = self.lock_store()?;
        sessions::get(store.conn(), session_id)?
            .ok_or_else(|| Error::Schema(format!("session {session_id} disappeared")))
    }

    /// A party's language changes when the judge detects another enabled
    /// language (`docs/judgments.md` §3). Returns the parties that changed.
    fn update_languages(&self, session: &Session, facts: &Facts, now: i64) -> Result<Vec<Party>> {
        let default = self.settings.default_language.as_str();
        let mut changed = Vec::new();
        let store = self.lock_store()?;
        for party in [Party::Buyer, Party::Seller] {
            let detected = facts.party(party).and_then(|f| f.language.as_deref());
            if let Some(lang) = detected
                && lang != session.language(party, default)
            {
                sessions::set_language(store.conn(), &session.session_id, party, lang, now)?;
                changed.push(party);
            }
        }
        Ok(changed)
    }

    /// Sends a party its templates, in its language, in order.
    async fn ask(&self, session: &Session, party: Party, templates: &[&str]) -> Result<()> {
        for template in templates {
            self.send_template(session, party, template).await?;
        }
        Ok(())
    }

    fn count_round(&self, session: &Session, next: &NextQuestions, now: i64) -> Result<()> {
        if next.counts_as_round() {
            let store = self.lock_store()?;
            sessions::increment_rounds(store.conn(), &session.session_id, now)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_evaluation(
        &self,
        ready: &ReadyJudge,
        session: &Session,
        messages: &[Message],
        judged: &crate::judge::Judged,
        action: &Action,
        latency_ms: u32,
        now: i64,
    ) -> Result<()> {
        let answers = serde_json::to_value(&judged.answers)
            .map_err(|e| Error::Schema(format!("answers do not serialize: {e}")))?;
        let action = serde_json::to_value(action)
            .map_err(|e| Error::Schema(format!("action does not serialize: {e}")))?;
        let store = self.lock_store()?;
        evaluations::insert(
            store.conn(),
            &evaluations::NewEvaluation {
                session_id: &session.session_id,
                question_set_version: ready.turn.id(),
                judge_id: ready.judge.id(),
                last_message_id: messages.last().map_or(0, |m| m.id),
                answers: &answers,
                action: &action,
                input_tokens: judged.input_tokens,
                latency_ms: Some(latency_ms),
                now,
            },
        )?;
        Ok(())
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
