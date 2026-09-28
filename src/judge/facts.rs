//! From judge answers to facts (`docs/judgments.md` §3).

use crate::config::Thresholds;
use crate::store::sessions::Party;

use super::{Answer, Answers};

/// What a party's latest messages are mainly doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Answers,
    Greeting,
    AsksLanguage,
    NotUnderstood,
    AsksNextStep,
    Other,
}

/// Facts about one party's latest messages; present only for a party who
/// wrote in this turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyFacts {
    pub wants_human: bool,
    pub rejects_path: bool,
    pub language: Option<String>,
    pub message_kind: MessageKind,
}

/// The facts of one turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub buyer_sent: bool,
    pub buyer_not_sent: bool,
    pub buyer_not_sent_for_guide: bool,
    pub buyer_has_details: bool,
    pub seller_received: bool,
    pub seller_received_for_guide: bool,
    pub seller_not_received: bool,
    pub seller_has_checked: bool,
    pub conflict: bool,
    pub fraud: bool,
    pub outside_scope: bool,
    pub buyer: Option<PartyFacts>,
    pub seller: Option<PartyFacts>,
}

impl Facts {
    /// Neither a sent nor a not-sent claim is known: ask the buyer.
    pub fn buyer_payment_unknown(&self) -> bool {
        !self.buyer_sent && !self.buyer_not_sent
    }

    /// Neither a received nor a not-received claim is known, and the seller
    /// has not objected to a payment (that is `outside_scope`): ask the
    /// seller.
    pub fn seller_receipt_unknown(&self) -> bool {
        !self.seller_received && !self.seller_not_received && !self.outside_scope
    }

    pub fn party(&self, party: Party) -> Option<&PartyFacts> {
        match party {
            Party::Buyer => self.buyer.as_ref(),
            Party::Seller => self.seller.as_ref(),
        }
    }
}

/// Minimum probability of the winning `<party>_language` option before the
/// party's language changes; below it the current language is kept.
const LANGUAGE_MIN_PROBABILITY: f64 = 0.7;

/// Minimum Serbero-computed confidence of `<party>_message_kind`; below it
/// the messages are treated as answers.
const MESSAGE_KIND_MIN_CONFIDENCE: f64 = 0.5;

/// `dispute_topic` options that put a dispute outside payment confirmation.
const OUTSIDE_SCOPE_TOPICS: [&str; 4] = [
    "wrong_amount",
    "wrong_account_or_method",
    "technical_problem",
    "other",
];

/// Converts the answers of one turn into facts. `answers` must have passed
/// `check_answers` against the turn's question set; a question that was not
/// asked counts as stating nothing, so a fact is never known by default.
/// `languages` are the enabled language codes.
pub fn from_answers(answers: &Answers, thresholds: &Thresholds, languages: &[String]) -> Facts {
    let buyer_payment = |option| probability(answers, "buyer_payment", option);
    let seller_receipt = |option| probability(answers, "seller_receipt", option);
    let topic_outside: f64 = OUTSIDE_SCOPE_TOPICS
        .iter()
        .map(|topic| probability(answers, "dispute_topic", topic))
        .sum();

    Facts {
        buyer_sent: buyer_payment("says_sent") >= thresholds.fact,
        buyer_not_sent: buyer_payment("says_not_sent") >= thresholds.fact,
        buyer_not_sent_for_guide: buyer_payment("says_not_sent") >= thresholds.guide,
        buyer_has_details: p_yes(answers, "buyer_details") >= thresholds.fact,
        seller_received: seller_receipt("says_received") >= thresholds.fact,
        seller_received_for_guide: seller_receipt("says_received") >= thresholds.guide,
        seller_not_received: seller_receipt("says_not_received") >= thresholds.fact,
        seller_has_checked: p_yes(answers, "seller_checked") >= thresholds.fact,
        conflict: p_yes(answers, "claims_conflict") >= thresholds.conflict,
        fraud: p_yes(answers, "fraud_signal") >= thresholds.fraud,
        outside_scope: topic_outside >= thresholds.outside_scope
            || seller_receipt("says_received_with_problem") >= thresholds.outside_scope,
        buyer: party_facts(answers, Party::Buyer, thresholds, languages),
        seller: party_facts(answers, Party::Seller, thresholds, languages),
    }
}

/// `None` when the party's per-party questions were not asked, that is,
/// when the party did not write in this turn.
fn party_facts(
    answers: &Answers,
    party: Party,
    thresholds: &Thresholds,
    languages: &[String],
) -> Option<PartyFacts> {
    let message_kind = answers.get(&format!("{party}_message_kind"))?;
    let language = answers
        .get(&format!("{party}_language"))
        .and_then(|answer| {
            let winner = answer.winner()?;
            let enabled = languages.iter().any(|code| code == winner);
            (enabled && answer.probability(winner) >= LANGUAGE_MIN_PROBABILITY)
                .then(|| winner.to_owned())
        });
    Some(PartyFacts {
        wants_human: p_yes(answers, &format!("{party}_wants_human")) >= thresholds.human_request,
        // Asked only while guiding; `conflict` is its threshold (§3).
        rejects_path: p_yes(answers, &format!("{party}_rejects_path")) >= thresholds.conflict,
        language,
        message_kind: kind(message_kind),
    })
}

fn kind(answer: &Answer) -> MessageKind {
    let confident = answer
        .confidence()
        .is_some_and(|c| c >= MESSAGE_KIND_MIN_CONFIDENCE);
    match answer.winner().filter(|_| confident) {
        Some("greeting") => MessageKind::Greeting,
        Some("asks_language") => MessageKind::AsksLanguage,
        Some("not_understood") => MessageKind::NotUnderstood,
        Some("asks_next_step") => MessageKind::AsksNextStep,
        Some("other") => MessageKind::Other,
        _ => MessageKind::Answers,
    }
}

fn probability(answers: &Answers, id: &str, option: &str) -> f64 {
    answers.get(id).map_or(0.0, |a| a.probability(option))
}

fn p_yes(answers: &Answers, id: &str) -> f64 {
    match answers.get(id) {
        Some(Answer::Noul { p_yes }) => *p_yes,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests;
