//! The brief request (`docs/judgments.md` §5): quotes for the solver and an
//! advisory evidence reading, asked once on the last turn's state.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::config::Thresholds;

use super::questions::{QUESTION_SET_VERSION, short_hash};
use super::{Answer, Answers, Question, QuestionSet};

const QUOTE_BUYER_PAYMENT: &str = "quote_buyer_payment";
const QUOTE_BUYER_DETAILS: &str = "quote_buyer_details";
const QUOTE_SELLER_RECEIPT: &str = "quote_seller_receipt";
const QUOTE_CONCERN: &str = "quote_concern";
const EVIDENCE_BALANCE: &str = "evidence_balance";

/// The option that quotes nothing.
const NONE: &str = "none";

/// Whose messages a quote question may choose from.
#[derive(Clone, Copy)]
enum Quoted {
    Buyer,
    Seller,
    /// Both parties; never Serbero.
    Parties,
}

impl Quoted {
    fn includes(self, from: &str) -> bool {
        match self {
            Self::Buyer => from == "buyer",
            Self::Seller => from == "seller",
            Self::Parties => from == "buyer" || from == "seller",
        }
    }
}

/// A quote question: id, whose messages it offers, its instructions and
/// what `none` means (`docs/judgments.md` §5).
struct Quote {
    id: &'static str,
    quoted: Quoted,
    instructions: &'static str,
    none: &'static str,
}

const QUOTES: [Quote; 4] = [
    Quote {
        id: QUOTE_BUYER_PAYMENT,
        quoted: Quoted::Buyer,
        instructions: "Which message in `transcript` is the buyer's clearest statement about whether they sent the fiat payment?",
        none: "No buyer message addresses this.",
    },
    Quote {
        id: QUOTE_BUYER_DETAILS,
        quoted: Quoted::Buyer,
        instructions: "Which message in `transcript` contains the buyer's most specific payment details?",
        none: "No buyer message contains payment details.",
    },
    Quote {
        id: QUOTE_SELLER_RECEIPT,
        quoted: Quoted::Seller,
        instructions: "Which message in `transcript` is the seller's clearest statement about whether the fiat payment arrived?",
        none: "No seller message addresses this.",
    },
    Quote {
        id: QUOTE_CONCERN,
        quoted: Quoted::Parties,
        instructions: "Which message in `transcript` is the strongest sign of bad faith, contradiction, or a problem a human should read first?",
        none: "Nothing stands out.",
    },
];

const EVIDENCE_INSTRUCTIONS: &str = "Considering only what the parties wrote in `transcript`, how strongly does the conversation indicate that the buyer actually sent the fiat payment?";

const EVIDENCE_LEVELS: [&str; 5] = [
    "The buyer says they did not pay, or the conversation clearly indicates no payment was made.",
    "The buyer's claim to have paid is vague or contradicted and unsupported by details.",
    "There is not enough information to lean either way.",
    "The buyer gives specific, consistent payment details, but the seller has not confirmed receipt.",
    "The seller confirms the payment arrived.",
];

/// The brief question set. Its options are the transcript's message ids, so
/// its identifier is computed over the fixed text only: every quote
/// question offered with `none` alone, and the evidence question.
#[derive(Debug, Clone, PartialEq)]
pub struct BriefQuestions {
    id: String,
}

impl BriefQuestions {
    pub fn new() -> Self {
        let template: BTreeMap<String, Question> = QUOTES
            .iter()
            .map(|quote| (quote.id.to_owned(), quote_question(quote, Vec::new())))
            .chain([(EVIDENCE_BALANCE.to_owned(), evidence_question())])
            .collect();
        Self {
            id: format!("{QUESTION_SET_VERSION}-{}", short_hash(&template)),
        }
    }

    /// The identifier stored with every brief evaluation.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The brief questions over one state. A quote question whose party
    /// wrote nothing is left out: `none` would be its only option.
    pub fn for_state(&self, state: &Value) -> QuestionSet {
        let mut questions: BTreeMap<String, Question> = QUOTES
            .iter()
            .filter_map(|quote| {
                let ids = message_ids(state, quote.quoted);
                (!ids.is_empty()).then(|| (quote.id.to_owned(), quote_question(quote, ids)))
            })
            .collect();
        questions.insert(EVIDENCE_BALANCE.to_owned(), evidence_question());
        QuestionSet {
            version: self.id.clone(),
            questions,
        }
    }
}

impl Default for BriefQuestions {
    fn default() -> Self {
        Self::new()
    }
}

/// Options are message ids with `null` criteria (the text is in the state),
/// then `none`.
fn quote_question(quote: &Quote, ids: Vec<String>) -> Question {
    let options = ids
        .into_iter()
        .map(|id| (id, None))
        .chain([(NONE.to_owned(), Some(json!(quote.none)))])
        .collect();
    Question::Choice {
        instructions: json!(quote.instructions),
        options,
    }
}

fn evidence_question() -> Question {
    Question::Score {
        instructions: json!(EVIDENCE_INSTRUCTIONS),
        levels: EVIDENCE_LEVELS.iter().map(|level| json!(level)).collect(),
    }
}

/// Ids of the transcript messages written by `quoted`, in transcript order.
fn message_ids(state: &Value, quoted: Quoted) -> Vec<String> {
    state["transcript"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["from"].as_str().is_some_and(|from| quoted.includes(from)))
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect()
}

/// The reading of `evidence_balance`: its value from 0 to 1 and the
/// distribution over its five levels.
#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceBalance {
    pub value: f64,
    pub distribution: Vec<f64>,
}

/// What the brief shows the solver. Quotes are short message ids (`m4`);
/// `State::message_id` maps them to stored messages.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Brief {
    pub quote_buyer_payment: Option<String>,
    pub quote_buyer_details: Option<String>,
    pub quote_seller_receipt: Option<String>,
    pub quote_concern: Option<String>,
    pub evidence_balance: Option<EvidenceBalance>,
}

/// Reads a brief from its answers over `state`. A quote is kept only if it
/// is not `none`, its share reaches `fact`, and it is a message the right
/// party wrote in `state`; the last check holds even for answers that were
/// not checked against this state's questions.
pub fn from_answers(state: &Value, answers: &Answers, thresholds: &Thresholds) -> Brief {
    let quote = |id: &str| {
        let spec = QUOTES.iter().find(|q| q.id == id)?;
        let answer = answers.get(id)?;
        let winner = answer.winner()?;
        let allowed = message_ids(state, spec.quoted);
        (winner != NONE
            && answer.share(winner) >= thresholds.fact
            && allowed.iter().any(|m| m == winner))
        .then(|| winner.to_owned())
    };
    let evidence_balance = match answers.get(EVIDENCE_BALANCE) {
        Some(answer @ Answer::Score { probabilities }) => {
            answer.score_value().map(|value| EvidenceBalance {
                value,
                distribution: probabilities.clone(),
            })
        }
        _ => None,
    };
    Brief {
        quote_buyer_payment: quote(QUOTE_BUYER_PAYMENT),
        quote_buyer_details: quote(QUOTE_BUYER_DETAILS),
        quote_seller_receipt: quote(QUOTE_SELLER_RECEIPT),
        quote_concern: quote(QUOTE_CONCERN),
        evidence_balance,
    }
}

#[cfg(test)]
mod tests;
