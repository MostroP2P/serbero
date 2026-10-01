//! The turn loop (`docs/spec.md` §7.3): settle a burst, judge the session,
//! decide, act, and record the evaluation.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::handoff::{TurnReading, last_seen};
use super::{Mediator, ReadyJudge, history, settle::Settle};
use crate::error::{Error, Result};
use crate::judge::brief::BriefQuestions;
use crate::judge::facts::{self, Facts};
use crate::judge::state;
use crate::nostr::dm::DmSender;
use crate::policy::next::{History, next_questions};
use crate::policy::{Action, HandoffReason, NextQuestions, Phase, Turn, decide, timers};
use crate::store::messages::{self, Direction, Message};
use crate::store::sessions::{self, Party, Session, SessionState};
use crate::store::{disputes, evaluations, events};

/// Event recording a turn in which a party sent too many messages.
const FLOOD_STRIKE: &str = "flood_strike";

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Nothing to judge: the session is not live, no party wrote, or the
    /// judge is not ready.
    Skipped(&'static str),
    /// Not judged yet; the turn runs again after the quiet period.
    Deferred(&'static str),
    /// The judge failed after its retries; the session was handed off as
    /// `judge_unavailable`.
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
        // Party messages stored before a restart, and not answered, are not
        // forwarded again by the relays (they are duplicates): schedule them.
        for session_id in self.pending_sessions() {
            settle.touch(&session_id, Instant::now());
        }
        // Each session's turn runs in its own task, so a turn stuck on a
        // slow relay or judge never holds back another session. A session
        // has at most one turn running; one that falls due meanwhile runs
        // right after it.
        let mut running = JoinSet::new();
        let mut sessions_of = HashMap::new();
        let mut due_again = HashSet::new();
        loop {
            let deadline = settle.next_deadline();
            tokio::select! {
                message = received.recv() => match message {
                    Some(session_id) => settle.touch(&session_id, Instant::now()),
                    None => return,
                },
                () = sleep_until(deadline), if deadline.is_some() => {
                    for session_id in settle.take_due(Instant::now()) {
                        if sessions_of.values().any(|running| running == &session_id) {
                            due_again.insert(session_id);
                            continue;
                        }
                        let mediator = std::sync::Arc::clone(&self);
                        let id = session_id.clone();
                        let task = running.spawn(async move {
                            mediator.turn_logged(&id, crate::daemon::now()).await
                        });
                        sessions_of.insert(task.id(), session_id);
                    }
                }
                Some(done) = running.join_next_with_id(), if !running.is_empty() => {
                    let (task, again) = match done {
                        Ok((task, again)) => (task, again),
                        Err(e) => {
                            tracing::error!(error = %e, "turn task failed");
                            (e.id(), true)
                        }
                    };
                    if let Some(session_id) = sessions_of.remove(&task)
                        && (again | due_again.remove(&session_id))
                    {
                        // Not judged yet (judge not ready, or newer
                        // messages came in while judging), or due again
                        // while it ran: keep it due.
                        settle.touch(&session_id, Instant::now());
                    }
                }
            }
        }
    }

    /// Live sessions whose newest message, by arrival, is a party's.
    fn pending_sessions(&self) -> Vec<String> {
        let Ok(store) = self.lock_store() else {
            return Vec::new();
        };
        let Ok(live) = sessions::list_live(store.conn()) else {
            return Vec::new();
        };
        live.into_iter()
            .filter(|session| {
                let Ok(messages) = messages::list_for_session(store.conn(), &session.session_id)
                else {
                    return false;
                };
                if session.state == SessionState::HandedOff {
                    // Messages the solvers have not seen, even when a notice
                    // was stored after them.
                    let seen = events::list_for_dispute(store.conn(), &session.dispute_id)
                        .map(|history| last_seen(&history, &session.session_id));
                    return seen.is_ok_and(|seen| {
                        messages
                            .iter()
                            .any(|m| m.direction == Direction::In && m.id > seen)
                    });
                }
                messages
                    .iter()
                    .max_by_key(|m| m.id)
                    .is_some_and(|m| m.direction == Direction::In)
            })
            .map(|session| session.session_id)
            .collect()
    }

    /// Runs one turn and logs it. Returns whether the turn must run again.
    async fn turn_logged(&self, session_id: &str, now: i64) -> bool {
        let outcome = self.run_turn(session_id, now).await;
        let again = matches!(outcome, Ok(TurnOutcome::Deferred(_)));
        match outcome {
            Ok(TurnOutcome::Deferred(why)) => tracing::debug!(session_id, why, "turn deferred"),
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
        again
    }

    /// One turn over the session as stored now.
    pub async fn run_turn(&self, session_id: &str, now: i64) -> Result<TurnOutcome> {
        let Some((session, messages, opened_by)) = self.load(session_id)? else {
            return Ok(TurnOutcome::Skipped("session not live"));
        };
        // Forwarding after a handoff needs no judge.
        if session.state == SessionState::HandedOff {
            return Ok(TurnOutcome::Forwarded(
                self.forward_updates(&session, now).await?,
            ));
        }
        if session.state == SessionState::Guiding {
            // Guidance that did not reach a party goes out now.
            if let Err(e) = self.resend_guides(&session).await {
                tracing::warn!(session_id, error = %e, "cannot resend guidance");
            }
        }
        let Some(ready) = self.ready_judge() else {
            return Ok(TurnOutcome::Deferred("judge not ready"));
        };
        let phase = match session.state {
            SessionState::Active => Phase::Gathering,
            SessionState::Guiding => Phase::Guiding,
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
        if self.flooded(&session, &messages, now)? {
            // The flooding turn is not judged; the solvers get the last
            // judged turn's reading, as timer handoffs do.
            let last = self.last_reading(&session, &messages)?;
            let reading = last.as_ref().map(|l| TurnReading {
                state: &l.state,
                answers: &l.answers,
                facts: &l.facts,
                last_message_id: l.last_message_id,
            });
            self.hand_off(&session, HandoffReason::Flood, reading, now)
                .await?;
            return Ok(TurnOutcome::Decided(Action::Handoff(HandoffReason::Flood)));
        }
        let questions = ready.turn.for_turn(&wrote, phase == Phase::Guiding);
        let started = std::time::Instant::now();
        let judged = match ready.judge.evaluate(&built.value, &questions).await {
            Ok(judged) => judged,
            Err(e) => {
                // The adapter already retried: the solvers take over with the
                // transcript, and no fact is shown (`docs/spec.md` §7.6).
                self.hand_off(&session, HandoffReason::JudgeUnavailable, None, now)
                    .await?;
                return Ok(TurnOutcome::JudgeFailed(e.to_string()));
            }
        };
        let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        if self.newer_than(&session, &messages)? {
            // The party wrote again while the judge worked: acting now would
            // answer an outdated state. The next turn judges everything.
            return Ok(TurnOutcome::Deferred("newer party messages arrived"));
        }
        if self.moved_on(&session)? {
            // A timer or the notifier handed off or ended the session while
            // the judge worked.
            return Ok(TurnOutcome::Skipped("session changed while judging"));
        }
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
            buyer_rounds: session.buyer_rounds,
            seller_rounds: session.seller_rounds,
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
                    last_message_id: messages.iter().map(|m| m.id).max().unwrap_or(0),
                };
                self.hand_off(&session, *reason, Some(reading), now).await?;
            }
            Action::Guide(path) => {
                let reading = TurnReading {
                    state: &built.value,
                    answers: &judged.answers,
                    facts: &facts,
                    last_message_id: messages.iter().map(|m| m.id).max().unwrap_or(0),
                };
                self.guide(&session, *path, reading, now).await?;
            }
            Action::Wait => {}
        }
        Ok(TurnOutcome::Decided(action))
    }

    /// Records a flood strike for each party over `max_messages_per_turn`
    /// in this turn; a party with `FLOOD_STRIKES` strikes floods the session
    /// (`docs/judgments.md` §4.2). Checked before the judge is called.
    ///
    /// A turn's messages are those after the last judged turn or strike, in
    /// arrival order: messages already counted never count again, even when
    /// Serbero sent nothing in between (a guiding `Wait`).
    fn flooded(&self, session: &Session, messages: &[Message], now: i64) -> Result<bool> {
        let store = self.lock_store()?;
        let history = events::list_for_dispute(store.conn(), &session.dispute_id)?;
        let strikes: Vec<_> = history
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(&session.session_id))
            .filter(|e| e.kind == FLOOD_STRIKE)
            .collect();
        // Turn evaluations only: a brief is not a settled turn.
        let brief_set = BriefQuestions::new();
        let judged = evaluations::list_for_session(store.conn(), &session.session_id)?
            .iter()
            .filter(|e| e.question_set_version != brief_set.id())
            .map(|e| e.last_message_id)
            .max()
            .unwrap_or(0);
        let counted = strikes
            .iter()
            .filter_map(|e| e.payload["last_message_id"].as_i64())
            .max()
            .unwrap_or(0)
            .max(judged);
        let last_message_id = messages.iter().map(|m| m.id).max().unwrap_or(0);
        let mut flooded = false;
        for party in [Party::Buyer, Party::Seller] {
            let count = messages
                .iter()
                .filter(|m| m.direction == Direction::In && m.party == party && m.id > counted)
                .count();
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            if !timers::over_limit(count, self.settings.max_messages_per_turn) {
                continue;
            }
            events::append(
                store.conn(),
                &events::NewEvent {
                    dispute_id: &session.dispute_id,
                    session_id: Some(&session.session_id),
                    kind: FLOOD_STRIKE,
                    payload: json!({
                        "party": party.to_string(),
                        "messages": count,
                        "last_message_id": last_message_id,
                    }),
                    now,
                },
            )?;
            let earlier = strikes
                .iter()
                .filter(|e| e.payload["party"] == party.to_string())
                .count();
            flooded |= timers::is_flood(u32::try_from(earlier + 1).unwrap_or(u32::MAX));
        }
        Ok(flooded)
    }

    /// Whether the session left the state this turn read it in.
    fn moved_on(&self, session: &Session) -> Result<bool> {
        let store = self.lock_store()?;
        Ok(sessions::get(store.conn(), &session.session_id)?
            .is_none_or(|current| current.state != session.state))
    }

    /// Whether a party message arrived after `messages` was read.
    fn newer_than(&self, session: &Session, messages: &[Message]) -> Result<bool> {
        let seen = messages.iter().map(|m| m.id).max().unwrap_or(0);
        let store = self.lock_store()?;
        Ok(
            messages::list_for_session(store.conn(), &session.session_id)?
                .iter()
                .any(|m| m.direction == Direction::In && m.id > seen),
        )
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
            let asked: Vec<Party> = [Party::Buyer, Party::Seller]
                .into_iter()
                .filter(|party| next.asks(*party))
                .collect();
            let store = self.lock_store()?;
            sessions::increment_rounds(store.conn(), &session.session_id, &asked, now)?;
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
                // The state covers every stored row; rows are ordered by
                // time, so the newest one is not always the last.
                last_message_id: messages.iter().map(|m| m.id).max().unwrap_or(0),
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
