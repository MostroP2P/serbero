//! Self-resolution paths (`docs/spec.md` §7.4): explaining the option that
//! fits, watching the dispute, and closing when the parties resolve it.
//! Serbero only explains; the parties act from their own Mostro apps.

use serde_json::json;

use super::Mediator;
use super::handoff::{TurnReading, recipients};
use crate::chat::{Outbound, send_after_close};
use crate::error::Result;
use crate::nostr::dm::DmSender;
use crate::notifier::notify_solvers;
use crate::policy::{HandoffReason, Path};
use crate::solver::{self, Outcome, Subject};
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

impl<S: DmSender> Mediator<S> {
    /// Briefs the solvers, enters `guiding`, and explains the path to both
    /// parties. Guidance is sent once per session.
    pub async fn guide(
        &self,
        session: &Session,
        path: Path,
        reading: TurnReading<'_>,
        now: i64,
    ) -> Result<()> {
        self.brief_solvers(session, Subject::Guide(path), Some(reading), now)
            .await?;
        {
            let store = self.lock_store()?;
            sessions::set_state(
                store.conn(),
                &session.session_id,
                SessionState::Guiding,
                now,
            )?;
            events::append(
                store.conn(),
                &events::NewEvent {
                    dispute_id: &session.dispute_id,
                    session_id: Some(&session.session_id),
                    kind: "guided",
                    payload: json!({ "path": path.as_str() }),
                    now,
                },
            )?;
        }
        for (party, template) in guides(path) {
            self.send_template(session, party, template).await?;
        }
        Ok(())
    }

    /// The dispute reached its final status and its session, if any, is
    /// closed: thank the parties if they resolved it, and send the solvers
    /// the final report. Disputes Serbero never mediated are left alone.
    pub async fn finish(
        &self,
        dispute_id: &str,
        status: &str,
        by_parties: bool,
        now: i64,
    ) -> Result<Option<Outcome>> {
        let session = {
            let store = self.lock_store()?;
            sessions::latest_for_dispute(store.conn(), dispute_id)?
        };
        let Some(session) = session else {
            return Ok(None);
        };
        let _ = self.chats.close(&session.session_id).await;
        let outcome = outcome(&session, by_parties);
        if outcome == Outcome::SelfResolved {
            for party in [Party::Buyer, Party::Seller] {
                let (text, lang) = self.render_for(&session, party, RESOLVED_THANKS)?;
                send_after_close(
                    &self.client,
                    &self.gate,
                    &self.store,
                    &self.keys,
                    &session,
                    &Outbound {
                        party,
                        text: &text,
                        template_id: Some(RESOLVED_THANKS),
                        lang: Some(&lang),
                    },
                )
                .await?;
            }
        }
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
        notify_solvers(
            &self.store,
            &self.sender,
            &to,
            dispute_id,
            "final_report",
            &report,
            now,
        )
        .await?;
        Ok(Some(outcome))
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
