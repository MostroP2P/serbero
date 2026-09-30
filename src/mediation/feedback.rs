//! Solver feedback (`docs/evaluation.md` §5): a solver replies to a brief
//! with `wrong <question>` and Serbero records it against the evaluation
//! the brief was built from, so the turn can become a golden case.

use std::sync::Mutex;

use mostro_core::message::{Action, Payload};
use mostro_core::transport::unwrap_message_nip44;
use nostr_sdk::prelude::{Event, Keys};
use serde_json::json;

use crate::error::{Error, Result};
use crate::judge::questions::TurnQuestions;
use crate::notifier::Solver;
use crate::store::{Store, evaluations, events, sessions};

/// The notification a feedback answers.
const BRIEF: &str = "brief";

/// What a solver said was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feedback {
    /// A turn question id, e.g. `seller_receipt`.
    pub question: String,
    /// The dispute the solver named, if any.
    pub dispute_id: Option<String>,
}

/// Where a feedback was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub dispute_id: String,
    pub evaluation_id: i64,
}

/// Parses `wrong <question> [<dispute_id>]` (case-insensitive keyword). The
/// question must be one of the turn questions, so a typo is not recorded.
pub fn parse(text: &str) -> Option<Feedback> {
    let mut words = text.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("wrong") {
        return None;
    }
    let question = words.next()?.to_ascii_lowercase();
    let dispute_id = words.next().map(str::to_owned);
    if words.next().is_some() || !known_question(&question) {
        return None;
    }
    Some(Feedback {
        question,
        dispute_id,
    })
}

/// Turn question ids do not depend on the enabled languages.
fn known_question(id: &str) -> bool {
    TurnQuestions::new(&[]).all().questions.contains_key(id)
}

/// The solver's DM a feedback came in.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    /// Its event id: the same DM, delivered by several relays or replayed,
    /// is recorded once.
    pub id: &'a str,
    /// When the solver wrote it.
    pub written_at: i64,
}

/// Slack for the solver's clock when matching a reply to the brief before it.
const CLOCK_SKEW_SECS: i64 = 120;

/// Links a feedback to the evaluation it is about and records it. The reply
/// answers the newest brief that solver got before writing it (for the named
/// dispute, if any), and without such a brief it is not recorded. The
/// evaluation is the session's newest turn evaluation that asked the
/// question and existed when that brief was sent, so a later turn the
/// solver never saw is not blamed. `None` when nothing matches or the DM
/// was already recorded.
pub fn record(
    store: &Mutex<Store>,
    solver: &str,
    feedback: &Feedback,
    source: Source<'_>,
    now: i64,
) -> Result<Option<Recorded>> {
    let store = store
        .lock()
        .map_err(|_| Error::Schema("store lock poisoned".into()))?;
    let conn = store.conn();
    if events::feedback_recorded(conn, source.id)? {
        return Ok(None);
    }
    let until = source.written_at.saturating_add(CLOCK_SKEW_SECS);
    let dispute_id = match &feedback.dispute_id {
        Some(id) => id.clone(),
        None => match events::last_notification_to(conn, solver, BRIEF, until)? {
            Some(event) => event.dispute_id,
            None => return Ok(None),
        },
    };
    let Some(briefed_at) = events::list_for_dispute(conn, &dispute_id)?
        .into_iter()
        .rev()
        .find(|e| {
            e.kind == "notification_sent"
                && e.payload["notification"] == BRIEF
                && e.payload["solver"] == solver
                && e.created_at <= until
        })
        .map(|e| e.created_at)
    else {
        // The solver never got a brief for it: a typo or a guess.
        return Ok(None);
    };
    let Some(session) = sessions::latest_for_dispute(conn, &dispute_id)? else {
        return Ok(None);
    };
    let Some(evaluation) = evaluations::list_for_session(conn, &session.session_id)?
        .into_iter()
        .rev()
        .filter(|e| e.created_at <= briefed_at)
        .find(|e| e.answers.get(&feedback.question).is_some())
    else {
        return Ok(None);
    };
    events::append(
        conn,
        &events::NewEvent {
            dispute_id: &dispute_id,
            session_id: Some(&session.session_id),
            kind: "solver_feedback",
            payload: json!({
                "solver": solver,
                "question": feedback.question,
                "evaluation_id": evaluation.id,
                "source": source.id,
            }),
            now,
        },
    )?;
    Ok(Some(Recorded {
        dispute_id,
        evaluation_id: evaluation.id,
    }))
}

