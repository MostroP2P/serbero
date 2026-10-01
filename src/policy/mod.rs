//! The decision table (`docs/judgments.md` §4): a pure function from the
//! facts of a turn and the session's history to one action.
//!
//! Code owns decisions (AGENTS.md rule 7): the judge only supplies facts.
//! Every path toward a human is checked before any path that guides the
//! parties, and guidance needs both parties' languages to be validated for
//! the active judge (`docs/spec.md` §7.7).

use std::collections::BTreeSet;

use serde::Serialize;

use crate::judge::facts::Facts;
use crate::store::sessions::Party;

/// Party template ids the policy refers to (`docs/messages.md` §2).
pub mod template {
    pub const ASK_BUYER_SENT: &str = "ask_buyer_sent";
    pub const ASK_BUYER_SENT_SIMPLE: &str = "ask_buyer_sent_simple";
    pub const ASK_BUYER_DETAILS: &str = "ask_buyer_details";
    pub const ASK_SELLER_RECEIVED: &str = "ask_seller_received";
    pub const ASK_SELLER_RECEIVED_SIMPLE: &str = "ask_seller_received_simple";
    pub const ASK_SELLER_CHECK_ACCOUNT: &str = "ask_seller_check_account";
    pub const WHAT_HAPPENS_NEXT: &str = "what_happens_next";
    pub const THANKS_WAITING: &str = "thanks_waiting";
    pub const REMINDER: &str = "reminder";

    /// Templates that ask a party for a fact. A turn that sends one counts
    /// as a round, and only these keep a turn from handing off (§4 row 10).
    pub const QUESTIONS: [&str; 6] = [
        ASK_BUYER_SENT,
        ASK_BUYER_SENT_SIMPLE,
        ASK_BUYER_DETAILS,
        ASK_SELLER_RECEIVED,
        ASK_SELLER_RECEIVED_SIMPLE,
        ASK_SELLER_CHECK_ACCOUNT,
    ];

    pub fn is_question(id: &str) -> bool {
        QUESTIONS.contains(&id)
    }

    /// Templates that may be sent again when a party's language changes
    /// before it answered (§4.1 step 1).
    const RESENDABLE: [&str; 9] = [
        ASK_BUYER_SENT,
        ASK_BUYER_SENT_SIMPLE,
        ASK_BUYER_DETAILS,
        ASK_SELLER_RECEIVED,
        ASK_SELLER_RECEIVED_SIMPLE,
        ASK_SELLER_CHECK_ACCOUNT,
        WHAT_HAPPENS_NEXT,
        THANKS_WAITING,
        REMINDER,
    ];

    /// The static id of a stored template id that may be resent.
    pub fn resendable(id: &str) -> Option<&'static str> {
        RESENDABLE.into_iter().find(|t| *t == id)
    }

    /// The `_simple` variant of a question, if it has one.
    pub fn simple_variant(id: &str) -> Option<&'static str> {
        match id {
            ASK_BUYER_SENT => Some(ASK_BUYER_SENT_SIMPLE),
            ASK_SELLER_RECEIVED => Some(ASK_SELLER_RECEIVED_SIMPLE),
            _ => None,
        }
    }
}

/// What Serbero does after a turn (`docs/spec.md` §7.3).
/// Serialized as `{"ask": {"buyer": [...], "seller": [...]}}`,
/// `{"guide": "payment_arrived"}`, `{"handoff": "fraud_signal"}` or
/// `"wait"`, and stored with each evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Send these templates, in order, to each party.
    Ask {
        buyer: Vec<&'static str>,
        seller: Vec<&'static str>,
    },
    /// Explain a self-resolution path to both parties.
    Guide(Path),
    /// Brief the solver and send the parties `handoff_notice`.
    Handoff(HandoffReason),
    /// Nothing to send; a timer or the next message moves the session.
    Wait,
}

/// A self-resolution path (`docs/spec.md` §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Path {
    /// The seller says the fiat arrived.
    PaymentArrived,
    /// The buyer says they have not paid.
    PaymentNotSent,
}

