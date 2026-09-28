//! Solver messages for mediation (`docs/messages.md` §3): the brief, the
//! transcript, updates after handoff, the mediation notice and the final
//! report. Pure rendering; the notifier sends them.
//!
//! Solver messages are in English. Party text is quoted verbatim, except
//! that Nostr identifiers are redacted, so a party's pubkey never reaches a
//! solver even if the party pasted it.

use serde_json::Value;

use crate::judge::Answers;
use crate::judge::brief::Brief;
use crate::judge::facts::Facts;
use crate::policy::{HandoffReason, Path};
use crate::store::sessions::Party;

/// Why the brief is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Handoff(HandoffReason),
    Guide(Path),
}

/// The order facts Serbero knows; unknown ones are left out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Order<'a> {
    pub amount: Option<&'a str>,
    pub currency: Option<&'a str>,
    pub payment_method: Option<&'a str>,
    /// How long before the dispute the order was created.
    pub created_before_dispute_secs: Option<i64>,
}

/// The judge's reading of the last turn; absent when the judge was
/// unavailable.
#[derive(Debug, Clone, Copy)]
pub struct Reading<'a> {
    pub state: &'a Value,
    pub answers: &'a Answers,
    pub facts: &'a Facts,
    pub brief: &'a Brief,
}

/// Everything the brief shows.
#[derive(Debug, Clone, Copy)]
pub struct BriefInput<'a> {
    pub dispute_id: &'a str,
    pub subject: Subject,
    pub rounds: u32,
    pub duration_secs: i64,
    pub order: Order<'a>,
    /// The language each party is addressed in.
    pub buyer_language: &'a str,
    pub seller_language: &'a str,
    pub reading: Option<Reading<'a>>,
    /// Messages in the transcript DM that follows.
    pub transcript_messages: usize,
}

/// The brief (`docs/messages.md` §3), sent before the transcript.
pub fn brief(input: &BriefInput<'_>) -> String {
    let mut out = vec![header(input)];
    let summary = format!(
        "rounds: {} · duration: {}",
        input.rounds,
        duration(input.duration_secs)
    );
    out.push(match &input.reading {
        Some(reading) => format!("Topic: {} · {summary}", topic(reading.answers)),
        None => capitalize(&summary),
    });
    if let Some(order) = order_line(&input.order) {
        out.push(order);
    }
    out.push(languages_line(input));
    out.push(String::new());
    match &input.reading {
        Some(reading) => out.extend(reading_lines(reading)),
        None => out.push("Automated reading unavailable".to_owned()),
    }
    out.push(String::new());
    out.push(format!(
        "Transcript ({} messages) follows in the next message.",
        input.transcript_messages
    ));
    out.join("\n")
}

fn header(input: &BriefInput<'_>) -> String {
    let what = match input.subject {
        Subject::Handoff(reason) => format!("handed off: {}", reason.as_str()),
        Subject::Guide(path) => format!("guidance sent: {}", path.as_str()),
    };
    format!("Dispute {} · {what}", input.dispute_id)
}

fn topic(answers: &Answers) -> String {
    match answers.get("dispute_topic") {
        Some(answer) => {
            let winner = answer.winner().unwrap_or("not_yet_clear");
            format!("{winner} ({})", p(answer.share(winner)))
        }
        None => "not_yet_clear".to_owned(),
    }
}

fn order_line(order: &Order<'_>) -> Option<String> {
    let amount = match (order.amount, order.currency) {
        (Some(amount), Some(currency)) => Some(format!("{amount} {currency}")),
        _ => None,
    };
    let mut what = amount.unwrap_or_default();
    if let Some(method) = order.payment_method {
        what = format!("{what} via {method}").trim_start().to_owned();
    }
    let mut parts: Vec<String> = Vec::new();
    if !what.is_empty() {
        parts.push(what);
    }
    if let Some(before) = order.created_before_dispute_secs {
        parts.push(format!("created {} before the dispute", duration(before)));
    }
    (!parts.is_empty()).then(|| format!("Order: {}", parts.join(" · ")))
}

/// A party the judge reads as writing a language that is not enabled is
/// shown as `other`, never named (`docs/spec.md` §7.7).
fn languages_line(input: &BriefInput<'_>) -> String {
    let language = |party: Party, addressed_in: &str| {
        let other = input.reading.is_some_and(|r| {
            r.answers
                .get(&format!("{party}_language"))
                .and_then(|a| a.winner())
                == Some("other")
        });
        if other {
            format!("{party} other (not enabled; addressed in {addressed_in})")
        } else {
            format!("{party} {addressed_in}")
        }
    };
    format!(
        "Languages: {} · {}",
        language(Party::Buyer, input.buyer_language),
        language(Party::Seller, input.seller_language)
    )
}

