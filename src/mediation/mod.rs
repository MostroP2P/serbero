//! Assisted mediation (`docs/spec.md` §7): which disputes Serbero takes,
//! and opening a session with both parties.
//!
//! Everything here runs in its own task, off the dispute event loop, so a
//! slow node or relay never delays notification (AGENTS.md, relays rule 1).
//! Serbero never moves funds: it takes a dispute only to talk to the
//! parties, and a human solver can take it over at any time.

pub mod eligibility;
pub mod guide;
pub mod handoff;
pub mod history;
pub mod settle;
pub mod timers;
pub mod turn;

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use mostro_core::dispute::SolverDisputeInfo;
use nostr_sdk::prelude::{Client, Keys, PublicKey};
use serde_json::json;
use uuid::Uuid;

use self::eligibility::{Candidate, Ineligible};
use crate::catalog::{Amount, Catalogs};
use crate::chat::channels::Chats;
use crate::chat::{Outbound, OutboundGate, send_to_party};
use crate::error::{Error, Result};
use crate::mostro::{node, order, take};
use crate::nostr::dm::DmSender;
use crate::notifier::{OwnTakes, Solver, notify_solvers};
use crate::policy::template;
use crate::store::sessions::{self, NewSession, Party, Session, SessionState};
use crate::store::{Store, disputes, events};

/// How long the node, the take and the order lookup may each take.
pub const MOSTRO_TIMEOUT: Duration = Duration::from_secs(15);

/// Each party's first question, sent with the intro (`docs/spec.md` §7.2).
const OPENERS: [(Party, &str); 2] = [
    (Party::Buyer, template::ASK_BUYER_SENT),
    (Party::Seller, template::ASK_SELLER_RECEIVED),
];

/// Builds the configured judge. Only a live provider can mediate; the
/// `recorded` provider exists for tests and evaluation.
pub fn judge_from_config(
    config: &crate::config::JudgeConfig,
    api_key: Option<&str>,
) -> std::result::Result<Arc<dyn crate::judge::Judge>, String> {
    match config.provider.as_str() {
        "typesafe" => {
            let key = api_key.ok_or_else(|| format!("{} is not set", config.api_key_env))?;
            let judge = crate::judge::providers::typesafe::TypeSafeJudge::new(config, key)
                .map_err(|e| e.to_string())?;
            Ok(Arc::new(judge))
        }
        other => Err(format!("provider {other} cannot mediate live disputes")),
    }
}

/// Runs the judge's startup checks (thresholds, capabilities, health) and
/// returns whether mediation may run. The health check calls the provider,
/// so the caller runs this in the background.
pub async fn check_judge(
    judge: &dyn crate::judge::Judge,
    thresholds: Option<&crate::config::Thresholds>,
    turn: &crate::judge::questions::TurnQuestions,
) -> eligibility::Readiness {
    let problems = judge.capabilities().check(turn.all());
    let health = judge.health_check().await.err().map(|e| e.to_string());
    eligibility::readiness(&eligibility::JudgeChecks {
        has_thresholds: thresholds.is_some(),
        capability_problems: &problems,
        health_error: health.as_deref(),
    })
}

/// The `[mediation]` settings a session needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediationSettings {
    pub enabled: bool,
    pub default_language: String,
    /// Enabled language codes (`[mediation].languages`).
    pub languages: Vec<String>,
    pub max_rounds: u32,
    pub max_message_chars: usize,
    pub max_messages_per_turn: u32,
    pub response_timeout: Duration,
    pub self_resolution_timeout: Duration,
}

/// A judge that passed its startup checks, with what judging needs.
pub struct ReadyJudge {
    pub judge: Arc<dyn crate::judge::Judge>,
    pub thresholds: crate::config::Thresholds,
    pub turn: crate::judge::questions::TurnQuestions,
}

/// What mediation needs from the rest of the daemon.
pub struct Mediator<S> {
    pub client: Client,
    pub keys: Keys,
    pub mostro: PublicKey,
    pub store: Arc<Mutex<Store>>,
    pub gate: Arc<OutboundGate>,
    pub chats: Arc<Chats>,
    pub catalogs: Catalogs,
    pub settings: MediationSettings,
    pub sender: S,
    pub solvers: Vec<Solver>,
    pub own_takes: OwnTakes,
    /// Set once the judge passed its startup checks; no dispute is taken
    /// and no turn is judged before.
    pub judge: RwLock<Option<Arc<ReadyJudge>>>,
}