/// Why a session goes to a human (`docs/spec.md` §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffReason {
    SelfResolutionStalled,
    FactsGathered,
    ConflictingClaims,
    FraudSignal,
    HumanRequested,
    OutsideScope,
    Unresponsive,
    RoundLimit,
    Uncertain,
    JudgeUnavailable,
    Flood,
    /// Serbero took the dispute but could not reach the parties.
    OpeningFailed,
}

impl Path {
    /// The name used in solver messages and the store.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PaymentArrived => "payment_arrived",
            Self::PaymentNotSent => "payment_not_sent",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [Self::PaymentArrived, Self::PaymentNotSent]
            .into_iter()
            .find(|p| p.as_str() == name)
    }
}

impl HandoffReason {
    pub const ALL: [Self; 12] = [
        Self::SelfResolutionStalled,
        Self::FactsGathered,
        Self::ConflictingClaims,
        Self::FraudSignal,
        Self::HumanRequested,
        Self::OutsideScope,
        Self::Unresponsive,
        Self::RoundLimit,
        Self::Uncertain,
        Self::JudgeUnavailable,
        Self::Flood,
        Self::OpeningFailed,
    ];

    /// The reason stored under `name`, if it is one.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.as_str() == name)
    }

    /// The name in `docs/spec.md` §7.6, used in solver messages and the
    /// store.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelfResolutionStalled => "self_resolution_stalled",
            Self::FactsGathered => "facts_gathered",
            Self::ConflictingClaims => "conflicting_claims",
            Self::FraudSignal => "fraud_signal",
            Self::HumanRequested => "human_requested",
            Self::OutsideScope => "outside_scope",
            Self::Unresponsive => "unresponsive",
            Self::RoundLimit => "round_limit",
            Self::Uncertain => "uncertain",
            Self::JudgeUnavailable => "judge_unavailable",
            Self::Flood => "flood",
            Self::OpeningFailed => "opening_failed",
        }
    }
}

/// Where a live session is: gathering facts, or watching after guidance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Gathering,
    Guiding,
}

/// Template ids already sent to each party, in any language.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asked {
    pub buyer: BTreeSet<String>,
    pub seller: BTreeSet<String>,
}

impl Asked {
    pub fn sent(&self, party: Party, template: &str) -> bool {
        match party {
            Party::Buyer => self.buyer.contains(template),
            Party::Seller => self.seller.contains(template),
        }
    }
}

/// The templates §4.1 picks for each party this turn, in sending order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NextQuestions {
    pub buyer: Vec<&'static str>,
    pub seller: Vec<&'static str>,
    /// The party's templates are a resend in a new language (§4.1 step 1),
    /// which never counts as a round.
    pub buyer_resend: bool,
    pub seller_resend: bool,
}

impl NextQuestions {
    /// The turn asks `party` a question that is not a language resend,
    /// which counts as one of the party's rounds (§4.1).
    pub fn asks(&self, party: Party) -> bool {
        let (templates, resend) = match party {
            Party::Buyer => (&self.buyer, self.buyer_resend),
            Party::Seller => (&self.seller, self.seller_resend),
        };
        !resend && templates.iter().any(|t| template::is_question(t))
    }

    /// A turn counts as a round of the session when it asks either party.
    pub fn counts_as_round(&self) -> bool {
        self.asks(Party::Buyer) || self.asks(Party::Seller)
    }
}

/// Everything one decision reads.
#[derive(Debug, Clone, Copy)]
pub struct Turn<'a> {
    pub phase: Phase,
    pub facts: &'a Facts,
    /// Each party's current language, after this turn's detection.
    pub buyer_language: &'a str,
    pub seller_language: &'a str,
    /// The active judge's `validated_languages`.
    pub validated_languages: &'a [String],
    /// Question rounds each party was asked so far.
    pub buyer_rounds: u32,
    pub seller_rounds: u32,
    pub max_rounds: u32,
    pub asked: &'a Asked,
    pub next: &'a NextQuestions,
}