fn reading_lines(reading: &Reading<'_>) -> Vec<String> {
    let answers = reading.answers;
    let mut out = vec![format!(
        "Buyer — {}, {}",
        claim(answers, "buyer_payment"),
        noul_line(
            answers,
            "buyer_details",
            "details given",
            "no details given"
        )
    )];
    let buyer_quotes = [
        &reading.brief.quote_buyer_payment,
        &reading.brief.quote_buyer_details,
    ];
    let mut shown: Vec<&str> = Vec::new();
    for id in buyer_quotes.into_iter().flatten() {
        if !shown.contains(&id.as_str()) {
            shown.push(id);
            out.extend(quote(reading.state, id));
        }
    }
    out.push(format!(
        "Seller — {}, {}",
        claim(answers, "seller_receipt"),
        noul_line(
            answers,
            "seller_checked",
            "checked account",
            "has not said they checked"
        )
    ));
    if let Some(id) = &reading.brief.quote_seller_receipt {
        out.extend(quote(reading.state, id));
    }
    out.push(String::new());
    out.push(signals(reading));
    if let Some(id) = &reading.brief.quote_concern
        && let Some((text, _)) = message(reading.state, id)
    {
        out.push(format!("First to read: \"{}\"", verbatim(text)));
    }
    if let Some(balance) = &reading.brief.evidence_balance {
        out.push(String::new());
        out.push("Reading of the conversation (advisory, not a verdict):".to_owned());
        out.push(evidence(balance));
    }
    out
}

/// The winning option of a payment question, in words, with its share.
fn claim(answers: &Answers, id: &str) -> String {
    let Some(answer) = answers.get(id) else {
        return "has not said".to_owned();
    };
    let winner = answer.winner().unwrap_or("not_stated");
    let words = match winner {
        "says_sent" => "says sent",
        "says_not_sent" => "says not sent",
        "says_received" => "says received",
        "says_received_with_problem" => "says a payment arrived with a problem",
        "says_not_received" => "says not received",
        _ => "has not said",
    };
    format!("{words} ({})", p(answer.share(winner)))
}

fn noul_line(answers: &Answers, id: &str, yes: &str, no: &str) -> String {
    let p_yes = match answers.get(id) {
        Some(crate::judge::Answer::Noul { p_yes }) => *p_yes,
        _ => 0.0,
    };
    if p_yes >= 0.5 {
        format!("{yes} ({})", p(p_yes))
    } else {
        format!("{no} ({})", p(1.0 - p_yes))
    }
}

fn signals(reading: &Reading<'_>) -> String {
    let p_yes = |id: &str| match reading.answers.get(id) {
        Some(crate::judge::Answer::Noul { p_yes }) => *p_yes,
        _ => 0.0,
    };
    let human = [Party::Buyer, Party::Seller]
        .into_iter()
        .any(|party| reading.facts.party(party).is_some_and(|f| f.wants_human));
    format!(
        "Signals: conflict {} · fraud {} · human requested: {}",
        p(p_yes("claims_conflict")),
        p(p_yes("fraud_signal")),
        if human { "yes" } else { "no" }
    )
}

fn evidence(balance: &crate::judge::brief::EvidenceBalance) -> String {
    let top = balance.distribution.len().saturating_sub(1);
    let levels: Vec<String> = balance
        .distribution
        .iter()
        .enumerate()
        .map(|(i, q)| format!("{i}:{}", p(*q)))
        .collect();
    format!(
        "  evidence that fiat was sent: {:.1} / {top}  [{}]",
        balance.value * top as f64,
        levels.join(" ")
    )
}

/// A quoted message: its text verbatim (Nostr identifiers redacted) and
/// its attachment count.
fn quote(state: &Value, id: &str) -> Option<String> {
    let (text, attachments) = message(state, id)?;
    Some(format!(
        "  \"{}\"{}",
        verbatim(text),
        attachments_note(attachments)
    ))
}

fn message<'a>(state: &'a Value, id: &str) -> Option<(&'a str, u64)> {
    let entry = state["transcript"]
        .as_array()?
        .iter()
        .find(|m| m["id"].as_str() == Some(id))?;
    let text = entry["text"].as_str()?;
    Some((text, entry["attachments"].as_u64().unwrap_or(0)))
}

/// Who wrote a transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    Party(Party),
    /// Serbero, writing to this party.
    Serbero(Party),
}

/// One transcript message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line<'a> {
    /// Unix time, in seconds.
    pub at: i64,
    pub speaker: Speaker,
    pub text: &'a str,
    pub attachments: u32,
}

/// Largest DM, in characters; a longer transcript is split into parts.
pub const MAX_DM_CHARS: usize = 30_000;

