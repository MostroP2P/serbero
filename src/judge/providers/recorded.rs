//! The `recorded` provider: replays answers another judge gave before, for
//! tests and dry runs. It never calls a model.
//!
//! A recording holds the source judge's id, the question set version, and
//! one entry per case: its id, the exact state sent, and the answers. A
//! request is answered by looking up its state together with the ids of the
//! questions asked, so the turn and brief questions over one state are
//! recorded separately. A request that was not recorded is an
//! `InvalidRequest`, never a guess.

use std::collections::HashMap;
use std::path::Path;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::judge::{
    Answers, Capabilities, Judge, JudgeError, QuestionKind, QuestionSet, check_answers,
};

/// Answers recorded from one judge on one question set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recording {
    /// The judge that gave the answers, e.g. `"typesafe/jev-1.13.0"`.
    pub judge: String,
    pub question_set: String,
    pub cases: Vec<RecordedCase>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedCase {
    pub case_id: String,
    pub state: Value,
    pub answers: Answers,
}

pub struct RecordedJudge {
    id: String,
    question_set: String,
    /// Keyed by `request_key`.
    by_request: HashMap<String, Answers>,
}

impl RecordedJudge {
    /// Its id is `recorded/<source judge id>`, so replayed evaluations are
    /// never mistaken for live ones.
    pub fn new(recording: Recording) -> Result<Self, JudgeError> {
        let mut by_request = HashMap::with_capacity(recording.cases.len());
        for case in recording.cases {
            let key = request_key(&case.state, case.answers.keys());
            if by_request.insert(key, case.answers).is_some() {
                return Err(JudgeError::InvalidRequest(format!(
                    "case {} repeats the request of an earlier case",
                    case.case_id
                )));
            }
        }
        Ok(Self {
            id: format!("recorded/{}", recording.judge),
            question_set: recording.question_set,
            by_request,
        })
    }

    /// Reads a recording written as JSON.
    pub fn load(path: &Path) -> Result<Self, JudgeError> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            JudgeError::InvalidRequest(format!("cannot read {}: {e}", path.display()))
        })?;
        let recording = serde_json::from_str(&text).map_err(|e| {
            JudgeError::InvalidRequest(format!("{} is not a recording: {e}", path.display()))
        })?;
        Self::new(recording)
    }

    fn answer(&self, state: &Value, questions: &QuestionSet) -> Result<Answers, JudgeError> {
        if questions.version != self.question_set {
            return Err(JudgeError::InvalidRequest(format!(
                "recorded on question set {}, asked {}",
                self.question_set, questions.version
            )));
        }
        let answers = self
            .by_request
            .get(&request_key(state, questions.questions.keys()))
            .cloned()
            .ok_or_else(|| {
                JudgeError::InvalidRequest("no recorded answers for this request".into())
            })?;
        check_answers(questions, &answers)?;
        Ok(answers)
    }
}

/// The sorted question ids and the state's JSON text (object keys are
/// sorted, so equal states give equal text).
fn request_key<'a>(state: &Value, question_ids: impl Iterator<Item = &'a String>) -> String {
    let ids: Vec<&str> = question_ids.map(String::as_str).collect();
    format!("{}\n{}", ids.join(","), state)
}

