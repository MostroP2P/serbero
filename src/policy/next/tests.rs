use super::*;
use crate::judge::facts::{MessageKind, PartyFacts};
use crate::policy::template::*;
use crate::store::sessions::Party;

/// A turn in which the party given wrote with `kind`; the other party is
/// silent with a question outstanding, so it gets nothing.
struct Case {
    facts: Facts,
    asked: Asked,
    buyer: PartyHistory<'static>,
    seller: PartyHistory<'static>,
}

impl Case {
    fn new() -> Self {
        let outstanding = PartyHistory {
            outstanding: true,
            ..PartyHistory::default()
        };
        Self {
            facts: Facts::default(),
            asked: Asked::default(),
            buyer: outstanding,
            seller: outstanding,
        }
    }

    /// The party wrote this turn: it has party facts and no outstanding
    /// question.
    fn wrote(mut self, party: Party, kind: MessageKind) -> Self {
        let facts = Some(PartyFacts {
            wants_human: false,
            rejects_path: false,
            language: None,
            message_kind: kind,
        });
        match party {
            Party::Buyer => {
                self.facts.buyer = facts;
                self.buyer.outstanding = false;
            }
            Party::Seller => {
                self.facts.seller = facts;
                self.seller.outstanding = false;
            }
        }
        self
    }

    fn sent(mut self, party: Party, templates: &[&'static str]) -> Self {
        let set = match party {
            Party::Buyer => &mut self.asked.buyer,
            Party::Seller => &mut self.asked.seller,
        };
        set.extend(templates.iter().map(|t| (*t).to_owned()));
        let history = match party {
            Party::Buyer => &mut self.buyer,
            Party::Seller => &mut self.seller,
        };
        if let Some(last) = templates.last() {
            history.last_template = Some(last);
        }
        if let Some(q) = templates.iter().rev().find(|t| is_question(t)) {
            history.last_question = Some(q);
        }
        self
    }

    fn with(mut self, change: impl FnOnce(&mut Self)) -> Self {
        change(&mut self);
        self
    }

    fn next(&self) -> NextQuestions {
        next_questions(
            &self.facts,
            &History {
                asked: &self.asked,
                buyer: self.buyer,
                seller: self.seller,
            },
        )
    }
}

fn buyer(templates: &[&'static str]) -> NextQuestions {
    NextQuestions {
        buyer: templates.to_vec(),
        ..NextQuestions::default()
    }
}

fn seller(templates: &[&'static str]) -> NextQuestions {
    NextQuestions {
        seller: templates.to_vec(),
        ..NextQuestions::default()
    }
}

// Step 2: the needed fact, in table order, never repeating.

#[test]
fn an_unknown_payment_asks_the_normal_question_then_the_simple_one() {
    let first = Case::new().wrote(Party::Buyer, MessageKind::Answers);
    let second = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::Answers);
    let third = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT, ASK_BUYER_SENT_SIMPLE])
        .wrote(Party::Buyer, MessageKind::Answers);

    assert_eq!(first.next(), buyer(&[ASK_BUYER_SENT]));
    assert_eq!(second.next(), buyer(&[ASK_BUYER_SENT_SIMPLE]));
    assert_eq!(
        third.next(),
        buyer(&[]),
        "never repeat, and never thank while a fact is needed"
    );
}

#[test]
fn a_payment_claim_without_details_asks_for_details_once() {
    let claim = |c: &mut Case| c.facts.buyer_sent = true;
    let first = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::Answers)
        .with(claim);
    let again = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT, ASK_BUYER_DETAILS])
        .wrote(Party::Buyer, MessageKind::Answers)
        .with(claim);

    assert_eq!(first.next(), buyer(&[ASK_BUYER_DETAILS]));
    assert_eq!(
        again.next(),
        buyer(&[WAITING_OTHER_PARTY]),
        "the details are asked once; the buyer is then told the seller is awaited"
    );
}

#[test]
fn an_unknown_receipt_asks_the_normal_question_then_the_simple_one() {
    let first = Case::new().wrote(Party::Seller, MessageKind::Answers);
    let second = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .wrote(Party::Seller, MessageKind::Answers);

    assert_eq!(first.next(), seller(&[ASK_SELLER_RECEIVED]));
    assert_eq!(second.next(), seller(&[ASK_SELLER_RECEIVED_SIMPLE]));
}

