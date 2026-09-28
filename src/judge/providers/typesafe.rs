//! The `typesafe` provider: Jev via `POST {api_base}/v1/systemone`.
//!
//! Wire types stay private to this module. Status codes map onto
//! `JudgeError` (401/403 → `Unauthorized`; 408, 429, 5xx including 529, and
//! network failures or timeouts → `Unavailable`; other 4xx such as 422 →
//! `InvalidRequest`). Retryable failures are retried with exponential
//! backoff, honoring `retry-after` / `retry-after-ms` when the response
//! carries one, as TypeSafe's own SDKs do. Error bodies are dropped, and
//! answers from a model other than the pinned one are `Malformed`.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::future::BoxFuture;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::JudgeConfig;
use crate::judge::{
    Answer, Answers, Capabilities, Judge, JudgeError, Question, QuestionKind, QuestionSet,
    check_answers,
};

/// Jev's documented limits (`https://docs.typesafe.ai/models`).
const MAX_CHOICE_OPTIONS: usize = 255;
const MAX_SCORE_LEVELS: usize = 10;
const MAX_CONTEXT_TOKENS: u32 = 64_000;

/// Longest wait a `retry-after` header can impose.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Retries of retryable failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl RetryPolicy {
    /// TypeSafe's SDK defaults: 0.5 s doubling up to 5 s.
    pub fn new(max_retries: u32) -> Self {
        Self {
            max_retries,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(5),
        }
    }

    fn backoff(&self, retry: u32) -> Duration {
        self.initial_backoff
            .saturating_mul(2u32.saturating_pow(retry))
            .min(self.max_backoff)
    }
}

pub struct TypeSafeJudge {
    id: String,
    model: String,
    api_base: String,
    api_key: String,
    http: reqwest::Client,
    retry: RetryPolicy,
}

impl TypeSafeJudge {
    pub fn new(config: &JudgeConfig, api_key: &str) -> Result<Self, JudgeError> {
        // Nostr already links rustls with `ring`; use it rather than a
        // second crypto provider. Installing twice is harmless.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| JudgeError::InvalidRequest(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            id: config.judge_key(),
            model: config.model.clone(),
            api_base: config.api_base.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            http,
            retry: RetryPolicy::new(config.max_retries),
        })
    }

    pub fn with_retry(self, retry: RetryPolicy) -> Self {
        Self { retry, ..self }
    }

    async fn evaluate_once(
        &self,
        body: &WireRequest<'_>,
        questions: &QuestionSet,
    ) -> Result<Answers, Failure> {
        let response = self
            .http
            .post(format!("{}/v1/systemone", self.api_base))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(Failure::network)?;
        let text = success_body(response).await?;
        let wire: WireResponse = serde_json::from_str(&text)
            .map_err(|e| Failure::from(JudgeError::Malformed(format!("response body: {e}"))))?;
        // Thresholds are calibrated per model: answers from any other model
        // (a fallback, or an alias that moved) must not be used under this
        // judge's id.
        if wire.model != self.model {
            return Err(JudgeError::Malformed(format!(
                "answered by model {}, configured {}",
                wire.model, self.model
            ))
            .into());
        }
        let answers = wire
            .answers
            .into_iter()
            .map(|(id, answer)| Ok((id, answer.into_answer()?)))
            .collect::<Result<Answers, JudgeError>>()?;
        check_answers(questions, &answers)?;
        Ok(answers)
    }

    async fn health_once(&self) -> Result<(), Failure> {
        let response = self
            .http
            .get(format!("{}/v1/models", self.api_base))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(Failure::network)?;
        success_body(response).await.map(drop)
    }

    /// Runs `attempt` until it succeeds, fails for good, or retries run out.
    async fn with_retries<T, F, Fut>(&self, mut attempt: F) -> Result<T, JudgeError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, Failure>>,
    {
        let mut retry = 0;
        loop {
            match attempt().await {
                Ok(value) => return Ok(value),
                Err(failure) if failure.error.is_retryable() && retry < self.retry.max_retries => {
                    let wait = failure
                        .retry_after
                        .unwrap_or_else(|| self.retry.backoff(retry));
                    tracing::warn!(error = %failure.error, retry = retry + 1, ?wait, "judge call failed; retrying");
                    tokio::time::sleep(wait).await;
                    retry += 1;
                }
                Err(failure) => return Err(failure.error),
            }
        }
    }
}

