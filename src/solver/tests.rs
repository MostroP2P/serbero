#![allow(clippy::unwrap_used)] // test helpers

use serde_json::json;

use super::*;
use crate::judge::Answer;
use crate::judge::brief::EvidenceBalance;
use crate::judge::facts::{MessageKind, PartyFacts};

const NPUB: &str = "npub1sg6plzptd64u62a878hep2kev88swjh3tw00gjsfl8f237lmu63q0uf63m";

fn choice(pairs: &[(&str, f64)]) -> Answer {
    Answer::Choice {
        probabilities: pairs.iter().map(|(o, p)| ((*o).to_owned(), *p)).collect(),
    }
}

fn noul(p_yes: f64) -> Answer {
    Answer::Noul { p_yes }
}

/// The example of `docs/messages.md` §3: the seller wrote m3, the buyer m4
/// with one attachment.
fn state() -> Value {
    json!({
        "transcript": [
            { "id": "m1", "from": "serbero", "to": "buyer", "text": "…" },
            { "id": "m2", "from": "serbero", "to": "seller", "text": "…" },
            { "id": "m3", "from": "seller", "text": "revisé el home banking y no hay ningún ingreso en pesos" },
            { "id": "m4", "from": "buyer", "text": "ya envié el pago a las 14:10 desde mi cuenta de Mercado Pago, ref 8841", "attachments": 1 },
        ]
    })
}

fn answers() -> Answers {
    [
        (
            "buyer_payment",
            choice(&[
                ("says_sent", 0.96),
                ("says_not_sent", 0.01),
                ("not_stated", 0.03),
            ]),
        ),
        ("buyer_details", noul(0.88)),
        (
            "seller_receipt",
            choice(&[
                ("says_received", 0.02),
                ("says_received_with_problem", 0.0),
                ("says_not_received", 0.93),
                ("not_stated", 0.05),
            ]),
        ),
        ("seller_checked", noul(0.90)),
        ("claims_conflict", noul(0.87)),
        ("fraud_signal", noul(0.08)),
        (
            "dispute_topic",
            choice(&[("payment_not_confirmed", 0.91), ("other", 0.09)]),
        ),
        (
            "seller_language",
            choice(&[("en", 0.05), ("es", 0.05), ("other", 0.9)]),
        ),
    ]
    .into_iter()
    .map(|(id, a)| (id.to_owned(), a))
    .collect()
}

fn facts() -> Facts {
    let party = Some(PartyFacts {
        wants_human: false,
        rejects_path: false,
        language: None,
        message_kind: MessageKind::Answers,
    });
    Facts {
        buyer: party.clone(),
        seller: party,
        ..Facts::default()
    }
}

fn the_brief() -> Brief {
    Brief {
        quote_buyer_payment: Some("m4".into()),
        quote_buyer_details: Some("m4".into()),
        quote_seller_receipt: Some("m3".into()),
        quote_concern: None,
        evidence_balance: Some(EvidenceBalance {
            value: 0.76,
            distribution: vec![0.02, 0.05, 0.12, 0.49, 0.32],
        }),
    }
}

fn input<'a>(reading: Option<Reading<'a>>) -> BriefInput<'a> {
    BriefInput {
        dispute_id: "d1",
        subject: Subject::Handoff(HandoffReason::ConflictingClaims),
        rounds: 2,
        duration_secs: 14 * 60 + 30,
        order: Order {
            amount: Some("50000"),
            currency: Some("ARS"),
            payment_method: Some("Mercado Pago"),
            created_before_dispute_secs: Some(3 * 3600 + 20 * 60),
        },
        buyer_language: "es",
        seller_language: "en",
        reading,
        transcript_messages: 18,
    }
}

#[test]
fn the_brief_follows_the_spec_layout() {
    let (state, answers, facts, brief) = (state(), answers(), facts(), the_brief());
    let reading = Reading {
        state: &state,
        answers: &answers,
        facts: &facts,
        brief: &brief,
    };

    let text = super::brief(&input(Some(reading)));

    assert_eq!(
        text,
        "Dispute d1 · handed off: conflicting_claims\n\
         Topic: payment_not_confirmed (0.91) · rounds: 2 · duration: 14 min\n\
         Order: 50000 ARS via Mercado Pago · created 3 h 20 min before the dispute\n\
         Languages: buyer es · seller other (not enabled; addressed in en)\n\
         \n\
         Buyer — says sent (0.96), details given (0.88)\n\
         \x20 \"ya envié el pago a las 14:10 desde mi cuenta de Mercado Pago, ref 8841\"  [1 attachment]\n\
         Seller — says not received (0.93), checked account (0.90)\n\
         \x20 \"revisé el home banking y no hay ningún ingreso en pesos\"\n\
         \n\
         Signals: conflict 0.87 · fraud 0.08 · human requested: no\n\
         \n\
         Reading of the conversation (advisory, not a verdict):\n\
         \x20 evidence that fiat was sent: 3.0 / 4  [0:0.02 1:0.05 2:0.12 3:0.49 4:0.32]\n\
         \n\
         Transcript (18 messages) follows in the next message."
    );
}

