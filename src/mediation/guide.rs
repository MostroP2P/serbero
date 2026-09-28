//! Self-resolution paths (`docs/spec.md` §7.4): explaining the option that
//! fits, watching the dispute, and closing when the parties resolve it.
//! Serbero only explains; the parties act from their own Mostro apps.

use serde_json::json;

use super::Mediator;
use super::handoff::{BRIEF_PENDING, TurnReading, recipients};
use crate::chat::{Outbound, send_after_close};
use crate::error::Result;
use crate::nostr::dm::DmSender;
use crate::notifier::notify_solvers;
use crate::policy::{HandoffReason, Path};
use crate::solver::{self, Outcome, Subject};
use crate::store::messages::{self, Direction};
use crate::store::sessions::{self, Party, Session, SessionState};
use crate::store::{disputes, events};

/// Sent to both parties when they resolved the dispute themselves.
const RESOLVED_THANKS: &str = "resolved_thanks";

/// The guidance for each path, the party who would act first
/// (`docs/spec.md` §7.4 and AGENTS.md rule 3).
fn guides(path: Path) -> [(Party, &'static str); 2] {
    match path {
        Path::PaymentArrived => [
            (Party::Seller, "guide_arrived_seller"),
            (Party::Buyer, "guide_arrived_buyer"),
        ],
        Path::PaymentNotSent => [
            (Party::Buyer, "guide_not_sent_buyer"),
            (Party::Seller, "guide_not_sent_seller"),
        ],
    }
}

/// How the session ended, from what it recorded before it closed.
pub fn outcome(session: &Session, by_parties: bool) -> Outcome {
    if let Some(reason) = session
        .handoff_reason
        .as_deref()
        .and_then(HandoffReason::parse)
    {
        return Outcome::HandedOff(reason);
    }
    if session.state == SessionState::Superseded || !by_parties {
        // A final status set by a solver means a human took the dispute.
        return Outcome::Superseded;
    }
    Outcome::SelfResolved
}

/// Recorded once a resolved dispute's closing is complete: the parties
/// thanked (when they resolved it) and the final report delivered.
const FINISHED_EVENT: &str = "finished";
const FINAL_REPORT: &str = "final_report";

/// How far back startup looks for closings that did not complete.
pub const FINISH_LOOKBACK_SECS: i64 = 7 * 24 * 3600;

impl<S: DmSender> Mediator<S> {
    /// Enters `guiding`, briefs the solvers, and explains the path to both
    /// parties. The session is claimed first, so guidance is sent once and
    /// never after the session moved on. A brief no solver received is
    /// recorded as `brief_pending`; a party who did not get the guidance
    /// gets it on the next turn (`resend_guides`).
    pub async fn guide(
        &self,
        session: &Session,
        path: Path,
        reading: TurnReading<'_>,
        now: i64,
    ) -> Result<()> {
        {
            let store = self.lock_store()?;
            // One transaction: a `guiding` session always has its path.
            let tx = store.conn().unchecked_transaction()?;
            if !sessions::start_guiding(&tx, &session.session_id, now)? {
                return Ok(());
            }
            events::append(
                &tx,
                &events::NewEvent {
                    dispute_id: &session.dispute_id,
                    session_id: Some(&session.session_id),
                    kind: "guided",
                    payload: json!({ "path": path.as_str() }),
                    now,
                },
            )?;
            tx.commit()?;
        }
        let delivered = self
            .brief_solvers(session, Subject::Guide(path), Some(reading), now)
            .await?;
        if delivered == 0 {
            let store = self.lock_store()?;
            events::append(
                store.conn(),
                &events::NewEvent {
                    dispute_id: &session.dispute_id,
                    session_id: Some(&session.session_id),
                    kind: BRIEF_PENDING,
                    payload: json!({ "path": path.as_str() }),
                    now,
                },
            )?;
        }
        self.send_guides(session, path).await?;
        Ok(())
    }

    /// Sends the guidance a guiding session's parties have not received
    /// yet. Returns how many templates are still missing.
    pub async fn resend_guides(&self, session: &Session) -> Result<usize> {
        let path = {
            let store = self.lock_store()?;
            events::list_for_dispute(store.conn(), &session.dispute_id)?
                .into_iter()
                .rev()
                .find(|e| {
                    e.kind == "guided" && e.session_id.as_deref() == Some(&session.session_id)
                })
                .and_then(|e| e.payload["path"].as_str().and_then(Path::parse))
        };
        match path {
            Some(path) => self.send_guides(session, path).await,
            None => Ok(0),
        }
    }

    /// Sends each guide template its party has not received, each on its
    /// own, so one failed send does not keep the other party waiting.
    async fn send_guides(&self, session: &Session, path: Path) -> Result<usize> {
        let mut missing = 0;
        for (party, template) in guides(path) {
            if self.received(session, party, template)? {
                continue;
            }
            if let Err(e) = self.send_template(session, party, template).await {
                missing += 1;
                tracing::warn!(session_id = %session.session_id, %party, template, error = %e, "guidance not sent; retried on the next turn");
            }
        }
        Ok(missing)
    }

    /// Whether the party already received this template in this session.
    fn received(&self, session: &Session, party: Party, template: &str) -> Result<bool> {
        let store = self.lock_store()?;
        Ok(
            messages::list_for_session(store.conn(), &session.session_id)?
                .iter()
                .any(|m| {
                    m.direction == Direction::Out
                        && m.party == party
                        && m.template_id.as_deref() == Some(template)
                }),
        )
    }

    /// The dispute reached its final status and its session, if any, is
    /// closed: thank the parties if they resolved it, and send the solvers
    /// the final report. Each step is attempted on its own and skipped once
    /// done, and the closing is recorded as `finished` only when all of it
    /// went out, so `finish_pending` can complete it later. Disputes Serbero
    /// never mediated are left alone.
    pub async fn finish(
        &self,
        dispute_id: &str,
        status: &str,
        by_parties: bool,
        now: i64,
    ) -> Result<Option<Outcome>> {
        // One closing at a time: the resolution hook and `finish_pending`
        // may both try the same dispute.
        let _closing = self.finishing.lock().await;
        let (session, history) = {
            let store = self.lock_store()?;
            (
                sessions::latest_for_dispute(store.conn(), dispute_id)?,
                events::list_for_dispute(store.conn(), dispute_id)?,
            )
        };
        let Some(session) = session else {
            return Ok(None);
        };
        if history.iter().any(|e| e.kind == FINISHED_EVENT) {
            return Ok(None);
        }
        let _ = self.chats.close(&session.session_id).await;
        let outcome = outcome(&session, by_parties);
        let mut complete = true;
        if outcome == Outcome::SelfResolved {
            for party in [Party::Buyer, Party::Seller] {
                if self.received(&session, party, RESOLVED_THANKS)? {
                    continue;
                }
                if let Err(e) = self.send_thanks(&session, party).await {
                    complete = false;
                    tracing::warn!(dispute_id, %party, error = %e, "thanks not sent; retried later");
                }
            }
        }
        let reported = history
            .iter()
            .any(|e| e.kind == "notification_sent" && e.payload["notification"] == FINAL_REPORT);
        if !reported {
            let report = solver::final_report(
                dispute_id,
                status,
                outcome,
                session.rounds,
                now - session.opened_at,
            );
            let assigned = {
                let store = self.lock_store()?;
                disputes::get(store.conn(), dispute_id)?.and_then(|d| d.assigned_solver)
            };
            let to = recipients(&self.solvers, assigned.as_deref());
            let delivered = notify_solvers(
                &self.store,
                &self.sender,
                &to,
                dispute_id,
                FINAL_REPORT,
                &report,
                now,
            )
            .await?;
            complete &= delivered > 0 || to.is_empty();
        }
        if complete {
            let store = self.lock_store()?;
            events::append(
                store.conn(),
                &events::NewEvent {
                    dispute_id,
                    session_id: Some(&session.session_id),
                    kind: FINISHED_EVENT,
                    payload: json!({ "status": status }),
                    now,
                },
            )?;
        }
        Ok(Some(outcome))
    }

    /// Completes the closings of disputes resolved since `since` that did
    /// not finish: the process stopped before, a send failed, or the
    /// resolution was applied before the hook was installed. Returns how
    /// many it attempted.
    pub async fn finish_pending(&self, since: i64, now: i64) -> usize {
        let pending = match self
            .lock_store()
            .and_then(|store| events::unfinished_resolutions(store.conn(), since))
        {
            Ok(pending) => pending,
            Err(e) => {
                tracing::error!(error = %e, "cannot list unfinished resolutions");
                return 0;
            }
        };
        for resolution in &pending {
            if let Err(e) = self
                .finish(
                    &resolution.dispute_id,
                    &resolution.status,
                    resolution.by_parties,
                    now,
                )
                .await
            {
                tracing::warn!(dispute_id = resolution.dispute_id, error = %e, "cannot close the mediation");
            }
        }
        pending.len()
    }

    async fn send_thanks(&self, session: &Session, party: Party) -> Result<()> {
        let (text, lang) = self.render_for(session, party, RESOLVED_THANKS)?;
        send_after_close(
            &self.client,
            &self.gate,
            &self.store,
            &self.keys,
            session,
            &Outbound {
                party,
                text: &text,
                template_id: Some(RESOLVED_THANKS),
                lang: Some(&lang),
            },
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sessions::testing::store_with_session;

    fn session(state: SessionState, reason: Option<&str>) -> Session {
        let store = store_with_session();
        let mut session = sessions::get(store.conn(), "s1").unwrap().unwrap();
        session.state = state;
        session.handoff_reason = reason.map(str::to_owned);
        session
    }

    #[test]
    fn the_outcome_follows_what_the_session_recorded() {
        assert_eq!(
            outcome(&session(SessionState::Closed, Some("fraud_signal")), true),
            Outcome::HandedOff(HandoffReason::FraudSignal)
        );
        assert_eq!(
            outcome(&session(SessionState::Superseded, None), true),
            Outcome::Superseded
        );
        assert_eq!(
            outcome(&session(SessionState::Closed, None), false),
            Outcome::Superseded
        );
        assert_eq!(
            outcome(&session(SessionState::Closed, None), true),
            Outcome::SelfResolved
        );
    }

    #[test]
    fn each_path_guides_the_party_who_acts_first() {
        assert_eq!(
            guides(Path::PaymentArrived)[0],
            (Party::Seller, "guide_arrived_seller")
        );
        assert_eq!(
            guides(Path::PaymentNotSent)[0],
            (Party::Buyer, "guide_not_sent_buyer")
        );
    }
}