impl Judge for TypeSafeJudge {
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
            max_choice_options: MAX_CHOICE_OPTIONS,
            max_score_levels: MAX_SCORE_LEVELS,
            max_context_tokens: Some(MAX_CONTEXT_TOKENS),
        }
    }

    fn evaluate<'a>(
        &'a self,
        state: &'a Value,
        questions: &'a QuestionSet,
    ) -> BoxFuture<'a, Result<Answers, JudgeError>> {
        Box::pin(async move {
            let body = WireRequest {
                state,
                model: &self.model,
                questions: questions
                    .questions
                    .iter()
                    .map(|(id, q)| (id.as_str(), WireQuestion::from(q)))
                    .collect(),
            };
            self.with_retries(|| self.evaluate_once(&body, questions))
                .await
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), JudgeError>> {
        Box::pin(self.with_retries(|| self.health_once()))
    }
}

/// A failed attempt, with the server's requested wait if it gave one.
struct Failure {
    error: JudgeError,
    retry_after: Option<Duration>,
}

impl From<JudgeError> for Failure {
    fn from(error: JudgeError) -> Self {
        Self {
            error,
            retry_after: None,
        }
    }
}

impl Failure {
    fn network(e: reqwest::Error) -> Self {
        let kind = if e.is_timeout() { "timeout" } else { "network" };
        JudgeError::Unavailable(format!("{kind}: {e}")).into()
    }
}

/// The body of a 2xx response, or the mapped error.
async fn success_body(response: reqwest::Response) -> Result<String, Failure> {
    let status = response.status();
    let retry_after = retry_after(response.headers());
    if status.is_success() {
        return response.text().await.map_err(Failure::network);
    }
    // The error body is never kept: it may echo the state, which holds party
    // text, and errors are logged (AGENTS.md, privacy).
    let detail = format!("HTTP {status}");
    let error = match status.as_u16() {
        401 | 403 => JudgeError::Unauthorized(detail),
        408 | 429 | 500..=599 => JudgeError::Unavailable(detail),
        _ => JudgeError::InvalidRequest(detail),
    };
    Err(Failure { error, retry_after })
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    // Capped before conversion: a huge value (remote input) would make
    // `Duration::from_secs_f64` panic.
    let seconds = header("retry-after-ms")
        .map(|ms| ms / 1_000.0)
        .or_else(|| header("retry-after"))?;
    Some(Duration::from_secs_f64(
        seconds.min(MAX_RETRY_AFTER.as_secs_f64()),
    ))
}

#[derive(Serialize)]
struct WireRequest<'a> {
    state: &'a Value,
    model: &'a str,
    questions: BTreeMap<&'a str, WireQuestion<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireQuestion<'a> {
    Noul {
        instructions: &'a Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulWireCriteria<'a>>,
    },
    Choice {
        instructions: &'a Value,
        criteria: Map<String, Value>,
    },
    Score {
        instructions: &'a Value,
        criteria: &'a [Value],
    },
}

#[derive(Serialize)]
struct NoulWireCriteria<'a> {
    #[serde(rename = "true")]
    yes: &'a Value,
    #[serde(rename = "false")]
    no: &'a Value,
}

impl<'a> From<&'a Question> for WireQuestion<'a> {
    fn from(question: &'a Question) -> Self {
        match question {
            Question::Noul {
                instructions,
                criteria,
            } => Self::Noul {
                instructions,
                criteria: criteria.as_ref().map(|c| NoulWireCriteria {
                    yes: &c.yes,
                    no: &c.no,
                }),
            },
            Question::Choice {
                instructions,
                options,
            } => Self::Choice {
                instructions,
                criteria: options
                    .iter()
                    .map(|(name, description)| {
                        (name.clone(), description.clone().unwrap_or(Value::Null))
                    })
                    .collect(),
            },
            Question::Score {
                instructions,
                levels,
            } => Self::Score {
                instructions,
                criteria: levels,
            },
        }
    }
}

#[derive(Deserialize)]
struct WireResponse {
    model: String,
    answers: BTreeMap<String, WireAnswer>,
}

/// Only the probabilities are read; TypeSafe's own `choice`, `score`,
/// `legend` and `confidence` are ignored (Serbero computes its own).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        probabilities: BTreeMap<String, f64>,
    },
}

impl WireAnswer {
    fn into_answer(self) -> Result<Answer, JudgeError> {
        Ok(match self {
            Self::Noul { noul } => Answer::Noul { p_yes: noul },
            Self::Choice { probabilities } => Answer::Choice { probabilities },
            Self::Score { probabilities } => Answer::Score {
                probabilities: (0..probabilities.len())
                    .map(|level| {
                        probabilities
                            .get(&level.to_string())
                            .copied()
                            .ok_or_else(|| {
                                JudgeError::Malformed(format!("score has no level {level}"))
                            })
                    })
                    .collect::<Result<_, _>>()?,
            },
        })
    }
}

#[cfg(test)]
mod tests;
