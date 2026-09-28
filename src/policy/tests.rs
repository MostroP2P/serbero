use super::template::*;
use super::*;
use crate::judge::facts::{MessageKind, PartyFacts};

/// A turn that matches no row but the last: gathering, nothing known, both
/// parties in a validated language, no rounds used, nothing to ask.
struct Case {
    phase: Phase,
    facts: Facts,
    buyer_language: &'static str,
    seller_language: &'static str,
    validated: Vec<String>,
    rounds: u32,
    asked: Asked,
    next: NextQuestions,
}

const MAX_ROUNDS: u32 = 3;

impl Case {
    fn new() -> Self {
        Self {
            phase: Phase::Gathering,
            facts: Facts::default(),
            buyer_language: "es",
            seller_language: "es",
            validated: vec!["en".into(), "es".into()],
            rounds: 0,
            asked: Asked::default(),
            next: NextQuestions::default(),
        }
    }

    fn decide(&self) -> Action {
        decide(&Turn {
            phase: self.phase,
            facts: &self.facts,
            buyer_language: self.buyer_language,
            seller_language: self.seller_language,
            validated_languages: &self.validated,
            rounds: self.rounds,
            max_rounds: MAX_ROUNDS,
            asked: &self.asked,
            next: &self.next,
        })
    }

    fn with(mut self, change: impl FnOnce(&mut Self)) -> Self {
        change(&mut self);
        self
    }
}

fn party(wants_human: bool, rejects_path: bool) -> Option<PartyFacts> {
    Some(PartyFacts {
        wants_human,
        rejects_path,
        language: None,
        message_kind: MessageKind::Answers,
    })
}

fn asked(buyer: &[&str], seller: &[&str]) -> Asked {
    Asked {
        buyer: buyer.iter().map(|t| (*t).to_owned()).collect(),
        seller: seller.iter().map(|t| (*t).to_owned()).collect(),
    }
}

// Setters for each row's condition, so precedence tests can combine them.

fn human(c: &mut Case) {
    c.facts.buyer = party(true, false);
}
fn fraud(c: &mut Case) {
    c.facts.fraud = true;
}
fn outside(c: &mut Case) {
    c.facts.outside_scope = true;
}
fn guiding(c: &mut Case) {
    c.phase = Phase::Guiding;
}
fn rejects(c: &mut Case) {
    c.facts.seller = party(false, true);
}
fn seller_confirms(c: &mut Case) {
    c.facts.seller_received = true;
    c.facts.seller_received_for_guide = true;
}
fn buyer_denies(c: &mut Case) {
    c.facts.buyer_not_sent = true;
    c.facts.buyer_not_sent_for_guide = true;
}
fn conflict_answered(c: &mut Case) {
    c.facts.buyer_sent = true;
    c.facts.seller_not_received = true;
    c.facts.conflict = true;
    c.facts.buyer_has_details = true;
    c.facts.seller_has_checked = true;
}
fn rounds_used(c: &mut Case) {
    c.rounds = MAX_ROUNDS;
}
fn question_pending(c: &mut Case) {
    c.next.buyer = vec![ASK_BUYER_SENT];
}
fn uncertain(c: &mut Case) {
    c.asked = asked(&[ASK_BUYER_SENT, ASK_BUYER_SENT_SIMPLE], &[]);
}
fn both_known(c: &mut Case) {
    c.facts.buyer_sent = true;
    c.facts.seller_not_received = true;
}

fn ask_buyer_sent() -> Action {
    Action::Ask {
        buyer: vec![ASK_BUYER_SENT],
        seller: vec![],
    }
}

// One test per row.

#[test]
fn row_1_a_request_for_a_human_from_either_party_hands_off() {
    let buyer = Case::new().with(human);
    let seller = Case::new().with(|c| c.facts.seller = party(true, false));

    assert_eq!(
        buyer.decide(),
        Action::Handoff(HandoffReason::HumanRequested)
    );
    assert_eq!(
        seller.decide(),
        Action::Handoff(HandoffReason::HumanRequested)
    );
}

