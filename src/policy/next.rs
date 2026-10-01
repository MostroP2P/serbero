//! The next question for each party (`docs/judgments.md` §4.1).

use crate::judge::facts::{Facts, MessageKind};
use crate::store::sessions::Party;

use super::{Asked, NextQuestions, template};

/// What Serbero last sent a party, and whether it still waits for an answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartyHistory<'a> {
    /// The last template sent to the party, if any.
    pub last_template: Option<&'a str>,
    /// The last question template sent to the party, if any.
    pub last_question: Option<&'a str>,
    /// A question was sent and the party has not written since.
    pub outstanding: bool,
    /// This turn changed the party's language.
    pub language_changed: bool,
}

/// The session's history as §4.1 reads it.
#[derive(Debug, Clone, Copy)]
pub struct History<'a> {
    pub asked: &'a Asked,
    pub buyer: PartyHistory<'a>,
    pub seller: PartyHistory<'a>,
}

/// Picks the templates to send each party this turn.
pub fn next_questions(facts: &Facts, history: &History<'_>) -> NextQuestions {
    let (buyer, buyer_resend) = for_party(Party::Buyer, facts, history);
    let (seller, seller_resend) = for_party(Party::Seller, facts, history);
    NextQuestions {
        buyer,
        seller,
        buyer_resend,
        seller_resend,
    }
}

/// Serbero writes only to a party who wrote this turn, or to a silent party
/// with no question outstanding whose needed question is new. A silent
/// party is never only thanked.
fn for_party(party: Party, facts: &Facts, history: &History<'_>) -> (Vec<&'static str>, bool) {
    let own = match party {
        Party::Buyer => history.buyer,
        Party::Seller => history.seller,
    };
    let sent = |t: &str| history.asked.sent(party, t);
    let Some(party_facts) = facts.party(party) else {
        if own.outstanding {
            return (Vec::new(), false);
        }
        return (
            needed_question(party, facts, &sent).into_iter().collect(),
            false,
        );
    };

    // Step 1: what the party's messages are doing.
    let answered = matches!(
        party_facts.message_kind,
        MessageKind::Answers | MessageKind::AsksNextStep
    );
    if own.language_changed && !answered {
        // The only allowed repeat: the same template, now in the party's
        // new language. A party who did not answer most likely could not
        // read it ("no entiendo"); without a language change it would be a
        // plain repeat, so the turn continues normally.
        if let Some(last) = language_resend(&own) {
            return (vec![last], true);
        }
    }
    if party_facts.message_kind == MessageKind::NotUnderstood {
        let simple = own
            .last_question
            .and_then(template::simple_variant)
            .filter(|s| !sent(s));
        if let Some(simple) = simple {
            return (vec![simple], false);
        }
    }
    let mut templates = Vec::new();
    if party_facts.message_kind == MessageKind::AsksNextStep && !sent(template::WHAT_HAPPENS_NEXT) {
        templates.push(template::WHAT_HAPPENS_NEXT);
    }

    // Step 2: the needed fact, never repeating a template.
    if let Some(question) = needed_question(party, facts, &sent) {
        templates.push(question);
    } else if nothing_needed(party, facts) && !sent(template::THANKS_WAITING) {
        templates.push(template::THANKS_WAITING);
    }
    (templates, false)
}

/// The template a language resend repeats: the last one sent, except a
/// reminder, which only points at the unanswered question, so the question
/// itself goes out again and its response timer restarts.
fn language_resend(own: &PartyHistory<'_>) -> Option<&'static str> {
    let last = match own.last_template {
        Some(template::REMINDER) => own.last_question,
        last => last,
    };
    last.and_then(template::resendable)
}

/// The first question of the §4.1 table whose fact is needed and which was
/// not sent yet. A normal question comes before its `_simple` variant.
fn needed_question(
    party: Party,
    facts: &Facts,
    sent: &dyn Fn(&str) -> bool,
) -> Option<&'static str> {
    let rows: [(bool, &'static str); 3] = match party {
        Party::Buyer => [
            (facts.buyer_payment_unknown(), template::ASK_BUYER_SENT),
            (
                facts.buyer_payment_unknown(),
                template::ASK_BUYER_SENT_SIMPLE,
            ),
            (
                facts.buyer_sent && !facts.buyer_has_details,
                template::ASK_BUYER_DETAILS,
            ),
        ],
        Party::Seller => [
            (
                facts.seller_receipt_unknown(),
                template::ASK_SELLER_RECEIVED,
            ),
            (
                facts.seller_receipt_unknown(),
                template::ASK_SELLER_RECEIVED_SIMPLE,
            ),
            (
                facts.seller_not_received && !facts.seller_has_checked,
                template::ASK_SELLER_CHECK_ACCOUNT,
            ),
        ],
    };
    rows.into_iter()
        .find(|(needed, question)| *needed && !sent(question))
        .map(|(_, question)| question)
}

/// No fact is needed from the party. `thanks_waiting` says so to the party,
/// so it is never sent while a fact is still missing, even when every
/// question for it was already used.
fn nothing_needed(party: Party, facts: &Facts) -> bool {
    match party {
        Party::Buyer => {
            !facts.buyer_payment_unknown() && !(facts.buyer_sent && !facts.buyer_has_details)
        }
        Party::Seller => {
            !facts.seller_receipt_unknown()
                && !(facts.seller_not_received && !facts.seller_has_checked)
        }
    }
}

#[cfg(test)]
mod tests;