#[test]
fn a_guidance_brief_names_the_path() {
    let (state, answers, facts, brief) = (state(), answers(), facts(), the_brief());
    let reading = Reading {
        state: &state,
        answers: &answers,
        facts: &facts,
        brief: &brief,
    };
    let mut guide = input(Some(reading));
    guide.subject = Subject::Guide(Path::PaymentNotSent);

    let text = super::brief(&guide);

    assert!(
        text.starts_with("Dispute d1 · guidance sent: payment_not_sent\n"),
        "{text}"
    );
}

#[test]
fn the_concern_quote_is_shown_first_to_read() {
    let (state, answers, facts) = (state(), answers(), facts());
    let brief = Brief {
        quote_concern: Some("m3".into()),
        ..the_brief()
    };
    let reading = Reading {
        state: &state,
        answers: &answers,
        facts: &facts,
        brief: &brief,
    };

    let text = super::brief(&input(Some(reading)));

    assert!(
        text.contains(
            "\nFirst to read: \"revisé el home banking y no hay ningún ingreso en pesos\"\n"
        ),
        "{text}"
    );
}

#[test]
fn a_request_for_a_human_is_a_signal() {
    let (state, answers, brief) = (state(), answers(), the_brief());
    let mut facts = facts();
    facts.seller.as_mut().unwrap().wants_human = true;
    let reading = Reading {
        state: &state,
        answers: &answers,
        facts: &facts,
        brief: &brief,
    };

    let text = super::brief(&input(Some(reading)));

    assert!(text.contains("human requested: yes"), "{text}");
}

#[test]
fn without_a_reading_the_facts_are_replaced_by_a_notice() {
    let mut unavailable = input(None);
    unavailable.subject = Subject::Handoff(HandoffReason::JudgeUnavailable);

    let text = super::brief(&unavailable);

    assert_eq!(
        text,
        "Dispute d1 · handed off: judge_unavailable\n\
         Rounds: 2 · duration: 14 min\n\
         Order: 50000 ARS via Mercado Pago · created 3 h 20 min before the dispute\n\
         Languages: buyer es · seller en\n\
         \n\
         Automated reading unavailable\n\
         \n\
         Transcript (18 messages) follows in the next message."
    );
}

#[test]
fn unknown_order_facts_are_left_out() {
    let mut unknown = input(None);
    unknown.order = Order::default();
    let mut method_only = input(None);
    method_only.order = Order {
        payment_method: Some("SEPA"),
        ..Order::default()
    };

    assert!(!super::brief(&unknown).contains("Order:"));
    assert!(super::brief(&method_only).contains("\nOrder: via SEPA\n"));
}

#[test]
fn a_quote_that_is_not_in_the_state_is_not_shown() {
    let (state, answers, facts) = (state(), answers(), facts());
    let brief = Brief {
        quote_seller_receipt: Some("m99".into()),
        ..the_brief()
    };
    let reading = Reading {
        state: &state,
        answers: &answers,
        facts: &facts,
        brief: &brief,
    };

    let text = super::brief(&input(Some(reading)));

    assert!(!text.contains("revisé"), "{text}");
}

#[test]
fn quoted_text_never_carries_a_nostr_identifier() {
    let state = json!({ "transcript": [
        { "id": "m1", "from": "buyer", "text": format!("mi clave es {NPUB} ya pagué") },
    ]});
    let (answers, facts) = (answers(), facts());
    let brief = Brief {
        quote_buyer_payment: Some("m1".into()),
        ..Brief::default()
    };
    let reading = Reading {
        state: &state,
        answers: &answers,
        facts: &facts,
        brief: &brief,
    };

    let text = super::brief(&input(Some(reading)));

    assert!(!text.contains(NPUB), "{text}");
    assert!(
        text.contains("\"mi clave es [redacted] ya pagué\""),
        "{text}"
    );
}