#[test]
fn row_2_a_fraud_signal_hands_off() {
    assert_eq!(
        Case::new().with(fraud).decide(),
        Action::Handoff(HandoffReason::FraudSignal)
    );
}

#[test]
fn row_3_a_dispute_outside_scope_hands_off() {
    assert_eq!(
        Case::new().with(outside).decide(),
        Action::Handoff(HandoffReason::OutsideScope)
    );
}

#[test]
fn row_4_rejecting_the_path_while_guiding_hands_off() {
    let seller = Case::new().with(guiding).with(rejects);
    let buyer = Case::new()
        .with(guiding)
        .with(|c| c.facts.buyer = party(false, true));

    assert_eq!(
        seller.decide(),
        Action::Handoff(HandoffReason::SelfResolutionStalled)
    );
    assert_eq!(
        buyer.decide(),
        Action::Handoff(HandoffReason::SelfResolutionStalled)
    );
}

#[test]
fn row_4_does_not_apply_before_guiding() {
    assert_eq!(Case::new().with(rejects).decide(), Action::Wait);
}

#[test]
fn row_5_guiding_waits_for_the_resolution() {
    assert_eq!(Case::new().with(guiding).decide(), Action::Wait);
}

#[test]
fn row_6_the_seller_confirming_the_payment_guides_to_release() {
    assert_eq!(
        Case::new().with(seller_confirms).decide(),
        Action::Guide(Path::PaymentArrived)
    );
}

#[test]
fn row_7_the_buyer_saying_they_did_not_pay_guides_to_cancel() {
    assert_eq!(
        Case::new().with(buyer_denies).decide(),
        Action::Guide(Path::PaymentNotSent)
    );
}

#[test]
fn row_8_contradicting_claims_with_both_sides_answered_hand_off() {
    assert_eq!(
        Case::new().with(conflict_answered).decide(),
        Action::Handoff(HandoffReason::ConflictingClaims)
    );
}

#[test]
fn row_8_counts_details_and_check_as_answered_once_asked() {
    let case = Case::new().with(conflict_answered).with(|c| {
        c.facts.buyer_has_details = false;
        c.facts.seller_has_checked = false;
        c.asked = asked(&[ASK_BUYER_DETAILS], &[ASK_SELLER_CHECK_ACCOUNT]);
    });

    assert_eq!(
        case.decide(),
        Action::Handoff(HandoffReason::ConflictingClaims)
    );
}

#[test]
fn row_8_waits_for_the_details_and_the_check_question() {
    let no_details = Case::new()
        .with(conflict_answered)
        .with(|c| c.facts.buyer_has_details = false)
        .with(|c| c.next.buyer = vec![ASK_BUYER_DETAILS]);
    let no_check = Case::new()
        .with(conflict_answered)
        .with(|c| c.facts.seller_has_checked = false)
        .with(|c| c.next.seller = vec![ASK_SELLER_CHECK_ACCOUNT]);

    assert_eq!(
        no_details.decide(),
        Action::Ask {
            buyer: vec![ASK_BUYER_DETAILS],
            seller: vec![]
        }
    );
    assert_eq!(
        no_check.decide(),
        Action::Ask {
            buyer: vec![],
            seller: vec![ASK_SELLER_CHECK_ACCOUNT]
        }
    );
}

#[test]
fn row_8_needs_a_conflict() {
    let case = Case::new()
        .with(conflict_answered)
        .with(|c| c.facts.conflict = false);

    assert_eq!(case.decide(), Action::Handoff(HandoffReason::FactsGathered));
}

#[test]
fn row_9_the_round_limit_hands_off() {
    assert_eq!(
        Case::new().with(rounds_used).decide(),
        Action::Handoff(HandoffReason::RoundLimit)
    );
}

#[test]
fn row_10_a_pending_question_is_asked() {
    let case = Case::new().with(|c| {
        c.next = NextQuestions {
            buyer: vec![WHAT_HAPPENS_NEXT, ASK_BUYER_SENT],
            seller: vec![THANKS_WAITING],
            ..NextQuestions::default()
        }
    });

    assert_eq!(
        case.decide(),
        Action::Ask {
            buyer: vec![WHAT_HAPPENS_NEXT, ASK_BUYER_SENT],
            seller: vec![THANKS_WAITING],
        }
    );
}

