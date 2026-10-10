use super::*;

const TIMEOUTS: Timeouts = Timeouts {
    response: Duration::from_secs(1800),
    self_resolution: Duration::from_secs(7200),
    handoff_grace: Duration::from_secs(3600),
};
const T0: i64 = 1_700_000_000;
const R: i64 = 1800;
const S: i64 = 7200;
const G: i64 = 3600;

fn gathering(now: i64, buyer: PartyClock, seller: PartyClock) -> Clocks {
    Clocks {
        phase: Phase::Gathering,
        now,
        buyer,
        seller,
        guided_at: None,
        held_at: None,
    }
}

fn asked_at(t: i64) -> PartyClock {
    PartyClock {
        question_at: Some(t),
        ..PartyClock::default()
    }
}

fn check_at(clocks: &Clocks) -> Timer {
    check(clocks, &TIMEOUTS)
}

#[test]
fn a_held_session_hands_off_as_facts_gathered_after_the_grace() {
    let held = |now| Clocks {
        held_at: Some(T0),
        ..gathering(now, PartyClock::default(), PartyClock::default())
    };

    assert_eq!(check_at(&held(T0 + G - 1)), Timer::Nothing);
    assert_eq!(
        check_at(&held(T0 + G)),
        Timer::Handoff(HandoffReason::FactsGathered)
    );
}

#[test]
fn the_hold_wins_over_a_reminder_and_keeps_the_response_timers_meanwhile() {
    // The seller still owes an answer to a later question: its reminder
    // goes out during the hold, and the hold ends the session at its own
    // time, before the seller's silence would.
    let during = Clocks {
        held_at: Some(T0),
        ..gathering(T0 + R, PartyClock::default(), asked_at(T0))
    };
    let over = Clocks {
        held_at: Some(T0),
        ..gathering(T0 + G, PartyClock::default(), asked_at(T0 + G - R))
    };

    assert_eq!(check_at(&during), Timer::Remind(vec![Party::Seller]));
    assert_eq!(
        check_at(&over),
        Timer::Handoff(HandoffReason::FactsGathered)
    );
}

#[test]
fn an_unanswered_question_gets_a_reminder_at_the_timeout() {
    let before = gathering(T0 + R - 1, asked_at(T0), PartyClock::default());
    let at = gathering(T0 + R, asked_at(T0), PartyClock::default());

    assert_eq!(check_at(&before), Timer::Nothing);
    assert_eq!(check_at(&at), Timer::Remind(vec![Party::Buyer]));
}

#[test]
fn both_parties_can_be_reminded_at_once() {
    let clocks = gathering(T0 + R, asked_at(T0), asked_at(T0));

    assert_eq!(
        check_at(&clocks),
        Timer::Remind(vec![Party::Buyer, Party::Seller])
    );
}

#[test]
fn an_answered_question_needs_no_reminder() {
    let answered = PartyClock {
        question_at: Some(T0),
        replied_at: Some(T0 + 10),
        ..PartyClock::default()
    };
    let clocks = gathering(T0 + 10 * R, answered, PartyClock::default());

    assert_eq!(check_at(&clocks), Timer::Nothing);
}

#[test]
fn a_reply_before_the_question_does_not_answer_it() {
    let clock = PartyClock {
        question_at: Some(T0),
        replied_at: Some(T0 - 5),
        ..PartyClock::default()
    };

    assert_eq!(
        check_at(&gathering(T0 + R, clock, PartyClock::default())),
        Timer::Remind(vec![Party::Buyer])
    );
}

#[test]
fn silence_after_the_reminder_hands_off_as_unresponsive() {
    let reminded = PartyClock {
        question_at: Some(T0),
        reminded_at: Some(T0 + R),
        ..PartyClock::default()
    };
    let before = gathering(T0 + 2 * R - 1, PartyClock::default(), reminded);
    let at = gathering(T0 + 2 * R, PartyClock::default(), reminded);

    assert_eq!(check_at(&before), Timer::Nothing);
    assert_eq!(check_at(&at), Timer::Handoff(HandoffReason::Unresponsive));
}

#[test]
fn the_reminder_is_sent_once_per_party() {
    // Reminded for an earlier question, then answered; a new question goes
    // unanswered: no second reminder, the timeout hands off.
    let clock = PartyClock {
        question_at: Some(T0 + 3 * R),
        replied_at: Some(T0 + 2 * R),
        reminded_at: Some(T0 + R),
    };
    let before = gathering(T0 + 4 * R - 1, clock, PartyClock::default());
    let at = gathering(T0 + 4 * R, clock, PartyClock::default());

    assert_eq!(check_at(&before), Timer::Nothing);
    assert_eq!(check_at(&at), Timer::Handoff(HandoffReason::Unresponsive));
}

#[test]
fn a_handoff_wins_over_a_reminder() {
    let reminded = PartyClock {
        question_at: Some(T0),
        reminded_at: Some(T0 + R),
        ..PartyClock::default()
    };
    let clocks = gathering(T0 + 2 * R, asked_at(T0 + R), reminded);

    assert_eq!(
        check_at(&clocks),
        Timer::Handoff(HandoffReason::Unresponsive)
    );
}

#[test]
fn guidance_that_does_not_resolve_in_time_hands_off() {
    let guiding = |now| Clocks {
        phase: Phase::Guiding,
        now,
        buyer: PartyClock::default(),
        seller: PartyClock::default(),
        guided_at: Some(T0),
        held_at: None,
    };

    assert_eq!(check_at(&guiding(T0 + S - 1)), Timer::Nothing);
    assert_eq!(
        check_at(&guiding(T0 + S)),
        Timer::Handoff(HandoffReason::SelfResolutionStalled)
    );
}

#[test]
fn while_guiding_an_old_question_never_times_out() {
    let clocks = Clocks {
        phase: Phase::Guiding,
        now: T0 + 3 * R,
        buyer: asked_at(T0),
        seller: PartyClock::default(),
        guided_at: Some(T0 + R),
        held_at: None,
    };

    assert_eq!(check_at(&clocks), Timer::Nothing);
}

#[test]
fn a_party_over_the_message_limit_is_flagged() {
    assert!(!over_limit(10, 10));
    assert!(over_limit(11, 10));
}

#[test]
fn flooding_twice_hands_off() {
    assert!(!is_flood(0));
    assert!(!is_flood(1));
    assert!(is_flood(FLOOD_STRIKES));
    assert!(is_flood(FLOOD_STRIKES + 1));
}

#[test]
fn a_huge_timeout_never_overflows() {
    let clocks = gathering(T0, asked_at(T0), PartyClock::default());

    let huge = Timeouts {
        response: Duration::MAX,
        self_resolution: Duration::MAX,
        handoff_grace: Duration::MAX,
    };

    assert_eq!(check(&clocks, &huge), Timer::Nothing);
}

#[test]
fn a_reply_in_the_same_second_as_the_question_answers_it() {
    // Which came first cannot be told; no reminder is the safer reading.
    let clock = PartyClock {
        question_at: Some(T0),
        replied_at: Some(T0),
        ..PartyClock::default()
    };

    assert_eq!(
        check_at(&gathering(T0 + R, clock, PartyClock::default())),
        Timer::Nothing
    );
}