/// Handles a `kind 14` addressed to Serbero: a `send-dm` from a configured
/// solver that parses as feedback is recorded. Anything else (Mostro's own
/// replies, other senders, other text) is ignored.
pub fn handle(
    event: &Event,
    serbero: &Keys,
    solvers: &[Solver],
    store: &Mutex<Store>,
    now: i64,
) -> Result<Option<Recorded>> {
    let Ok(Some(opened)) = unwrap_message_nip44(event, serbero) else {
        return Ok(None);
    };
    if !solvers.iter().any(|s| s.pubkey == opened.identity) {
        return Ok(None);
    }
    let kind = opened.message.get_inner_message_kind();
    let (Action::SendDm, Some(Payload::TextMessage(text))) = (&kind.action, &kind.payload) else {
        return Ok(None);
    };
    let Some(feedback) = parse(text) else {
        return Ok(None);
    };
    record(
        store,
        &opened.identity.to_hex(),
        &feedback,
        Source {
            id: &event.id.to_hex(),
            written_at: i64::try_from(event.created_at.as_secs()).unwrap_or(i64::MAX),
        },
        now,
    )
}

#[cfg(test)]
mod tests {
    use nostr_sdk::prelude::Keys;
    use serde_json::json;

    use super::*;
    use crate::config::Permission;
    use crate::nostr::dm::solver_dm;
    use crate::store::sessions::testing::store_with_session;

    #[test]
    fn feedback_names_a_turn_question_and_maybe_a_dispute() {
        assert_eq!(
            parse("wrong seller_receipt"),
            Some(Feedback {
                question: "seller_receipt".into(),
                dispute_id: None
            })
        );
        assert_eq!(
            parse("  WRONG  Buyer_Payment  d1 "),
            Some(Feedback {
                question: "buyer_payment".into(),
                dispute_id: Some("d1".into())
            })
        );
        assert_eq!(
            parse("wrong buyer_wants_human").map(|f| f.question),
            Some("buyer_wants_human".into())
        );
    }