#[test]
fn row_10_needs_a_question_not_only_courtesy_templates() {
    // Both facts known: sending only thanks_waiting would leave the session
    // with no question and no timer; it hands off instead.
    let case = Case::new().with(both_known).with(|c| {
        c.next = NextQuestions {
            buyer: vec![THANKS_WAITING],
            seller: vec![WHAT_HAPPENS_NEXT],
            ..NextQuestions::default()
        }
    });

    assert_eq!(case.decide(), Action::Handoff(HandoffReason::FactsGathered));
}

#[test]
fn row_11_a_fact_unknown_after_both_variants_hands_off() {
    let buyer = Case::new().with(uncertain);
    let seller = Case::new().with(|c| {
        c.asked = asked(&[], &[ASK_SELLER_RECEIVED, ASK_SELLER_RECEIVED_SIMPLE]);
    });

    assert_eq!(buyer.decide(), Action::Handoff(HandoffReason::Uncertain));
    assert_eq!(seller.decide(), Action::Handoff(HandoffReason::Uncertain));
}

#[test]
fn row_11_one_variant_is_not_enough() {
    let case = Case::new().with(|c| c.asked = asked(&[ASK_BUYER_SENT], &[]));

    assert_eq!(case.decide(), Action::Wait);
}

#[test]
fn row_11_does_not_apply_once_the_fact_is_known() {
    let case = Case::new()
        .with(uncertain)
        .with(|c| c.facts.buyer_sent = true);

    assert_eq!(case.decide(), Action::Wait);
}

#[test]
fn row_12_both_payment_facts_known_hand_off() {
    assert_eq!(
        Case::new().with(both_known).decide(),
        Action::Handoff(HandoffReason::FactsGathered)
    );
}

#[test]
fn row_12_needs_both_facts() {
    let buyer_only = Case::new().with(|c| c.facts.buyer_sent = true);

    assert_eq!(buyer_only.decide(), Action::Wait);
}

#[test]
fn row_13_still_sends_courtesy_templates_while_waiting() {
    // The buyer answered everything; the seller still owes an answer.
    let case = Case::new()
        .with(|c| {
            c.facts.buyer_sent = true;
            c.facts.buyer_has_details = true;
        })
        .with(|c| c.next.buyer = vec![THANKS_WAITING]);

    assert_eq!(
        case.decide(),
        Action::Ask {
            buyer: vec![THANKS_WAITING],
            seller: vec![],
        }
    );
}

#[test]
fn row_13_otherwise_waits() {
    assert_eq!(Case::new().decide(), Action::Wait);
}

// Guidance and languages.

#[test]
fn a_buyer_claiming_payment_never_starts_a_path() {
    let case = Case::new().with(|c| {
        c.facts.buyer_sent = true;
        c.facts.buyer_has_details = true;
    });

    assert_eq!(case.decide(), Action::Wait);
}

#[test]
fn guidance_needs_the_for_guide_facts_not_only_the_facts() {
    let seller = Case::new().with(|c| c.facts.seller_received = true);
    let buyer = Case::new().with(|c| c.facts.buyer_not_sent = true);

    assert_ne!(seller.decide(), Action::Guide(Path::PaymentArrived));
    assert_ne!(buyer.decide(), Action::Guide(Path::PaymentNotSent));
}

#[test]
fn a_conversational_language_hands_off_instead_of_guiding() {
    for (buyer, seller) in [("pt", "es"), ("es", "pt"), ("pt", "pt")] {
        for path in [seller_confirms as fn(&mut Case), buyer_denies] {
            let case = Case::new().with(path).with(|c| {
                c.buyer_language = buyer;
                c.seller_language = seller;
            });

            assert_eq!(
                case.decide(),
                Action::Handoff(HandoffReason::FactsGathered),
                "buyer {buyer}, seller {seller}"
            );
        }
    }
}

