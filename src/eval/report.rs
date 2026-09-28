//! Running cases against a judge and writing the report
//! (`docs/evaluation.md` §2–§3).

use std::time::Instant;

use futures_util::stream::{self, StreamExt, TryStreamExt};

use crate::config::Thresholds;
use crate::judge::providers::recorded::{RecordedCase, Recording};
use crate::judge::questions::TurnQuestions;
use crate::judge::{Judge, JudgeError, Judged};

use super::case::{Case, Label};
use super::metrics::{self, Goal, Metric, Scored};

/// Requests in flight at once.
const CONCURRENCY: usize = 4;

/// One case with the judge's answers.
#[derive(Debug, Clone)]
pub struct CaseRun {
    pub case: Case,
    pub judged: Judged,
    pub latency_ms: u64,
}

/// Sends every case to `judge`, in order, a few at a time.
pub async fn run(
    judge: &dyn Judge,
    turn: &TurnQuestions,
    cases: Vec<Case>,
) -> Result<Vec<CaseRun>, JudgeError> {
    stream::iter(cases)
        .map(|case| async move {
            let questions = case.questions(turn);
            let started = Instant::now();
            let judged = judge.evaluate(&case.state, &questions).await?;
            let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            Ok(CaseRun {
                case,
                judged,
                latency_ms,
            })
        })
        .buffered(CONCURRENCY)
        .try_collect()
        .await
}

/// The answers as a recording `RecordedJudge` can replay
/// (`docs/evaluation.md` §4).
pub fn recording(judge_id: &str, question_set: &str, runs: &[CaseRun]) -> Recording {
    Recording {
        judge: judge_id.to_owned(),
        question_set: question_set.to_owned(),
        cases: runs
            .iter()
            .map(|run| RecordedCase {
                case_id: run.case.id.clone(),
                state: run.case.state.clone(),
                answers: run.judged.answers.clone(),
            })
            .collect(),
    }
}

/// What a report is about.
#[derive(Debug, Clone, Copy)]
pub struct Header<'a> {
    pub judge_id: &'a str,
    pub question_set: &'a str,
    pub lang: &'a str,
    pub thresholds: &'a Thresholds,
    /// Where the thresholds come from, e.g. `config` or `defaults`.
    pub thresholds_source: &'a str,
}

/// The Markdown report.
pub fn render(header: &Header<'_>, runs: &[CaseRun]) -> String {
    let items: Vec<Scored> = runs
        .iter()
        .flat_map(|run| metrics::score(&run.case, &run.judged.answers))
        .collect();
    let mut out = vec![
        format!("# Evaluation: {} · {}", header.lang, header.judge_id),
        String::new(),
        format!(
            "Question set `{}`, {} cases, {} labelled answers. Thresholds from {}: {}.",
            header.question_set,
            runs.len(),
            items.len(),
            header.thresholds_source,
            thresholds_line(header.thresholds)
        ),
    ];
    out.extend(targets_section(header.thresholds, &items));
    out.extend(recommended_section(&items));
    out.extend(accuracy_section(&items));
    out.extend(usage_section(runs));
    out.extend(misses_section(&items));
    out.join("\n") + "\n"
}

fn thresholds_line(t: &Thresholds) -> String {
    format!(
        "guide {}, fact {}, human_request {}, fraud {}, conflict {}, outside_scope {}",
        t.guide, t.fact, t.human_request, t.fraud, t.conflict, t.outside_scope
    )
}

fn targets_section(thresholds: &Thresholds, items: &[Scored]) -> Vec<String> {
    let mut out = vec![
        String::new(),
        "## Targets (evaluation.md §2)".into(),
        String::new(),
        "| Question | Metric | Value | Over | Target | Met |".into(),
        "|---|---|---:|---:|---:|---|".into(),
    ];
    let mut met = 0;
    let targets = metrics::targets(thresholds);
    for target in &targets {
        let measured = target.measure(items);
        let ok = measured.is_some_and(|(value, _)| value >= target.target);
        met += usize::from(ok);
        let (value, over) = measured.map_or(("—".into(), "0".into()), |(v, n)| {
            (format!("{v:.3}"), n.to_string())
        });
        out.push(format!(
            "| `{}` | {} | {value} | {over} | ≥ {} | {} |",
            target.base,
            metric_name(&target.metric),
            target.target,
            if ok { "yes" } else { "**no**" }
        ));
    }
    out.push(String::new());
    out.push(format!(
        "{met} of {} targets met. A target without items is not met.",
        targets.len()
    ));
    out
}