    #[test]
    fn anything_else_is_not_feedback() {
        for text in [
            "wrong",
            "wrong seller_recipt",
            "right seller_receipt",
            "wrong a b c",
            "thanks",
            "",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    /// The session `s1` of dispute `d1` with two turn evaluations and a
    /// brief evaluation, and a brief sent to `solver` for `d1`.
    fn judged_store(solver: &str) -> (Mutex<Store>, i64, i64) {
        let store = store_with_session();
        let insert = |answers: serde_json::Value, at: i64| {
            let action = json!("wait");
            evaluations::insert(
                store.conn(),
                &evaluations::NewEvaluation {
                    session_id: "s1",
                    question_set_version: "qs-1-x",
                    judge_id: "test",
                    last_message_id: 0,
                    answers: &answers,
                    action: &action,
                    input_tokens: None,
                    latency_ms: None,
                    now: at,
                },
            )
            .unwrap()
        };
        let first = insert(json!({ "seller_receipt": {}, "buyer_payment": {} }), 10);
        let second = insert(json!({ "buyer_payment": {} }), 20);
        insert(json!({ "quote_buyer_payment": {} }), 30);
        events::append(
            store.conn(),
            &events::NewEvent {
                dispute_id: "d1",
                session_id: None,
                kind: "notification_sent",
                payload: json!({ "notification": "brief", "solver": solver }),
                now: 40,
            },
        )
        .unwrap();
        (Mutex::new(store), first, second)
    }

    #[test]
    fn feedback_is_linked_to_the_newest_evaluation_that_asked_the_question() {
        let (store, first, second) = judged_store("aa");
        let receipt = Feedback {
            question: "seller_receipt".into(),
            dispute_id: None,
        };
        let payment = Feedback {
            question: "buyer_payment".into(),
            dispute_id: None,
        };

        let on_receipt = record(&store, "aa", &receipt, dm("e1"), 50)
            .unwrap()
            .unwrap();
        let on_payment = record(&store, "aa", &payment, dm("e2"), 51)
            .unwrap()
            .unwrap();

        assert_eq!(
            on_receipt,
            Recorded {
                dispute_id: "d1".into(),
                evaluation_id: first
            }
        );
        assert_eq!(on_payment.evaluation_id, second);
        let store = store.lock().unwrap();
        let recorded: Vec<_> = events::list_for_dispute(store.conn(), "d1")
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "solver_feedback")
            .collect();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].session_id.as_deref(), Some("s1"));
        assert_eq!(recorded[0].payload["question"], "seller_receipt");
    }

    #[test]
    fn feedback_is_about_what_the_brief_showed_and_is_recorded_once() {
        let (store, first, _) = judged_store("aa");
        // A later guiding turn the solver never saw asks the same question.
        {
            let store = store.lock().unwrap();
            let (answers, action) = (json!({ "seller_receipt": {} }), json!("wait"));
            evaluations::insert(
                store.conn(),
                &evaluations::NewEvaluation {
                    session_id: "s1",
                    question_set_version: "qs-1-x",
                    judge_id: "test",
                    last_message_id: 0,
                    answers: &answers,
                    action: &action,
                    input_tokens: None,
                    latency_ms: None,
                    now: 45,
                },
            )
            .unwrap();
        }
        let receipt = Feedback {
            question: "seller_receipt".into(),
            dispute_id: None,
        };

        let recorded = record(&store, "aa", &receipt, dm("e1"), 50).unwrap();
        let replayed = record(&store, "aa", &receipt, dm("e1"), 55).unwrap();

        assert_eq!(recorded.map(|r| r.evaluation_id), Some(first));
        assert_eq!(replayed, None);
        let feedback = events::list_for_dispute(store.lock().unwrap().conn(), "d1")
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "solver_feedback")
            .count();
        assert_eq!(feedback, 1);
    }

    /// A solver DM written at 45, after the brief at 40.
    fn dm(id: &str) -> Source<'_> {
        Source { id, written_at: 45 }
    }

    #[test]
    fn a_late_delivered_reply_answers_the_brief_before_it() {
        let (store, first, _) = judged_store("aa");
        events::append(
            store.lock().unwrap().conn(),
            &events::NewEvent {
                dispute_id: "d2",
                session_id: None,
                kind: "notification_sent",
                payload: json!({ "notification": "brief", "solver": "aa" }),
                now: 900,
            },
        )
        .unwrap();
        let receipt = Feedback {
            question: "seller_receipt".into(),
            dispute_id: None,
        };

        let recorded = record(&store, "aa", &receipt, dm("e1"), 1_000).unwrap();

        assert_eq!(
            recorded,
            Some(Recorded {
                dispute_id: "d1".into(),
                evaluation_id: first
            }),
            "written at 45, before the brief for d2"
        );
    }

    #[test]
    fn a_named_dispute_the_solver_was_not_briefed_on_is_not_recorded() {
        let (store, _, _) = judged_store("aa");
        let named = Feedback {
            question: "seller_receipt".into(),
            dispute_id: Some("d1".into()),
        };

        assert_eq!(record(&store, "zz", &named, dm("e1"), 50).unwrap(), None);
        assert!(
            record(&store, "aa", &named, dm("e2"), 50)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn feedback_with_nothing_to_link_is_not_recorded() {
        let (store, _, _) = judged_store("aa");
        let receipt = Feedback {
            question: "seller_receipt".into(),
            dispute_id: None,
        };

        assert_eq!(
            record(&store, "zz", &receipt, dm("e3"), 50).unwrap(),
            None,
            "no brief went to this solver"
        );
        let other = Feedback {
            dispute_id: Some("d9".into()),
            ..receipt
        };
        assert_eq!(
            record(&store, "aa", &other, dm("e4"), 50).unwrap(),
            None,
            "no session for the dispute"
        );
    }

    #[test]
    fn only_a_configured_solvers_dm_is_read() {
        let serbero = Keys::generate();
        let solver = Keys::generate();
        let stranger = Keys::generate();
        let (store, first, _) = judged_store(&solver.public_key().to_hex());
        let solvers = [Solver {
            pubkey: solver.public_key(),
            permission: Permission::Write,
        }];

        let from_solver =
            solver_dm(&solver, serbero.public_key(), None, "wrong seller_receipt").unwrap();
        let from_stranger = solver_dm(
            &stranger,
            serbero.public_key(),
            None,
            "wrong seller_receipt",
        )
        .unwrap();
        let chatter = solver_dm(&solver, serbero.public_key(), None, "gracias").unwrap();

        assert_eq!(
            handle(&from_solver, &serbero, &solvers, &store, 60)
                .unwrap()
                .map(|r| r.evaluation_id),
            Some(first)
        );
        assert_eq!(
            handle(&from_stranger, &serbero, &solvers, &store, 61).unwrap(),
            None
        );
        assert_eq!(
            handle(&chatter, &serbero, &solvers, &store, 62).unwrap(),
            None
        );
    }
}
