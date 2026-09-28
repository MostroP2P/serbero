//! Scoring a judge against labels (`docs/evaluation.md` §2–§3).

use crate::config::Thresholds;
use crate::judge::{Answer, Answers};

use super::case::{Case, Label};

/// One labelled question of one case, with the judge's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub case_id: String,
    /// The question id as asked, e.g. `buyer_language`.
    pub question: String,
    /// The id with the party replaced by `<party>` for per-party questions.
    pub base: String,
    pub label: Label,
    pub answer: Answer,
}

impl Scored {
    /// The judge's own pick: the winning option, or yes at 0.5.
    pub fn predicted(&self) -> Label {
        match &self.answer {
            Answer::Noul { p_yes } => Label::Yes(*p_yes >= 0.5),
            answer => Label::Option(answer.winner().unwrap_or_default().to_owned()),
        }
    }

    pub fn correct(&self) -> bool {
        self.predicted() == self.label
    }

    /// The probability the judge gives to `label`: an option's share, or
    /// `p_yes` / `1 − p_yes`.
    pub fn p(&self, label: &Label) -> f64 {
        match (&self.answer, label) {
            (Answer::Noul { p_yes }, Label::Yes(yes)) => {
                if *yes {
                    *p_yes
                } else {
                    1.0 - p_yes
                }
            }
            (answer, Label::Option(option)) => answer.share(option),
            _ => 0.0,
        }
    }

    /// The probability of the judge's own pick.
    pub fn confidence(&self) -> f64 {
        self.p(&self.predicted())
    }
}

/// Per-party questions scored together, whichever party they asked about.
pub fn base_question(id: &str) -> String {
    for party in ["buyer_", "seller_"] {
        if let Some(rest) = id.strip_prefix(party)
            && ["message_kind", "language", "wants_human"].contains(&rest)
        {
            return format!("<party>_{rest}");
        }
    }
    id.to_owned()
}

/// Every labelled question of a case that has an answer.
pub fn score(case: &Case, answers: &Answers) -> Vec<Scored> {
    case.expect
        .iter()
        .filter_map(|(question, label)| {
            Some(Scored {
                case_id: case.id.clone(),
                question: question.clone(),
                base: base_question(question),
                label: label.clone(),
                answer: answers.get(question)?.clone(),
            })
        })
        .collect()
}

fn of<'a>(items: &'a [Scored], base: &'a str) -> impl Iterator<Item = &'a Scored> {
    items.iter().filter(move |item| item.base == base)
}

/// A rate and how many items it is computed over; `None` without items.
pub type Rate = Option<(f64, usize)>;

fn rate(hits: usize, total: usize) -> Rate {
    (total > 0).then(|| (hits as f64 / total as f64, total))
}

/// Of the items where `positive` reaches `threshold`, the share labelled
/// `positive`.
pub fn precision_at(items: &[Scored], base: &str, positive: &Label, threshold: f64) -> Rate {
    let fired: Vec<&Scored> = of(items, base)
        .filter(|i| i.p(positive) >= threshold)
        .collect();
    rate(
        fired.iter().filter(|i| &i.label == positive).count(),
        fired.len(),
    )
}

/// Of the items labelled `positive`, the share where it reaches `threshold`.
pub fn recall_at(items: &[Scored], base: &str, positive: &Label, threshold: f64) -> Rate {
    let labelled: Vec<&Scored> = of(items, base).filter(|i| &i.label == positive).collect();
    rate(
        labelled
            .iter()
            .filter(|i| i.p(positive) >= threshold)
            .count(),
        labelled.len(),
    )
}

/// Of the items whose pick reaches `threshold`, the share that is right.
pub fn accuracy_above(items: &[Scored], base: &str, threshold: f64) -> Rate {
    let above: Vec<&Scored> = of(items, base)
        .filter(|i| i.confidence() >= threshold)
        .collect();
    rate(above.iter().filter(|i| i.correct()).count(), above.len())
}

/// The share of items whose pick reaches `threshold`.
pub fn coverage(items: &[Scored], base: &str, threshold: f64) -> Rate {
    let all: Vec<&Scored> = of(items, base).collect();
    rate(
        all.iter().filter(|i| i.confidence() >= threshold).count(),
        all.len(),
    )
}

pub fn accuracy(items: &[Scored], base: &str) -> Rate {
    let all: Vec<&Scored> = of(items, base).collect();
    rate(all.iter().filter(|i| i.correct()).count(), all.len())
}

/// Bins for the expected calibration error.
pub const ECE_BINS: usize = 10;