#[test]
fn a_seller_who_did_not_receive_is_asked_to_check_once() {
    let denies = |c: &mut Case| c.facts.seller_not_received = true;
    let first = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .wrote(Party::Seller, MessageKind::Answers)
        .with(denies);
    let checked = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .wrote(Party::Seller, MessageKind::Answers)
        .with(denies)
        .with(|c| c.facts.seller_has_checked = true);

    assert_eq!(first.next(), seller(&[ASK_SELLER_CHECK_ACCOUNT]));
    assert_eq!(checked.next(), seller(&[THANKS_WAITING]));
}

#[test]
fn nothing_needed_thanks_the_party_once() {
    let known = |c: &mut Case| {
        c.facts.buyer_sent = true;
        c.facts.buyer_has_details = true;
    };
    let first = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::Answers)
        .with(known);
    let again = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT, THANKS_WAITING])
        .wrote(Party::Buyer, MessageKind::Answers)
        .with(known);

    assert_eq!(first.next(), buyer(&[THANKS_WAITING]));
    assert_eq!(
        again.next(),
        buyer(&[WAITING_OTHER_PARTY]),
        "thanked once; writing again, the buyer is told the seller is awaited"
    );
}

#[test]
fn the_buyer_saying_they_did_not_pay_needs_nothing_more() {
    let case = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::Answers)
        .with(|c| c.facts.buyer_not_sent = true);

    assert_eq!(case.next(), buyer(&[THANKS_WAITING]));
}

#[test]
fn a_seller_objecting_to_a_payment_is_not_asked_about_receipt() {
    let case = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .wrote(Party::Seller, MessageKind::Answers)
        .with(|c| c.facts.outside_scope = true);

    assert_eq!(case.next(), seller(&[THANKS_WAITING]));
}

// Step 1: message kind first.

#[test]
fn asking_about_the_language_resends_the_last_template_in_the_new_language() {
    let case = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::AsksLanguage)
        .with(|c| c.buyer.language_changed = true);

    let next = case.next();

    assert_eq!(next.buyer, [ASK_BUYER_SENT]);
    assert!(next.buyer_resend);
    assert!(!next.counts_as_round(), "a language resend is not a round");
}

#[test]
fn asking_about_the_language_without_a_change_continues_normally() {
    let case = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::AsksLanguage);

    assert_eq!(case.next(), buyer(&[ASK_BUYER_SENT_SIMPLE]));
}

#[test]
fn asking_about_the_language_before_any_template_continues_normally() {
    let case = Case::new()
        .wrote(Party::Buyer, MessageKind::AsksLanguage)
        .with(|c| c.buyer.language_changed = true);

    assert_eq!(case.next(), buyer(&[ASK_BUYER_SENT]));
}

#[test]
fn not_understanding_after_a_language_change_resends_the_last_template() {
    // Staging: the buyer got both payment questions in English and wrote
    // "no entiendo"; the simple variant was used, so nothing was sent.
    let case = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT, ASK_BUYER_SENT_SIMPLE])
        .wrote(Party::Buyer, MessageKind::NotUnderstood)
        .with(|c| c.buyer.language_changed = true);

    let next = case.next();

    assert_eq!(next.buyer, [ASK_BUYER_SENT_SIMPLE]);
    assert!(next.buyer_resend);
    assert!(!next.counts_as_round(), "a language resend is not a round");
}

#[test]
fn a_language_change_after_a_reminder_resends_the_unanswered_question() {
    // The reminder only points at the earlier question; resending it would
    // leave nothing to answer and no response timer running.
    let case = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT, REMINDER])
        .wrote(Party::Buyer, MessageKind::NotUnderstood)
        .with(|c| c.buyer.language_changed = true);

    let next = case.next();

    assert_eq!(next.buyer, [ASK_BUYER_SENT]);
    assert!(next.buyer_resend);
}

