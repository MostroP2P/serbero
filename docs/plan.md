# Implementation plan

Serbero is built in six phases. Every task is one pull request: small enough to
review in one sitting, with its own tests, and leaving `main` releasable.
Tasks marked **(trivial)** may be grouped with their neighbours in one PR.

Conventions for every task (see [AGENTS.md](../AGENTS.md)):

- PR title: conventional commit plus task id, for example
  `feat(store): add migration runner [T0.5]`.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
  `cargo test` pass.
- New dependencies are added with `cargo add` at their latest version.
- If a task changes behavior described in `docs/`, the docs change in the same
  PR.

## Overview

```text
Phase 0  Foundation ──▶ Phase 1  Notifier (release v0.1)
                    ├─▶ Phase 2  Mostro chat transport ─────────┐
                    ├─▶ Phase 3  Judge + evaluation harness ────┼─▶ Phase 5  Assisted mediation ─▶ Phase 6  Release v1.0
                    └─▶ Phase 4  Policy + messages (pure) ──────┘
```

Phases 2, 3 and 4 depend mostly on Phase 0 and can proceed in parallel; the
table rows list the exact dependencies. Phase 1 ships on its own and is useful
before any mediation exists.

---

## Phase 0 — Foundation

Goal: an empty but well-formed daemon that loads config, opens its database,
and runs CI.