fn metric_name(metric: &Metric) -> String {
    let label = |l: &Label| match l {
        Label::Option(o) => format!("`{o}`"),
        Label::Yes(y) => format!("`{y}`"),
    };
    match metric {
        Metric::PrecisionAt {
            positive,
            threshold,
        } => {
            format!("precision of {} at {threshold}", label(positive))
        }
        Metric::RecallAt {
            positive,
            threshold,
        } => {
            format!("recall of {} at {threshold}", label(positive))
        }
        Metric::AccuracyAbove { threshold } => format!("accuracy above {threshold}"),
        Metric::Coverage { threshold } => format!("coverage at {threshold}"),
        Metric::Accuracy => "accuracy".into(),
    }
}

/// The thresholds §3 would choose from this run.
fn recommended_section(items: &[Scored]) -> Vec<String> {
    let rows: [(&str, &str, Label, Goal, f64); 4] = [
        (
            "guide",
            "seller_receipt",
            Label::Option("says_received".into()),
            Goal::Precision,
            0.98,
        ),
        (
            "guide",
            "buyer_payment",
            Label::Option("says_not_sent".into()),
            Goal::Precision,
            0.98,
        ),
        (
            "human_request",
            "<party>_wants_human",
            Label::Yes(true),
            Goal::Recall,
            0.90,
        ),
        (
            "fraud",
            "fraud_signal",
            Label::Yes(true),
            Goal::Recall,
            0.85,
        ),
    ];
    let mut out = vec![
        String::new(),
        "## Recommended thresholds (evaluation.md §3)".into(),
        String::new(),
        "| Threshold | From | Rule | Value |".into(),
        "|---|---|---|---:|".into(),
    ];
    for (name, base, positive, goal, target) in rows {
        let rule = match goal {
            Goal::Precision => format!("lowest with precision ≥ {target}"),
            Goal::Recall => format!("highest with recall ≥ {target}"),
        };
        let value = metrics::recommended_threshold(items, base, &positive, goal, target)
            .map_or("none".into(), |t| format!("{t:.2}"));
        out.push(format!("| `{name}` | `{base}` | {rule} | {value} |"));
    }
    out.push(String::new());
    out.push("Values are swept from 0.50 to 0.95 in steps of 0.05.".into());
    out
}

fn accuracy_section(items: &[Scored]) -> Vec<String> {
    let mut bases: Vec<&str> = items.iter().map(|i| i.base.as_str()).collect();
    bases.sort_unstable();
    bases.dedup();
    let mut out = vec![
        String::new(),
        "## Accuracy per question".into(),
        String::new(),
        "| Question | Labels | Accuracy |".into(),
        "|---|---:|---:|".into(),
    ];
    for base in bases {
        if let Some((accuracy, n)) = metrics::accuracy(items, base) {
            out.push(format!("| `{base}` | {n} | {accuracy:.3} |"));
        }
    }
    out.push(String::new());
    out.push(match metrics::ece(items) {
        Some(ece) => format!(
            "Expected calibration error ({} bins, all answers): {ece:.3}.",
            metrics::ECE_BINS
        ),
        None => "Expected calibration error: no labelled answers.".into(),
    });
    out
}

fn usage_section(runs: &[CaseRun]) -> Vec<String> {
    let mut latencies: Vec<u64> = runs.iter().map(|r| r.latency_ms).collect();
    latencies.sort_unstable();
    let percentile = |q: f64| {
        let index = ((latencies.len() as f64 - 1.0) * q).round() as usize;
        latencies.get(index).copied().unwrap_or(0)
    };
    let tokens: Vec<u32> = runs.iter().filter_map(|r| r.judged.input_tokens).collect();
    let tokens = if tokens.is_empty() {
        "not reported".to_owned()
    } else {
        let mean = tokens.iter().map(|&t| f64::from(t)).sum::<f64>() / tokens.len() as f64;
        format!("mean {mean:.0}")
    };
    vec![
        String::new(),
        format!(
            "Latency: median {} ms, p95 {} ms. Input tokens: {tokens}.",
            percentile(0.5),
            percentile(0.95)
        ),
    ]
}

fn misses_section(items: &[Scored]) -> Vec<String> {
    let misses: Vec<&Scored> = items.iter().filter(|i| !i.correct()).collect();
    let mut out = vec![String::new(), format!("## Misses ({})", misses.len())];
    if misses.is_empty() {
        return out;
    }
    out.extend([
        String::new(),
        "| Case | Question | Label | Answer | P(label) |".into(),
        "|---|---|---|---|---:|".into(),
    ]);
    let show = |l: &Label| match l {
        Label::Option(o) => o.clone(),
        Label::Yes(y) => y.to_string(),
    };
    for miss in misses {
        out.push(format!(
            "| `{}` | `{}` | `{}` | `{}` ({:.2}) | {:.2} |",
            miss.case_id,
            miss.question,
            show(&miss.label),
            show(&miss.predicted()),
            miss.confidence(),
            miss.p(&miss.label)
        ));
    }
    out
}

#[cfg(test)]
mod tests;
