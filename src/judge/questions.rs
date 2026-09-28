//! The turn question set (`docs/judgments.md` §2), in provider-neutral types.
//!
//! The full set (case facts, the per-party questions for both parties, and
//! the question asked while guiding) is built once from the enabled
//! languages. Each turn sends a subset of it (`TurnQuestions::for_turn`),
//! always under the full set's identifier: `QUESTION_SET_VERSION` plus a
//! short hash of the rendered questions. Enabling a language adds a
//! `<party>_language` option and so changes the identifier, which keeps
//! recordings and reports from being reused across language sets
//! (`docs/judgments.md` §6).

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::{NoulCriteria, Question, QuestionSet};
use crate::store::sessions::Party;

/// Bump on any change to an instruction, option, or criterion, add a line to
/// the snapshot test, and re-run the golden set (`docs/evaluation.md`).
pub const QUESTION_SET_VERSION: &str = "qs-1";

/// Hex characters of the SHA-256 kept in the identifier.
const HASH_CHARS: usize = 8;

/// Asked for each party with new messages (`docs/judgments.md` §2.2).
const PER_PARTY: [&str; 3] = ["message_kind", "language", "wants_human"];

/// Asked for each party with new messages while the session is `guiding`.
const GUIDING: &str = "rejects_path";

/// Options of `<party>_language` added after the enabled languages.
const LANGUAGE_OTHER: &str = "other";
const LANGUAGE_UNKNOWN: &str = "unknown";

/// An enabled language: its code and the English name from its catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language<'a> {
    pub code: &'a str,
    pub name: &'a str,
}

/// The full turn question set for one list of enabled languages.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnQuestions {
    all: QuestionSet,
}

