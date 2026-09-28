//! `cargo run --bin eval -- --lang es`: runs the golden cases of one
//! language against a judge, writes the report, and saves the answers for
//! `RecordedJudge` (`docs/evaluation.md`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serbero::catalog::Catalogs;
use serbero::config::{Config, JudgeConfig, Thresholds};
use serbero::eval::{case, report};
use serbero::judge::Judge;
use serbero::judge::providers::recorded::RecordedJudge;
use serbero::judge::providers::typesafe::TypeSafeJudge;
use serbero::judge::questions::{Language, TurnQuestions};

const USAGE: &str = "usage: eval --lang <code> [--cases <dir>] [--config <file>]
            [--provider typesafe|recorded] [--model <model>] [--recording <file>]
            [--limit <n>] [--report <file>] [--record-to <file>] [--no-record]";

/// The starting thresholds of `docs/judgments.md` §3, used when the
/// configuration has none for the judge being evaluated.
const DEFAULT_THRESHOLDS: Thresholds = Thresholds {
    guide: 0.90,
    fact: 0.80,
    human_request: 0.80,
    fraud: 0.60,
    conflict: 0.75,
    outside_scope: 0.80,
    validated_languages: Vec::new(),
};

#[derive(Debug, Default)]
struct Options {
    lang: String,
    cases: PathBuf,
    config: Option<PathBuf>,
    provider: Option<String>,
    model: Option<String>,
    recording: Option<PathBuf>,
    limit: Option<usize>,
    report: Option<PathBuf>,
    record_to: Option<PathBuf>,
    record: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        cases: PathBuf::from("eval/golden"),
        record: true,
        ..Options::default()
    };
    let mut args = args.peekable();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--lang" => options.lang = value()?,
            "--cases" => options.cases = value()?.into(),
            "--config" => options.config = Some(value()?.into()),
            "--provider" => options.provider = Some(value()?),
            "--model" => options.model = Some(value()?),
            "--recording" => options.recording = Some(value()?.into()),
            "--limit" => {
                let n = value()?;
                options.limit = Some(
                    n.parse()
                        .map_err(|_| format!("--limit {n} is not a number"))?,
                );
            }
            "--report" => options.report = Some(value()?.into()),
            "--record-to" => options.record_to = Some(value()?.into()),
            "--no-record" => options.record = false,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if options.lang.is_empty() {
        return Err("--lang is required".into());
    }
    Ok(options)
}

/// The judge settings, enabled languages and thresholds to evaluate with.
struct Setup {
    judge: JudgeConfig,
    languages: Vec<String>,
    thresholds: Thresholds,
    thresholds_source: &'static str,
}

fn setup(options: &Options) -> Result<Setup, String> {
    let config = match &options.config {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            Some(Config::parse(&text).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let (mut judge, languages) = match config {
        Some(config) => (config.judge, config.mediation.languages),
        None => (
            JudgeConfig::default(),
            serbero::catalog::embedded_codes()
                .into_iter()
                .map(str::to_owned)
                .collect(),
        ),
    };
    if let Some(provider) = &options.provider {
        judge.provider.clone_from(provider);
    }
    if let Some(model) = &options.model {
        judge.model.clone_from(model);
    }
    let (thresholds, thresholds_source) = match judge.active_thresholds() {
        Some(t) => (t.clone(), "config"),
        None => (DEFAULT_THRESHOLDS, "defaults (judgments.md §3)"),
    };
    Ok(Setup {
        judge,
        languages,
        thresholds,
        thresholds_source,
    })
}

fn build_judge(setup: &Setup, options: &Options) -> Result<Box<dyn Judge>, String> {
    match setup.judge.provider.as_str() {
        "typesafe" => {
            let key = std::env::var(&setup.judge.api_key_env)
                .map_err(|_| format!("{} is not set", setup.judge.api_key_env))?;
            Ok(Box::new(
                TypeSafeJudge::new(&setup.judge, &key).map_err(|e| e.to_string())?,
            ))
        }
        "recorded" => {
            let path = options
                .recording
                .as_ref()
                .ok_or("the recorded provider needs --recording <file>")?;
            Ok(Box::new(
                RecordedJudge::load(path).map_err(|e| e.to_string())?,
            ))
        }
        other => Err(format!("unknown provider {other}")),
    }
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

async fn evaluate(options: Options) -> Result<(), String> {
    let setup = setup(&options)?;
    let catalogs = Catalogs::embedded().map_err(|e| e.to_string())?;
    let languages: Vec<Language<'_>> = setup
        .languages
        .iter()
        .map(|code| {
            let name = catalogs.get(code).map(|c| c.name.as_str()).unwrap_or(code);
            Language { code, name }
        })
        .collect();
    let turn = TurnQuestions::new(&languages);
    let mut cases =
        case::load_dir(&options.cases, &options.lang, &turn).map_err(|e| e.to_string())?;
    if let Some(limit) = options.limit {
        cases.truncate(limit);
    }
    if cases.is_empty() {
        return Err(format!(
            "no {} cases in {}",
            options.lang,
            options.cases.display()
        ));
    }

    let judge = build_judge(&setup, &options)?;
    let runs = report::run(judge.as_ref(), &turn, cases)
        .await
        .map_err(|e| e.to_string())?;

    let header = report::Header {
        judge_id: judge.id(),
        question_set: turn.id(),
        lang: &options.lang,
        thresholds: &setup.thresholds,
        thresholds_source: setup.thresholds_source,
    };
    let text = report::render(&header, &runs);
    let report_path = options.report.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "eval/reports/{}/{}/{}.md",
            judge.id(),
            turn.id(),
            options.lang
        ))
    });
    write(&report_path, &text)?;
    println!("{text}");
    eprintln!("report written to {}", report_path.display());

    // A replayed run is already recorded; only live answers are saved.
    if options.record && setup.judge.provider != "recorded" {
        let recording = report::recording(judge.id(), turn.id(), &runs);
        let json = serde_json::to_string_pretty(&recording).map_err(|e| e.to_string())?;
        let path = options.record_to.clone().unwrap_or_else(|| {
            PathBuf::from(format!(
                "eval/recorded/{}/{}/{}.json",
                judge.id(),
                turn.id(),
                options.lang
            ))
        });
        write(&path, &(json + "\n"))?;
        eprintln!("answers recorded to {}", path.display());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match evaluate(options).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("eval: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Options, String> {
        parse_args(list.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn arguments_are_parsed() {
        let options = args(&[
            "--lang",
            "es",
            "--limit",
            "10",
            "--model",
            "jev-2",
            "--no-record",
        ])
        .unwrap_or_default();

        assert_eq!(options.lang, "es");
        assert_eq!(options.limit, Some(10));
        assert_eq!(options.model.as_deref(), Some("jev-2"));
        assert!(!options.record);
        assert_eq!(options.cases, PathBuf::from("eval/golden"));
    }

    #[test]
    fn bad_arguments_are_refused() {
        assert!(args(&[]).is_err(), "--lang is required");
        assert!(args(&["--lang"]).is_err(), "a flag without its value");
        assert!(args(&["--lang", "es", "--limit", "ten"]).is_err());
        assert!(args(&["--lang", "es", "--verbose"]).is_err());
    }
}
