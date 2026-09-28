//! Timers (`docs/judgments.md` §4.2): pure functions of the session's
//! clocks. Timers never call the judge.

use std::time::Duration;

use crate::store::sessions::Party;

use super::{HandoffReason, Phase};

/// Unix times, in seconds, of one party's last question, last message and
/// reminder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartyClock {
    /// When the last question template was sent to the party.
    pub question_at: Option<i64>,
    /// When the party last wrote.
    pub replied_at: Option<i64>,
    /// When the party got its reminder; there is at most one per session.
    pub reminded_at: Option<i64>,
}

/// Everything the timers read.
#[derive(Debug, Clone, Copy)]
pub struct Clocks {
    pub phase: Phase,
    pub now: i64,
    pub buyer: PartyClock,
    pub seller: PartyClock,
    /// When the guidance was sent, while guiding.
    pub guided_at: Option<i64>,
}

/// What the timer task does for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Timer {
    Handoff(HandoffReason),
    /// Send `reminder` to these parties.
    Remind(Vec<Party>),
    Nothing,
}

/// Turns in which a party sent more than `max_messages_per_turn` before the
/// session hands off with `flood`.
pub const FLOOD_STRIKES: u32 = 2;

/// The timer action for a session. A handoff wins over reminders.
///
/// While gathering, a party with an unanswered question gets its reminder
/// `response_timeout` after the question, and the session hands off as
/// `unresponsive` `response_timeout` after the reminder. The reminder is
/// sent once per session: a later unanswered question hands off at its own
/// timeout. While guiding, no question awaits an answer; only
/// `self_resolution_timeout` after the guidance applies.
pub fn check(
    clocks: &Clocks,
    response_timeout: Duration,
    self_resolution_timeout: Duration,
) -> Timer {
    if clocks.phase == Phase::Guiding {
        let stalled = clocks
            .guided_at
            .is_some_and(|at| clocks.now >= at.saturating_add(secs(self_resolution_timeout)));
        return if stalled {
            Timer::Handoff(HandoffReason::SelfResolutionStalled)
        } else {
            Timer::Nothing
        };
    }

    let mut remind = Vec::new();
    for (party, clock) in [(Party::Buyer, clocks.buyer), (Party::Seller, clocks.seller)] {
        match response(&clock, clocks.now, secs(response_timeout)) {
            Response::Unresponsive => return Timer::Handoff(HandoffReason::Unresponsive),
            Response::Remind => remind.push(party),
            Response::Waiting => {}
        }
    }
    if remind.is_empty() {
        Timer::Nothing
    } else {
        Timer::Remind(remind)
    }
}

enum Response {
    Waiting,
    Remind,
    Unresponsive,
}

fn response(clock: &PartyClock, now: i64, timeout: i64) -> Response {
    let Some(question_at) = clock.question_at else {
        return Response::Waiting;
    };
    if clock
        .replied_at
        .is_some_and(|replied| replied >= question_at)
    {
        return Response::Waiting;
    }
    match clock.reminded_at {
        Some(reminded) if reminded >= question_at => {
            if now >= reminded.saturating_add(timeout) {
                Response::Unresponsive
            } else {
                Response::Waiting
            }
        }
        // Reminded for an earlier question: no second reminder.
        Some(_) if now >= question_at.saturating_add(timeout) => Response::Unresponsive,
        None if now >= question_at.saturating_add(timeout) => Response::Remind,
        _ => Response::Waiting,
    }
}

fn secs(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

/// A turn with more messages from a party than `max_messages_per_turn`
/// counts as one flood strike for that party.
pub fn over_limit(messages_in_turn: u32, max_messages_per_turn: u32) -> bool {
    messages_in_turn > max_messages_per_turn
}

/// Flooding "repeatedly" (`docs/spec.md` §7.6) means `FLOOD_STRIKES` turns.
pub fn is_flood(strikes: u32) -> bool {
    strikes >= FLOOD_STRIKES
}

#[cfg(test)]
mod tests;
