//! Settling bursts into turns (`docs/spec.md` §7.3, step 2): a turn starts
//! `quiet_period` after the latest party message of a session, so "hola" +
//! "ayuda" + "?" are judged once.

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

/// Per-session deadlines. Pure bookkeeping; the turn task sleeps until the
/// next deadline.
#[derive(Debug)]
pub struct Settle {
    quiet: Duration,
    deadlines: HashMap<String, Instant>,
}

impl Settle {
    pub fn new(quiet: Duration) -> Self {
        Self {
            quiet,
            deadlines: HashMap::new(),
        }
    }

    /// A party message arrived: the session's turn moves to `quiet` from
    /// now.
    pub fn touch(&mut self, session_id: &str, now: Instant) {
        self.deadlines
            .insert(session_id.to_owned(), now + self.quiet);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadlines.values().min().copied()
    }

    /// The sessions whose turn is due, in id order; they are forgotten until
    /// their next message.
    pub fn take_due(&mut self, now: Instant) -> Vec<String> {
        let mut due: Vec<String> = self
            .deadlines
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        due.sort();
        for id in &due {
            self.deadlines.remove(id);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Duration = Duration::from_secs(20);

    #[test]
    fn a_burst_becomes_one_turn_after_the_last_message() {
        let t0 = Instant::now();
        let mut settle = Settle::new(QUIET);

        settle.touch("s1", t0);
        settle.touch("s1", t0 + Duration::from_secs(5));
        settle.touch("s1", t0 + Duration::from_secs(12));

        assert_eq!(settle.next_deadline(), Some(t0 + Duration::from_secs(32)));
        assert!(settle.take_due(t0 + Duration::from_secs(31)).is_empty());
        assert_eq!(settle.take_due(t0 + Duration::from_secs(32)), ["s1"]);
        assert!(
            settle.take_due(t0 + Duration::from_secs(90)).is_empty(),
            "taken once"
        );
        assert_eq!(settle.next_deadline(), None);
    }

    #[test]
    fn sessions_settle_independently() {
        let t0 = Instant::now();
        let mut settle = Settle::new(QUIET);

        settle.touch("b", t0);
        settle.touch("a", t0 + Duration::from_secs(10));

        assert_eq!(settle.take_due(t0 + Duration::from_secs(20)), ["b"]);
        assert_eq!(settle.next_deadline(), Some(t0 + Duration::from_secs(30)));
        assert_eq!(settle.take_due(t0 + Duration::from_secs(40)), ["a"]);
    }

    #[test]
    fn due_sessions_come_in_id_order() {
        let t0 = Instant::now();
        let mut settle = Settle::new(QUIET);
        settle.touch("s2", t0);
        settle.touch("s1", t0);

        assert_eq!(settle.take_due(t0 + QUIET), ["s1", "s2"]);
    }
}