/// Expected calibration error over every item: the size-weighted gap
/// between confidence and accuracy in each of `ECE_BINS` equal bins.
pub fn ece(items: &[Scored]) -> Option<f64> {
    if items.is_empty() {
        return None;
    }
    let mut bins = vec![(0usize, 0.0f64, 0usize); ECE_BINS];
    for item in items {
        let confidence = item.confidence().clamp(0.0, 1.0);
        let bin = ((confidence * ECE_BINS as f64) as usize).min(ECE_BINS - 1);
        bins[bin].0 += 1;
        bins[bin].1 += confidence;
        bins[bin].2 += usize::from(item.correct());
    }
    let total = items.len() as f64;
    Some(
        bins.iter()
            .filter(|(n, _, _)| *n > 0)
            .map(|(n, confidence, correct)| {
                let n = *n as f64;
                (n / total) * (*correct as f64 / n - confidence / n).abs()
            })
            .sum(),
    )
}

/// How a target is measured.
#[derive(Debug, Clone, PartialEq)]
pub enum Metric {
    PrecisionAt { positive: Label, threshold: f64 },
    RecallAt { positive: Label, threshold: f64 },
    AccuracyAbove { threshold: f64 },
    Coverage { threshold: f64 },
    Accuracy,
}

/// One row of the targets table.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub base: &'static str,
    pub metric: Metric,
    pub target: f64,
}

impl Target {
    pub fn measure(&self, items: &[Scored]) -> Rate {
        match &self.metric {
            Metric::PrecisionAt {
                positive,
                threshold,
            } => precision_at(items, self.base, positive, *threshold),
            Metric::RecallAt {
                positive,
                threshold,
            } => recall_at(items, self.base, positive, *threshold),
            Metric::AccuracyAbove { threshold } => accuracy_above(items, self.base, *threshold),
            Metric::Coverage { threshold } => coverage(items, self.base, *threshold),
            Metric::Accuracy => accuracy(items, self.base),
        }
    }
}

fn option(name: &str) -> Label {
    Label::Option(name.to_owned())
}

/// The targets that validate a language (`docs/evaluation.md` §2).
pub fn targets(thresholds: &Thresholds) -> Vec<Target> {
    let mut targets = vec![
        Target {
            base: "seller_receipt",
            metric: Metric::PrecisionAt {
                positive: option("says_received"),
                threshold: thresholds.guide,
            },
            target: 0.98,
        },
        Target {
            base: "buyer_payment",
            metric: Metric::PrecisionAt {
                positive: option("says_not_sent"),
                threshold: thresholds.guide,
            },
            target: 0.98,
        },
    ];
    for base in ["buyer_payment", "seller_receipt"] {
        targets.push(Target {
            base,
            metric: Metric::AccuracyAbove {
                threshold: thresholds.fact,
            },
            target: 0.95,
        });
        targets.push(Target {
            base,
            metric: Metric::Coverage {
                threshold: thresholds.fact,
            },
            target: 0.70,
        });
    }
    targets.extend([
        Target {
            base: "<party>_wants_human",
            metric: Metric::RecallAt {
                positive: Label::Yes(true),
                threshold: thresholds.human_request,
            },
            target: 0.90,
        },
        Target {
            base: "fraud_signal",
            metric: Metric::RecallAt {
                positive: Label::Yes(true),
                threshold: thresholds.fraud,
            },
            target: 0.85,
        },
        Target {
            base: "<party>_language",
            metric: Metric::Accuracy,
            target: 0.95,
        },
        Target {
            base: "<party>_message_kind",
            metric: Metric::Accuracy,
            target: 0.85,
        },
    ]);
    targets
}

/// Threshold values the sweep tries: 0.50 to 0.95 in steps of 0.05.
pub fn sweep_values() -> impl Iterator<Item = f64> {
    (10..=19).map(|step| step as f64 * 0.05)
}

/// Which rate a threshold is chosen for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    Precision,
    Recall,
}

/// The threshold `docs/evaluation.md` §3 recommends from the sweep: for a
/// precision target the lowest value that meets it (precision rises with
/// the threshold, coverage falls); for a recall target the highest value
/// that still meets it (recall falls with the threshold, false positives
/// fall too). `None` when no swept value meets the target.
pub fn recommended_threshold(
    items: &[Scored],
    base: &str,
    positive: &Label,
    goal: Goal,
    target: f64,
) -> Option<f64> {
    let meets = |t: f64| {
        let measured = match goal {
            Goal::Precision => precision_at(items, base, positive, t),
            Goal::Recall => recall_at(items, base, positive, t),
        };
        measured.is_some_and(|(value, _)| value >= target)
    };
    match goal {
        Goal::Precision => sweep_values().find(|&t| meets(t)),
        Goal::Recall => sweep_values().filter(|&t| meets(t)).last(),
    }
}

#[cfg(test)]
mod tests;