| ID | Task | Depends on | Done when |
|---|---|---|---|
| T0.1 | **Scaffold (trivial).** Edition 2024, `rustfmt.toml`, `clippy.toml`, MIT `LICENSE`, `README.md` stub, `.gitignore` (target, `*.db`, `config.toml`). | — | `cargo build` passes; README links to `docs/`. |
| T0.2 | **CI (trivial).** GitHub Actions running fmt, clippy (`-D warnings`) and tests on push and PR, with the latest stable toolchain. | T0.1 | CI is green on `main`. |
| T0.3 | **Errors and logging (trivial).** `Error` enum with `thiserror`; `tracing-subscriber` initialised from `log_level` or `SERBERO_LOG`. | T0.1 | Unit test for error display; binary logs a startup line. |
| T0.4 | **Config.** `config` module: parse `config.toml` ([spec.md §9](spec.md#9-configuration)), env overrides for secrets, duration parsing, validation with actionable errors; `config.sample.toml`. | T0.3 | Tests: valid sample loads; each invalid case (missing key env, unknown language, bad pubkey, bad duration) fails with a clear message. |
| T0.5 | **Store: migration runner.** Open SQLite (bundled), WAL mode, `schema_version` table, ordered idempotent migrations. | T0.3 | Tests: fresh DB migrates; re-running is a no-op. |
| T0.6 | **Store: migration 1.** `disputes` and `events` tables with typed insert and query functions. | T0.5 | Tests for insert-or-ignore, lifecycle updates, and event append. |

## Phase 1 — Notifier

Goal: every dispute reaches every solver, with reminders, assignment and
resolution tracking. Release **v0.1.0** at the end of the phase.

| ID | Task | Depends on | Done when |
|---|---|---|---|
| T1.1 | **Nostr client.** Load keys from env, connect to configured relays, graceful shutdown. | T0.4 | Starts and stops cleanly against a local relay (integration test or documented manual check). |
| T1.2 | **Solver DM sender.** NIP-17 / NIP-59 gift-wrapped DM helper; every attempt recorded in `events`. | T1.1, T0.6 | Test: DM builds and unwraps to the expected rumor; failed sends are recorded. |
| T1.3 | **Dispute event parser.** Pure parsing of `kind 38386` tags (`d`, `s`, `initiator`, `y`, `p`) into a typed `DisputeEvent`. | T0.1 | Table-driven tests, including malformed and foreign-Mostro events. |
| T1.4 | **Detection.** Subscription filter, dispatch by status, insert-or-ignore into `disputes`. | T1.1, T1.3, T0.6 | Test: replayed event is a no-op; foreign `y` is ignored. |
| T1.5 | **New-dispute notification.** DM every solver ([messages.md §3](messages.md#3-solver-messages)); move to `notified` when one send succeeds. | T1.2, T1.4 | Tests: all solvers targeted; lifecycle and events correct on partial failure. |
| T1.6 | **Reminder timer.** Periodic task re-notifying disputes still `notified` after `renotify_after`. | T1.5 | Test with a controllable clock: fires once per interval, never for taken disputes. |
| T1.7 | **Taken.** On `s = in-progress`, store the assigned solver and send the taken DM. | T1.5 | Test: idempotent on replay; reminders stop. |
| T1.8 | **Resolved.** On terminal statuses, move to `resolved`. | T1.7 | Test for each terminal status. |
| T1.9 | **Operator docs and release (trivial).** README: install, configure, run, `sqlite3` recipes; tag v0.1.0. | T1.8 | A new operator can run Serbero from the README alone. |

## Phase 2 — Mostro chat transport

Goal: Serbero can take a dispute and exchange messages with both parties.

| ID | Task | Depends on | Done when |
|---|---|---|---|
| T2.1 | **Store: migration 2.** `sessions` and `messages` tables ([spec.md §8](spec.md#8-data-model)) with typed functions, including the one-live-session index. | T0.6 | Tests: a second live session for the same dispute is rejected; message dedup by `inner_event_id`. |
| T2.2 | **Shared-key derivation.** ECDH between Serbero's key and a trade pubkey, compatible with Mostro clients. | T0.1 | Test vector cross-checked against a Mostro client implementation. |
| T2.3 | **Take dispute.** Send `AdminTakeDispute`; await `AdminTookDispute` (or `CantDo`) with timeout; parse `SolverDisputeInfo`. | T1.1 | Test with recorded Mostro responses; timeout and `CantDo` return typed errors. |
| T2.4 | **Outbound party message.** Send text to a party's shared pubkey and persist it in `messages`. | T2.1, T2.2 | Test: the event decrypts with the party-side key; the row is persisted with `template_id` and `lang`. |
| T2.5 | **Inbound party messages.** One live subscription for all active shared pubkeys; unwrap, authenticate the author against the trade pubkey, dedup, count attachments, persist. | T2.4 | Tests: forged author rejected, duplicates ignored, attachments counted and not stored. |
| T2.6 | **Human takeover.** Detect a human solver taking the dispute and mark the session `superseded`; nothing more is sent. | T1.7, T2.1 | Mechanism verified against `mostrod`; test: no outbound after supersession. |
| T2.7 | **Restart resume.** Re-derive keys from stored trade pubkeys and rebuild the subscription at startup. | T2.5 | Test: after restart, inbound messages for live sessions are received and deduplicated. |
| T2.8 | **Staging check (trivial).** Documented manual run on a test Mostro: take, send, receive, human takeover. | T2.7 | Checklist in `docs/staging.md` completed and linked in the PR. |

## Phase 3 — Judge and evaluation harness

Goal: a provider-neutral judge, with Jev as its first adapter, answering the
question set reliably in English and Spanish, measured on a golden set.

| ID | Task | Depends on | Done when |
|---|---|---|---|
| T3.1 | **Judge contract.** Provider-neutral `Question`, `Answer`, `QuestionSet`, `Capabilities`, `JudgeError` and the `Judge` trait ([spec.md §5.1](spec.md#51-judge-providers)); confidence computed from probabilities; `[judge]` config with per-provider thresholds. | T0.4 | Tests for confidence, capability validation of a question set, and config parsing. |
| T3.2 | **RecordedJudge (trivial).** `recorded` provider replaying stored answers keyed by judge id and case id. | T3.1 | Test: recorded answers round-trip. |
| T3.3 | **TypeSafe adapter.** `typesafe` provider for Jev: wire format translation, bearer auth from env, timeout, error mapping (429, 529, network → `Unavailable`; 401 → `Unauthorized`; 422 → `InvalidRequest`), exponential backoff, capabilities. | T3.1 | Tests against a mock HTTP server for each status code and answer type; no TypeSafe type is visible outside the adapter module. |
| T3.4 | **State builder.** Build the judge state from session and messages ([judgments.md §1](judgments.md#1-state)): roles, order, ids, `latest`, truncation, attachments; no pubkeys. | T2.1 | Snapshot tests; test that no pubkey or event id can appear in the state. |
| T3.5 | **Question set qs-1.** Turn questions, per-party questions (omitted when `latest` is empty), the guiding question, and language options from config; `QUESTION_SET_VERSION`; snapshot hash test. | T3.2 | Serialized JSON matches [judgments.md §2](judgments.md#2-turn-request); changing a string without a version bump fails. |
| T3.6 | **Facts.** Convert answers to `Facts` with thresholds ([judgments.md §3](judgments.md#3-from-answers-to-facts)). | T3.5 | Table-driven tests at, below and above each threshold. |
| T3.7 | **Eval binary.** `cargo run --bin eval`: load golden cases, call the configured judge (or `--provider`/`--model`), score labels, compute precision, recall, coverage and ECE, write a report, save answers for `RecordedJudge`. | T3.6, T3.3 | Runs on a sample of 10 cases and produces a report. |
| T3.8 | **Golden set: English.** About 600 labelled cases covering every option ([evaluation.md §1](evaluation.md#1-golden-set)). May be split into one PR per question group. | T3.7 | Cases validate against the schema; first report committed. |
| T3.9 | **Golden set: Spanish.** Same coverage, written by a native speaker, including informal and regional phrasing. | T3.7 | Same as T3.8. |
| T3.10 | **Calibration.** Choose thresholds for `typesafe/jev-<version>` from the reports ([evaluation.md §3](evaluation.md#3-choosing-thresholds)); commit them to the config defaults. | T3.8, T3.9 | en and es meet every target; the report is committed. If a target is missed, the question is revised (new version) and this task repeats. |
| T3.11 | **Brief questions.** Quote-selection choices over message ids and `evidence_balance` ([judgments.md §5](judgments.md#5-brief-request)), with golden cases. | T3.5 | Quotes are always ids of messages from the right party; eval report for brief questions committed. |

## Phase 4 — Policy and messages

Goal: all product logic as pure, exhaustively tested code.

| ID | Task | Depends on | Done when |
|---|---|---|---|
| T4.1 | **Message catalog: English.** `messages/catalog.toml` with every template ([messages.md §2](messages.md#2-party-templates)), loader embedded at build time, `{amount}` rendering and `_noamount` forms. | T0.1 | Tests: every template renders; the missing-amount path works. |
| T4.2 | **Catalog: Spanish and Portuguese.** Human translations, reviewed by native speakers. | T4.1 | Reviewer named in the PR. |
| T4.3 | **Catalog rule tests (trivial).** Every language present, fund-action words only in `guide_*`, no verdict words, length limit ([messages.md §4](messages.md#4-template-rules)). | T4.2 | Tests fail when a rule is broken (checked with a deliberately bad fixture). |
| T4.4 | **Decision table.** `policy::decide` implementing [judgments.md §4](judgments.md#4-decision-table) in row order. | T3.6 | One test per row, plus tests proving row precedence. |
| T4.5 | **Next question.** [judgments.md §4.1](judgments.md#41-next-question-for-each-party): message-kind handling, needed-fact templates, never-repeat, language resend, round counting. | T4.4, T4.1 | Table-driven tests for every branch. |
| T4.6 | **Timers.** Pure functions deciding reminders, `unresponsive`, `flood` and `self_resolution_stalled` from session state and a clock. | T4.4 | Tests with a fixed clock for each timer. |
| T4.7 | **Solver renderers.** Brief, transcript DM, post-handoff update, assisting notice, final report ([messages.md §3](messages.md#3-solver-messages)). | T4.1, T3.11 | Snapshot tests; quotes are verbatim; no pubkeys appear. |

## Phase 5 — Assisted mediation

Goal: complete sessions on a test Mostro — self-resolution paths, escalation,
and every degraded case.

| ID | Task | Depends on | Done when |
|---|---|---|---|
| T5.1 | **Store: migration 3 (trivial).** `evaluations` table and typed functions. | T2.1 | Tests for insert and replay query. |
| T5.2 | **Eligibility and opening.** Deterministic eligibility ([spec.md §7.1](spec.md#71-eligibility)), take, opener in `default_language`, assisting notice to solvers. | T2.3, T2.4, T4.1, T4.7 | Test: an ineligible dispute is never taken; an opening failure leaves plain notification intact. |
| T5.3 | **Turn loop.** Settle with `quiet_period`, build state, call the judge, persist the evaluation, decide, act. | T5.1, T5.2, T3.4, T4.5 | Session-script tests with `RecordedJudge`: burst messages become one turn; the language switch works. |
| T5.4 | **Self-resolution paths.** Send `guide_*` templates, enter `guiding`, watch the dispute status, send `resolved_thanks` and close on resolution. | T5.3, T1.8 | Scripts: `PaymentArrived` resolved by release; `PaymentNotSent` resolved by cooperative cancel; a buyer-only claim never triggers guidance. |
| T5.5 | **Handoff.** Brief request, brief and transcript DMs, `handoff_notice`, recipient selection, forwarding new messages after handoff. | T5.3, T4.7 | Scripts for each handoff reason in [spec.md §7.6](spec.md#76-handoff-reasons). |
| T5.6 | **Timer task.** Wire the pure timers into a periodic task. | T5.3, T4.6 | Scripts: reminder then `unresponsive`; `self_resolution_stalled`. |
| T5.7 | **Degraded paths.** `judge_unavailable` handoff with transcript; mediation disabled when the startup health check fails, the question set exceeds the provider's capabilities, or no thresholds exist for the configured provider and model. | T5.5 | Tests with a failing judge; notification continues unaffected. |
| T5.8 | **Staging run.** Scripted scenarios with the Jev adapter on a test Mostro in English and Spanish; results recorded in `docs/staging.md`. | T5.7, T3.10 | Every scenario behaves as specified; findings are filed as issues. |

## Phase 6 — Release

| ID | Task | Depends on | Done when |
|---|---|---|---|
| T6.1 | **Solver feedback.** Parse `wrong <fact>` replies to a brief and record them as events. | T5.5 | Test: feedback is linked to the right evaluation. |
| T6.2 | **Operator reports (trivial).** Weekly-report and cost `sqlite3` scripts; README monitoring section. | T5.8 | Scripts run against a staging database. |
| T6.3 | **Release pipeline.** Tagged builds producing binaries for the supported platforms; `CHANGELOG.md`. | T0.2 | A tag produces release artifacts. |
| T6.4 | **Release v1.0.0 (trivial).** Mediation documented as opt-in; English and Spanish enabled. | T6.1–T6.3 | Tag pushed; release notes published. |
| T6.5 | **Portuguese.** Golden set, evaluation, then enable `pt`. | T6.4 | pt meets every target in [evaluation.md §2](evaluation.md#2-metrics-and-targets). |
| T6.6 | **Dependency updates (trivial).** Automated dependency-update PRs (Dependabot or Renovate), keeping crates on their latest versions. | T0.2 | Configuration merged; the first update PR is green. |