/// How an attempt to mediate a dispute ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opening {
    Ineligible(Ineligible),
    /// Serbero did not take it (node, take or order problem); solvers were
    /// already notified as usual.
    NotTaken(String),
    /// Taken, but the parties could not be reached: handed to the solvers.
    HandedOff(String),
    Opened {
        session_id: String,
    },
}

impl<S: DmSender> Mediator<S> {
    pub fn set_ready(&self, judge: ReadyJudge) {
        if let Ok(mut slot) = self.judge.write() {
            *slot = Some(Arc::new(judge));
        }
    }

    /// The judge, once it passed its startup checks.
    pub fn ready_judge(&self) -> Option<Arc<ReadyJudge>> {
        self.judge.read().ok().and_then(|slot| slot.clone())
    }

    /// Mediates the dispute if it is eligible; every outcome is logged and
    /// recorded, none is an error for the caller.
    pub async fn consider(&self, dispute_id: &str, now: i64) -> Opening {
        let outcome = match self.claim(dispute_id) {
            Ok(Err(reason)) => Opening::Ineligible(reason),
            Ok(Ok(())) => {
                let outcome = self.open(dispute_id, now).await;
                self.release(dispute_id);
                outcome.unwrap_or_else(|e| Opening::NotTaken(e.to_string()))
            }
            Err(e) => Opening::NotTaken(e.to_string()),
        };
        match &outcome {
            Opening::Ineligible(reason) => {
                tracing::debug!(dispute_id, ?reason, "not mediating")
            }
            Opening::NotTaken(reason) => {
                tracing::warn!(dispute_id, reason, "mediation not opened")
            }
            Opening::HandedOff(reason) => {
                tracing::warn!(
                    dispute_id,
                    reason,
                    "took the dispute but could not open mediation"
                )
            }
            Opening::Opened { session_id } => {
                tracing::info!(dispute_id, session_id, "mediation opened")
            }
        }
        outcome
    }

    /// Checks eligibility and, if eligible, marks the take as in flight in
    /// the same step, so two events for one dispute cannot both take it.
    fn claim(&self, dispute_id: &str) -> Result<std::result::Result<(), Ineligible>> {
        let (lifecycle, had_session) = {
            let store = self.lock_store()?;
            let Some(dispute) = disputes::get(store.conn(), dispute_id)? else {
                return Err(Error::Schema(format!("unknown dispute {dispute_id}")));
            };
            (
                dispute.lifecycle,
                sessions::exists_for_dispute(store.conn(), dispute_id)?,
            )
        };
        let mut taking = self
            .own_takes
            .lock()
            .map_err(|_| Error::Schema("own takes lock poisoned".into()))?;
        let candidate = Candidate {
            enabled: self.settings.enabled,
            ready: self.ready_judge().is_some(),
            lifecycle,
            had_session,
            taking: taking.contains(dispute_id),
        };
        let verdict = eligibility::check(&candidate);
        if verdict.is_ok() {
            taking.insert(dispute_id.to_owned());
        }
        Ok(verdict)
    }

    fn release(&self, dispute_id: &str) {
        if let Ok(mut taking) = self.own_takes.lock() {
            taking.remove(dispute_id);
        }
    }

    /// `docs/spec.md` §7.2: take, store the facts, open the chat, greet both
    /// parties, and tell the solvers.
    async fn open(&self, dispute_id: &str, now: i64) -> Result<Opening> {
        let info = match self.take(dispute_id).await {
            Ok(info) => info,
            Err(reason) => {
                self.record(dispute_id, None, "mediation_not_opened", &reason, now)?;
                return Ok(Opening::NotTaken(reason));
            }
        };
        let session = match self.create_session(dispute_id, &info, now).await {
            Ok(session) => session,
            Err(reason) => return self.abandon(dispute_id, None, &reason, now).await,
        };
        if let Err(e) = self.greet(&session).await {
            let reason = format!("cannot reach the parties: {e}");
            return self.abandon(dispute_id, Some(&session), &reason, now).await;
        }
        {
            let store = self.lock_store()?;
            sessions::set_state(store.conn(), &session.session_id, SessionState::Active, now)?;
        }
        let text = crate::solver::mediation_started(dispute_id);
        notify_solvers(
            &self.store,
            &self.sender,
            &self.solvers,
            dispute_id,
            "mediation_started",
            &text,
            now,
        )
        .await?;
        self.record(
            dispute_id,
            Some(&session.session_id),
            "mediation_opened",
            "",
            now,
        )?;
        Ok(Opening::Opened {
            session_id: session.session_id,
        })
    }

