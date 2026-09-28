//! T6.2: the operator's `sqlite3` reports run on a database with Serbero's
//! real schema and give the expected numbers.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::ValueRef;
use rusqlite::{Batch, Connection};
use serbero::store::disputes::{self, Initiator, NewDispute};
use serbero::store::sessions::{self, NewSession};
use serbero::store::{Store, evaluations, events};
use serde_json::json;

fn now() -> i64 {
    serbero::daemon::now()
}

/// Two mediated disputes this week: `d1` handed off as `uncertain`, `d2`
/// resolved by its parties; three evaluations and one feedback.
fn staged() -> Store {
    let store = Store::open_in_memory().unwrap();
    let conn = store.conn();
    for (dispute, session) in [("d1", "s1"), ("d2", "s2")] {
        disputes::insert_if_new(
            conn,
            &NewDispute {
                dispute_id: dispute,
                initiator: Initiator::Buyer,
                status: "in-progress",
                status_at: now(),
                now: now(),
            },
        )
        .unwrap();
        sessions::insert(
            conn,
            &NewSession {
                session_id: session,
                dispute_id: dispute,
                buyer_trade_pubkey: "b",
                seller_trade_pubkey: "s",
                fiat_amount: None,
                fiat_code: None,
                payment_method: None,
                order_published_at: None,
                now: now(),
            },
        )
        .unwrap();
    }
    sessions::hand_off(conn, "s1", "uncertain", now()).unwrap();
    let event = |dispute: &str, kind: &str, payload: serde_json::Value| {
        events::append(
            conn,
            &events::NewEvent {
                dispute_id: dispute,
                session_id: (kind == "handoff").then_some("s1"),
                kind,
                payload,
                now: now(),
            },
        )
        .unwrap();
    };
    event("d1", "handoff", json!({ "reason": "uncertain" }));
    event(
        "d2",
        "resolved",
        json!({ "status": "released", "resolved_by": "parties" }),
    );
    event(
        "d1",
        "solver_feedback",
        json!({ "question": "seller_receipt" }),
    );
    let (answers, action) = (json!({}), json!("wait"));
    for (session, latency) in [("s1", 300), ("s1", 100), ("s2", 200)] {
        evaluations::insert(
            conn,
            &evaluations::NewEvaluation {
                session_id: session,
                question_set_version: "qs-1-x",
                judge_id: "typesafe/jev-1.13.0",
                last_message_id: 0,
                answers: &answers,
                action: &action,
                input_tokens: Some(2_000_000),
                latency_ms: Some(latency),
                now: now(),
            },
        )
        .unwrap();
    }
    store
}

/// Every result set of a script, as text, the way `sqlite3` prints it.
fn run(conn: &Connection, script: &str, usd_per_million_tokens: f64) -> Vec<Vec<Vec<String>>> {
    let mut results = Vec::new();
    let mut batch = Batch::new(conn, script);
    while let Some(mut statement) = batch.next().unwrap() {
        if let Some(index) = statement
            .parameter_index(":usd_per_million_tokens")
            .unwrap()
        {
            statement
                .raw_bind_parameter(index, usd_per_million_tokens)
                .unwrap();
        }
        let columns = statement.column_count();
        let mut rows = statement.raw_query();
        let mut set = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            set.push(
                (0..columns)
                    .map(|i| match row.get_ref(i).unwrap() {
                        ValueRef::Null => "NULL".to_owned(),
                        ValueRef::Integer(v) => v.to_string(),
                        ValueRef::Real(v) => v.to_string(),
                        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned(),
                        ValueRef::Blob(_) => "<blob>".to_owned(),
                    })
                    .collect(),
            );
        }
        results.push(set);
    }
    results
}

#[test]
fn the_weekly_report_counts_this_weeks_mediation() {
    let store = staged();

    let sets = run(
        store.conn(),
        include_str!("../scripts/weekly-report.sql"),
        0.0,
    );

    assert_eq!(sets.len(), 4, "one result set per section");
    assert_eq!(
        sets[0],
        [["2", "1", "1"]],
        "opened, resolved by parties, handed off"
    );
    assert_eq!(sets[1], [["uncertain", "1", "100"]], "a 100% share");
    assert_eq!(sets[2], [["seller_receipt", "1"]]);
    assert_eq!(
        sets[3],
        [["3", "200", "6000000"]],
        "requests, median latency, tokens"
    );
}

#[test]
fn an_old_handoff_resolved_this_week_is_not_counted_again() {
    let store = staged();
    let conn = store.conn();
    let ten_days_ago = now() - 10 * 24 * 3600;
    disputes::insert_if_new(
        conn,
        &NewDispute {
            dispute_id: "d3",
            initiator: Initiator::Seller,
            status: "in-progress",
            status_at: ten_days_ago,
            now: ten_days_ago,
        },
    )
    .unwrap();
    sessions::insert(
        conn,
        &NewSession {
            session_id: "s3",
            dispute_id: "d3",
            buyer_trade_pubkey: "b",
            seller_trade_pubkey: "s",
            fiat_amount: None,
            fiat_code: None,
            payment_method: None,
            order_published_at: None,
            now: ten_days_ago,
        },
    )
    .unwrap();
    sessions::hand_off(conn, "s3", "fraud_signal", ten_days_ago).unwrap();
    events::append(
        conn,
        &events::NewEvent {
            dispute_id: "d3",
            session_id: Some("s3"),
            kind: "handoff",
            payload: json!({ "reason": "fraud_signal" }),
            now: ten_days_ago,
        },
    )
    .unwrap();
    // Resolved this week: the session changes now.
    sessions::set_state(conn, "s3", sessions::SessionState::Closed, now()).unwrap();

    let sets = run(conn, include_str!("../scripts/weekly-report.sql"), 0.0);

    assert_eq!(sets[0][0][2], "1", "only this week's handoff");
    assert_eq!(sets[1], [["uncertain", "1", "100"]]);
}

#[test]
fn the_median_of_an_even_count_is_the_mean_of_the_middle_two() {
    let store = staged();
    let (answers, action) = (json!({}), json!("wait"));
    evaluations::insert(
        store.conn(),
        &evaluations::NewEvaluation {
            session_id: "s2",
            question_set_version: "qs-1-x",
            judge_id: "typesafe/jev-1.13.0",
            last_message_id: 0,
            answers: &answers,
            action: &action,
            input_tokens: None,
            latency_ms: Some(400),
            now: now(),
        },
    )
    .unwrap();

    let sets = run(
        store.conn(),
        include_str!("../scripts/weekly-report.sql"),
        0.0,
    );

    assert_eq!(sets[3][0][1], "250", "100, 200, 300, 400");
}

#[test]
fn the_cost_report_prices_tokens_per_day_and_judge() {
    let store = staged();

    let sets = run(
        store.conn(),
        include_str!("../scripts/cost-report.sql"),
        0.25,
    );

    let row = &sets[0][0];
    assert_eq!(row[1..], ["typesafe/jev-1.13.0", "3", "6000000", "1.5"]);
}

#[test]
fn the_reports_are_read_only() {
    let store = staged();
    let before: i64 = store
        .conn()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();

    run(
        store.conn(),
        include_str!("../scripts/weekly-report.sql"),
        0.0,
    );
    run(
        store.conn(),
        include_str!("../scripts/cost-report.sql"),
        0.25,
    );

    let after: i64 = store
        .conn()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
}