fn line(at: i64, speaker: Speaker, text: &str) -> Line<'_> {
    Line {
        at,
        speaker,
        text,
        attachments: 0,
    }
}

/// 14:52 UTC on a day.
const AT_1452: i64 = 1_700_000_000 - 1_700_000_000 % 86_400 + 14 * 3600 + 52 * 60;

#[test]
fn the_transcript_has_one_line_per_message() {
    let lines = [
        line(AT_1452, Speaker::Serbero(Party::Seller), "¿Te llegó?"),
        Line {
            attachments: 2,
            ..line(AT_1452 + 60, Speaker::Party(Party::Seller), "no")
        },
        line(AT_1452 + 120, Speaker::Party(Party::Buyer), "sí\npagué"),
    ];

    let dms = transcript("d1", &lines);

    assert_eq!(
        dms,
        ["Dispute d1 · transcript (3 messages, times UTC)\n\
          [14:52] serbero → seller: ¿Te llegó?\n\
          [14:53] seller: no  [2 attachments]\n\
          [14:54] buyer: sí\n  pagué"]
    );
}

#[test]
fn the_transcript_never_carries_a_nostr_identifier() {
    let text = format!("mira {NPUB}");
    let lines = [line(AT_1452, Speaker::Party(Party::Buyer), &text)];

    let dms = transcript("d1", &lines);

    assert!(!dms[0].contains(NPUB));
    assert!(dms[0].contains("buyer: mira [redacted]"));
}

#[test]
fn a_long_transcript_is_split_into_numbered_parts() {
    let long = "x".repeat(2000);
    let lines: Vec<Line<'_>> = (0..40)
        .map(|i| line(AT_1452 + i, Speaker::Party(Party::Buyer), &long))
        .collect();

    let dms = transcript("d1", &lines);

    assert!(dms.len() > 1);
    for (i, dm) in dms.iter().enumerate() {
        assert!(dm.chars().count() <= MAX_DM_CHARS, "part {i} is too long");
        assert!(
            dm.starts_with(&format!(
                "Dispute d1 · transcript (40 messages, times UTC, part {}/{})\n",
                i + 1,
                dms.len()
            )),
            "{}",
            dm.lines().next().unwrap()
        );
    }
    let total: usize = dms.iter().map(|dm| dm.matches("] buyer: ").count()).sum();
    assert_eq!(total, 40, "no message is lost or repeated");
}

#[test]
fn an_update_lists_the_new_messages() {
    let lines = [
        line(
            AT_1452,
            Speaker::Party(Party::Seller),
            "ya me llegó, estaba pendiente",
        ),
        line(
            AT_1452 + 60,
            Speaker::Party(Party::Buyer),
            "perfecto gracias",
        ),
    ];

    assert_eq!(
        update("d1", &lines),
        ["Dispute d1 · new messages since handoff (2)\n\
          [14:52] seller: ya me llegó, estaba pendiente\n\
          [14:53] buyer: perfecto gracias"]
    );
}

#[test]
fn the_mediation_notice_matches_the_spec() {
    assert_eq!(
        mediation_started("d1"),
        "Serbero is mediating dispute d1.\n\
         You can take it over at any time; Serbero stops as soon as you do."
    );
}

#[test]
fn the_final_report_matches_the_spec() {
    let handed_off = final_report(
        "d1",
        "settled",
        Outcome::HandedOff(HandoffReason::ConflictingClaims),
        2,
        41 * 60,
    );
    let self_resolved = final_report("d1", "released", Outcome::SelfResolved, 1, 3 * 3600);
    let not_mediated = final_report("d1", "seller-refunded", Outcome::NotMediated, 0, 0);
    let superseded = final_report("d1", "settled", Outcome::Superseded, 1, 600);

    assert_eq!(
        handed_off,
        "Dispute d1 resolved: settled\n\
         mediation: yes · outcome: handed_off (conflicting_claims) · rounds: 2 · duration: 41 min"
    );
    assert_eq!(
        self_resolved,
        "Dispute d1 resolved: released\n\
         mediation: yes · outcome: self_resolved · rounds: 1 · duration: 3 h 0 min"
    );
    assert_eq!(
        not_mediated,
        "Dispute d1 resolved: seller-refunded\nmediation: no"
    );
    assert_eq!(
        superseded,
        "Dispute d1 resolved: settled\n\
         mediation: yes · outcome: superseded · rounds: 1 · duration: 10 min"
    );
}