#[test]
fn no_language_is_validated_before_calibration() {
    let case = Case::new()
        .with(seller_confirms)
        .with(|c| c.validated.clear());

    assert_eq!(case.decide(), Action::Handoff(HandoffReason::FactsGathered));
}

// Precedence: each row wins over the next one when both apply.

#[test]
fn rows_are_checked_in_order() {
    type Setter = fn(&mut Case);
    let rows: [(&str, Setter, Action); 12] = [
        ("1", human, Action::Handoff(HandoffReason::HumanRequested)),
        ("2", fraud, Action::Handoff(HandoffReason::FraudSignal)),
        ("3", outside, Action::Handoff(HandoffReason::OutsideScope)),
        (
            "4",
            |c| {
                guiding(c);
                rejects(c)
            },
            Action::Handoff(HandoffReason::SelfResolutionStalled),
        ),
        ("5", guiding, Action::Wait),
        ("6", seller_confirms, Action::Guide(Path::PaymentArrived)),
        ("7", buyer_denies, Action::Guide(Path::PaymentNotSent)),
        (
            "8",
            conflict_answered,
            Action::Handoff(HandoffReason::ConflictingClaims),
        ),
        ("9", rounds_used, Action::Handoff(HandoffReason::RoundLimit)),
        ("10", question_pending, ask_buyer_sent()),
        ("11", uncertain, Action::Handoff(HandoffReason::Uncertain)),
        (
            "12",
            both_known,
            Action::Handoff(HandoffReason::FactsGathered),
        ),
    ];

    for (i, (row, set, expected)) in rows.iter().enumerate() {
        for (later, later_set, _) in &rows[i + 1..] {
            // Row 11 needs a payment fact unknown and row 12 needs both
            // known: they never apply together.
            if (*row, *later) == ("11", "12") {
                continue;
            }
            let case = Case::new().with(*later_set).with(*set);

            assert_eq!(&case.decide(), expected, "row {row} over row {later}");
        }
    }
}

#[test]
fn handoff_reasons_use_the_spec_names() {
    let spec = include_str!("../../docs/spec.md");
    for reason in [
        HandoffReason::SelfResolutionStalled,
        HandoffReason::FactsGathered,
        HandoffReason::ConflictingClaims,
        HandoffReason::FraudSignal,
        HandoffReason::HumanRequested,
        HandoffReason::OutsideScope,
        HandoffReason::Unresponsive,
        HandoffReason::RoundLimit,
        HandoffReason::Uncertain,
        HandoffReason::JudgeUnavailable,
        HandoffReason::Flood,
        HandoffReason::OpeningFailed,
    ] {
        assert!(
            spec.contains(&format!("| `{}` |", reason.as_str())),
            "{} is not in spec.md §7.6",
            reason.as_str()
        );
    }
}

#[test]
fn actions_serialize_with_the_spec_names() {
    let ask = Action::Ask {
        buyer: vec![ASK_BUYER_SENT],
        seller: vec![],
    };

    assert_eq!(
        serde_json::to_value(&ask).unwrap(),
        serde_json::json!({ "ask": { "buyer": ["ask_buyer_sent"], "seller": [] } })
    );
    assert_eq!(
        serde_json::to_value(Action::Guide(Path::PaymentArrived)).unwrap(),
        serde_json::json!({ "guide": "payment_arrived" })
    );
    assert_eq!(
        serde_json::to_value(Action::Handoff(HandoffReason::FraudSignal)).unwrap(),
        serde_json::json!({ "handoff": "fraud_signal" })
    );
    assert_eq!(
        serde_json::to_value(Action::Wait).unwrap(),
        serde_json::json!("wait")
    );
    for reason in [
        HandoffReason::SelfResolutionStalled,
        HandoffReason::OpeningFailed,
    ] {
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            serde_json::json!(reason.as_str())
        );
    }
}

#[test]
fn every_handoff_reason_parses_back_from_its_name() {
    for reason in HandoffReason::ALL {
        assert_eq!(HandoffReason::parse(reason.as_str()), Some(reason));
    }
    assert_eq!(HandoffReason::parse("nope"), None);
}
