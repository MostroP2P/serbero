//! The committed evaluation sample replays offline: its cases load, its
//! recording matches the current question set, and the report is written.
//! If a question changes, this fails until the sample is run again
//! (`docs/evaluation.md` §4).

#![allow(clippy::unwrap_used)] // test helpers outside #[test] functions

use std::path::Path;

use serbero::catalog::{Catalogs, embedded_codes};
use serbero::config::Thresholds;
use serbero::eval::{case, report};
use serbero::judge::providers::recorded::RecordedJudge;
use serbero::judge::questions::{Language, TurnQuestions};

#[tokio::test]
async fn the_sample_replays_from_its_recording() {
    let catalogs = Catalogs::embedded().unwrap();
    let languages: Vec<Language<'_>> = embedded_codes()
        .into_iter()
        .map(|code| Language {
            code,
            name: &catalogs.get(code).unwrap().name,
        })
        .collect();
    let turn = TurnQuestions::new(&languages);
    let cases = case::load_dir(Path::new("eval/sample/cases"), "es", &turn).unwrap();
    let judge = RecordedJudge::load(Path::new("eval/sample/recorded-es.json")).unwrap();

    let runs = report::run(&judge, &turn, cases).await.unwrap();
    let thresholds = Thresholds {
        guide: 0.9,
        fact: 0.8,
        human_request: 0.8,
        fraud: 0.6,
        conflict: 0.75,
        outside_scope: 0.8,
        validated_languages: Vec::new(),
    };
    let text = report::render(
        &report::Header {
            judge_id: "recorded/typesafe/jev-1.13.0",
            question_set: turn.id(),
            lang: "es",
            thresholds: &thresholds,
            thresholds_source: "test",
        },
        &runs,
    );

    assert_eq!(runs.len(), 10);
    assert!(text.contains("10 cases"), "{text}");
}