/// The decision table of `docs/judgments.md` §4: rows in order, first
/// match wins.
pub fn decide(turn: &Turn<'_>) -> Action {
    let facts = turn.facts;
    let any_party = |check: fn(&crate::judge::facts::PartyFacts) -> bool| {
        [Party::Buyer, Party::Seller]
            .into_iter()
            .any(|party| facts.party(party).is_some_and(check))
    };
    let guiding = turn.phase == Phase::Guiding;

    if any_party(|p| p.wants_human) {
        return Action::Handoff(HandoffReason::HumanRequested);
    }
    if facts.fraud {
        return Action::Handoff(HandoffReason::FraudSignal);
    }
    if facts.outside_scope {
        return Action::Handoff(HandoffReason::OutsideScope);
    }
    if guiding && any_party(|p| p.rejects_path) {
        return Action::Handoff(HandoffReason::SelfResolutionStalled);
    }
    if guiding {
        return Action::Wait;
    }
    if facts.seller_received_for_guide {
        return guide_or_hand_off(turn, Path::PaymentArrived);
    }
    if facts.buyer_not_sent_for_guide {
        return guide_or_hand_off(turn, Path::PaymentNotSent);
    }
    if conflict_with_both_sides_answered(turn) {
        return Action::Handoff(HandoffReason::ConflictingClaims);
    }
    if turn.buyer_rounds.max(turn.seller_rounds) >= turn.max_rounds {
        return Action::Handoff(HandoffReason::RoundLimit);
    }
    let next = turn.next;
    if next
        .buyer
        .iter()
        .chain(&next.seller)
        .any(|t| template::is_question(t))
    {
        return Action::Ask {
            buyer: next.buyer.clone(),
            seller: next.seller.clone(),
        };
    }
    if unknown_after_both_variants(turn) {
        return Action::Handoff(HandoffReason::Uncertain);
    }
    if !facts.buyer_payment_unknown() && !facts.seller_receipt_unknown() {
        return Action::Handoff(HandoffReason::FactsGathered);
    }
    // Row 13: nothing to ask, but a party who answered everything is still
    // told so (`thanks_waiting`, `what_happens_next`) while the other one
    // is awaited.
    if next.buyer.is_empty() && next.seller.is_empty() {
        Action::Wait
    } else {
        Action::Ask {
            buyer: next.buyer.clone(),
            seller: next.seller.clone(),
        }
    }
}

/// Guidance mentions a fund action, so it needs both parties' languages to
/// be validated for the judge; otherwise the facts go to a human (§7.7).
fn guide_or_hand_off(turn: &Turn<'_>, path: Path) -> Action {
    let validated = |lang: &str| turn.validated_languages.iter().any(|v| v == lang);
    if validated(turn.buyer_language) && validated(turn.seller_language) {
        Action::Guide(path)
    } else {
        Action::Handoff(HandoffReason::FactsGathered)
    }
}

/// Row 8: the buyer says they paid, the seller says nothing arrived, the
/// claims conflict, and each side was asked its follow-up (or answered it).
fn conflict_with_both_sides_answered(turn: &Turn<'_>) -> bool {
    let facts = turn.facts;
    let details =
        facts.buyer_has_details || turn.asked.sent(Party::Buyer, template::ASK_BUYER_DETAILS);
    let checked = facts.seller_has_checked
        || turn
            .asked
            .sent(Party::Seller, template::ASK_SELLER_CHECK_ACCOUNT);
    facts.buyer_sent && facts.seller_not_received && facts.conflict && details && checked
}

/// Row 11: a payment fact is still unknown after both of its questions
/// (the normal and the `_simple` variant) were sent.
fn unknown_after_both_variants(turn: &Turn<'_>) -> bool {
    let both_sent =
        |party, normal, simple| turn.asked.sent(party, normal) && turn.asked.sent(party, simple);
    (turn.facts.buyer_payment_unknown()
        && both_sent(
            Party::Buyer,
            template::ASK_BUYER_SENT,
            template::ASK_BUYER_SENT_SIMPLE,
        ))
        || (turn.facts.seller_receipt_unknown()
            && both_sent(
                Party::Seller,
                template::ASK_SELLER_RECEIVED,
                template::ASK_SELLER_RECEIVED_SIMPLE,
            ))
}

pub mod next;
pub mod timers;

#[cfg(test)]
mod tests;
