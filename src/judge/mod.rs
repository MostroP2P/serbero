//! The provider-neutral judge contract (`docs/spec.md` §5.2).
//!
//! Everything outside `judge::providers` uses these types. Each provider
//! adapter translates a `QuestionSet` to its wire format and its answers back;
//! no provider-specific type crosses that boundary. Serbero derives what it
//! needs (winner, score value, confidence) from the probabilities with one
//! formula for every provider.

use std::collections::BTreeMap;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod providers;
pub mod questions;
pub mod state;

/// What a `yes` and a `no` mean for a noul question.
#[derive(Debug, Clone, PartialEq)]
pub struct NoulCriteria {
    pub yes: Value,
    pub no: Value,
}

/// One typed question. `instructions` may be a string or structured JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    /// Yes or no; answered with the probability of yes.
    Noul {
        instructions: Value,
        criteria: Option<NoulCriteria>,
    },
    /// One of a set of named options, each with an optional description.
    Choice {
        instructions: Value,
        options: Vec<(String, Option<Value>)>,
    },
    /// A position on ordered levels, each described.
    Score {
        instructions: Value,
        levels: Vec<Value>,
    },
}

/// The kind of a question or answer, for capability and shape checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuestionKind {
    Noul,
    Choice,
    Score,
}

impl Question {
    pub fn kind(&self) -> QuestionKind {
        match self {
            Self::Noul { .. } => QuestionKind::Noul,
            Self::Choice { .. } => QuestionKind::Choice,
            Self::Score { .. } => QuestionKind::Score,
        }
    }
}

/// Questions asked together over one state, keyed by id. The version names
/// the question set (`docs/judgments.md`) and is stored with every answer.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionSet {
    pub version: String,
    pub questions: BTreeMap<String, Question>,
}

/// A raw answer: probabilities only. Serialized as
/// `{"type": "noul", "p_yes": 0.3}` and so on, for recordings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Answer {
    Noul {
        p_yes: f64,
    },
    Choice {
        probabilities: BTreeMap<String, f64>,
    },
    /// One probability per level, in level order.
    Score {
        probabilities: Vec<f64>,
    },
}

/// Answers keyed by question id.
pub type Answers = BTreeMap<String, Answer>;

/// How far a probability distribution may be from summing to 1.
const SUM_TOLERANCE: f64 = 0.01;

impl Answer {
    pub fn kind(&self) -> QuestionKind {
        match self {
            Self::Noul { .. } => QuestionKind::Noul,
            Self::Choice { .. } => QuestionKind::Choice,
            Self::Score { .. } => QuestionKind::Score,
        }
    }

    /// The option with the highest probability; a tie goes to the first
    /// option in name order, so the result never depends on the provider.
    pub fn winner(&self) -> Option<&str> {
        let Self::Choice { probabilities } = self else {
            return None;
        };
        let mut best: Option<(&str, f64)> = None;
        for (option, &p) in probabilities {
            if best.is_none_or(|(_, bp)| p > bp) {
                best = Some((option, p));
            }
        }
        best.map(|(option, _)| option)
    }

    /// The probability of one option of a choice (0 when absent).
    pub fn probability(&self, option: &str) -> f64 {
        match self {
            Self::Choice { probabilities } => probabilities.get(option).copied().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// The probability-weighted level of a score, from 0 to 1 (level `i` of
    /// `n` counts as `i / (n - 1)`). The weights are normalized by their sum,
    /// so a distribution within the accepted tolerance of 1 stays in range.
    pub fn score_value(&self) -> Option<f64> {
        let Self::Score { probabilities } = self else {
            return None;
        };
        let top = probabilities.len().checked_sub(1).filter(|&t| t > 0)? as f64;
        let total: f64 = probabilities.iter().sum();
        if total <= 0.0 {
            return None;
        }
        let weighted: f64 = probabilities
            .iter()
            .enumerate()
            .map(|(i, p)| p * i as f64 / top)
            .sum();
        Some((weighted / total).clamp(0.0, 1.0))
    }

    /// How concentrated a choice or score distribution is, from 0 (flat) to
    /// 1 (one certain answer): `(n · peak − 1) / (n − 1)`, clamped. A noul
    /// has no confidence; its probability is the answer.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Noul { .. } => None,
            Self::Choice { probabilities } => Some(concentration(probabilities.values().copied())),
            Self::Score { probabilities } => Some(concentration(probabilities.iter().copied())),
        }
    }

    /// Checks that this answer fits `question`: same kind, exactly the
    /// question's options or levels, probabilities in [0, 1] summing to 1.
    pub fn check(&self, question: &Question) -> Result<(), String> {
        match (self, question) {
            (Self::Noul { p_yes }, Question::Noul { .. }) => check_unit(*p_yes),
            (Self::Choice { probabilities }, Question::Choice { options, .. }) => {
                let mut asked: Vec<&str> = options.iter().map(|(o, _)| o.as_str()).collect();
                asked.sort_unstable();
                let answered: Vec<&str> = probabilities.keys().map(String::as_str).collect();
                if answered != asked {
                    return Err(format!("options {answered:?} do not match {asked:?}"));
                }
                check_distribution(probabilities.values().copied())
            }
            (Self::Score { probabilities }, Question::Score { levels, .. }) => {
                if probabilities.len() != levels.len() {
                    return Err(format!(
                        "{} probabilities for {} levels",
                        probabilities.len(),
                        levels.len()
                    ));
                }
                check_distribution(probabilities.iter().copied())
            }
            _ => Err(format!(
                "a {:?} answer to a {:?} question",
                self.kind(),
                question.kind()
            )),
        }
    }
}