impl TurnQuestions {
    /// Builds the set. The `<party>_language` options are `languages`, then
    /// `other` and `unknown`; they are rendered keyed by code, so only the
    /// set of languages, not their order, changes the identifier.
    pub fn new(languages: &[Language<'_>]) -> Self {
        let mut questions = case_facts();
        for party in [Party::Buyer, Party::Seller] {
            questions.extend(per_party(party, languages));
            questions.insert(key(party, GUIDING), rejects_path(party));
        }
        let hash = short_hash(&questions);
        Self {
            all: QuestionSet {
                version: format!("{QUESTION_SET_VERSION}-{hash}"),
                questions,
            },
        }
    }

    /// The identifier stored with every evaluation, e.g. `qs-1-3fa2c1d0`.
    pub fn id(&self) -> &str {
        &self.all.version
    }

    /// Every question any turn may ask, for the startup capability check.
    pub fn all(&self) -> &QuestionSet {
        &self.all
    }

    /// The questions for one turn: the case facts, plus the per-party
    /// questions for each party with new messages, plus `<party>_rejects_path`
    /// for those parties while guiding. A party with an empty `latest` list
    /// is asked nothing about its latest messages.
    pub fn for_turn(&self, with_new_messages: &[Party], guiding: bool) -> QuestionSet {
        let questions = self
            .all
            .questions
            .iter()
            .filter(|(id, _)| match split_party(id) {
                None => true,
                Some((party, suffix)) => {
                    with_new_messages.contains(&party) && (suffix != GUIDING || guiding)
                }
            })
            .map(|(id, question)| (id.clone(), question.clone()))
            .collect();
        QuestionSet {
            version: self.all.version.clone(),
            questions,
        }
    }
}

/// The canonical JSON of a question set (`docs/judgments.md` §2): each
/// question as `{"type", "instructions", "criteria"}`, keyed by id.
pub fn canonical_json(questions: &BTreeMap<String, Question>) -> Value {
    questions
        .iter()
        .map(|(id, question)| (id.clone(), canonical_question(question)))
        .collect::<Map<_, _>>()
        .into()
}

fn canonical_question(question: &Question) -> Value {
    match question {
        Question::Noul {
            instructions,
            criteria,
        } => {
            let mut value = json!({ "type": "noul", "instructions": instructions });
            if let Some(c) = criteria {
                value["criteria"] = json!({ "true": c.yes, "false": c.no });
            }
            value
        }
        Question::Choice {
            instructions,
            options,
        } => {
            let criteria: Map<String, Value> = options
                .iter()
                .map(|(name, description)| {
                    (name.clone(), description.clone().unwrap_or(Value::Null))
                })
                .collect();
            json!({ "type": "choice", "instructions": instructions, "criteria": criteria })
        }
        Question::Score {
            instructions,
            levels,
        } => json!({ "type": "score", "instructions": instructions, "criteria": levels }),
    }
}

/// The first `HASH_CHARS` hex characters of the SHA-256 of the canonical
/// JSON. The canonical JSON has sorted keys, so the hash does not depend on
/// insertion order; SHA-256 keeps it stable across Rust releases.
pub(crate) fn short_hash(questions: &BTreeMap<String, Question>) -> String {
    let digest = Sha256::digest(canonical_json(questions).to_string().as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(HASH_CHARS)
        .collect()
}

fn key(party: Party, suffix: &str) -> String {
    format!("{party}_{suffix}")
}

fn split_party(id: &str) -> Option<(Party, &str)> {
    [Party::Buyer, Party::Seller].into_iter().find_map(|party| {
        let suffix = id.strip_prefix(&format!("{party}_"))?;
        (PER_PARTY.contains(&suffix) || suffix == GUIDING).then_some((party, suffix))
    })
}

fn noul(instructions: &str, yes: &str, no: &str) -> Question {
    Question::Noul {
        instructions: json!(instructions),
        criteria: Some(NoulCriteria {
            yes: json!(yes),
            no: json!(no),
        }),
    }
}

fn choice(instructions: &str, options: &[(&str, &str)]) -> Question {
    Question::Choice {
        instructions: json!(instructions),
        options: options
            .iter()
            .map(|(name, description)| ((*name).to_owned(), Some(json!(description))))
            .collect(),
    }
}

/// `docs/judgments.md` §2.1: facts read from the whole transcript.
fn case_facts() -> BTreeMap<String, Question> {
    BTreeMap::from([
        (
            "buyer_payment".into(),
            choice(
                "Across the whole `transcript`, what has the buyer said about sending the fiat payment for this order?",
                &[
                    (
                        "says_sent",
                        "The buyer states they already sent or completed the fiat payment.",
                    ),
                    (
                        "says_not_sent",
                        "The buyer states they have not sent the payment, or not yet.",
                    ),
                    (
                        "not_stated",
                        "The buyer has not said either way, or their messages are too vague to tell.",
                    ),
                ],
            ),
        ),
        (
            "buyer_details".into(),
            noul(
                "Has the buyer given at least one concrete, checkable detail about the fiat payment they say they sent, such as the time, the exact amount, a transaction reference, the account or app used, or a receipt attached to their message?",
                "At least one specific detail about the transfer is present in the buyer's messages.",
                "The buyer only claims to have paid, or has given no payment details.",
            ),
        ),
        (
            "seller_receipt".into(),
            choice(
                "Across the whole `transcript`, what has the seller said about receiving the fiat payment for this order?",
                &[
                    (
                        "says_received",
                        "The seller states the full fiat payment arrived in their account and raises no problem with it.",
                    ),
                    (
                        "says_received_with_problem",
                        "The seller states a payment arrived but objects to it, for example a different amount, a sender other than the buyer, or a payment later reversed or charged back.",
                    ),
                    (
                        "says_not_received",
                        "The seller states the fiat payment has not arrived.",
                    ),
                    (
                        "not_stated",
                        "The seller has not said either way, or their messages are too vague to tell.",
                    ),
                ],
            ),
        ),
        (
            "seller_checked".into(),
            noul(
                "Does the seller say they checked the account or app where the payment should arrive?",
                "The seller describes having looked at their bank account, app, or statement for this payment.",
                "The seller has not said they checked.",
            ),
        ),
        (
            "claims_conflict".into(),
            noul(
                "Do the buyer and the seller make factual claims about the payment that cannot both be true?",
                "For example, the buyer says the payment was sent and completed while the seller says nothing arrived in the account.",
                "Their accounts are compatible, or at least one party has not made a claim yet.",
            ),
        ),
        (
            "fraud_signal".into(),
            noul(
                "Does anything in the `transcript` suggest deliberate bad faith by either party?",
                "Signs such as an admitted or described altered receipt, a payment from a third party's account, a reversed or charged-back payment, asking to move the conversation or the trade to another app or contact (for example a Telegram or WhatsApp username, or a phone number), pointing to a supposed Mostro support or administrator outside this chat, requests for passwords, seed words or private keys, or threats.",
                "Nothing beyond an ordinary disagreement or delay.",
            ),
        ),
        (
            "dispute_topic".into(),
            choice(
                "What is this dispute mainly about, based on the `transcript`?",
                &[
                    (
                        "payment_not_confirmed",
                        "Whether the fiat payment was sent or received.",
                    ),
                    (
                        "wrong_amount",
                        "The fiat payment arrived but for a different amount.",
                    ),
                    (
                        "wrong_account_or_method",
                        "The payment was sent to a different account, person, or payment method than agreed.",
                    ),
                    (
                        "counterpart_unresponsive",
                        "One party stopped answering during the trade.",
                    ),
                    (
                        "technical_problem",
                        "An app, wallet, invoice, or Lightning problem rather than a payment disagreement.",
                    ),
                    ("other", "Something else."),
                    ("not_yet_clear", "The parties have not said enough to tell."),
                ],
            ),
        ),
    ])
}

/// `docs/judgments.md` §2.2: questions about one party's latest messages.
fn per_party(party: Party, languages: &[Language<'_>]) -> BTreeMap<String, Question> {
    let mut language_options: Vec<(&str, &str)> =
        languages.iter().map(|l| (l.code, l.name)).collect();
    language_options.push((LANGUAGE_OTHER, "Another language"));
    language_options.push((
        LANGUAGE_UNKNOWN,
        "The messages are too short or mixed to tell.",
    ));

    BTreeMap::from([
        (
            key(party, "message_kind"),
            choice(
                &format!(
                    "Look only at the {party}'s messages listed in `latest.{party}`. What are they mainly doing?"
                ),
                &[
                    (
                        "answers",
                        "Giving information about the trade or the payment.",
                    ),
                    (
                        "greeting",
                        "Only greeting, saying hello, or calling for attention.",
                    ),
                    (
                        "asks_language",
                        "Asking whether Serbero speaks a particular language, or asking to switch language.",
                    ),
                    (
                        "not_understood",
                        "Saying they do not understand the question or the situation.",
                    ),
                    (
                        "asks_next_step",
                        "Asking what they should do or what happens next.",
                    ),
                    ("other", "Anything else."),
                ],
            ),
        ),
        (
            key(party, "language"),
            choice(
                &format!(
                    "Which language does the {party} want to be addressed in? Use the language they write in, unless they explicitly ask for another one."
                ),
                &language_options,
            ),
        ),
        (
            key(party, "wants_human"),
            noul(
                &format!(
                    "In the messages listed in `latest.{party}`, does the {party} explicitly ask to talk to a human person, a solver, an administrator, or support staff instead of the automated assistant?"
                ),
                "A direct request for a person, such as 'I want a human' or 'let me talk to someone from support'.",
                "No such request. Impatience or frustration alone is not a request.",
            ),
        ),
    ])
}

/// `docs/judgments.md` §2.2: asked only while the session is `guiding`.
fn rejects_path(party: Party) -> Question {
    noul(
        &format!(
            "In the messages listed in `latest.{party}`, does the {party} reject the way forward Serbero described, or deny what the other party reported?"
        ),
        "For example, the buyer says they never got the bitcoin and disagrees, the seller says the payment did not actually arrive, or a party refuses to cancel.",
        "The party agrees, asks a practical question, or says nothing against it.",
    )
}

#[cfg(test)]
mod tests;