/// The transcript DM, one line per message; split into numbered parts
/// when it would exceed `MAX_DM_CHARS`.
pub fn transcript(dispute_id: &str, lines: &[Line<'_>]) -> Vec<String> {
    let header = |part: Option<(usize, usize)>| {
        let part = part.map_or(String::new(), |(i, n)| format!(", part {i}/{n}"));
        format!(
            "Dispute {dispute_id} · transcript ({} messages, times UTC{part})",
            lines.len()
        )
    };
    let rendered: Vec<String> = lines.iter().map(transcript_line).collect();
    let single = std::iter::once(header(None))
        .chain(rendered.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n");
    if single.chars().count() <= MAX_DM_CHARS {
        return vec![single];
    }
    // The part header is at most this long ("part 999/999").
    let budget = MAX_DM_CHARS - header(Some((999, 999))).chars().count();
    let chunks = chunk(&rendered, budget);
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, body)| format!("{}\n{body}", header(Some((i + 1, n)))))
        .collect()
}

/// New party messages after a handoff, forwarded to the solver.
pub fn update(dispute_id: &str, lines: &[Line<'_>]) -> Vec<String> {
    let header = format!(
        "Dispute {dispute_id} · new messages since handoff ({})",
        lines.len()
    );
    let rendered: Vec<String> = lines.iter().map(transcript_line).collect();
    let budget = MAX_DM_CHARS - header.chars().count() - 1;
    chunk(&rendered, budget)
        .into_iter()
        .map(|body| format!("{header}\n{body}"))
        .collect()
}

/// Sent to solvers when Serbero takes a dispute to mediate it.
pub fn mediation_started(dispute_id: &str) -> String {
    format!(
        "Serbero is mediating dispute {dispute_id}.\n\
         You can take it over at any time; Serbero stops as soon as you do."
    )
}

fn transcript_line(line: &Line<'_>) -> String {
    let who = match line.speaker {
        Speaker::Party(party) => party.to_string(),
        Speaker::Serbero(party) => format!("serbero → {party}"),
    };
    format!(
        "[{}] {who}: {}{}",
        clock(line.at),
        verbatim(line.text),
        attachments_note(u64::from(line.attachments))
    )
}

/// Groups lines into bodies of at most `budget` characters. A line longer
/// than the budget is split across bodies, so nothing is cut.
fn chunk(lines: &[String], budget: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for line in lines {
        let chars: Vec<char> = line.chars().collect();
        let mut rest = chars.as_slice();
        loop {
            let separator = usize::from(used > 0);
            let room = budget.saturating_sub(used + separator);
            if room == 0 || (used > 0 && rest.len() > room && rest.len() <= budget) {
                chunks.push(std::mem::take(&mut current));
                used = 0;
                continue;
            }
            if separator == 1 {
                current.push('\n');
                used += 1;
            }
            let take = rest.len().min(room);
            current.extend(&rest[..take]);
            used += take;
            rest = &rest[take..];
            if rest.is_empty() {
                break;
            }
        }
    }
    chunks.push(current);
    chunks
}

/// How a mediated (or not mediated) dispute ended for Serbero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    NotMediated,
    SelfResolved,
    HandedOff(HandoffReason),
    Superseded,
}

/// Sent when the dispute resolves (`docs/messages.md` §3).
pub fn final_report(
    dispute_id: &str,
    status: &str,
    outcome: Outcome,
    rounds: u32,
    duration_secs: i64,
) -> String {
    let first = format!("Dispute {dispute_id} resolved: {status}");
    let outcome = match outcome {
        Outcome::NotMediated => return format!("{first}\nmediation: no"),
        Outcome::SelfResolved => "self_resolved".to_owned(),
        Outcome::HandedOff(reason) => format!("handed_off ({})", reason.as_str()),
        Outcome::Superseded => "superseded".to_owned(),
    };
    format!(
        "{first}\nmediation: yes · outcome: {outcome} · rounds: {rounds} · duration: {}",
        duration(duration_secs)
    )
}

/// Party text as written, with Nostr identifiers redacted and continuation
/// lines indented under the first.
fn verbatim(text: &str) -> String {
    crate::judge::state::redact(text).replace('\n', "\n  ")
}

fn attachments_note(count: u64) -> String {
    match count {
        0 => String::new(),
        1 => "  [1 attachment]".to_owned(),
        n => format!("  [{n} attachments]"),
    }
}

/// `14 min`, or `3 h 20 min` from an hour on.
fn duration(secs: i64) -> String {
    let minutes = secs.max(0) / 60;
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{} h {} min", minutes / 60, minutes % 60)
    }
}

/// `hh:mm` in UTC.
fn clock(at: i64) -> String {
    let day = at.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, day % 3600 / 60)
}

fn p(value: f64) -> String {
    format!("{value:.2}")
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests;