fn check_unit(p: f64) -> Result<(), String> {
    if (0.0..=1.0).contains(&p) {
        Ok(())
    } else {
        Err(format!("probability {p} is not in [0, 1]"))
    }
}

fn check_distribution(probabilities: impl Iterator<Item = f64>) -> Result<(), String> {
    let mut sum = 0.0;
    for p in probabilities {
        check_unit(p)?;
        sum += p;
    }
    if (sum - 1.0).abs() > SUM_TOLERANCE {
        return Err(format!("probabilities sum to {sum}"));
    }
    Ok(())
}

/// The peak is taken as a share of the total, so a distribution accepted
/// within the sum tolerance gives the same confidence as its normalized form.
fn concentration(probabilities: impl Iterator<Item = f64>) -> f64 {
    let (n, peak, total) = probabilities.fold((0usize, 0.0f64, 0.0f64), |(n, peak, total), p| {
        (n + 1, peak.max(p), total + p)
    });
    if n < 2 {
        return 1.0;
    }
    if total <= 0.0 {
        return 0.0;
    }
    let n = n as f64;
    ((n * peak / total - 1.0) / (n - 1.0)).clamp(0.0, 1.0)
}

/// What a provider supports. At startup Serbero checks the question set
/// against it and keeps mediation off if a question cannot be expressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub question_kinds: Vec<QuestionKind>,
    pub max_choice_options: usize,
    pub max_score_levels: usize,
    /// Largest request the provider accepts, in tokens, if it states one.
    pub max_context_tokens: Option<u32>,
}

impl Capabilities {
    /// Every reason `set` cannot be sent to this provider; empty when it can.
    pub fn check(&self, set: &QuestionSet) -> Vec<String> {
        let mut problems = Vec::new();
        for (id, question) in &set.questions {
            if !self.question_kinds.contains(&question.kind()) {
                problems.push(format!(
                    "{id}: {:?} questions are not supported",
                    question.kind()
                ));
                continue;
            }
            match question {
                Question::Noul { .. } => {}
                Question::Choice { options, .. } => {
                    let count = options.len();
                    if !(2..=self.max_choice_options).contains(&count) {
                        problems.push(format!(
                            "{id}: {count} options; 2 to {} are supported",
                            self.max_choice_options
                        ));
                    }
                    let mut names: Vec<&str> = options.iter().map(|(o, _)| o.as_str()).collect();
                    names.sort_unstable();
                    if names.windows(2).any(|w| w[0] == w[1]) {
                        problems.push(format!("{id}: duplicate option names"));
                    }
                }
                Question::Score { levels, .. } => {
                    let count = levels.len();
                    if !(2..=self.max_score_levels).contains(&count) {
                        problems.push(format!(
                            "{id}: {count} levels; 2 to {} are supported",
                            self.max_score_levels
                        ));
                    }
                }
            }
        }
        problems
    }
}

/// Provider-neutral failures. Each adapter maps its status codes onto these.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JudgeError {
    /// Retryable: overload, rate limit, network failure, timeout.
    #[error("judge unavailable: {0}")]
    Unavailable(String),
    /// The API key is missing, wrong, or revoked.
    #[error("judge rejected the credentials: {0}")]
    Unauthorized(String),
    /// The provider refused the request as sent.
    #[error("judge rejected the request: {0}")]
    InvalidRequest(String),
    /// The response does not match the question set.
    #[error("malformed judge response: {0}")]
    Malformed(String),
}

impl JudgeError {
    /// Whether retrying the same request may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// A model that answers typed questions with probabilities.
pub trait Judge: Send + Sync {
    /// Stable identifier, e.g. `"typesafe/jev-1.13.0"`, stored with every
    /// evaluation so results from different judges are never mixed.
    fn id(&self) -> &str;

    fn capabilities(&self) -> Capabilities;

    /// Answers every question of `questions` over `state`. Implementations
    /// return answers that pass `check_answers`.
    fn evaluate<'a>(
        &'a self,
        state: &'a Value,
        questions: &'a QuestionSet,
    ) -> BoxFuture<'a, Result<Answers, JudgeError>>;

    /// A cheap call proving the provider is reachable and the key works.
    fn health_check(&self) -> BoxFuture<'_, Result<(), JudgeError>>;
}

/// Checks a full answer map against its question set: one valid answer per
/// question and nothing else.
pub fn check_answers(questions: &QuestionSet, answers: &Answers) -> Result<(), JudgeError> {
    if let Some(extra) = answers
        .keys()
        .find(|id| !questions.questions.contains_key(*id))
    {
        return Err(JudgeError::Malformed(format!(
            "answer to unknown question {extra}"
        )));
    }
    for (id, question) in &questions.questions {
        let answer = answers
            .get(id)
            .ok_or_else(|| JudgeError::Malformed(format!("no answer to {id}")))?;
        answer
            .check(question)
            .map_err(|e| JudgeError::Malformed(format!("{id}: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