    /// The node must speak protocol v2; then `admin-take-dispute`.
    async fn take(&self, dispute_id: &str) -> std::result::Result<SolverDisputeInfo, String> {
        let node = node::fetch(&self.client, self.mostro, MOSTRO_TIMEOUT)
            .await
            .map_err(|e| format!("cannot read the node's info: {e}"))?
            .ok_or("the node's info event was not found")?;
        if !node.speaks_v2() {
            return Err("the node does not speak protocol v2".into());
        }
        let id =
            Uuid::parse_str(dispute_id).map_err(|e| format!("dispute id is not a uuid: {e}"))?;
        take::take_dispute(
            &self.client,
            &self.keys,
            self.mostro,
            id,
            node.pow_for_serbero(),
            MOSTRO_TIMEOUT,
        )
        .await
        .map_err(|e| format!("take failed: {e}"))
    }

    /// Stores the session with the trade keys and the order facts, and opens
    /// its chat channels. Unknown order facts stay unknown (§7.2).
    async fn create_session(
        &self,
        dispute_id: &str,
        info: &SolverDisputeInfo,
        now: i64,
    ) -> std::result::Result<Session, String> {
        let (Some(buyer), Some(seller)) = (&info.buyer_pubkey, &info.seller_pubkey) else {
            return Err("the node did not send both trade keys".into());
        };
        let facts = order::fetch(
            &self.client,
            self.mostro,
            &info.id.to_string(),
            MOSTRO_TIMEOUT,
        )
        .await
        .unwrap_or_default();
        let session_id = Uuid::new_v4().to_string();
        let amount = info.fiat_amount.to_string();
        let method = (!info.payment_method.is_empty()).then_some(info.payment_method.as_str());
        let session = {
            let store = self.lock_store().map_err(|e| e.to_string())?;
            let inserted = sessions::insert(
                store.conn(),
                &NewSession {
                    session_id: &session_id,
                    dispute_id,
                    buyer_trade_pubkey: buyer,
                    seller_trade_pubkey: seller,
                    fiat_amount: Some(&amount),
                    fiat_code: facts.fiat_code.as_deref(),
                    payment_method: method,
                    order_published_at: facts.published_at,
                    now,
                },
            )
            .map_err(|e| e.to_string())?;
            if !inserted {
                return Err("the dispute already has a live session".into());
            }
            sessions::get(store.conn(), &session_id)
                .map_err(|e| e.to_string())?
                .ok_or("the new session is missing")?
        };
        self.chats
            .open(&session)
            .await
            .map_err(|e| format!("cannot open the chat channels: {e}"))?;
        Ok(session)
    }

    /// The intro and each party's first question, in `default_language`
    /// (`_noamount` forms when the currency is unknown).
    async fn greet(&self, session: &Session) -> Result<()> {
        let catalog = self
            .catalogs
            .get(&self.settings.default_language)
            .ok_or_else(|| {
                Error::Catalog(format!("no catalog for {}", self.settings.default_language))
            })?;
        let amount = match (&session.fiat_amount, &session.fiat_code) {
            (Some(value), Some(currency)) => Some(Amount { value, currency }),
            _ => None,
        };
        for (party, question) in OPENERS {
            let text = catalog.render_opening(question, amount)?;
            send_to_party(
                &self.client,
                &self.gate,
                &self.store,
                &self.keys,
                session,
                &Outbound {
                    party,
                    text: &text,
                    template_id: Some(question),
                    lang: Some(&self.settings.default_language),
                },
            )
            .await?;
        }
        Ok(())
    }

    /// Serbero holds the dispute but cannot mediate it: the session (if any)
    /// is handed off and every solver is asked to take over.
    async fn abandon(
        &self,
        dispute_id: &str,
        session: Option<&Session>,
        reason: &str,
        now: i64,
    ) -> Result<Opening> {
        if let Some(session) = session {
            let _ = self.chats.close(&session.session_id).await;
            let store = self.lock_store()?;
            sessions::hand_off(
                store.conn(),
                &session.session_id,
                crate::policy::HandoffReason::OpeningFailed.as_str(),
                now,
            )?;
        }
        let text = crate::solver::opening_failed(dispute_id);
        notify_solvers(
            &self.store,
            &self.sender,
            &self.solvers,
            dispute_id,
            "handoff",
            &text,
            now,
        )
        .await?;
        let session_id = session.map(|s| s.session_id.as_str());
        self.record(dispute_id, session_id, "mediation_failed", reason, now)?;
        Ok(Opening::HandedOff(reason.to_owned()))
    }

