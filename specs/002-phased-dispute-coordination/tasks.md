# Tasks: Phased Dispute Coordination (Phases 1–2)

**Input**: Design documents from `/specs/002-phased-dispute-coordination/`
**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md, contracts/, quickstart.md

**Tests**: Included. `plan.md` explicitly specifies a Testing Strategy with
unit tests (inline `#[cfg(test)]` modules) and integration tests under
`tests/` covering Phase 1 and Phase 2 scenarios.

**Organization**: Tasks are grouped by user story. Only Phase 1 (User
Story 1, P1) and Phase 2 (User Story 2, P2) are in scope for this
implementation. Phase 3–5 user stories (US3, US4, US5) are out of
scope and covered by future specification amendments.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2)
- Include exact file paths in descriptions

## Path Conventions

Single Rust binary crate at repository root. Source under `src/`,
integration tests under `tests/`, unit tests inline via
`#[cfg(test)]` modules in their respective source files (per
`plan.md` project structure).

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Initialize the Rust binary crate and baseline tooling.

- [X] T001 Create `Cargo.toml` at repository root declaring a binary crate `serbero` (edition 2021) with dependencies pinned per `plan.md`: `nostr-sdk = "0.44.1"`, `mostro-core = "0.8.4"`, `rusqlite` (with `bundled` feature), `tokio` (with `full` feature), `serde` (with `derive`), `toml`, `tracing`, `tracing-subscriber` (with `env-filter`), `thiserror`, `anyhow`
- [X] T002 [P] Add/extend `.gitignore` at repository root to ignore `/target`, `*.db`, `*.db-journal`, and local operator config overrides (e.g., `config.local.toml`)
- [X] T003 [P] Add `rustfmt.toml` (defaults + edition 2021) and a `clippy.toml` at repository root; enable `#![deny(rust_2018_idioms, unused_must_use)]` in `src/main.rs`
- [X] T004 Create the `src/` module skeleton per `plan.md` §Project Structure: `src/main.rs`, `src/config.rs`, `src/daemon.rs`, `src/dispatcher.rs`, `src/error.rs`, `src/nostr/mod.rs`, `src/nostr/client.rs`, `src/nostr/subscriptions.rs`, `src/nostr/notifier.rs`, `src/handlers/mod.rs`, `src/handlers/dispute_detected.rs`, `src/handlers/dispute_updated.rs`, `src/db/mod.rs`, `src/db/migrations.rs`, `src/db/disputes.rs`, `src/db/notifications.rs`, `src/db/state_transitions.rs`, `src/models/mod.rs`, `src/models/config.rs`, `src/models/dispute.rs`, `src/models/notification.rs` — each stubbed with a module declaration and TODO placeholder so `cargo check` succeeds

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Shared infrastructure that MUST be complete before any
user story can be implemented: error types, configuration,
persistence baseline, Nostr client baseline, dispatcher skeleton,
daemon loop, and logging.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [X] T005 Implement `src/error.rs` with a top-level `Error` enum using `thiserror`, covering `Config`, `Db`, `Nostr`, `Notification`, and `Io` variants; define `pub type Result<T> = std::result::Result<T, Error>`
- [X] T006 [P] Implement `src/models/config.rs` typed config structs: `Config { serbero, mostro, relays: Vec<RelayConfig>, solvers: Vec<SolverConfig>, timeouts }`, `SerberoConfig { private_key: String (hex), db_path: String, log_level: String }`, `MostroConfig { pubkey: String (hex) }`, `RelayConfig { url: String }`, `SolverConfig { pubkey: String (hex), permission: SolverPermission }`, `SolverPermission::{Read, Write}` (serde rename_all = "lowercase"), `TimeoutsConfig { renotification_seconds: u64, renotification_check_interval_seconds: u64 }` — all with `serde::Deserialize` derives
- [X] T007 [P] Implement `src/models/dispute.rs` with `Dispute { dispute_id, event_id, mostro_pubkey, initiator_role, dispute_status, event_timestamp, detected_at }` matching `data-model.md` §disputes; add `InitiatorRole::{Buyer, Seller}` and `DisputeStatus::{Initiated, InProgress}` enums (do NOT add Phase 2 lifecycle columns here yet — added in US2)
- [X] T008 [P] Implement `src/models/notification.rs` with `NotificationRecord { id, dispute_id, solver_pubkey, sent_at, status, error_message, notif_type }`, `NotificationStatus::{Sent, Failed}`, `NotificationType::{Initial, ReNotification, Assignment, Escalation}` matching `data-model.md` §notifications
- [X] T009 Implement `src/config.rs::load_config(path: &Path) -> Result<Config>`: read `config.toml` from the given path (defaulting to `./config.toml`), parse via `toml`, apply env overrides for `SERBERO_PRIVATE_KEY` → `serbero.private_key`, `SERBERO_DB_PATH` → `serbero.db_path`, `SERBERO_LOG` → `serbero.log_level` (per `plan.md` §Configuration Surface). No CLI flag surface is committed — env + file only
- [X] T010 Implement `src/db/mod.rs::open_connection(path: &str) -> Result<rusqlite::Connection>` applying pragmas `foreign_keys=ON` and `journal_mode=WAL`
- [X] T011 Implement `src/db/migrations.rs::run_migrations(&Connection)` creating Phase 1 tables exactly per `data-model.md` §Phase 1 Tables: `disputes` (PK `dispute_id`, UNIQUE `event_id`, `mostro_pubkey`, `initiator_role`, `dispute_status` DEFAULT 'initiated', `event_timestamp`, `detected_at`) and `notifications` (INTEGER PK AUTOINCREMENT, FK `dispute_id` → `disputes.dispute_id`, `solver_pubkey`, `sent_at`, `status`, `error_message` NULL, `notif_type` DEFAULT 'initial'). Use a `schema_version` table for idempotent migrations; only Phase 1 schema here (Phase 2 migration added in US2)
- [X] T012 [P] Implement `src/nostr/client.rs::build_client(config) -> Result<Client>`: construct `Keys` from the hex-encoded private key, create an `nostr_sdk::Client`, add every `RelayConfig::url` from the config, call `client.connect().await`; rely on nostr-sdk's built-in auto-reconnect (per `research.md` R-001 verification points — confirm exact API at implementation time)
- [X] T013 [P] Implement `src/nostr/subscriptions.rs::phase1_filter(mostro_pubkey: &str) -> Filter`: `Kind::Custom(38386)`, custom tag filters for `#z=dispute`, `#y=<mostro_pubkey>`, `#s=initiated` (use the SDK's single-letter-tag filter API confirmed in R-001 verification)
- [X] T014 [P] Implement `src/nostr/notifier.rs::send_gift_wrap_notification(client: &Client, receiver_pubkey: &PublicKey, message: &str) -> Result<()>`: use the nostr-sdk 0.44.1 private-messaging helper that produces NIP-17/NIP-59 gift-wrapped output (per `research.md` R-002). If the expected `send_private_msg`-style helper has moved or been renamed in 0.44.1, use the equivalent SDK-supported private-messaging path and update R-002 accordingly; do NOT hand-roll NIP-59 wrapping
- [X] T015 Implement `src/dispatcher.rs::dispatch(event: &Event, ctx: &DispatchContext)`: accepts incoming events and routes kind 38386 events by the `s` tag — Phase 1 only wires the `s=initiated` branch to `handlers::dispute_detected::handle` (unimplemented stub for now); unknown kinds / unknown `s` values are logged at debug and skipped without error
- [X] T016 Implement `src/daemon.rs::run(config: Config) -> Result<()>` orchestration: open DB, run migrations, build Nostr client, subscribe using `phase1_filter`, enter the nostr-sdk notification-handling loop, call `dispatcher::dispatch` for each `RelayPoolNotification::Event`; propagate graceful shutdown
- [X] T017 Wire `src/main.rs`: `#[tokio::main] async fn main()`, initialize `tracing_subscriber` from `SERBERO_LOG` (falling back to `config.serbero.log_level`), load config via `config::load_config`, call `daemon::run`, install a `tokio::signal` handler for SIGINT/SIGTERM that triggers cooperative shutdown without crashing
- [X] T018 [P] Inline unit tests in `src/config.rs` (`#[cfg(test)] mod tests`): parse a full valid TOML fixture, assert every field populated; verify `SERBERO_PRIVATE_KEY`, `SERBERO_DB_PATH`, and `SERBERO_LOG` env overrides replace file values; assert malformed TOML yields a `Config` variant error
- [X] T019 [P] Inline unit tests in `src/dispatcher.rs` (`#[cfg(test)] mod tests`): feed a synthetic kind 38386 event with `s=initiated` and verify the dispute_detected handler is invoked; feed a kind 38386 event with an unknown `s` value and verify it is skipped without error; feed an unrelated kind and verify it is ignored

**Checkpoint**: Foundation ready — user story implementation can now begin.

---

## Phase 3: User Story 1 — Solver Notification (Priority: P1) 🎯 MVP

**Goal**: Detect newly initiated Mostro disputes on Nostr and send an
encrypted gift-wrap notification to every configured solver, with
SQLite-backed deduplication that survives relay replays, reconnects,
and daemon restarts.

**Independent Test**: With Serbero running against a local Nostr relay
and a mock solver client, publish a kind 38386 event whose `pubkey` is the configured Mostro pubkey, with
`s=initiated`, `z=dispute`, `y=["mostro", "optional-instance-name"]`, `d=<dispute_id>`,
`initiator=buyer`. Verify the mock solver receives a gift-wrap DM
within 30 seconds containing the dispute ID, initiator role, and
event timestamp. Publish the same event again and restart the daemon
pointed at the same SQLite file — verify the solver receives exactly
one notification in total.

### Tests for User Story 1

> Write the failing integration tests first, then implement the
> handler wiring until they pass. Unit tests are inline in the
> relevant source files.

- [X] T020 [P] [US1] Integration test `tests/phase1_detection.rs`: spin up a local test relay (or an in-process relay helper), start a mock solver `nostr_sdk::Client` that records incoming gift-wrap DMs, boot Serbero with a temp-file SQLite DB and a generated Mostro/solver keypair; publish a kind 38386 `s=initiated` event and assert the mock solver receives a gift-wrap DM within 30 seconds whose decrypted payload contains the dispute ID, initiator role (buyer/seller), and event timestamp
- [X] T021 [P] [US1] Integration test `tests/phase1_dedup.rs`: publish the same dispute event twice against a running Serbero; assert the mock solver received exactly one notification and the `disputes` table has exactly one row. Then stop Serbero, restart with the same `SERBERO_DB_PATH`, replay the event, and assert no additional notification is sent
- [X] T022 [P] [US1] Integration test `tests/phase1_failure.rs`: simulate relay drop/reconnect and assert Serbero resumes listening without manual intervention. Separately, force a notifier send failure (e.g., configure an invalid/unreachable solver pubkey alongside a valid one) and assert the `notifications` table records both a `status='sent'` row for the valid solver and a `status='failed'` row with a populated `error_message` for the invalid one, while the daemon keeps running
- [X] T023 [P] [US1] Inline unit tests in `src/db/disputes.rs` (`#[cfg(test)] mod tests`) against `rusqlite::Connection::open_in_memory()`: insert a Dispute, read it back by `dispute_id`, assert a second `insert_dispute` with the same `dispute_id` reports the duplicate outcome without raising an unhandled error
- [X] T024 [P] [US1] Inline unit tests in `src/db/notifications.rs` (`#[cfg(test)] mod tests`) against an in-memory connection: insert a `Sent` notification and a `Failed` notification with `error_message`; verify both rows are retrievable; verify the `notif_type` column defaults to `'initial'` when not specified and that the FK to `disputes` rejects orphaned inserts

### Implementation for User Story 1

- [X] T025 [US1] Implement `src/db/disputes.rs::insert_dispute(&Connection, &Dispute) -> Result<InsertOutcome>` where `InsertOutcome::{Inserted, Duplicate}`; use `INSERT ... ON CONFLICT(dispute_id) DO NOTHING` and inspect the change count to distinguish outcomes. Also implement `get_dispute(&Connection, &str) -> Result<Option<Dispute>>`
- [X] T026 [US1] Implement `src/db/notifications.rs::record_notification(&Connection, dispute_id, solver_pubkey, status: NotificationStatus, error_message: Option<&str>, notif_type: NotificationType) -> Result<()>` inserting one row into the `notifications` table
- [X] T027 [US1] Implement `src/handlers/dispute_detected.rs::handle(event, ctx)`: (a) parse `dispute_id` from the `d` tag, `initiator_role` from the `initiator` tag, `mostro_pubkey` from the `y` tag, and the event's `id` and `created_at`; (b) build a `Dispute` with `dispute_status = Initiated` and `detected_at = now`; (c) call `insert_dispute` — if `Duplicate`, log at debug and return; if the INSERT itself fails, log an error and return without notifying (no in-memory queue — per `plan.md:224` the dispute may not be notified unless observed again after persistence recovers); (d) on `Inserted`, build the notification payload (dispute ID, initiator role, event timestamp, short "new dispute requires attention" instruction) and for EVERY configured solver (regardless of `permission` — Phase 1 does not filter by permission per `plan.md` §Configuration Surface) call `nostr::notifier::send_gift_wrap_notification`; (e) record each attempt via `record_notification` with `notif_type = Initial` and the appropriate `Sent`/`Failed` status
- [X] T028 [US1] Wire `src/dispatcher.rs` so real kind 38386 `s=initiated` events call `handlers::dispute_detected::handle`; ensure `src/daemon.rs` subscribes using `phase1_filter` on startup
- [X] T029 [US1] Emit structured `tracing` spans/events across the detection path in `src/handlers/dispute_detected.rs` and `src/nostr/notifier.rs`: one span per dispute event with fields `dispute_id`, `initiator_role`, and child events for `detected`, `duplicate_skip`, `persistence_failed`, `notification_sent`, `notification_failed` (capturing `solver_pubkey` and error message). This satisfies FR-017 audit requirements for Phase 1
- [X] T030 [US1] Handle the "no solvers configured" startup path in `src/daemon.rs`: log a WARN once at startup, continue running, and have `dispute_detected` still persist detected disputes while skipping the notification loop (per `plan.md` degraded-mode table and `spec.md` edge case at line 330)

**Checkpoint**: User Story 1 (Phase 1 MVP) is fully functional and independently testable — ship candidate.

---

## Phase 4: User Story 2 — Dispute Assignment Visibility (Priority: P2)

**Goal**: Track dispute lifecycle state transitions (new → notified →
taken → resolved), detect solver assignment via `s=in-progress`
events, send assignment notifications to all solvers, re-notify
unattended disputes after a configurable timeout, and suppress
further notifications once a dispute is taken.

**Independent Test**: With US1 running, publish an `s=in-progress`
event for a previously detected dispute. Verify Serbero transitions
the dispute to `taken`, records `assigned_solver`, sends an
assignment notification to every configured solver, and the
re-notification timer no longer re-notifies that dispute. Separately,
with a short `renotification_seconds`, leave a dispute in `notified`
past the timeout and verify exactly one re-notification fires with
`notif_type='re-notification'`.

### Tests for User Story 2

- [X] T031 [P] [US2] Integration test `tests/phase2_lifecycle.rs`: walk a dispute through `new` → `notified` → `taken` by driving real events through Serbero; assert the `disputes.lifecycle_state` column updates correctly and a corresponding row is appended to `dispute_state_transitions` for each transition (including the `trigger` and `transitioned_at` fields)
- [X] T032 [P] [US2] Integration test `tests/phase2_assignment.rs`: seed a dispute in `notified` state, publish a kind 38386 `s=in-progress` event for that dispute, and assert: `lifecycle_state='taken'`, `assigned_solver` populated, one assignment notification (`notif_type='assignment'`) sent to every configured solver, and the re-notification timer running in the background does not emit any further notifications for that dispute across at least two timer ticks
- [X] T033 [P] [US2] Integration test `tests/phase2_renotification.rs`: configure `renotification_seconds=2` and `renotification_check_interval_seconds=1`, seed a dispute in `notified` state with `last_notified_at` far enough in the past, and assert that within ~3 seconds exactly one re-notification with `notif_type='re-notification'` is emitted to each solver, `last_notified_at` is updated, and no second re-notification fires on the next tick (until the next timeout window elapses)
- [X] T034 [P] [US2] Inline unit tests for the lifecycle state machine in `src/models/dispute.rs` (`#[cfg(test)] mod tests`): assert allowed transitions (`New→Notified`, `Notified→Taken`, `Notified→Notified` for re-notify, `Taken→Waiting`, `Taken→Resolved`, etc. per `data-model.md` diagram) return Ok, and disallowed transitions (e.g., `Resolved→Notified`, `Taken→New`) return a typed error

### Implementation for User Story 2

- [X] T035 [US2] Extend `src/db/migrations.rs` with a Phase 2 migration (guarded by `schema_version`): `ALTER TABLE disputes ADD COLUMN lifecycle_state TEXT NOT NULL DEFAULT 'new'`, `ADD COLUMN assigned_solver TEXT`, `ADD COLUMN last_notified_at INTEGER`, `ADD COLUMN last_state_change INTEGER`; `CREATE TABLE dispute_state_transitions` exactly per `data-model.md` §Phase 2 Additions (INTEGER PK AUTOINCREMENT, FK `dispute_id`, `from_state` NULL, `to_state` NOT NULL, `transitioned_at` NOT NULL, `trigger` NULL)
- [X] T036 [US2] Implement `src/models/dispute.rs::LifecycleState` enum (`New`, `Notified`, `Taken`, `Waiting`, `Escalated`, `Resolved`) with `can_transition_to(&self, next: LifecycleState) -> bool` enforcing the transitions in `data-model.md` §State Machine
- [X] T037 [US2] Implement `src/db/state_transitions.rs`: `record_transition(&Connection, dispute_id, from: Option<LifecycleState>, to: LifecycleState, trigger: Option<&str>) -> Result<()>` and `list_unattended_disputes(&Connection, cutoff_ts: i64) -> Result<Vec<Dispute>>` returning disputes where `lifecycle_state = 'notified' AND last_notified_at < cutoff_ts`
- [X] T038 [US2] Extend `src/db/disputes.rs` with lifecycle helpers: `set_lifecycle_state(&Connection, dispute_id, new_state, trigger)` which performs the UPDATE and the `record_transition` INSERT within a single `rusqlite` transaction; `set_assigned_solver(&Connection, dispute_id, solver_pubkey)`; `update_last_notified_at(&Connection, dispute_id, ts)`
- [X] T039 [US2] Update `src/handlers/dispute_detected.rs` so a successful initial notification also transitions the dispute `New → Notified` via `set_lifecycle_state(..., trigger = Some("initial_notification"))` and sets `last_notified_at`; if no solvers are configured, leave the dispute in `New`
- [X] T040 [US2] Extend `src/nostr/subscriptions.rs` with `phase2_filters(mostro_pubkey)` that additionally subscribes to kind 38386 events with `s=in-progress` (scoped to `#y=<mostro_pubkey>`); update `src/daemon.rs` to use the Phase 2 filter set once US2 is active
- [X] T041 [US2] Implement `src/handlers/dispute_updated.rs::handle(event, ctx)` for `s=in-progress` events: look up the dispute; if the current `lifecycle_state` is already `Taken`, `Waiting`, `Escalated`, or `Resolved`, treat as idempotent no-op and log at debug; otherwise transition `→ Taken` via `set_lifecycle_state(..., trigger = Some(event.id))`, extract the assigned solver pubkey from the Mostro event (per Mostro's published tag shape for assignment events), persist via `set_assigned_solver`, and send an assignment notification to every configured solver with `notif_type = Assignment` — record each attempt via `record_notification`
- [X] T042 [US2] Extend `src/dispatcher.rs` to route kind 38386 events by `s` tag: `s=initiated → handlers::dispute_detected::handle`, `s=in-progress → handlers::dispute_updated::handle`
- [X] T043 [US2] Implement the re-notification timer in `src/daemon.rs`: spawn a `tokio::task` that ticks every `timeouts.renotification_check_interval_seconds`; on each tick compute `cutoff = now - renotification_seconds`, call `list_unattended_disputes(cutoff)`, and for each returned dispute re-send via `nostr::notifier::send_gift_wrap_notification` with `notif_type = ReNotification`, include current `lifecycle_state` and elapsed-since-creation in the payload, call `update_last_notified_at(..., now)`, and record each attempt in `notifications`
- [X] T044 [US2] Guarantee the re-notification timer never fires for disputes not in `Notified` (the SQL filter already enforces this) and does not double-send within a single tick — `update_last_notified_at` runs before the next iteration, and the query uses `<` on `last_notified_at`
- [X] T045 [US2] Extend `tracing` instrumentation for Phase 2: spans/events for `state_transition { dispute_id, from, to, trigger }`, `assignment_detected { dispute_id, assigned_solver }`, and `renotification_tick { scanned, re_notified }` — continuing to satisfy FR-017

**Checkpoint**: User Story 2 (Phase 2 coordination visibility) is fully functional and independently testable.

---

## Polish & Cross-Cutting Concerns

**Purpose**: Final validation and hygiene before Phase 3 planning.

- [X] T046 [P] Execute `quickstart.md` §Verify Phase 1 and §Verify Phase 2 end-to-end against the built `./target/release/serbero` binary (TOML config + env vars only, no CLI flags per `plan.md`); update `quickstart.md` if any command or observable output drifts
- [X] T047 [P] Run `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check`; fix findings
- [X] T048 [P] Run `cargo test` (unit + integration) and ensure all Phase 1 and Phase 2 tests pass; capture test output for the PR description
- [X] T049 Verify no reasoning-backend code has been scaffolded into `src/` (no `ReasoningBackend` trait, no `src/reasoning/` module) per `plan.md` §Reasoning Backend Interface (planning artifact only); if any sneaked in during implementation, remove it
- [X] T050 Manual observability pass: run the daemon through a full US1 + US2 lifecycle (detection → initial notification → re-notification → assignment → suppression) and review the `tracing` output and the `disputes`, `notifications`, and `dispute_state_transitions` tables to confirm FR-017 audit coverage — sufficient information to reconstruct each action, state transition, and notification attempt for operator oversight, debugging, and postmortem analysis

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on Setup completion — BLOCKS all user stories.
- **User Story 1 (Phase 3)**: Depends on Foundational — standalone Phase 1 MVP.
- **User Story 2 (Phase 4)**: Depends on User Story 1 (extends Phase 1 schema, handlers, and subscriptions).
- **Polish (future work)**: Depends on both user stories being complete.

### User Story Dependencies

- **User Story 1 (P1)**: Can start once Foundational is complete. No dependencies on other stories.
- **User Story 2 (P2)**: Extends User Story 1. Not independent of US1 — the Phase 2 schema, lifecycle state machine, and assignment handler build on Phase 1's `disputes` and `notifications` tables and the Phase 1 dispatcher wiring. Per `plan.md` §Phased Implementation Order, Phase 2 explicitly "extends Phase 1".

### Within Each User Story

- Integration tests SHOULD be written first and initially fail.
- Models and DB helpers before handlers.
- Handlers before dispatcher wiring.
- Dispatcher wiring before the daemon loop enables the new filter set.

### Parallel Opportunities

- All `[P]`-marked Setup tasks (T002, T003) can run in parallel.
- All `[P]`-marked Foundational tasks (T006, T007, T008, T012, T013, T014, T018, T019) can run in parallel — they touch different files and have no cross-dependencies within Foundational.
- Within US1: tests T020, T021, T022, T023, T024 are all `[P]` and target different files.
- Within US2: tests T031, T032, T033, T034 are all `[P]` and target different files.
- All Polish tasks T046, T047, T048 can run in parallel.

---

## Parallel Example: User Story 1 Tests

```bash
# Launch all US1 integration tests in parallel (different files, no ordering):
Task: "Integration test tests/phase1_detection.rs"
Task: "Integration test tests/phase1_dedup.rs"
Task: "Integration test tests/phase1_failure.rs"

# Launch inline unit tests in parallel:
Task: "Inline unit tests in src/db/disputes.rs"
Task: "Inline unit tests in src/db/notifications.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1 Setup (T001–T004).
2. Complete Phase 2 Foundational (T005–T019) — CRITICAL, blocks all stories.
3. Complete Phase 3 User Story 1 (T020–T030).
4. **STOP and VALIDATE**: run `quickstart.md` §Verify Phase 1 end-to-end.
5. Ship Phase 1 as the MVP.

### Incremental Delivery

1. Setup + Foundational → foundation ready.
2. Add User Story 1 → verify Phase 1 acceptance criteria (SC-001 through SC-006) → ship Phase 1.
3. Add User Story 2 → verify Phase 2 success criteria (SC-007, SC-008, SC-009) → ship Phase 2.
4. Run Polish phase before tagging the Phase 2 release.

### Out of Scope for This Tasks File

Phases 3 and 4, plus adapter improvements (guided mediation, escalation support, reasoning
backend) are explicitly out of scope. User Stories 3, 4, and 5 in
`spec.md` and the `contracts/reasoning-backend.md` interface are
planning artifacts only for this implementation round — no
corresponding tasks are generated here. Future phases will be covered
by separate specification amendments.

---

## Notes

- `[P]` tasks = different files, no dependencies on incomplete tasks.
- `[Story]` label maps tasks to their user story for traceability.
- Phase 1 intentionally notifies ALL configured solvers regardless of `permission` — permission-based routing is a later-phase concern (`plan.md` §Configuration Surface, Phase 4 escalation).
- Phase 1 has no in-memory notification queue — if the initial SQLite INSERT fails, the dispute may not be notified unless the event is observed again after persistence recovers (`plan.md:224`, `spec.md:50`).
- `nostr-sdk` v0.44.1 API shapes for custom tag filters and private-messaging helpers are pinned as verification points in `research.md` R-001 and R-002 — confirm exact signatures during implementation rather than assuming historical names.
- Phase 3+ schema in `data-model.md` is forward-looking and provisional. Do NOT create those tables as part of this tasks file.
- No `ReasoningBackend` trait or `src/reasoning/` module is added in this implementation round.
