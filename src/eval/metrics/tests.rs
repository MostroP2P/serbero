use super::*;

fn choice(pairs: &[(&str, f64)]) -> Answer {
    Answer::Choice {
        probabilities: pairs.iter().map(|(o, p)| ((*o).to_owned(), *p)).collect(),
    }
}

fn item(question: &str, label: Label, answer: Answer) -> Scored {
    Scored {
        case_id: "c".into(),
        question: question.into(),
        base: base_question(question),
        label,
        answer,
    }
}

fn opt(name: &str) -> Label {
    Label::Option(name.into())
}

/// Seller receipt items: (label, P(says_received)); the rest goes to
/// not_stated.
fn receipts(rows: &[(&str, f64)]) -> Vec<Scored> {
    rows.iter()
        .map(|(label, p)| {
            item(
                "seller_receipt",
                opt(label),
                choice(&[("says_received", *p), ("not_stated", 1.0 - p)]),
            )
        })
        .collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn per_party_questions_share_a_base() {
    assert_eq!(base_question("buyer_language"), "<party>_language");
    assert_eq!(base_question("seller_wants_human"), "<party>_wants_human");
    assert_eq!(base_question("buyer_payment"), "buyer_payment");
    assert_eq!(base_question("seller_receipt"), "seller_receipt");
}

#[test]
fn a_noul_is_scored_at_one_half() {
    let yes = item(
        "fraud_signal",
        Label::Yes(true),
        Answer::Noul { p_yes: 0.7 },
    );
    let no = item(
        "fraud_signal",
        Label::Yes(false),
        Answer::Noul { p_yes: 0.7 },
    );

    assert!(yes.correct());
    assert!(!no.correct());
    assert!(close(no.p(&Label::Yes(false)), 0.3));
    assert!(close(yes.confidence(), 0.7));
}

#[test]
fn precision_counts_only_what_fires() {
    let items = receipts(&[
        ("says_received", 0.95),
        ("says_received", 0.92),
        ("not_stated", 0.91),
        ("says_received", 0.50),
    ]);

    let (precision, fired) =
        precision_at(&items, "seller_receipt", &opt("says_received"), 0.9).unwrap();

    assert_eq!(fired, 3);
    assert!(close(precision, 2.0 / 3.0));
    assert!(precision_at(&items, "seller_receipt", &opt("says_received"), 0.99).is_none());
}

#[test]
fn recall_counts_only_labelled_positives() {
    let items = vec![
        item(
            "buyer_wants_human",
            Label::Yes(true),
            Answer::Noul { p_yes: 0.95 },
        ),
        item(
            "seller_wants_human",
            Label::Yes(true),
            Answer::Noul { p_yes: 0.6 },
        ),
        item(
            "buyer_wants_human",
            Label::Yes(false),
            Answer::Noul { p_yes: 0.99 },
        ),
    ];

    let (recall, positives) =
        recall_at(&items, "<party>_wants_human", &Label::Yes(true), 0.8).unwrap();

    assert_eq!(positives, 2, "both parties' questions count together");
    assert!(close(recall, 0.5));
}

#[test]
fn accuracy_above_and_coverage_use_the_pick() {
    let items = receipts(&[
        ("says_received", 0.95),
        ("not_stated", 0.9),
        ("not_stated", 0.3),
        ("not_stated", 0.5),
    ]);

    let (accuracy, above) = accuracy_above(&items, "seller_receipt", 0.8).unwrap();
    let (covered, total) = coverage(&items, "seller_receipt", 0.8).unwrap();

    // Picks: received 0.95 ✓, received 0.9 ✗, not_stated 0.7 (below), tie
    // at 0.5 goes to not_stated (name order) and is below.
    assert_eq!(above, 2);
    assert!(close(accuracy, 0.5));
    assert_eq!(total, 4);
    assert!(close(covered, 0.5));
}

#[test]
fn ece_is_zero_when_confidence_matches_accuracy() {
    let calibrated = vec![
        item(
            "fraud_signal",
            Label::Yes(true),
            Answer::Noul { p_yes: 1.0 },
        ),
        item(
            "fraud_signal",
            Label::Yes(false),
            Answer::Noul { p_yes: 0.0 },
        ),
    ];
    let overconfident = vec![
        item(
            "fraud_signal",
            Label::Yes(false),
            Answer::Noul { p_yes: 1.0 },
        ),
        item(
            "fraud_signal",
            Label::Yes(true),
            Answer::Noul { p_yes: 1.0 },
        ),
    ];

    assert!(close(ece(&calibrated).unwrap(), 0.0));
    assert!(close(ece(&overconfident).unwrap(), 0.5));
    assert!(ece(&[]).is_none());
}

#[test]
fn every_target_of_the_spec_is_listed() {
    let thresholds = Thresholds {
        guide: 0.9,
        fact: 0.8,
        human_request: 0.8,
        fraud: 0.6,
        conflict: 0.75,
        outside_scope: 0.8,
        validated_languages: Vec::new(),
    };

    let targets = targets(&thresholds);

    assert_eq!(targets.len(), 10);
    assert!(targets.iter().any(|t| t.base == "fraud_signal"
        && t.target == 0.85
        && t.metric
            == Metric::RecallAt {
                positive: Label::Yes(true),
                threshold: 0.6
            }));
    assert!(targets.iter().any(|t| t.base == "seller_receipt"
        && t.target == 0.98
        && t.metric
            == Metric::PrecisionAt {
                positive: opt("says_received"),
                threshold: 0.9
            }));
}

#[test]
fn a_precision_threshold_is_the_lowest_that_meets_the_target() {
    let items = receipts(&[
        ("says_received", 0.97),
        ("not_stated", 0.82),
        ("says_received", 0.9),
    ]);

    let t = recommended_threshold(
        &items,
        "seller_receipt",
        &opt("says_received"),
        Goal::Precision,
        0.98,
    );

    assert!(close(t.unwrap(), 0.85), "{t:?}");
}

#[test]
fn a_recall_threshold_is_the_highest_that_still_meets_the_target() {
    let items = vec![
        item(
            "fraud_signal",
            Label::Yes(true),
            Answer::Noul { p_yes: 0.72 },
        ),
        item(
            "fraud_signal",
            Label::Yes(true),
            Answer::Noul { p_yes: 0.9 },
        ),
    ];

    let t = recommended_threshold(
        &items,
        "fraud_signal",
        &Label::Yes(true),
        Goal::Recall,
        0.85,
    );

    assert!(close(t.unwrap(), 0.7), "{t:?}");
}

#[test]
fn no_threshold_meets_an_impossible_target() {
    let items = receipts(&[("not_stated", 0.99)]);

    assert!(
        recommended_threshold(
            &items,
            "seller_receipt",
            &opt("says_received"),
            Goal::Precision,
            0.98
        )
        .is_none()
    );
}

#[test]
fn a_noul_at_exactly_one_half_counts_as_yes() {
    let tie = item(
        "fraud_signal",
        Label::Yes(true),
        Answer::Noul { p_yes: 0.5 },
    );

    assert_eq!(tie.predicted(), Label::Yes(true));
}

#[test]
fn one_guide_value_satisfies_both_precision_targets() {
    let mut items = receipts(&[("says_received", 0.97), ("not_stated", 0.82)]);
    items.push(item(
        "buyer_payment",
        opt("says_not_sent"),
        choice(&[("says_not_sent", 0.96), ("not_stated", 0.04)]),
    ));
    items.push(item(
        "buyer_payment",
        opt("not_stated"),
        choice(&[("says_not_sent", 0.91), ("not_stated", 0.09)]),
    ));

    let guide = recommended_guide(&items).unwrap();

    // seller_receipt needs 0.85, buyer_payment needs 0.95: the stricter wins.
    assert!(close(guide, 0.95), "{guide}");
}

#[test]
fn the_fact_value_meets_accuracy_and_coverage_for_both_questions() {
    let mut items = receipts(&[
        ("says_received", 0.99),
        ("says_received", 0.95),
        ("not_stated", 0.1),
        ("not_stated", 0.72),
    ]);
    for (label, p) in [
        ("says_sent", 0.99),
        ("says_sent", 0.9),
        ("not_stated", 0.05),
    ] {
        items.push(item(
            "buyer_payment",
            opt(label),
            choice(&[("says_sent", p), ("not_stated", 1.0 - p)]),
        ));
    }

    let fact = recommended_fact(&items).unwrap();

    // The wrong pick sits at 0.72: from 0.75 on it is excluded, accuracy is
    // 1.0 and seller_receipt coverage 3/4 still meets 0.70.
    assert!(close(fact, 0.75), "{fact}");
}