    fn record(
        &self,
        dispute_id: &str,
        session_id: Option<&str>,
        kind: &str,
        reason: &str,
        now: i64,
    ) -> Result<()> {
        let store = self.lock_store()?;
        events::append(
            store.conn(),
            &events::NewEvent {
                dispute_id,
                session_id,
                kind,
                payload: if reason.is_empty() {
                    json!({})
                } else {
                    json!({ "reason": reason })
                },
                now,
            },
        )?;
        Ok(())
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, Store>> {
        self.store
            .lock()
            .map_err(|_| Error::Schema("store lock poisoned".into()))
    }
}

#[cfg(test)]
mod tests {
    use futures_util::future::BoxFuture;
    use serde_json::Value;

    use super::eligibility::Readiness;
    use super::*;
    use crate::judge::questions::{Language, TurnQuestions};
    use crate::judge::{Capabilities, Judge, JudgeError, Judged, QuestionKind, QuestionSet};

    /// A judge whose capabilities and health are set by the test.
    struct Probe {
        max_choice_options: usize,
        healthy: bool,
    }

    impl Judge for Probe {
        fn id(&self) -> &str {
            "test/probe"
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                question_kinds: vec![
                    QuestionKind::Noul,
                    QuestionKind::Choice,
                    QuestionKind::Score,
                ],
                max_choice_options: self.max_choice_options,
                max_score_levels: 10,
                max_context_tokens: None,
            }
        }

        fn evaluate<'a>(
            &'a self,
            _state: &'a Value,
            _questions: &'a QuestionSet,
        ) -> BoxFuture<'a, std::result::Result<Judged, JudgeError>> {
            Box::pin(async { Err(JudgeError::Unavailable("not in this test".into())) })
        }

        fn health_check(&self) -> BoxFuture<'_, std::result::Result<(), JudgeError>> {
            let healthy = self.healthy;
            Box::pin(async move {
                if healthy {
                    Ok(())
                } else {
                    Err(JudgeError::Unauthorized("bad key".into()))
                }
            })
        }
    }

    fn turn() -> TurnQuestions {
        TurnQuestions::new(&[Language {
            code: "en",
            name: "English",
        }])
    }

    fn thresholds() -> crate::config::Thresholds {
        serde_json::from_value(serde_json::json!({
            "guide": 0.9, "fact": 0.8, "human_request": 0.8,
            "fraud": 0.6, "conflict": 0.75, "outside_scope": 0.8
        }))
        .unwrap()
    }

    const HEALTHY: Probe = Probe {
        max_choice_options: 255,
        healthy: true,
    };

    #[tokio::test]
    async fn a_healthy_capable_calibrated_judge_is_ready() {
        let readiness = check_judge(&HEALTHY, Some(&thresholds()), &turn()).await;

        assert_eq!(readiness, Readiness::Ready);
    }

    #[tokio::test]
    async fn a_failed_health_check_keeps_mediation_off() {
        let down = Probe {
            healthy: false,
            ..HEALTHY
        };

        let readiness = check_judge(&down, Some(&thresholds()), &turn()).await;

        assert!(matches!(readiness, Readiness::Off(r) if r.contains("bad key")));
    }

    #[tokio::test]
    async fn a_question_set_the_provider_cannot_express_keeps_mediation_off() {
        // dispute_topic alone has seven options.
        let narrow = Probe {
            max_choice_options: 3,
            ..HEALTHY
        };

        let readiness = check_judge(&narrow, Some(&thresholds()), &turn()).await;

        assert!(matches!(readiness, Readiness::Off(r) if r.contains("dispute_topic")));
    }

    #[tokio::test]
    async fn missing_thresholds_keep_mediation_off() {
        let readiness = check_judge(&HEALTHY, None, &turn()).await;

        assert!(matches!(readiness, Readiness::Off(r) if r.contains("no thresholds")));
    }

    #[test]
    fn only_a_live_provider_can_mediate() {
        let recorded = crate::config::JudgeConfig {
            provider: "recorded".into(),
            ..Default::default()
        };
        let typesafe = crate::config::JudgeConfig::default();

        assert!(judge_from_config(&recorded, Some("k")).is_err());
        assert!(
            judge_from_config(&typesafe, None).is_err(),
            "the key is required"
        );
        assert!(judge_from_config(&typesafe, Some("k")).is_ok());
    }
}
