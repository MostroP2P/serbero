//! The `evaluations` table: each judge request with its answers and the
//! action taken (`docs/spec.md` §8). Answers are provider-neutral, so any
//! evaluation can be replayed against a newer question set.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    pub id: i64,
    pub session_id: String,
    pub question_set_version: String,
    pub judge_id: String,
    /// The stored id of the last message in the state that was judged.
    pub last_message_id: i64,
    pub answers: Value,
    pub action: Value,
    pub input_tokens: Option<u32>,
    pub latency_ms: Option<u32>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewEvaluation<'a> {
    pub session_id: &'a str,
    pub question_set_version: &'a str,
    pub judge_id: &'a str,
    pub last_message_id: i64,
    /// Provider-neutral answers. Never message text.
    pub answers: &'a Value,
    pub action: &'a Value,
    pub input_tokens: Option<u32>,
    pub latency_ms: Option<u32>,
    pub now: i64,
}

/// Appends an evaluation and returns its id.
pub fn insert(conn: &Connection, evaluation: &NewEvaluation<'_>) -> Result<i64> {
    conn.execute(
        "INSERT INTO evaluations (session_id, question_set_version, judge_id, last_message_id,
             answers_json, action_json, input_tokens, latency_ms, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            evaluation.session_id,
            evaluation.question_set_version,
            evaluation.judge_id,
            evaluation.last_message_id,
            evaluation.answers.to_string(),
            evaluation.action.to_string(),
            evaluation.input_tokens,
            evaluation.latency_ms,
            evaluation.now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

const COLUMNS: &str = "id, session_id, question_set_version, judge_id, last_message_id,
     answers_json, action_json, input_tokens, latency_ms, created_at";

/// A session's evaluations, oldest first: what a replay walks through.
pub fn list_for_session(conn: &Connection, session_id: &str) -> Result<Vec<Evaluation>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM evaluations WHERE session_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([session_id], row_values)?;
    rows.map(|row| from_values(row?)).collect()
}

/// The session's most recent evaluation, by insertion order.
pub fn latest_for_session(conn: &Connection, session_id: &str) -> Result<Option<Evaluation>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM evaluations WHERE session_id = ?1 ORDER BY id DESC LIMIT 1"
            ),
            [session_id],
            row_values,
        )
        .optional()?;
    row.map(from_values).transpose()
}

type Values = (
    i64,
    String,
    String,
    String,
    i64,
    String,
    String,
    Option<u32>,
    Option<u32>,
    i64,
);

fn row_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<Values> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn from_values(values: Values) -> Result<Evaluation> {
    let (id, session_id, question_set_version, judge_id, last_message_id) =
        (values.0, values.1, values.2, values.3, values.4);
    let json = |text: &str, what: &str| {
        serde_json::from_str(text)
            .map_err(|e| Error::Schema(format!("evaluation {id} has invalid {what}: {e}")))
    };
    Ok(Evaluation {
        id,
        session_id,
        question_set_version,
        judge_id,
        last_message_id,
        answers: json(&values.5, "answers")?,
        action: json(&values.6, "action")?,
        input_tokens: values.7,
        latency_ms: values.8,
        created_at: values.9,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::store::sessions::testing::store_with_session;

    fn evaluation<'a>(answers: &'a Value, action: &'a Value, at: i64) -> NewEvaluation<'a> {
        NewEvaluation {
            session_id: "s1",
            question_set_version: "qs-1-1e7ce156",
            judge_id: "typesafe/jev-1.13.0",
            last_message_id: 7,
            answers,
            action,
            input_tokens: Some(2090),
            latency_ms: Some(334),
            now: at,
        }
    }

    #[test]
    fn an_evaluation_round_trips() {
        let store = store_with_session();
        let answers = json!({ "fraud_signal": { "type": "noul", "p_yes": 0.08 } });
        let action = json!({ "handoff": "conflicting_claims" });

        let id = insert(store.conn(), &evaluation(&answers, &action, 100)).unwrap();
        let stored = list_for_session(store.conn(), "s1").unwrap();

        assert_eq!(
            stored,
            [Evaluation {
                id,
                session_id: "s1".into(),
                question_set_version: "qs-1-1e7ce156".into(),
                judge_id: "typesafe/jev-1.13.0".into(),
                last_message_id: 7,
                answers,
                action,
                input_tokens: Some(2090),
                latency_ms: Some(334),
                created_at: 100,
            }]
        );
    }

    #[test]
    fn replay_lists_a_sessions_evaluations_in_order() {
        let store = store_with_session();
        let (a, wait, ask) = (json!({}), json!("wait"), json!({ "ask": {} }));
        let first = insert(store.conn(), &evaluation(&a, &ask, 100)).unwrap();
        let second = insert(store.conn(), &evaluation(&a, &wait, 100)).unwrap();

        let ids: Vec<i64> = list_for_session(store.conn(), "s1")
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect();
        let latest = latest_for_session(store.conn(), "s1").unwrap().unwrap();

        assert_eq!(ids, [first, second]);
        assert_eq!(
            latest.id, second,
            "the latest is the last inserted, even at the same second"
        );
    }

    #[test]
    fn a_session_without_evaluations_has_none() {
        let store = store_with_session();

        assert!(list_for_session(store.conn(), "s1").unwrap().is_empty());
        assert!(latest_for_session(store.conn(), "s1").unwrap().is_none());
    }

    #[test]
    fn missing_usage_is_stored_as_unknown() {
        let store = store_with_session();
        let (a, wait) = (json!({}), json!("wait"));
        let new = NewEvaluation {
            input_tokens: None,
            latency_ms: None,
            ..evaluation(&a, &wait, 100)
        };

        insert(store.conn(), &new).unwrap();
        let stored = latest_for_session(store.conn(), "s1").unwrap().unwrap();

        assert_eq!((stored.input_tokens, stored.latency_ms), (None, None));
    }

    #[test]
    fn an_evaluation_needs_an_existing_session() {
        let store = store_with_session();
        let (a, wait) = (json!({}), json!("wait"));
        let orphan = NewEvaluation {
            session_id: "missing",
            ..evaluation(&a, &wait, 100)
        };

        assert!(insert(store.conn(), &orphan).is_err());
    }
}