#[test]
fn asking_what_happens_next_after_a_language_change_continues_normally() {
    let case = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .wrote(Party::Seller, MessageKind::AsksNextStep)
        .with(|c| {
            c.seller.language_changed = true;
            c.facts.seller_not_received = true;
        });

    assert_eq!(
        case.next(),
        seller(&[WHAT_HAPPENS_NEXT, ASK_SELLER_CHECK_ACCOUNT])
    );
}

#[test]
fn a_round_is_counted_only_for_the_party_asked() {
    let case = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::Answers);

    let next = case.next();

    assert!(next.asks(Party::Buyer));
    assert!(!next.asks(Party::Seller));
}

#[test]
fn a_language_resend_is_not_a_round_for_its_party() {
    let case = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::AsksLanguage)
        .with(|c| c.buyer.language_changed = true);

    assert!(!case.next().asks(Party::Buyer));
}

#[test]
fn a_greeting_after_a_language_change_resends_the_last_template() {
    let case = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .wrote(Party::Seller, MessageKind::Greeting)
        .with(|c| c.seller.language_changed = true);

    let next = case.next();

    assert_eq!(next.seller, [ASK_SELLER_RECEIVED]);
    assert!(next.seller_resend);
}

#[test]
fn an_answer_after_a_language_change_continues_normally() {
    // The party answered, so the next question goes out in the new
    // language instead of the answered one again.
    let case = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .wrote(Party::Seller, MessageKind::Answers)
        .with(|c| {
            c.seller.language_changed = true;
            c.facts.seller_not_received = true;
        });

    assert_eq!(case.next(), seller(&[ASK_SELLER_CHECK_ACCOUNT]));
}

#[test]
fn not_understanding_sends_the_simple_variant_of_the_last_question() {
    let case = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .wrote(Party::Seller, MessageKind::NotUnderstood);

    assert_eq!(case.next(), seller(&[ASK_SELLER_RECEIVED_SIMPLE]));
}

#[test]
fn not_understanding_never_repeats_a_simple_variant_already_sent() {
    // A history whose last question is the normal one although its simple
    // variant was already sent: the variant is not sent again.
    let case = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED_SIMPLE])
        .with(|c| {
            c.asked.seller.insert(ASK_SELLER_RECEIVED.to_owned());
            c.seller.last_question = Some(ASK_SELLER_RECEIVED);
        })
        .wrote(Party::Seller, MessageKind::NotUnderstood);

    assert_eq!(case.next(), seller(&[]));
}

#[test]
fn not_understanding_a_question_without_a_simple_variant_continues_normally() {
    let case = Case::new()
        .sent(
            Party::Seller,
            &[ASK_SELLER_RECEIVED, ASK_SELLER_CHECK_ACCOUNT],
        )
        .wrote(Party::Seller, MessageKind::NotUnderstood)
        .with(|c| c.facts.seller_not_received = true);

    assert_eq!(
        case.next(),
        seller(&[WAITING_OTHER_PARTY]),
        "the check question is never repeated; the seller is told the buyer is awaited"
    );
}

// Step 3: a party left without a template is told the other is awaited.

#[test]
fn a_party_with_nothing_left_to_ask_is_told_the_other_is_awaited_once() {
    let case = Case::new()
        .sent(
            Party::Seller,
            &[ASK_SELLER_RECEIVED, ASK_SELLER_CHECK_ACCOUNT],
        )
        .wrote(Party::Seller, MessageKind::Answers)
        .with(|c| c.facts.seller_not_received = true);

    assert_eq!(case.next(), seller(&[WAITING_OTHER_PARTY]));
    assert_eq!(
        case.sent(Party::Seller, &[WAITING_OTHER_PARTY]).next(),
        seller(&[]),
        "said once"
    );
}

#[test]
fn nothing_is_said_about_the_other_party_when_it_owes_nothing() {
    let case = Case::new()
        .sent(
            Party::Seller,
            &[ASK_SELLER_RECEIVED, ASK_SELLER_CHECK_ACCOUNT],
        )
        .sent(Party::Buyer, &[ASK_BUYER_SENT, THANKS_WAITING])
        .wrote(Party::Seller, MessageKind::Answers)
        .with(|c| {
            c.facts.seller_not_received = true;
            // The buyer answered everything and was thanked.
            c.facts.buyer_sent = true;
            c.facts.buyer_has_details = true;
            c.buyer.outstanding = false;
        });

    assert_eq!(case.next(), seller(&[]));
}

