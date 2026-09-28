//! Golden cases (`docs/evaluation.md` §1): one conversation's turn state and
//! the answers a person can give with confidence.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::judge::questions::TurnQuestions;
use crate::judge::{Question, QuestionSet};
use crate::store::sessions::Party;

/// An expected answer: a choice option or a noul's yes/no.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Label {
    Option(String),
    Yes(bool),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub lang: String,
    pub source: String,
    pub state: Value,
    /// The turn was judged while guiding, so `<party>_rejects_path` is
    /// asked too.
    #[serde(default)]
    pub guiding: bool,
    /// Unlabelled questions are not scored.
    pub expect: BTreeMap<String, Label>,
}

impl Case {
    /// The questions a turn over this state asks: per-party questions only
    /// for a party with messages in `latest`, and the guiding question only
    /// for a guiding turn.
    pub fn questions(&self, turn: &TurnQuestions) -> QuestionSet {
        let wrote: Vec<Party> = [Party::Buyer, Party::Seller]
            .into_iter()
            .filter(|party| {
                self.state["latest"][party.to_string()]
                    .as_array()
                    .is_some_and(|ids| !ids.is_empty())
            })
            .collect();
        turn.for_turn(&wrote, self.guiding)
    }

    /// Every label must name an asked question and one of its options (or
    /// a yes/no for a noul).
    pub fn validate(&self, turn: &TurnQuestions) -> Result<()> {
        let questions = self.questions(turn);
        let invalid = |detail: String| Err(Error::Config(format!("case {}: {detail}", self.id)));
        if !self.state["transcript"].is_array() {
            return invalid("state has no transcript".into());
        }
        for (id, label) in &self.expect {
            let Some(question) = questions.questions.get(id) else {
                return invalid(format!("{id} is labelled but not asked"));
            };
            match (question, label) {
                (Question::Noul { .. }, Label::Yes(_)) => {}
                (Question::Choice { options, .. }, Label::Option(option))
                    if options.iter().any(|(o, _)| o == option) => {}
                _ => return invalid(format!("{id}: {label:?} is not a valid answer")),
            }
        }
        Ok(())
    }
}

/// Every `*.json` case under `dir` (recursively) with this language, sorted
/// by id. Each case is validated against `turn`.
pub fn load_dir(dir: &Path, lang: &str, turn: &TurnQuestions) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    collect(dir, &mut cases)?;
    cases.retain(|case| case.lang == lang);
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(pair) = cases.windows(2).find(|pair| pair[0].id == pair[1].id) {
        return Err(Error::Config(format!("case id {} is repeated", pair[0].id)));
    }
    for case in &cases {
        case.validate(turn)?;
    }
    Ok(cases)
}

fn collect(dir: &Path, cases: &mut Vec<Case>) -> Result<()> {
    let read_error =
        |e: std::io::Error| Error::Config(format!("cannot read {}: {e}", dir.display()));
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(read_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(read_error)?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, cases)?;
        } else if path.extension().is_some_and(|ext| ext == "json") {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
            let case = serde_json::from_str(&text)
                .map_err(|e| Error::Config(format!("{} is not a case: {e}", path.display())))?;
            cases.push(case);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // test helpers

    use serde_json::json;

    use super::*;
    use crate::judge::questions::Language;

    fn turn() -> TurnQuestions {
        TurnQuestions::new(&[
            Language {
                code: "en",
                name: "English",
            },
            Language {
                code: "es",
                name: "Spanish",
            },
        ])
    }

    fn case(latest_buyer: &[&str], expect: Value) -> Case {
        serde_json::from_value(json!({
            "id": "c1",
            "lang": "es",
            "source": "synthetic",
            "state": {
                "transcript": [{ "id": "m1", "from": "buyer", "text": "ya pagué" }],
                "latest": { "buyer": latest_buyer, "seller": [] }
            },
            "expect": expect
        }))
        .unwrap()
    }

    #[test]
    fn per_party_questions_follow_latest() {
        let buyer_wrote = case(&["m1"], json!({}));
        let nobody = case(&[], json!({}));

        let asked = buyer_wrote.questions(&turn());
        let facts_only = nobody.questions(&turn());

        assert!(asked.questions.contains_key("buyer_language"));
        assert!(!asked.questions.contains_key("seller_language"));
        assert!(!facts_only.questions.contains_key("buyer_language"));
        assert!(!asked.questions.contains_key("buyer_rejects_path"));
    }

    #[test]
    fn a_guiding_case_asks_the_rejects_path_question() {
        let mut guiding = case(&["m1"], json!({ "buyer_rejects_path": true }));
        guiding.guiding = true;

        guiding.validate(&turn()).unwrap();
        assert!(
            case(&["m1"], json!({ "buyer_rejects_path": true }))
                .validate(&turn())
                .is_err()
        );
    }

    #[test]
    fn valid_labels_pass() {
        let good = case(
            &["m1"],
            json!({ "buyer_payment": "says_sent", "buyer_details": false, "buyer_language": "es" }),
        );

        good.validate(&turn()).unwrap();
    }

    #[test]
    fn invalid_labels_are_rejected() {
        for (expect, why) in [
            (json!({ "buyer_payment": "paid" }), "unknown option"),
            (json!({ "buyer_payment": true }), "yes/no for a choice"),
            (json!({ "buyer_details": "yes" }), "an option for a noul"),
            (json!({ "seller_language": "es" }), "a question not asked"),
            (json!({ "made_up": true }), "an unknown question"),
        ] {
            let bad = case(&["m1"], expect);

            assert!(bad.validate(&turn()).is_err(), "{why}");
        }
    }
}