impl Judge for RecordedJudge {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            question_kinds: vec![
                QuestionKind::Noul,
                QuestionKind::Choice,
                QuestionKind::Score,
            ],
            max_choice_options: usize::MAX,
            max_score_levels: usize::MAX,
            max_context_tokens: None,
        }
    }

    fn evaluate<'a>(
        &'a self,
        state: &'a Value,
        questions: &'a QuestionSet,
    ) -> BoxFuture<'a, Result<Answers, JudgeError>> {
        Box::pin(async move { self.answer(state, questions) })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), JudgeError>> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // test helpers

    use serde_json::json;

    use super::*;
    use crate::judge::{Answer, Question};

    fn questions(version: &str) -> QuestionSet {
        QuestionSet {
            version: version.into(),
            questions: [(
                "buyer_payment".to_owned(),
                Question::Choice {
                    instructions: json!("What does the buyer say about the payment?"),
                    options: vec![("says_sent".into(), None), ("says_not_sent".into(), None)],
                },
            )]
            .into(),
        }
    }

    fn recording() -> Recording {
        serde_json::from_value(json!({
            "judge": "typesafe/jev-1.13.0",
            "question_set": "qs-1",
            "cases": [{
                "case_id": "en-001",
                "state": { "transcript": [{ "id": "m1", "text": "I paid" }] },
                "answers": {
                    "buyer_payment": {
                        "type": "choice",
                        "probabilities": { "says_sent": 0.9, "says_not_sent": 0.1 }
                    }
                }
            }]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn recorded_answers_round_trip() {
        let judge = RecordedJudge::new(recording()).unwrap();
        let state = json!({ "transcript": [{ "text": "I paid", "id": "m1" }] });

        let answers = judge.evaluate(&state, &questions("qs-1")).await.unwrap();

        assert_eq!(judge.id(), "recorded/typesafe/jev-1.13.0");
        assert_eq!(answers["buyer_payment"].winner(), Some("says_sent"));
        let written = serde_json::to_value(recording()).unwrap();
        let reread: Recording = serde_json::from_value(written).unwrap();
        assert_eq!(reread, recording());
    }

    #[tokio::test]
    async fn an_unrecorded_state_is_refused() {
        let judge = RecordedJudge::new(recording()).unwrap();

        let err = judge
            .evaluate(&json!({ "transcript": [] }), &questions("qs-1"))
            .await
            .unwrap_err();

        assert_eq!(
            err,
            JudgeError::InvalidRequest("no recorded answers for this request".into())
        );
    }

    #[tokio::test]
    async fn another_question_set_is_refused() {
        let judge = RecordedJudge::new(recording()).unwrap();
        let state = recording().cases[0].state.clone();

        let err = judge
            .evaluate(&state, &questions("qs-2"))
            .await
            .unwrap_err();

        assert!(matches!(err, JudgeError::InvalidRequest(e) if e.contains("qs-2")));
    }

    #[tokio::test]
    async fn answers_that_no_longer_fit_the_questions_are_malformed() {
        let mut stale = recording();
        stale.cases[0]
            .answers
            .insert("buyer_payment".into(), Answer::Noul { p_yes: 0.9 });
        let judge = RecordedJudge::new(stale).unwrap();
        let state = recording().cases[0].state.clone();

        let err = judge
            .evaluate(&state, &questions("qs-1"))
            .await
            .unwrap_err();

        assert!(matches!(err, JudgeError::Malformed(_)));
    }

    #[tokio::test]
    async fn one_state_can_be_recorded_for_two_question_sets() {
        let mut both = recording();
        let mut brief = both.cases[0].clone();
        brief.case_id = "en-001-brief".into();
        brief.answers = [(
            "evidence".to_owned(),
            Answer::Score {
                probabilities: vec![0.2, 0.8],
            },
        )]
        .into();
        both.cases.push(brief);
        let judge = RecordedJudge::new(both).unwrap();
        let state = recording().cases[0].state.clone();
        let brief_questions = QuestionSet {
            version: "qs-1".into(),
            questions: [(
                "evidence".to_owned(),
                Question::Score {
                    instructions: json!("How strong is the evidence?"),
                    levels: vec![json!("Weak"), json!("Strong")],
                },
            )]
            .into(),
        };

        let turn = judge.evaluate(&state, &questions("qs-1")).await.unwrap();
        let brief = judge.evaluate(&state, &brief_questions).await.unwrap();

        assert_eq!(turn["buyer_payment"].winner(), Some("says_sent"));
        assert!(brief.contains_key("evidence"));
    }

    #[test]
    fn a_repeated_request_is_rejected() {
        let mut twice = recording();
        let mut copy = twice.cases[0].clone();
        copy.case_id = "en-002".into();
        twice.cases.push(copy);

        assert!(RecordedJudge::new(twice).is_err());
    }

    #[test]
    fn a_missing_file_is_reported() {
        let err = RecordedJudge::load(Path::new("/nonexistent/recording.json"))
            .err()
            .unwrap();

        assert!(matches!(err, JudgeError::InvalidRequest(e) if e.contains("cannot read")));
    }
}