#[test]
fn a_fact_unknown_after_both_variants_gets_no_waiting_notice() {
    // Row 11 hands off as `uncertain`; nothing is said about waiting.
    let case = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT, ASK_BUYER_SENT_SIMPLE])
        .wrote(Party::Buyer, MessageKind::Answers);

    assert_eq!(case.next(), buyer(&[]));
}

#[test]
fn a_party_who_gets_a_template_is_not_also_told_to_wait() {
    let thanked = Case::new()
        .wrote(Party::Seller, MessageKind::Answers)
        .with(|c| c.facts.seller_received = true);
    let asked = Case::new()
        .wrote(Party::Seller, MessageKind::Answers)
        .with(|c| c.facts.seller_not_received = true);

    assert_eq!(thanked.next(), seller(&[THANKS_WAITING]));
    assert_eq!(asked.next(), seller(&[ASK_SELLER_CHECK_ACCOUNT]));
}

#[test]
fn asking_what_happens_next_explains_once_then_continues() {
    let first = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT])
        .wrote(Party::Buyer, MessageKind::AsksNextStep);
    let again = Case::new()
        .sent(Party::Buyer, &[ASK_BUYER_SENT, WHAT_HAPPENS_NEXT])
        .wrote(Party::Buyer, MessageKind::AsksNextStep);

    assert_eq!(
        first.next(),
        buyer(&[WHAT_HAPPENS_NEXT, ASK_BUYER_SENT_SIMPLE])
    );
    assert_eq!(again.next(), buyer(&[ASK_BUYER_SENT_SIMPLE]));
}

#[test]
fn a_greeting_after_the_question_gets_its_simple_variant() {
    for kind in [MessageKind::Greeting, MessageKind::Other] {
        let case = Case::new()
            .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
            .wrote(Party::Seller, kind);

        assert_eq!(
            case.next(),
            seller(&[ASK_SELLER_RECEIVED_SIMPLE]),
            "{kind:?}"
        );
    }
}

// Who is written to.

#[test]
fn a_silent_party_with_a_question_outstanding_gets_nothing() {
    let case = Case::new().sent(Party::Seller, &[ASK_SELLER_RECEIVED]);

    assert_eq!(case.next(), NextQuestions::default());
}

#[test]
fn a_silent_party_with_nothing_outstanding_gets_its_needed_question() {
    let case = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .with(|c| {
            c.facts.seller_not_received = true;
            c.seller.outstanding = false;
        });

    assert_eq!(case.next(), seller(&[ASK_SELLER_CHECK_ACCOUNT]));
}

#[test]
fn a_silent_party_is_never_just_thanked() {
    let case = Case::new()
        .sent(Party::Seller, &[ASK_SELLER_RECEIVED])
        .with(|c| {
            c.facts.seller_received = true;
            c.seller.outstanding = false;
        });

    assert_eq!(case.next(), NextQuestions::default());
}

#[test]
fn both_parties_are_handled_in_one_turn() {
    let case = Case::new()
        .wrote(Party::Buyer, MessageKind::Answers)
        .wrote(Party::Seller, MessageKind::Greeting)
        .with(|c| c.facts.buyer_not_sent = true);

    assert_eq!(
        case.next(),
        NextQuestions {
            buyer: vec![THANKS_WAITING],
            seller: vec![ASK_SELLER_RECEIVED],
            ..NextQuestions::default()
        }
    );
}

// Rounds.

#[test]
fn a_turn_with_a_question_counts_as_a_round() {
    assert!(buyer(&[ASK_BUYER_SENT]).counts_as_round());
    assert!(seller(&[WHAT_HAPPENS_NEXT, ASK_SELLER_RECEIVED_SIMPLE]).counts_as_round());
    assert!(!buyer(&[THANKS_WAITING]).counts_as_round());
    assert!(!NextQuestions::default().counts_as_round());
}
